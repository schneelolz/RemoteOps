[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$gate = Join-Path $PSScriptRoot 'Test-ReleaseArtifacts.ps1'
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('remoteops-artifact-gate-' + [Guid]::NewGuid().ToString('N'))
$manifestPath = Join-Path $fixtureRoot 'manifest.json'

function New-VersionFixture {
    param([string]$Executable, [string]$Version)
    # Add-Type alone omits native resources; Windows FileVersionInfo needs them.
    Add-Type -AssemblyName Microsoft.CodeAnalysis.CSharp
    $source = @"
using System.Reflection;
[assembly: AssemblyFileVersion("0.2.0.0")]
[assembly: AssemblyInformationalVersion("$Version")]
public static class ArtifactFixture { }
"@
    $compilation = [Microsoft.CodeAnalysis.CSharp.CSharpCompilation]::Create('ArtifactFixture')
    $compilation = $compilation.WithOptions(
        $compilation.Options.WithOutputKind([Microsoft.CodeAnalysis.OutputKind]::DynamicallyLinkedLibrary)
    )
    $compilation = $compilation.AddSyntaxTrees([Microsoft.CodeAnalysis.CSharp.CSharpSyntaxTree]::ParseText($source))
    $compilation = $compilation.AddReferences([Microsoft.CodeAnalysis.MetadataReference]::CreateFromFile([object].Assembly.Location))
    $resources = $compilation.CreateDefaultWin32Resources($true, $false, $null, $null)
    $stream = [IO.File]::Create($Executable)
    try {
        $result = $compilation.Emit($stream, $null, $null, $resources)
        if (-not $result.Success) {
            throw ($result.Diagnostics -join "`n")
        }
    }
    finally {
        $stream.Dispose()
        $resources.Dispose()
    }
}

function Write-FixtureManifest {
    param([object[]]$Entries)
    [IO.File]::WriteAllText($manifestPath, (ConvertTo-Json -InputObject $Entries -Depth 4))
}

function Assert-GateRejects {
    param([string]$ExpectedMessage)
    try {
        & $gate -ArtifactRoot $fixtureRoot
    }
    catch {
        if ($_.Exception.Message.Contains($ExpectedMessage)) {
            Write-Host "Rejected as expected: $ExpectedMessage"
            return
        }
        throw
    }
    throw "Artifact gate unexpectedly accepted fixture: $ExpectedMessage"
}

New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
try {
    # Real PE version resources allow this regression to run on Windows and Unix.
    # The preview numbers intentionally differ, matching independent components.
    $manifest = @(
        foreach ($component in @(
            @{ Package = 'remoteops-agent-gui'; Version = '0.2.0-preview.9' },
            @{ Package = 'remoteops-ssh-askpass'; Version = '0.2.0-preview.6' }
        )) {
            $executable = Join-Path $fixtureRoot ($component.Package + '.exe')
            New-VersionFixture -Executable $executable -Version $component.Version
            $item = Get-Item -LiteralPath $executable
            if (
                $item.VersionInfo.FileVersion -cne '0.2.0.0' -or
                $item.VersionInfo.ProductVersion -cne $component.Version
            ) {
                throw "Fixture is missing expected Windows version resources: $($component.Package)"
            }
            [PSCustomObject]@{
                File = $item.Name
                Package = $component.Package
                CargoVersion = $component.Version
                FileVersion = $item.VersionInfo.FileVersion
                ProductVersion = $item.VersionInfo.ProductVersion
                Length = $item.Length
                Sha256 = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
    )
    $baseline = ConvertTo-Json -InputObject $manifest -Depth 4
    Write-FixtureManifest $manifest
    & $gate -ArtifactRoot $fixtureRoot

    $manifest[1].CargoVersion = $manifest[0].CargoVersion
    Write-FixtureManifest $manifest
    Assert-GateRejects 'Windows 文件版本与自身 Cargo 版本不匹配'

    $manifest = @($baseline | ConvertFrom-Json)
    $manifest[1].CargoVersion = ''
    Write-FixtureManifest $manifest
    Assert-GateRejects 'manifest.json 中缺少 Cargo 版本'

    foreach ($field in @('FileVersion', 'ProductVersion')) {
        $manifest = @($baseline | ConvertFrom-Json)
        $manifest[1].$field = '0.0.0'
        Write-FixtureManifest $manifest
        Assert-GateRejects 'manifest.json 中的 Windows 文件版本不匹配'
    }

    $manifest = @($baseline | ConvertFrom-Json)
    $manifest[1].Sha256 = '0' * 64
    Write-FixtureManifest $manifest
    Assert-GateRejects 'manifest.json 中的 SHA-256 不匹配'

    $manifest = @($baseline | ConvertFrom-Json)
    $manifest[1].Length += 1
    Write-FixtureManifest $manifest
    Assert-GateRejects 'manifest.json 中的文件大小不匹配'

    $manifest = @($baseline | ConvertFrom-Json)
    Write-FixtureManifest @($manifest[0], $manifest[1], $manifest[1])
    Assert-GateRejects 'manifest.json 必须且只能包含一个 remoteops-ssh-askpass.exe 条目'

    Write-FixtureManifest @($manifest[0])
    Assert-GateRejects 'manifest.json 必须且只能包含一个 remoteops-ssh-askpass.exe 条目'

    Write-FixtureManifest $manifest
    Remove-Item -LiteralPath (Join-Path $fixtureRoot 'remoteops-ssh-askpass.exe')
    Assert-GateRejects 'Windows Agent GUI 发布目录缺少 remoteops-ssh-askpass.exe'

    Write-Host 'Release artifact gate regression checks passed.'
}
finally {
    Remove-Item -LiteralPath $fixtureRoot -Recurse -Force
}
