[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$AssetRoot,

    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$')]
    [string]$Version
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$resolvedAssetRoot = [IO.Path]::GetFullPath($AssetRoot)
if (-not (Test-Path -LiteralPath $resolvedAssetRoot -PathType Container)) {
    throw "GitHub Release asset directory does not exist: $resolvedAssetRoot"
}

$expectedNames = @(
    "RemoteOps-Windows-x64-$Version.zip",
    "RemoteOps-MCP-Windows-x64-$Version.zip",
    "RemoteOps-MCP-macOS-arm64-$Version.tar.gz",
    "RemoteOps-Relay-Linux-x64-$Version.tar.gz",
    'SHA256SUMS.txt'
)
$actualFiles = @(Get-ChildItem -LiteralPath $resolvedAssetRoot -File)
$unexpected = @($actualFiles.Name | Where-Object { $_ -notin $expectedNames })
$missing = @($expectedNames | Where-Object { $_ -notin $actualFiles.Name })
$directories = @(Get-ChildItem -LiteralPath $resolvedAssetRoot -Directory)

if ($unexpected.Count -gt 0) {
    throw "GitHub Release asset directory contains unexpected files: $($unexpected -join ', ')"
}
if ($missing.Count -gt 0) {
    throw "GitHub Release asset directory is missing files: $($missing -join ', ')"
}
if ($directories.Count -gt 0) {
    throw "GitHub Release asset directory must not contain subdirectories: $($directories.Name -join ', ')"
}

$checksumPath = Join-Path $resolvedAssetRoot 'SHA256SUMS.txt'
$checksumLines = @(Get-Content -LiteralPath $checksumPath | Where-Object { $_.Trim().Length -gt 0 })
$archiveNames = @($expectedNames | Where-Object { $_ -ne 'SHA256SUMS.txt' })
if ($checksumLines.Count -ne $archiveNames.Count) {
    throw "SHA256SUMS.txt must contain exactly $($archiveNames.Count) lines."
}

$seenNames = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
foreach ($line in $checksumLines) {
    if ($line -notmatch '^(?<hash>[0-9a-fA-F]{64})  (?<name>[^/\\]+)$') {
        throw "Invalid SHA256SUMS.txt line: $line"
    }
    $name = $Matches.name
    if ($name -notin $archiveNames) {
        throw "SHA256SUMS.txt contains an unpublished file: $name"
    }
    if (-not $seenNames.Add($name)) {
        throw "SHA256SUMS.txt contains a duplicate file entry: $name"
    }
    $actualHash = (Get-FileHash -LiteralPath (Join-Path $resolvedAssetRoot $name) -Algorithm SHA256).Hash
    if (-not $actualHash.Equals($Matches.hash, [StringComparison]::OrdinalIgnoreCase)) {
        throw "SHA-256 mismatch: $name"
    }
}

if ($seenNames.Count -ne $archiveNames.Count) {
    throw 'SHA256SUMS.txt does not cover every release archive.'
}

$requiredLegalFiles = @('LICENSE', 'THIRD_PARTY_LICENSES.txt', 'DEPENDENCIES.json')
$forbiddenEntryPattern = '(?i)(?:\.log|\.dmp|\.pdb|\.key|\.pem|\.pfx|\.p12|\.token|\.secret)$'
foreach ($archiveName in $archiveNames) {
    $archivePath = Join-Path $resolvedAssetRoot $archiveName
    if ($archiveName.EndsWith('.zip', [StringComparison]::OrdinalIgnoreCase)) {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
        try {
            $entryNames = @($archive.Entries.FullName)
        }
        finally {
            $archive.Dispose()
        }
    }
    else {
        $entryNames = @(& tar -tzf $archivePath)
        if ($LASTEXITCODE -ne 0) {
            throw "Unable to read release archive: $archiveName"
        }
    }

    foreach ($legalFile in $requiredLegalFiles) {
        $hasLegalFile = $entryNames | Where-Object {
            [IO.Path]::GetFileName($_) -ceq $legalFile
        }
        if (-not $hasLegalFile) {
            throw "Release archive is missing $legalFile`: $archiveName"
        }
    }
    $forbiddenEntries = @($entryNames | Where-Object { $_ -match $forbiddenEntryPattern })
    $forbiddenEntries += @($entryNames | Where-Object {
            $_ -match '(?i)(?:^|[\\/])(?:logs?|dumps?|crashdumps?)(?:[\\/]|$)'
        })
    if ($forbiddenEntries.Count -gt 0) {
        throw "Release archive contains forbidden files: $archiveName -> $($forbiddenEntries -join ', ')"
    }
}

Write-Host "GitHub Release asset gate passed: $Version"

