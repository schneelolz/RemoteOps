[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$ConfigPath,
    [string]$ServiceExecutable,
    [string]$InstallRoot = "$env:ProgramFiles\RemoteOps\Agent",
    [string]$DataRoot = "$env:ProgramData\RemoteOps\Agent",
    [ValidatePattern('^[A-Za-z0-9_.-]{1,128}$')]
    [string]$ServiceName = 'RemoteOpsAgent',
    [ValidateNotNullOrEmpty()]
    [string]$DisplayName = 'RemoteOps Agent',
    [switch]$AllowLocalSystem,
    [switch]$StartService
)

$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 在脚本正文中才能可靠取得 PSScriptRoot。
if ([string]::IsNullOrWhiteSpace($ServiceExecutable)) {
    $ServiceExecutable = Join-Path $PSScriptRoot '..\..\..\target\release\remoteops-agent-service.exe'
}
if ($ServiceName -ne 'RemoteOpsAgent' -and (-not $PSBoundParameters.ContainsKey('InstallRoot') -or -not $PSBoundParameters.ContainsKey('DataRoot'))) {
    throw '定制服务必须显式指定独立 InstallRoot 与 DataRoot，避免覆盖通用服务目录'
}
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
# 结构化传递 PathName，避免 PowerShell 5.1 向 sc.exe 传参时重写内部引号。
$existingCim = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'" -ErrorAction Stop
$serviceArguments = @{
    DisplayName = $DisplayName
    PathName = $binPath
    StartMode = 'Automatic'
    StartName = $account
    # 仅支持上述两个内置账户；显式空密码不用于自定义服务账户。
    StartPassword = ''
}
if ($null -eq $existingCim) {
    $serviceArguments.Name = $serviceName
    $serviceArguments.ServiceType = [byte]16
    $serviceArguments.ErrorControl = [byte]1
    $serviceArguments.DesktopInteract = $false
    $result = Invoke-CimMethod -ClassName Win32_Service -MethodName Create -Arguments $serviceArguments -ErrorAction Stop
    $operation = '创建'
}
else {
    $result = Invoke-CimMethod -InputObject $existingCim -MethodName Change -Arguments $serviceArguments -ErrorAction Stop
    $operation = '更新'
}
if ($null -eq $result -or $result.ReturnValue -ne 0) {
    throw "$operation Windows Service 失败：$serviceName；CIM 返回码：$($result.ReturnValue)"
}

Set-Service -Name $serviceName -Description 'RemoteOps 可选被控端服务；权限仍受本机 FullAccess 授权和控制者审批约束。' -ErrorAction Stop
$failureOutput = & sc.exe failure $serviceName reset= 86400 actions= restart/5000/restart/30000/none/0 2>&1
$failureExitCode = $LASTEXITCODE
if ($failureExitCode -ne 0) {
    throw "配置 Windows Service 故障恢复失败：$serviceName；sc.exe 返回码：$failureExitCode；$($failureOutput -join [Environment]::NewLine)"
}

if ($StartService) {
    Start-Service -Name $serviceName
}
Write-Output "已安装 $serviceName；配置保存在 $installedConfig；服务账户：$account"

