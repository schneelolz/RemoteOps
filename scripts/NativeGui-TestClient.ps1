#requires -Version 7.2
<#
.SYNOPSIS
仅操作已校验的新原生测试进程；每次调用是独立的短命 UIA 客户端。
.DESCRIPTION
Snapshot JSON: process_id/process_start_utc/executable_sha256/main_handle/foreground_is_main;
main: handle/title/dpi/client_width/client_height/controls[];
controls: id/text/enabled/control_type/invoke/value_read_only;
observer: null 或 handle/title/dpi/client_width/client_height/text/text_pattern/read_only。
ProcessStartUtc 必须是启动方保存的 UTC "o" 字符串，用于拒绝 PID 复用。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [ValidateRange(1, 2147483647)] [int]$ProcessId,
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-fA-F]{64}$')] [string]$ExpectedHash,
    [Parameter(Mandatory)] [string]$ProcessStartUtc,
    [ValidateSet('Snapshot', 'Invoke', 'SetValue', 'Close', 'Capture', 'Escape', 'Confirm')] [string]$Action = 'Snapshot',
    [int]$ControlId = 0,
    [string]$Value,
    [string]$OutputPath,
    [ValidateRange(0, 10000)] [int]$CopyFeedbackWaitMilliseconds = 2500,
    [switch]$RequireCopyIdle,
    [string]$InvokeGateDirectory,
    [string]$InitializationScript = (Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$OutputEncoding = [Text.UTF8Encoding]::new($false)
if (-not $IsWindows) { throw '原生 UIA 测试客户端仅支持 Windows。' }
$testProcess = [Diagnostics.Process]::GetProcessById($ProcessId)

function Assert-TestIdentity {
    <# .SYNOPSIS 每次操作前复核进程启动时间、产品标识及文件哈希。 #>
    $testProcess.Refresh()
    if ($testProcess.HasExited -or $testProcess.StartTime.ToUniversalTime().ToString('o') -cne $ProcessStartUtc) {
        throw '测试进程已退出或启动身份不匹配。'
    }
    $path = $testProcess.MainModule.FileName
    if ([Diagnostics.FileVersionInfo]::GetVersionInfo($path).ProductName -cne 'RemoteOps Native UI Prototype') {
        throw '拒绝操作其他产品或旧版 Agent。'
    }
    if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $ExpectedHash) { throw '测试 EXE 的 SHA-256 不匹配。' }
    return $path
}

$executablePath = Assert-TestIdentity
& (Resolve-Path -LiteralPath $InitializationScript).Path | Out-Null
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class RemoteOpsProductWindowProbe {
    public delegate bool EnumCallback(IntPtr window, IntPtr parameter);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder name, int count);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr window, ref Point point);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr window, uint command);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtr(IntPtr window, int index);
    public static IntPtr[] Windows(int process, string className) {
        var matches = new List<IntPtr>();
        EnumWindows((window, _) => {
            uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner != process) return true;
            var name = new StringBuilder(128); GetClassName(window, name, name.Capacity);
            if (name.ToString() == className) matches.Add(window);
            return true;
        }, IntPtr.Zero);
        return matches.ToArray();
    }
}
'@
# 只改变本客户端线程的坐标解释，读取真实像素；不修改任何屏幕缩放。
if ([RemoteOpsProductWindowProbe]::SetThreadDpiAwarenessContext([IntPtr](-4)) -eq [IntPtr]::Zero) { throw '测试客户端无法启用实际 DPI 坐标。' }

function Get-TestWindow {
    <# .SYNOPSIS 限定为测试进程内唯一的原生主窗或观察窗。 #>
    param([bool]$Observer = $false)
    $className = if ($Observer) { 'RemoteOpsNativeObserver' } else { 'RemoteOpsNativeMain' }
    $windows = @([RemoteOpsProductWindowProbe]::Windows($ProcessId, $className))
    if ($windows.Count -ne 1) { throw ('测试窗口数量不正确：' + $className) }
    return $windows[0]
}

function Get-TestControl {
    <# .SYNOPSIS 已知控件只查目标窗直接子项，避免误操作所属的其他窗口。 #>
    param([int]$Id)
    $window = Get-TestWindow ($Id -ge 201 -and $Id -le 205)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    if ($root.Current.ProcessId -ne $ProcessId) { throw '窗口 UIA 身份不匹配。' }
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, [string]$Id)
    $target = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $condition)
    if ($null -eq $target -or $target.Current.ProcessId -ne $ProcessId -or
        -not [RemoteOpsProductWindowProbe]::IsWindowVisible([IntPtr]$target.Current.NativeWindowHandle)) { throw ('目标控件不存在：' + $Id) }
    return $target
}

function Get-WindowRow {
    <# .SYNOPSIS 记录真实窗口 DPI 与客户区，不能用布局注入冒充系统 DPI。 #>
    param([IntPtr]$Handle)
    $rect = [RemoteOpsProductWindowProbe+Rect]::new()
    if (-not [RemoteOpsProductWindowProbe]::GetClientRect($Handle, [ref]$rect)) { throw '读取窗口客户区失败。' }
    $title = [Text.StringBuilder]::new(1024)
    $outer = [RemoteOpsProductWindowProbe+Rect]::new()
    $origin = [RemoteOpsProductWindowProbe+Point]::new()
    if (-not [RemoteOpsProductWindowProbe]::GetWindowRect($Handle, [ref]$outer) -or
        -not [RemoteOpsProductWindowProbe]::ClientToScreen($Handle, [ref]$origin)) { throw '读取测试窗口客户区起点失败。' }
    $null = [RemoteOpsProductWindowProbe]::GetWindowText($Handle, $title, $title.Capacity)
    return [ordered]@{ handle = $Handle.ToInt64(); title = $title.ToString();
        dpi = [RemoteOpsProductWindowProbe]::GetDpiForWindow($Handle);
        client_width = $rect.Right - $rect.Left; client_height = $rect.Bottom - $rect.Top;
        client_origin_x = $origin.X - $outer.Left; client_origin_y = $origin.Y - $outer.Top;
        visible = [RemoteOpsProductWindowProbe]::IsWindowVisible($Handle) }
}

function Wait-TestInvokeGate {
    <# .SYNOPSIS 只同步本轮客户端与占用夹具，不替代 UIA Invoke。 #>
    if (-not $InvokeGateDirectory) { return }
    if (-not ($Action -eq 'Close' -or ($Action -eq 'Invoke' -and $ControlId -eq 304))) { throw 'Gate 仅允许本轮复制或正常退出测试。' }
    $gate = [IO.Path]::GetFullPath($InvokeGateDirectory)
    $temporary = [IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp'))
    if (-not $temporary.StartsWith('C:\', [StringComparison]::OrdinalIgnoreCase) -or
        -not $gate.StartsWith($temporary.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase) -or
        -not (Test-Path -LiteralPath $gate -PathType Container)) { throw 'Gate 必须是 C 盘已有隔离临时目录。' }
    [IO.File]::WriteAllText((Join-Path $gate 'ready'), 'ready')
    $gateWait = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath (Join-Path $gate 'go'))) {
        if ($gateWait.ElapsedMilliseconds -ge 15000) { throw '客户端 Gate 等待超时。' }
        Start-Sleep -Milliseconds 10
    }
    $null = Assert-TestIdentity
}

function Get-CopyFeedbackStatus([string]$Label, [string]$ErrorText, [bool]$Changed) {
    <# .SYNOPSIS 旧终态不代表本次调用完成；此函数只比较状态，不操作窗口。 #>
    if ($Label -eq '✓') { return $(if ($Changed) { 'success' } else { 'unknown' }) }
    if ($ErrorText -in @('复制失败：剪贴板被占用或响应超时，请重试。', 'Copy failed: clipboard busy or not responding; try again.')) {
        return $(if ($Changed) { 'failure' } else { 'unknown' })
    }
    if ($Label -eq '…') { return 'pending' }
    return 'unsettled'
}

$mainWindow = Get-TestWindow
$null = Assert-TestIdentity
$result = switch ($Action) {
    'Snapshot' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($mainWindow)
        $children = $root.FindAll([System.Windows.Automation.TreeScope]::Children,
            [System.Windows.Automation.Condition]::TrueCondition)
        $rows = [Collections.Generic.List[object]]::new()
        foreach ($target in $children) {
            $id = 0
            if (-not [int]::TryParse($target.Current.AutomationId, [ref]$id) -or $id -lt 300 -or $id -gt 337) { continue }
            # 只验收当前显示的页面；缓存的隐藏控件不属于当前用户界面。
            if (-not [RemoteOpsProductWindowProbe]::IsWindowVisible([IntPtr]$target.Current.NativeWindowHandle)) { continue }
            $patterns = @($target.GetSupportedPatterns() | ForEach-Object Id)
            $text = $target.Current.Name
            $readOnly = $null
            if ($patterns -contains [System.Windows.Automation.ValuePattern]::Pattern.Id) {
                $valuePattern = $target.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
                $text = $valuePattern.Current.Value; $readOnly = $valuePattern.Current.IsReadOnly
            }
            elseif ($patterns -contains [System.Windows.Automation.TextPattern]::Pattern.Id) {
                $text = $target.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1)
                $style = [RemoteOpsProductWindowProbe]::GetWindowLongPtr([IntPtr]$target.Current.NativeWindowHandle, -16).ToInt64()
                $readOnly = ($style -band 0x800) -ne 0
            }
            $rows.Add([pscustomobject]@{ id = $id; text = $text; enabled = $target.Current.IsEnabled;
                control_type = $target.Current.ControlType.ProgrammaticName;
                invoke = $patterns -contains [System.Windows.Automation.InvokePattern]::Pattern.Id;
                value_read_only = $readOnly })
        }
        $main = Get-WindowRow $mainWindow
        $main.controls = @($rows)
        $observer = $null
        $observerWindows = @([RemoteOpsProductWindowProbe]::Windows($ProcessId, 'RemoteOpsNativeObserver'))
        if ($observerWindows.Count -gt 1) { throw '观察窗不唯一。' }
        if ($observerWindows.Count -eq 1) {
            $observer = Get-WindowRow $observerWindows[0]
            # 隐藏父窗的子项不保证出现在 UIA Control View；内容在重新显示后再严格验收。
            $observer.text = $null
            $observer.text_pattern = $null
            $observer.read_only = $null
            if ($observer.visible) {
                $terminal = Get-TestControl 202
                $pattern = $terminal.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
                $observer.text = $pattern.DocumentRange.GetText(-1)
                $observer.text_pattern = $true
                # 标准 EDIT 的 ES_READONLY 是实际窗口样式，不依赖 UIA 的富文本描述。
                $style = [RemoteOpsProductWindowProbe]::GetWindowLongPtr([IntPtr]$terminal.Current.NativeWindowHandle, -16).ToInt64()
                $observer.read_only = ($style -band 0x800) -ne 0
            }
        }
        $foreground = [RemoteOpsProductWindowProbe]::GetForegroundWindow()
        [uint32]$foregroundProcess = 0
        $null = [RemoteOpsProductWindowProbe]::GetWindowThreadProcessId($foreground, [ref]$foregroundProcess)
        $ownedModalCount = @([RemoteOpsProductWindowProbe]::Windows($ProcessId, '#32770') |
            Where-Object { [RemoteOpsProductWindowProbe]::GetWindow($_, 4) -eq $mainWindow }).Count
        [ordered]@{ process_id = $ProcessId; process_start_utc = $ProcessStartUtc;
            executable_sha256 = $ExpectedHash.ToLowerInvariant(); main_handle = $mainWindow.ToInt64();
            foreground_handle = $foreground.ToInt64(); foreground_process_id = $foregroundProcess;
            owned_modal_dialog_count = $ownedModalCount;
            foreground_is_main = $foreground -eq $mainWindow;
            main = $main; observer = $observer }
    }
    'Invoke' {
        $target = Get-TestControl $ControlId
        if ($target.Current.ControlType -ne [System.Windows.Automation.ControlType]::Button) { throw 'Invoke 仅允许标准按钮。' }
        $pattern = $target.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
        $idleWait = [Diagnostics.Stopwatch]::StartNew()
        if ($RequireCopyIdle) {
            if ($ControlId -ne 304) { throw 'RequireCopyIdle 仅允许复制按钮。' }
            while ($target.Current.Name -notin @('复制', 'Copy') -or -not $target.Current.IsEnabled) {
                if ($idleWait.ElapsedMilliseconds -ge 5000) { throw '复制按钮没有在五秒内恢复空闲。' }
                Start-Sleep -Milliseconds 30
            }
        }
        $idleWait.Stop()
        Wait-TestInvokeGate
        $errorControl = $null
        $baselineLabel = $null
        $baselineError = $null
        if ($ControlId -eq 304) {
            $errorControl = Get-TestControl 312
            $baselineLabel = $target.Current.Name
            $baselineError = $errorControl.Current.Name
        }
        $invokeWatch = [Diagnostics.Stopwatch]::StartNew()
        $pattern.Invoke()
        $invokeWatch.Stop()
        $feedback = $null
        $feedbackStatus = $null
        $feedbackWait = 0
        $stateChanged = $false
        if ($ControlId -eq 304) {
            $wait = [Diagnostics.Stopwatch]::StartNew()
            while ($true) {
                $feedback = $target.Current.Name
                $errorText = $errorControl.Current.Name
                $stateChanged = $stateChanged -or $feedback -cne $baselineLabel -or $errorText -cne $baselineError
                $feedbackStatus = Get-CopyFeedbackStatus $feedback $errorText $stateChanged
                if ($feedbackStatus -in @('success', 'failure')) { break }
                if ($wait.ElapsedMilliseconds -ge $CopyFeedbackWaitMilliseconds) {
                    if ($CopyFeedbackWaitMilliseconds -gt 0) { $feedbackStatus = 'timeout' }
                    break
                }
                Start-Sleep -Milliseconds 30
            }
            $feedbackWait = $wait.ElapsedMilliseconds
        }
        @{ action = 'Invoke'; control_id = $ControlId; accepted = $true; feedback_text = $feedback;
            feedback_status = $feedbackStatus; feedback_transition_observed = $stateChanged; copy_idle_wait_ms = $idleWait.ElapsedMilliseconds;
            invoke_elapsed_ms = $invokeWatch.ElapsedMilliseconds; feedback_wait_ms = $feedbackWait }
    }
    'SetValue' {
        if ($ControlId -ne 314) { throw 'SetValue 仅允许首次设置的 Relay 输入框。' }
        $target = Get-TestControl $ControlId
        $target.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
        @{ action = 'SetValue'; control_id = $ControlId; accepted = $true }
    }
    'Close' {
        Wait-TestInvokeGate
        if (-not [RemoteOpsProductWindowProbe]::PostMessage($mainWindow, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)) { throw '发送本窗口正常关闭请求失败。' }
        if ($Value -eq 'Confirm') {
            $wait = [Diagnostics.Stopwatch]::StartNew()
            while ($wait.Elapsed.TotalSeconds -lt 5) {
                if ($testProcess.WaitForExit(50)) { break }
                $dialogs = @([RemoteOpsProductWindowProbe]::Windows($ProcessId, '#32770') |
                    Where-Object { [RemoteOpsProductWindowProbe]::GetWindow($_, 4) -eq $mainWindow })
                if ($dialogs.Count -eq 1) {
                    $null = Assert-TestIdentity
                    $dialog = [System.Windows.Automation.AutomationElement]::FromHandle($dialogs[0])
                    $condition = [System.Windows.Automation.PropertyCondition]::new(
                        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, '6')
                    $yes = $dialog.FindFirst([System.Windows.Automation.TreeScope]::Children, $condition)
                    if ($null -eq $yes -or $yes.Current.ProcessId -ne $ProcessId) { throw '退出确认按钮身份不匹配。' }
                    $yes.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                    break
                }
            }
        }
        @{ action = 'Close'; accepted = $true }
    }
    'Confirm' {
        if ($Value -notin @('Yes', 'No')) { throw 'Confirm 的 Value 仅允许 Yes 或 No。' }
        $wait = [Diagnostics.Stopwatch]::StartNew()
        $confirmed = $false
        while ($wait.Elapsed.TotalSeconds -lt 5) {
            $null = Assert-TestIdentity
            $dialogs = @([RemoteOpsProductWindowProbe]::Windows($ProcessId, '#32770') |
                Where-Object { [RemoteOpsProductWindowProbe]::GetWindow($_, 4) -eq $mainWindow })
            if ($dialogs.Count -gt 1) { throw '本轮测试主窗有多个确认框，拒绝猜测。' }
            if ($dialogs.Count -eq 1) {
                $dialog = [System.Windows.Automation.AutomationElement]::FromHandle($dialogs[0])
                $id = if ($Value -eq 'Yes') { '6' } else { '7' }
                $condition = [System.Windows.Automation.PropertyCondition]::new(
                    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $id)
                $button = $dialog.FindFirst([System.Windows.Automation.TreeScope]::Children, $condition)
                if ($null -eq $button -or $button.Current.ProcessId -ne $ProcessId -or -not $button.Current.IsEnabled -or
                    $button.Current.ControlType -ne [System.Windows.Automation.ControlType]::Button) { throw '确认按钮身份或类型不匹配。' }
                $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
                $confirmed = $true
                break
            }
            Start-Sleep -Milliseconds 50
        }
        if (-not $confirmed) { throw '没有出现本轮主窗所属的确认框。' }
        @{ action = 'Confirm'; answer = $Value; accepted = $true }
    }
    'Escape' {
        # 只测试本窗口的按键消息路径，不注入全局输入、不改变前台窗口。
        if (-not [RemoteOpsProductWindowProbe]::PostMessage($mainWindow, 0x100, [IntPtr]27, [IntPtr]1)) { throw '投递测试窗口 Esc 消息失败。' }
        $null = [RemoteOpsProductWindowProbe]::PostMessage($mainWindow, 0x101, [IntPtr]27, [IntPtr]1)
        @{ action = 'Escape'; accepted = $true; input_kind = 'targeted_window_message' }
    }
    'Capture' {
        if ([string]::IsNullOrWhiteSpace($OutputPath)) { throw 'Capture 必须提供 PNG OutputPath。' }
        Add-Type -AssemblyName System.Drawing
        $window = if ($ControlId -eq 202) { Get-TestWindow $true } else { $mainWindow }
        $rect = [RemoteOpsProductWindowProbe+Rect]::new()
        if (-not [RemoteOpsProductWindowProbe]::GetWindowRect($window, [ref]$rect)) { throw '读取测试窗口范围失败。' }
        $bitmap = [Drawing.Bitmap]::new($rect.Right-$rect.Left, $rect.Bottom-$rect.Top)
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $dc = $graphics.GetHdc()
            try { if (-not [RemoteOpsProductWindowProbe]::PrintWindow($window, $dc, 2)) { throw '测试窗口 PrintWindow 失败。' } }
            finally { $graphics.ReleaseHdc($dc) }
            $bitmap.Save([IO.Path]::GetFullPath($OutputPath), [Drawing.Imaging.ImageFormat]::Png)
        } finally { $graphics.Dispose(); $bitmap.Dispose() }
        @{ action = 'Capture'; path = [IO.Path]::GetFullPath($OutputPath); handle = $window.ToInt64() }
    }
}
$json = $result | ConvertTo-Json -Depth 12
if ($Action -ne 'Capture' -and -not [string]::IsNullOrWhiteSpace($OutputPath)) {
    [IO.File]::WriteAllText([IO.Path]::GetFullPath($OutputPath), $json, [Text.UTF8Encoding]::new($false))
}
$json
