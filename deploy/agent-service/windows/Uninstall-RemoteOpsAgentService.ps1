[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$InstallRoot = "$env:ProgramFiles\RemoteOps\Agent",
    [string]$DataRoot = "$env:ProgramData\RemoteOps\Agent",
    [switch]$PurgeData
)

$ErrorActionPreference = 'Stop'
$serviceName = 'RemoteOpsAgent'
function Assert-SafeChildPath([string]$Path, [string]$Root, [string]$Label) {
    $resolvedPath = [System.IO.Path]::GetFullPath($Path).TrimEnd('\')
    $resolvedRoot = [System.IO.Path]::GetFullPath($Root).TrimEnd('\')
    $requiredPrefix = "$resolvedRoot\"
    if ($resolvedPath.Equals($resolvedRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
        -not $resolvedPath.StartsWith($requiredPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "拒绝清理不在 $Label 子目录下的路径：$resolvedPath"
    }
    return $resolvedPath
}

$service = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
if ($null -ne $service) {
    if ($service.Status -ne 'Stopped' -and $PSCmdlet.ShouldProcess($serviceName, '停止 RemoteOps Agent Service')) {
        Stop-Service -Name $serviceName -Force -ErrorAction Stop
        $service.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    }
    if ($PSCmdlet.ShouldProcess($serviceName, '删除 RemoteOps Agent Service')) {
        & sc.exe delete $serviceName | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "删除 Windows Service 失败：$serviceName" }
    }
}
if (Test-Path -LiteralPath $InstallRoot) {
    $programFilesRoot = [System.IO.Path]::GetFullPath($env:ProgramFiles)
    $resolvedInstallRoot = Assert-SafeChildPath $InstallRoot $programFilesRoot 'Program Files'
    if ($PSCmdlet.ShouldProcess($resolvedInstallRoot, '删除 RemoteOps Agent Service 程序目录')) {
        Remove-Item -LiteralPath $resolvedInstallRoot -Recurse -Force
    }
}
if ($PurgeData -and (Test-Path -LiteralPath $DataRoot)) {
    $programDataRoot = [System.IO.Path]::GetFullPath($env:ProgramData)
    $resolvedDataRoot = Assert-SafeChildPath $DataRoot $programDataRoot 'ProgramData'
    if ($PSCmdlet.ShouldProcess($resolvedDataRoot, '清理 RemoteOps Agent Service 数据目录')) {
        Remove-Item -LiteralPath $resolvedDataRoot -Recurse -Force
    }
}
Write-Output "已卸载 $serviceName；默认保留 Agent 配置、身份状态和传输目录。"

