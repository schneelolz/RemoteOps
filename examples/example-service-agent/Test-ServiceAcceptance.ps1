[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [Parameter(Mandatory = $true)][string]$BinaryPath,
    [Parameter(Mandatory = $true)][string]$ConfigPath,
    [Parameter(Mandatory = $true)][string]$StatusPath,
    [string]$ServiceName = 'ExampleServiceAgent',
    [string]$ProductId = 'example-service-agent',
    [ValidateSet('NoLoggedOnUser', 'ActiveRdp')][string]$Scenario = 'ActiveRdp',
    [switch]$InstallAndStart,
    [string]$TargetContextFile,
    [string]$ControlStateFile,
    [string]$GraphRejectionReportFile
)
$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw '此脚本只用于 Windows SCM 验收' }
$binary = (Resolve-Path -LiteralPath $BinaryPath).Path
$config = (Resolve-Path -LiteralPath $ConfigPath).Path
$administrator = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($ServiceName -notmatch '^[A-Za-z0-9_.-]{1,128}$') { throw '请使用固定、无特殊字符的测试服务名，且必须与产品内置服务名一致' }
if ($InstallAndStart) {
    if (-not $administrator) { throw '真实 SCM 安装需要管理员终端；本脚本不会自动提权' }
    if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) { throw '测试服务名已存在，拒绝覆盖；请先人工核对既有服务' }
    & $binary --check-config --config $config
    if ($LASTEXITCODE -ne 0) { throw '配置验证失败，未安装服务' }
    if ($PSCmdlet.ShouldProcess($ServiceName, '安装并启动独立测试服务，图形环境变量设为 1')) {
        $command = '"{0}" --config "{1}" --status-file "{2}"' -f $binary, $config, $StatusPath
        New-Service -Name $ServiceName -BinaryPathName $command -StartupType Manual | Out-Null
        $registryPath = Join-Path 'HKLM:\SYSTEM\CurrentControlSet\Services' $ServiceName
        New-ItemProperty -LiteralPath $registryPath -Name Environment -PropertyType MultiString -Value @('REMOTEOPS_VISUAL_PROVIDER_ENABLED=1', 'REMOTEOPS_WINDOWS_MCP_ENABLED=1') -Force | Out-Null
        Start-Service -Name $ServiceName
        (Get-Service -Name $ServiceName).WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
    }
}
$checks = [System.Collections.Generic.List[object]]::new()
function Record-Check([string]$name, [bool]$passed, [string]$evidence) {
    $checks.Add([pscustomobject]@{ name = $name; result = $(if ($passed) { 'passed' } else { 'not_verified' }); evidence = $evidence })
}
$service = Get-CimInstance -ClassName Win32_Service -Filter ("Name='{0}'" -f $ServiceName)
$running = $null -ne $service -and $service.State -eq 'Running' -and $service.ProcessId -gt 0
Record-Check 'SCM运行中' $running '默认只读；没有 -InstallAndStart 时不创建、启动或停止服务'
if ($running) {
    $process = Get-Process -Id $service.ProcessId -ErrorAction Stop
    Record-Check '服务进程Session0' ($process.SessionId -eq 0) ('process_id={0}; session_id={1}' -f $service.ProcessId, $process.SessionId)
}
if ($TargetContextFile) {
    $context = Get-Content -LiteralPath $TargetContextFile -Raw | ConvertFrom-Json
    if ($context.details) { $context = $context.details }
    $visual = @($context.capabilities | Where-Object capability -eq 'visual')
    Record-Check 'Agent原生事实与产品上限' ($context.product_id -eq $ProductId -and $context.run_mode -eq 'windows_service' -and $context.facts.process_session_id -eq 0 -and $context.facts.interactive_desktop -eq 'unavailable' -and $visual.Count -eq 1 -and $visual[0].status -eq 'unavailable' -and @($visual[0].reasons) -contains 'product_policy_disabled') '从已认证目标导出 GetTargetContext 原始details；必须与本测试服务同目标'
} else { Record-Check 'Agent原生事实与产品上限' $false '尚未提供已认证目标的 TargetContext 证据文件' }
if ($ControlStateFile) {
    $control = Get-Content -LiteralPath $ControlStateFile -Raw | ConvertFrom-Json
    if ($control.state) { $control = $control.state }
    Record-Check 'FullAccess已生效' ($control.mode -eq 'full_access') '来自同一会话已认证控制状态的证据文件；不自动授予权限'
} else { Record-Check 'FullAccess已生效' $false '需通过门户配对并授权后导出同一会话控制状态' }
$operations = @('visual_observe', 'visual_wait_for', 'visual_invoke', 'visual_type_text', 'visual_send_input', 'visual_stop')
if ($GraphRejectionReportFile) {
    $results = @(Get-Content -LiteralPath $GraphRejectionReportFile -Raw | ConvertFrom-Json)
    foreach ($operation in $operations) {
        $entry = @($results | Where-Object operation -eq $operation)
        Record-Check $operation ($entry.Count -eq 1 -and $entry[0].error_code -eq 'product_policy_disabled') '同一 FullAccess 会话逐项发送合法图形协议，保存 operation/error_code；不能用工具隐藏代替请求'
    }
} else { foreach ($operation in $operations) { Record-Check $operation $false '尚未提供同一会话的六类图形拒绝证据' } }
[pscustomobject]@{
    scenario = $Scenario
    scenario_precondition = '执行者须事先确认无交互登录或有活动RDP；脚本不注销、断开或修改任何用户会话'
    is_administrator = $administrator
    service_name = $ServiceName
    installation_requested = [bool]$InstallAndStart
    checks = $checks
    remaining = '需人工复核证据属于同一目标/会话，并执行重连、重配、重启和撤销授权回归；Provider构造/调用0由Agent计数与合法duplex测试覆盖，不能仅凭无子进程推断'
} | ConvertTo-Json -Depth 8
