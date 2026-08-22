[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$ExecutablePath,
    [ValidateRange(1, 20)]
    [int]$DumpCount = 3,
    [switch]$Disable
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$imageName = 'remoteops-agent-gui.exe'
$registryPath = "HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\$imageName"
if ([string]::IsNullOrWhiteSpace($ExecutablePath)) {
    $ExecutablePath = Join-Path $PSScriptRoot $imageName
}

function Assert-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run PowerShell as administrator, then execute this script again.'
    }
}

if (-not $WhatIfPreference) {
    Assert-Administrator
}

if ($Disable) {
    if (Test-Path -LiteralPath $registryPath) {
        if ($PSCmdlet.ShouldProcess($registryPath, 'Remove the RemoteOps Agent GUI dump configuration')) {
            Remove-Item -LiteralPath $registryPath -Recurse -Force
        }
    }
    Write-Host 'The RemoteOps Agent GUI crash dump configuration is disabled.'
    return
}

$resolvedExecutable = [IO.Path]::GetFullPath($ExecutablePath)
if (-not (Test-Path -LiteralPath $resolvedExecutable -PathType Leaf)) {
    throw "Agent GUI does not exist: $resolvedExecutable"
}
if ([IO.Path]::GetFileName($resolvedExecutable) -ine $imageName) {
    throw "The executable file name must be $imageName"
}

$dumpFolder = Join-Path ([IO.Path]::GetDirectoryName($resolvedExecutable)) 'logs'
if ($PSCmdlet.ShouldProcess($registryPath, "Enable full crash dumps in $dumpFolder")) {
    New-Item -ItemType Directory -Path $dumpFolder -Force | Out-Null
    New-Item -Path $registryPath -Force | Out-Null
    New-ItemProperty `
        -Path $registryPath `
        -Name DumpFolder `
        -PropertyType ExpandString `
        -Value $dumpFolder `
        -Force | Out-Null
    New-ItemProperty `
        -Path $registryPath `
        -Name DumpType `
        -PropertyType DWord `
        -Value 2 `
        -Force | Out-Null
    New-ItemProperty `
        -Path $registryPath `
        -Name DumpCount `
        -PropertyType DWord `
        -Value $DumpCount `
        -Force | Out-Null
}

Write-Host "RemoteOps Agent GUI full crash dumps are enabled in: $dumpFolder"
Write-Host 'After reproducing the crash, provide the .log and .dmp files from the logs directory.'
Write-Host 'Run this script with -Disable after diagnostics are complete.'
