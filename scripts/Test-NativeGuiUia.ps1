#requires -Version 7.2
<#
.SYNOPSIS
在独立 UIA 客户端进程中验收原生按钮的类型、调用、焦点与禁用状态。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Executable,
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../artifacts/native-ui-prototype/uia'),
    [string]$InitializationScript = (Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1'),
    [switch]$SkipBootstrap
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'UIA 验收仅支持 Windows。' }
$executablePath = (Resolve-Path -LiteralPath $Executable).Path
if ([Diagnostics.FileVersionInfo]::GetVersionInfo($executablePath).ProductName -ne 'RemoteOps Native UI Prototype') {
    throw 'Executable 必须是独立原生小样，拒绝启动其他产品或旧版 Agent。'
}
$initializationPath = (Resolve-Path -LiteralPath $InitializationScript).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$caseDirectory = Join-Path (Resolve-Path -LiteralPath $OutputDirectory).Path ('中文 UIA 验收/' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $caseDirectory | Out-Null
$reportPath = Join-Path $caseDirectory 'UIA报告.json'
$applicationReport = Join-Path $caseDirectory '小样退出报告.json'
$utf8 = [Text.UTF8Encoding]::new($false)
$checks = [Collections.Generic.List[object]]::new()
$buttonRows = [Collections.Generic.List[object]]::new()
$process = $null
$failure = $null

function Add-Check {
    <# .SYNOPSIS 记录验收事实，失败时继续保留已完成检查。 #>
    param([string]$Name, [bool]$Passed, [string]$Detail)
    $checks.Add([pscustomobject]@{ name = $Name; passed = $Passed; detail = $Detail })
}

function Find-Control {
    <# .SYNOPSIS 在本次启动的主窗口范围内按 ID 查找控件。 #>
    param($Root, [string]$Id)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
    return $Root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
}

function Wait-Condition {
    <# .SYNOPSIS 有界等待目标进程的可观察变化。 #>
    param([scriptblock]$Condition, [string]$Description)
    $deadline = [Diagnostics.Stopwatch]::StartNew()
    while ($deadline.Elapsed.TotalSeconds -lt 8) {
        if (& $Condition) { return }
        if ($process.HasExited) { throw ('测试进程提前退出：' + $Description) }
        Start-Sleep -Milliseconds 40
    }
    throw ('等待超时：' + $Description)
}

try {
    # 必须先初始化，再第一次调用 AutomationElement；失败后不能以空壳 Pane 冒充成功。
    if ($SkipBootstrap) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
    }
    else {
        & $initializationPath
    }

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $executablePath
    $startInfo.WorkingDirectory = $caseDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.WindowStyle = [Diagnostics.ProcessWindowStyle]::Normal
    $startInfo.ArgumentList.Add('--demo')
    $startInfo.ArgumentList.Add('--report')
    $startInfo.ArgumentList.Add($applicationReport)
    foreach ($name in @($startInfo.Environment.Keys)) {
        if ($name -match '^(REMOTEOPS|JLAI)_') { $startInfo.Environment.Remove($name) | Out-Null }
    }
    foreach ($name in @('LOCALAPPDATA', 'APPDATA', 'TEMP', 'TMP')) {
        $path = Join-Path $caseDirectory $name.ToLowerInvariant()
        New-Item -ItemType Directory -Force -Path $path | Out-Null
        $startInfo.Environment[$name] = $path
    }
    $process = [Diagnostics.Process]::Start($startInfo)
    Wait-Condition { $process.Refresh(); $process.MainWindowHandle -ne [IntPtr]::Zero } '主窗口出现'
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
    if ($root.Current.ProcessId -ne $process.Id) { throw 'UIA 根窗口进程不匹配' }

    foreach ($id in @('110', '111', '112', '113', '114', '115', '116')) {
        $button = Find-Control $root $id
        if ($null -eq $button) { throw ('缺少控件 ' + $id) }
        $isButton = $button.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button
        $supportsInvoke = @($button.GetSupportedPatterns() | ForEach-Object Id) -contains [System.Windows.Automation.InvokePattern]::Pattern.Id
        $buttonRows.Add([pscustomobject]@{
            id = $id; name = $button.Current.Name; type = $button.Current.ControlType.ProgrammaticName
            enabled = $button.Current.IsEnabled; focusable = $button.Current.IsKeyboardFocusable
            invoke = $supportsInvoke
        })
        Add-Check ('button_type_' + $id) $isButton '标准按钮暴露 ControlType.Button'
        Add-Check ('button_invoke_pattern_' + $id) $supportsInvoke '支持真实 InvokePattern'
    }
    if (@($checks | Where-Object { -not $_.passed }).Count -gt 0) {
        throw '标准按钮的类型或 InvokePattern 不完整，停止后续输入。'
    }

    $disabled = Find-Control $root '116'
    Add-Check 'connect_disabled' (-not $disabled.Current.IsEnabled) '没有配置时连接按钮保持禁用'
    $disabledRejected = $false
    try { $disabled.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
    catch {
        $exception = $_.Exception
        while ($null -ne $exception) {
            if ($exception -is [System.Windows.Automation.ElementNotEnabledException]) { $disabledRejected = $true; break }
            $exception = $exception.InnerException
        }
        if (-not $disabledRejected) { throw }
    }
    Add-Check 'disabled_invoke_rejected' $disabledRejected '禁用按钮拒绝 UIA Invoke'

    $ssh = Find-Control $root '111'
    Add-Check 'ssh_keyboard_focusable' $ssh.Current.IsKeyboardFocusable '标准按钮可获得键盘焦点'
    $ssh.SetFocus()
    Wait-Condition { $focus = [System.Windows.Automation.AutomationElement]::FocusedElement; $focus.Current.ProcessId -eq $process.Id -and $focus.Current.AutomationId -eq '111' } 'SSH 按钮获得焦点'
    $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
    Add-Check 'ssh_set_focus' ($focused.Current.ProcessId -eq $process.Id -and $focused.Current.AutomationId -eq '111') 'UIA SetFocus 真实生效'
    $ssh.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Wait-Condition { (Find-Control $root '118').Current.Name -eq '[演示] SSH 场景展示结束' } 'SSH 演示通过 Invoke 完成'
    Wait-Condition { (Find-Control $root '103').Current.Name -eq '本次任务已结束，日志已接收' } 'SSH 后台完成'
    $terminal = Find-Control $root '202'
    if ($null -eq $terminal) { throw 'Invoke 后未创建观察窗' }
    $textPattern = $terminal.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
    $text = $textPattern.DocumentRange.GetText(-1)
    Add-Check 'ssh_invoke_effect' ($text.Contains('display version') -and $text.Contains('中文输出')) '观察窗显示演示命令和中文返回'
    Add-Check 'authentication_hidden' ($text.Contains('身份认证信息已隐藏')) '观察窗认证记录已隐藏'
    $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
    Add-Check 'observer_does_not_steal_focus' ($focused.Current.ProcessId -eq $process.Id -and $focused.Current.AutomationId -eq '111') '自动出现的观察窗保留原按钮焦点'

    (Find-Control $root '205').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Wait-Condition { $terminal.Current.IsOffscreen } '观察窗隐藏'
    $process.Refresh()
    Add-Check 'hide_keeps_main_alive' (-not $process.HasExited) '关闭观察窗按钮不会退出主程序'
    (Find-Control $root '114').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Wait-Condition { -not $terminal.Current.IsOffscreen } '观察窗重新打开'
    (Find-Control $root '112').GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Wait-Condition { (Find-Control $root '118').Current.Name -eq '[演示] Console 场景展示结束' } 'Console 演示通过 Invoke 完成'
    Add-Check 'console_invoke_effect' $true '主窗口及观察窗按钮调用后仍能继续处理任务'
}
catch {
    $failure = $_.Exception.ToString()
    Add-Check 'unexpected_exception' $false $failure
}
finally {
    if ($null -ne $process) {
        try {
            for ($attempt = 0; $attempt -lt 2 -and -not $process.HasExited; $attempt++) {
                $process.Refresh()
                if (-not $process.CloseMainWindow()) { break }
                $null = $process.WaitForExit(1500)
            }
            $cleanExit = $process.HasExited
            Add-Check 'clean_exit' $cleanExit '仅关闭本次启动的测试进程'
            if (-not $cleanExit) { $process.Kill($true); $process.WaitForExit() }
            if ($cleanExit) { Add-Check 'zero_exit_code' ($process.ExitCode -eq 0) '窗口关闭后退出码为零' }
        }
        catch { Add-Check 'cleanup_exception' $false $_.Exception.Message }
        finally { $process.Dispose() }
    }
    $result = [pscustomobject]@{
        schema_version = 1; passed = @($checks | Where-Object { -not $_.passed }).Count -eq 0
        bootstrap_enabled = -not $SkipBootstrap; powershell = $PSVersionTable.PSVersion.ToString()
        executable_sha256 = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash.ToLowerInvariant()
        checks = @($checks); buttons = @($buttonRows); application_report = $applicationReport
        limitations = @('仅自动化本次启动的小样，不替换旧版 RemoteOps；SSH/Console 是离线演示。', 'UIA 和焦点验证需要已登录的交互桌面。')
    }
    [IO.File]::WriteAllText($reportPath, ($result | ConvertTo-Json -Depth 8), $utf8)
    Write-Output ('UIA 验收记录：' + $reportPath)
}
if (-not $result.passed) { throw 'UIA 验收失败，详见报告。' }
