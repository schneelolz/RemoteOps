[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$ConfigPath,
    [string]$ServiceExecutable = (Join-Path $PSScriptRoot '..\..\..\target\release\remoteops-agent-service.exe'),
    [string]$InstallRoot = "$env:ProgramFiles\RemoteOps\Agent",
    [string]$DataRoot = "$env:ProgramData\RemoteOps\Agent",
    [switch]$AllowLocalSystem,
    [switch]$StartService
)

$ErrorActionPreference = 'Stop'
$serviceName = 'RemoteOpsAgent'
$resolvedExecutable = [System.IO.Path]::GetFullPath((Resolve-Path -LiteralPath $ServiceExecutable).Path)
$resolvedConfig = [System.IO.Path]::GetFullPath((Resolve-Path -LiteralPath $ConfigPath).Path)
$resolvedInstallRoot = [System.IO.Path]::GetFullPath($InstallRoot)
$resolvedDataRoot = [System.IO.Path]::GetFullPath($DataRoot)

$existing = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
if ($null -ne $existing -and $existing.Status -ne 'Stopped') {
    Stop-Service -Name $serviceName -Force -ErrorAction Stop
    $existing.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
}

New-Item -ItemType Directory -Path $resolvedInstallRoot, $resolvedDataRoot -Force | Out-Null
$installedExecutable = Join-Path $resolvedInstallRoot 'remoteops-agent-service.exe'
$installedConfig = Join-Path $resolvedDataRoot 'agent-config.json'
$statusFile = Join-Path $resolvedDataRoot 'runtime-status.json'
Copy-Item -LiteralPath $resolvedExecutable -Destination $installedExecutable -Force
Copy-Item -LiteralPath $resolvedConfig -Destination $installedConfig -Force

icacls.exe $resolvedDataRoot /inheritance:r /grant:r `
    '*S-1-5-18:(OI)(CI)(F)' `
    '*S-1-5-32-544:(OI)(CI)(F)' `
    '*S-1-5-19:(OI)(CI)(M)' | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "无法设置 Agent 服务数据目录 ACL：$resolvedDataRoot"
}

$account = if ($AllowLocalSystem) { 'LocalSystem' } else { 'NT AUTHORITY\LocalService' }
$binPath = "`"$installedExecutable`" --config `"$installedConfig`" --status-file `"$statusFile`""
$existing = Get-Service -Name $serviceName -ErrorAction SilentlyContinue
if ($null -eq $existing) {
    & sc.exe create $serviceName binPath= $binPath start= auto obj= $account DisplayName= 'RemoteOps Agent' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "创建 Windows Service 失败：$serviceName" }
}
else {
    if ($existing.Status -ne 'Stopped') { Stop-Service -Name $serviceName -Force -ErrorAction SilentlyContinue }
    & sc.exe config $serviceName binPath= $binPath start= auto obj= $account | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "更新 Windows Service 失败：$serviceName" }
}

& sc.exe description $serviceName 'RemoteOps 可选被控端服务；权限仍受本机 FullAccess 授权和控制者审批约束。' | Out-Null
& sc.exe failure $serviceName reset= 86400 actions= restart/5000/restart/30000/none/0 | Out-Null
if ($LASTEXITCODE -ne 0) { throw "配置 Windows Service 故障恢复失败：$serviceName" }

if ($StartService) {
    Start-Service -Name $serviceName
}
Write-Output "已安装 $serviceName；配置保存在 $installedConfig；服务账户：$account"
