[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspaceRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$resolvedOutput = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $resolvedOutput | Out-Null

Push-Location $workspaceRoot
try {
    $metadataText = & cargo metadata --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) {
        throw 'Failed to read Cargo dependency metadata.'
    }
}
finally {
    Pop-Location
}

$metadata = ($metadataText -join "`n") | ConvertFrom-Json
$workspaceMembers = [Collections.Generic.HashSet[string]]::new(
    [StringComparer]::Ordinal
)
foreach ($member in $metadata.workspace_members) {
    [void]$workspaceMembers.Add([string]$member)
}

$dependencies = @(
    $metadata.packages |
        Where-Object { -not $workspaceMembers.Contains([string]$_.id) } |
        Sort-Object name, version |
        ForEach-Object {
            if ([string]::IsNullOrWhiteSpace($_.license)) {
                throw "Third-party dependency is missing an SPDX license: $($_.name) $($_.version)"
            }
            [PSCustomObject][ordered]@{
                name = $_.name
                version = $_.version
                license = $_.license
                source = $_.source
                repository = $_.repository
            }
        }
)

$noticeLines = [Collections.Generic.List[string]]::new()
$noticeLines.Add('RemoteOps third-party dependency license manifest')
$noticeLines.Add('Generated from Cargo.lock via cargo metadata --locked.')
$noticeLines.Add('This manifest records SPDX expressions and available bundled upstream license texts.')
$noticeLines.Add('')
foreach ($dependency in $dependencies) {
    $dependencyLine = '{0} {1} | {2} | {3}' -f @(
        $dependency.name
        $dependency.version
        $dependency.license
        $dependency.source
    )
    $noticeLines.Add($dependencyLine)
}
$noticeLines.Add('')
$noticeLines.Add('Bundled upstream license and notice texts')
$noticeLines.Add('=========================================')
foreach ($package in $metadata.packages | Where-Object {
        -not $workspaceMembers.Contains([string]$_.id)
    } | Sort-Object name, version) {
    $packageDirectory = Split-Path -Parent $package.manifest_path
    $licenseFiles = @(
        Get-ChildItem -LiteralPath $packageDirectory -File |
            Where-Object {
                $_.Name -match '(?i)^(?:LICENSE|COPYING|NOTICE|COPYRIGHT)(?:[._-].*)?$'
            } |
            Sort-Object Name
    )
    $noticeLines.Add('')
    $noticeLines.Add("----- $($package.name) $($package.version) -----")
    if ($licenseFiles.Count -eq 0) {
        $noticeLines.Add("No bundled license file found; SPDX: $($package.license)")
        continue
    }
    foreach ($licenseFile in $licenseFiles) {
        $noticeLines.Add("--- $($licenseFile.Name) ---")
        foreach ($line in Get-Content -LiteralPath $licenseFile.FullName) {
            $noticeLines.Add($line)
        }
    }
}

[IO.File]::WriteAllLines(
    (Join-Path $resolvedOutput 'THIRD_PARTY_LICENSES.txt'),
    $noticeLines,
    [Text.UTF8Encoding]::new($false)
)
[IO.File]::WriteAllText(
    (Join-Path $resolvedOutput 'DEPENDENCIES.json'),
    ($dependencies | ConvertTo-Json -Depth 4) + "`n",
    [Text.UTF8Encoding]::new($false)
)
Copy-Item -LiteralPath (Join-Path $workspaceRoot 'LICENSE') -Destination $resolvedOutput -Force

Write-Host "Generated third-party notices for $($dependencies.Count) dependencies: $resolvedOutput"

