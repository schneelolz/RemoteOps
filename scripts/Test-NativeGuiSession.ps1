#requires -Version 7.2
<#
.SYNOPSIS
监视独立原生小样的真实会话重连与窗口 DPI，使用标记文件协调人工或客户端操作。
.DESCRIPTION
后台运行后读取 ready.json；向 commands/唯一名称.json 写入 action 和 name。
action 支持 snapshot、ssh、console、stress、finish；结果写入 responses/同名.json。
本脚本不会改变系统缩放、连接设置、凭据或其他进程，不注入 DPI/WTS 消息。
可选 InitialMainLeft/Top、InitialObserverLeft/Top 只放置本次测试窗口，不抢焦点。
RequireDpiRoundTrip 要求显式快照完成两个窗口的 96→144→192→96，之后才能 finish。
SnapshotInSubprocess 为默认关闭的实验：由短命客户端执行同源快照并退出后才继续。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Executable,
    [string]$InitializationScript = (Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1'),
    [Parameter(Mandatory)] [string]$OutputDirectory,
    [string]$ExpectedHash,
    [ValidateRange(30, 900)] [int]$TimeoutSeconds = 600,
    [ValidateRange(100, 1000)] [int]$PollMilliseconds = 250,
    [int[]]$RequiredDpi = @(96, 144, 192),
    [ValidateRange(1, 1000000)] [int]$PressureLines = 100000,
    [ValidateRange(0, 10)] [int]$RequiredReconnectCycles = 1,
    [ValidateRange(0, 10)] [int]$RequiredPressureDisconnects = 1,
    [switch]$RequireDpiRoundTrip,
    [switch]$SnapshotInSubprocess,
    [Nullable[int]]$InitialMainLeft,
    [Nullable[int]]$InitialMainTop,
    [Nullable[int]]$InitialObserverLeft,
    [Nullable[int]]$InitialObserverTop
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw '真实会话验收仅支持 Windows。' }
$exePath = (Resolve-Path -LiteralPath $Executable).Path
$exeVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($exePath)
if ($exeVersion.ProductName -ne 'RemoteOps Native UI Prototype' -or $exeVersion.ProductVersion -notin @('0.1.1', '0.1.2', '0.2.0', '0.2.1', '0.2.2', '0.3.0')) {
    throw 'Executable 必须是文件版本为 0.1.1、0.1.2、0.2.0、0.2.1、0.2.2 或 0.3.0 的独立原生小样。'
}
$exeHash = (Get-FileHash -LiteralPath $exePath -Algorithm SHA256).Hash.ToLowerInvariant()
if (($null -eq $InitialMainLeft) -ne ($null -eq $InitialMainTop) -or ($null -eq $InitialObserverLeft) -ne ($null -eq $InitialObserverTop)) {
    throw '初始位置的 Left 和 Top 必须成对提供。'
}
if ([string]::IsNullOrWhiteSpace($ExpectedHash)) {
    if ($exeVersion.ProductVersion -ne '0.1.1') { throw '新构建必须显式传入 ExpectedHash。' }
    $allowedHashes = @(
        'a2e71c2d380c8130a48975abe804ca4084a8a359c7a18b55c5f4b4fd6728cb9b',
        '44e3c008278b9cf00db27c588a0cb9d33ecf4017b9c63e7c1e03dcdc4a23c387'
    )
    if ($exeHash -notin $allowedHashes) { throw '产物不在已验收的 0.1.1 哈希名单内；请显式传入 ExpectedHash。' }
}
elseif ($ExpectedHash -notmatch '^[0-9a-fA-F]{64}$' -or $exeHash -ne $ExpectedHash.ToLowerInvariant()) {
    throw 'ExpectedHash 与待测 EXE 不符。'
}
$initializationPath = (Resolve-Path -LiteralPath $InitializationScript).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$runPath = (Resolve-Path -LiteralPath $OutputDirectory).Path
if (Test-Path -LiteralPath (Join-Path $runPath 'ready.json')) { throw 'OutputDirectory 已有验收记录；必须使用新的独立目录。' }
foreach ($subdirectory in @('commands', 'responses', 'processed', 'isolated')) {
    New-Item -ItemType Directory -Path (Join-Path $runPath $subdirectory) | Out-Null
}
$utf8 = [Text.UTF8Encoding]::new($false)
$checks = [Collections.Generic.List[object]]::new()
$samples = [Collections.Generic.List[object]]::new()
$snapshots = [Collections.Generic.List[object]]::new()
$snapshotWorkers = [Collections.Generic.List[object]]::new()
$snapshotElapsedOffset = 0L
$transitions = [Collections.Generic.List[object]]::new()
$pressureRuns = [Collections.Generic.List[object]]::new()
$watch = [Diagnostics.Stopwatch]::StartNew()
$process = $null
$testPid = $null
$failure = $null
$finished = $false
$previousKey = $null
$previousState = $null
$disconnectPending = $false
$reconnectCycles = 0
$activePressure = $null
$mainHwnd = 0L
$mainStart = $null
$mainPlaced = $false
$observerPlaced = $false
$reportPath = Join-Path $runPath 'report.json'
$applicationReportPath = Join-Path $runPath 'application-report.json'

function Write-JsonFile {
    <# .SYNOPSIS 原子写入本次目录内的结构化验收记录。 #>
    param([string]$Path, $Value)
    $json = $Value | ConvertTo-Json -Depth 16
    $temporary = $Path + '.writing'
    [IO.File]::WriteAllText($temporary, $json, $utf8)
    # 外部读取可能短暂不共享删除权限；保留原子替换，在有界时间内等待读句柄释放。
    $renameWait = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        try {
            [IO.File]::Move($temporary, $Path, $true)
            break
        }
        catch {
            $renameError = $_.Exception
            $retryable = $false
            while ($null -ne $renameError) {
                if ($renameError -is [IO.IOException] -or $renameError -is [UnauthorizedAccessException]) { $retryable = $true; break }
                $renameError = $renameError.InnerException
            }
            if (-not $retryable -or $renameWait.ElapsedMilliseconds -ge 1000) { throw }
            Start-Sleep -Milliseconds 25
        }
    }
}

function Add-Check {
    <# .SYNOPSIS 保留每次检查结果，不用最后一次成功覆盖之前失败。 #>
    param([string]$Name, [bool]$Passed, [string]$Detail)
    $checks.Add([pscustomobject]@{ name = $Name; passed = $Passed; detail = $Detail })
}

function Get-DpiRoundTrip {
    <# .SYNOPSIS 仅从同一进程和主窗口的显式快照核对两个可见窗口共同的 DPI 往返。 #>
    param([object[]]$Snapshots, [int]$ProcessId, [string]$ProcessStart, [long]$MainHandle)
    $required = @(96, 144, 192, 96)
    $stages = [Collections.Generic.List[int]]::new()
    $identityValid = $true
    foreach ($snapshot in $Snapshots) {
        if ($snapshot.process_id -ne $ProcessId -or $snapshot.process_start_utc -ne $ProcessStart -or $snapshot.main_handle -ne $MainHandle) {
            $identityValid = $false
            continue
        }
        $main = @($snapshot.windows | Where-Object { $_.Handle -eq $MainHandle -and $_.Visible })
        $observer = @($snapshot.windows | Where-Object { $_.Title -eq 'RemoteOps · 只读操作观察窗' -and $_.Visible })
        if ($main.Count -ne 1 -or $observer.Count -ne 1 -or $main[0].Dpi -ne $observer[0].Dpi) { continue }
        $dpi = [int]$main[0].Dpi
        if ($stages.Count -eq 0 -or $stages[$stages.Count - 1] -ne $dpi) { $stages.Add($dpi) }
    }
    $complete = $identityValid -and $stages.Count -eq $required.Count
    if ($complete) {
        for ($index = 0; $index -lt $required.Count; $index++) {
            if ($stages[$index] -ne $required[$index]) { $complete = $false; break }
        }
    }
    return [pscustomobject]@{ required = $required; observed = @($stages); identity_valid = $identityValid; complete = $complete }
}

function Set-InitialTestWindowPositions {
    <# .SYNOPSIS 每个测试窗口只放置一次，不激活、不改变尺寸或用户系统缩放。 #>
    if (($null -eq $InitialMainLeft -or $mainPlaced) -and ($null -eq $InitialObserverLeft -or $observerPlaced)) { return }
    $null = Get-Root
    $windows = @([RemoteOpsNativeSessionProbe]::Capture($process.Id).Windows)
    foreach ($window in $windows) {
        if (-not $mainPlaced -and $null -ne $InitialMainLeft -and $window.Handle -eq $mainHwnd) {
            [RemoteOpsNativeSessionProbe]::Place($process.Id, $window.Handle, $InitialMainLeft, $InitialMainTop)
            $script:mainPlaced = $true
        }
        if (-not $observerPlaced -and $null -ne $InitialObserverLeft -and $window.Title -eq 'RemoteOps · 只读操作观察窗') {
            [RemoteOpsNativeSessionProbe]::Place($process.Id, $window.Handle, $InitialObserverLeft, $InitialObserverTop)
            $script:observerPlaced = $true
        }
    }
}

function Find-Control {
    <# .SYNOPSIS 只在本次进程的主窗口及所属观察窗中查找控件。 #>
    param($Root, [string]$Id)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
    # 已知主窗口控件只查直接子项；观察窗控件仍保留完整后代查找。
    $numericId = 0
    $isMainControl = [int]::TryParse($Id, [ref]$numericId) -and $numericId -ge 101 -and $numericId -le 118
    $scope = if ($isMainControl) {
        [System.Windows.Automation.TreeScope]::Children
    } else { [System.Windows.Automation.TreeScope]::Descendants }
    return $Root.FindFirst($scope, $condition)
}

function Get-Root {
    <# .SYNOPSIS 校验 PID、开始时间和主窗口句柄后取得 UIA 根。 #>
    $process.Refresh()
    if ($process.HasExited -or $process.StartTime.ToUniversalTime().ToString('o') -ne $mainStart) { throw '测试进程已退出或身份变化。' }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$mainHwnd)
    if ($root.Current.ProcessId -ne $process.Id) { throw '主窗口已不属于本次测试进程。' }
    return $root
}

function Invoke-TestButton {
    <# .SYNOPSIS 通过标准 InvokePattern 触发本次小样按钮，不发送伪造窗口消息。 #>
    param([string]$Id)
    $root = Get-Root
    $button = Find-Control $root $Id
    if ($null -eq $button -or $button.Current.ControlType -ne [System.Windows.Automation.ControlType]::Button) {
        throw ('测试按钮未暴露 Button：' + $Id)
    }
    if (-not $button.Current.IsEnabled) { throw ('测试按钮不可用：' + $Id) }
    $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
}

function Get-WindowSnapshot {
    <# .SYNOPSIS 同时记录真实窗口 DPI、字体、控件边界和当前只读内容。 #>
    param([string]$Name)
    function Get-TerminalFontEvidence {
        <# .SYNOPSIS 根据真实控件类型核对字体，标准 EDIT 使用实际 HFONT，其他控件保留 UIA 严格换算。 #>
        param([string]$ClassName, $ObserverWindow, $UiaPoints, $UiaFace)
        $fontPassed = $false
        $font = $null
        if ($null -ne $ObserverWindow) {
            $expectedPixels = [Math]::Floor(14 * $ObserverWindow.Dpi / 96 + 0.5)
            if ($ClassName -ceq 'Edit') {
                $nativeFont = @($ObserverWindow.Controls | Where-Object { $_.Id -eq 202 })
                if ($nativeFont.Count -eq 1) {
                    $pixels = [Math]::Abs($nativeFont[0].FontHeight)
                    $fontPassed = $pixels -eq $expectedPixels -and [string]::Equals($nativeFont[0].Font, 'Microsoft YaHei UI', [StringComparison]::Ordinal)
                    $font = [pscustomobject]@{
                        source = 'WM_GETFONT/GetObjectW'; class_name = $ClassName
                        face = $nativeFont[0].Font; height = $nativeFont[0].FontHeight; pixels = $pixels; expected_pixels = $expectedPixels
                        uia_points = $UiaPoints; uia_face = $UiaFace
                    }
                }
            } elseif ($UiaPoints -is [double]) {
                $pixels = $UiaPoints * $ObserverWindow.Dpi / 72
                $fontPassed = [Math]::Abs($pixels - $expectedPixels) -le 0.2
                $font = [pscustomobject]@{ source = 'UIA.TextPattern'; face = [string]$UiaFace; points = $UiaPoints; pixels = $pixels; expected_pixels = $expectedPixels }
            }
        }
        return [pscustomobject]@{ passed = $fontPassed; font = $font }
    }
    $root = Get-Root
    $native = [RemoteOpsNativeSessionProbe]::Capture($process.Id)
    $windows = @($native.Windows)
    $main = @($windows | Where-Object { $_.Handle -eq $mainHwnd })
    Add-Check ($Name + ':same_main_window') ($main.Count -eq 1) '主窗口句柄仍属于同一进程'
    Add-Check ($Name + ':observer_exists') (@($windows | Where-Object { $_.Title -eq 'RemoteOps · 只读操作观察窗' }).Count -eq 1) '本次快照同时覆盖主窗口和观察窗'
    foreach ($window in $windows) {
        $kind = if ($window.Handle -eq $mainHwnd) { 'main' } else { 'observer' }
        if ($kind -eq 'observer' -and $window.Title -ne 'RemoteOps · 只读操作观察窗') { continue }
        Add-Check ($Name + ':' + $kind + ':per_monitor_v2') $window.PerMonitorV2 '实际窗口使用 PerMonitorV2 DPI awareness'
        $minimumWidth = if ($kind -eq 'main') { 600 } else { 500 }
        $minimumHeight = if ($kind -eq 'main') { 460 } else { 320 }
        $fitsMinimum = $window.ClientWidth -ge [Math]::Floor($minimumWidth * $window.Dpi / 96) -and $window.ClientHeight -ge [Math]::Floor($minimumHeight * $window.Dpi / 96)
        Add-Check ($Name + ':' + $kind + ':minimum_client') $fitsMinimum '实际客户区不小于该窗口 DPI 的最小尺寸'
        foreach ($child in @($window.Controls)) {
            Add-Check ($Name + ':bounds:' + $child.Id) $child.FitsClient '真实子控件边界落在客户区内'
            # 日志文本控件 不返回 WM_GETFONT，稍后通过真实 UIA 字体属性单独验证。
            if ($child.Id -eq 202) { continue }
            $baseSize = if ($child.Id -eq 101) { 22 } else { 14 }
            $expectedHeight = [int][Math]::Floor($baseSize * $window.Dpi / 96 + 0.5)
            Add-Check ($Name + ':font:' + $child.Id) ([Math]::Abs($child.FontHeight) -eq $expectedHeight) '实际 HFONT 高度随真实窗口 DPI 缩放'
        }
    }
    $buttonRows = foreach ($id in @('110', '111', '112', '113', '114', '115', '116')) {
        $button = Find-Control $root $id
        $isButton = $null -ne $button -and $button.Current.ControlType -eq [System.Windows.Automation.ControlType]::Button
        $invoke = $isButton -and (@($button.GetSupportedPatterns() | ForEach-Object Id) -contains [System.Windows.Automation.InvokePattern]::Pattern.Id)
        Add-Check ($Name + ':uia:' + $id) ($isButton -and $invoke) '真实 UIA 类型与调用模式正确'
        [pscustomobject]@{ id = $id; button = $isButton; invoke = $invoke }
    }
    $terminal = Find-Control $root '202'
    $text = $null
    $isReadonly = $null
    $terminalFont = $null
    if ($null -ne $terminal) {
        $textPattern = $terminal.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
        $documentRange = $textPattern.DocumentRange
        $text = $documentRange.GetText(-1)
        $readonly = $documentRange.GetAttributeValue([System.Windows.Automation.TextPattern]::IsReadOnlyAttribute)
        $isReadonly = $readonly -is [bool] -and $readonly
        Add-Check ($Name + ':readonly') $isReadonly '实际观察控件的 UIA 只读属性为 true'
        Add-Check ($Name + ':unicode') ($text -match '[\u4e00-\u9fff]' -and -not $text.Contains([char]0xFFFD)) '观察窗包含中文，未出现 Unicode 替换字符'
        Add-Check ($Name + ':redaction') (-not $text.Contains('原型测试秘密')) '压力样例口令不出现在控件文本中'
        $firstCharacter = $documentRange.Clone()
        $firstCharacter.MoveEndpointByRange([System.Windows.Automation.Text.TextPatternRangeEndpoint]::End, $firstCharacter, [System.Windows.Automation.Text.TextPatternRangeEndpoint]::Start)
        $null = $firstCharacter.MoveEndpointByUnit([System.Windows.Automation.Text.TextPatternRangeEndpoint]::End, [System.Windows.Automation.Text.TextUnit]::Character, 1)
        $points = $firstCharacter.GetAttributeValue([System.Windows.Automation.TextPattern]::FontSizeAttribute)
        $face = $firstCharacter.GetAttributeValue([System.Windows.Automation.TextPattern]::FontNameAttribute)
        $observerWindow = @($windows | Where-Object { $_.Title -eq 'RemoteOps · 只读操作观察窗' })
        $fontPassed = $false
        if ($observerWindow.Count -eq 1) {
            $fontEvidence = Get-TerminalFontEvidence $terminal.Current.ClassName $observerWindow[0] $points $face
            $fontPassed = $fontEvidence.passed
            $terminalFont = $fontEvidence.font
        }
        Add-Check ($Name + ':font:202') $fontPassed '日志文本控件字体严格匹配实际 DPI；标准 EDIT 核对 HFONT，其他控件按 UIA 字符字体点数换算像素'
    }
    $status = Find-Control $root '103'
    $counter = Find-Control $root '203'
    $snapshot = [pscustomobject]@{
        name = $Name; utc = [DateTime]::UtcNow.ToString('o'); elapsed_ms = $watch.ElapsedMilliseconds + $snapshotElapsedOffset
        process_id = $process.Id; process_start_utc = $mainStart; main_handle = $mainHwnd
        session = [RemoteOpsNativeSessionProbe]::Session($process.SessionId)
        windows = $windows; buttons = @($buttonRows); readonly = $isReadonly; terminal_font = $terminalFont
        status = $(if ($null -ne $status) { $status.Current.Name } else { $null })
        counters = $(if ($null -ne $counter) { $counter.Current.Name } else { $null })
        transcript = $text
    }
    $snapshots.Add($snapshot)
    return $snapshot
}

function ConvertFrom-SnapshotWorkerJson {
    <# .SYNOPSIS 保留原始 JSON 的精确 UTC 字符串，兼容 PowerShell 7.2 自动日期转换。 #>
    param([string]$Json)
    $result = $Json | ConvertFrom-Json
    $document = [System.Text.Json.JsonDocument]::Parse($Json)
    try {
        $snapshotElement = $document.RootElement.GetProperty('snapshot')
        if ($snapshotElement.ValueKind -eq [System.Text.Json.JsonValueKind]::Object) {
            $result.snapshot.process_start_utc = $snapshotElement.GetProperty('process_start_utc').GetString()
            $result.snapshot.utc = $snapshotElement.GetProperty('utc').GetString()
        }
    } finally { $document.Dispose() }
    return $result
}

function Assert-SnapshotWorkerIdentity {
    <# .SYNOPSIS 精确核对短命客户端结果的 PID、启动时间、主句柄和快照名称。 #>
    param($Snapshot, [int]$ProcessId, [string]$ProcessStart, [long]$MainHandle, [string]$Name)
    if ($null -eq $Snapshot -or $Snapshot.process_id -ne $ProcessId -or
        $Snapshot.process_start_utc -isnot [string] -or
        -not [string]::Equals($Snapshot.process_start_utc, $ProcessStart, [StringComparison]::Ordinal) -or
        $Snapshot.main_handle -ne $MainHandle -or
        -not [string]::Equals($Snapshot.name, $Name, [StringComparison]::Ordinal)) {
        throw '快照客户端结果的目标身份或名称不符。'
    }
}

function Get-SubprocessSnapshot {
    <# .SYNOPSIS 由短命只读客户端执行同一快照实现，确认客户端退出后才继续压力操作。 #>
    param([string]$Name)
    $null = Get-Root
    $workerDirectory = Join-Path $runPath 'snapshot-workers'
    New-Item -ItemType Directory -Force -Path $workerDirectory | Out-Null
    $workerOutput = Join-Path $workerDirectory ([Guid]::NewGuid().ToString('N') + '.json')
    $workerScript = Join-Path $PSScriptRoot 'Test-NativeGuiSessionSnapshot.ps1'
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = (Join-Path $PSHOME 'pwsh.exe')
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-STA', '-File', $workerScript,
        '-SessionScript', $PSCommandPath, '-InitializationScript', $initializationPath,
        '-ProcessId', [string]$testPid, '-ProcessStart', $mainStart, '-MainHandle', [string]$mainHwnd,
        '-SnapshotName', $Name, '-OutputFile', $workerOutput, '-BaseElapsedMilliseconds', [string]$watch.ElapsedMilliseconds)) {
        $start.ArgumentList.Add($argument)
    }
    $worker = $null
    $workerWatch = [Diagnostics.Stopwatch]::StartNew()
    $metadata = [pscustomobject]@{ name = $Name; source = 'short_lived_snapshot_worker'; process_id = $null; exited = $false; timed_out = $false; exit_code = $null; elapsed_ms = 0L; output_file = $workerOutput; failure = $null }
    try {
        $worker = [Diagnostics.Process]::Start($start)
        $metadata.process_id = $worker.Id
        $stdout = $worker.StandardOutput.ReadToEndAsync()
        $stderr = $worker.StandardError.ReadToEndAsync()
        if (-not $worker.WaitForExit(30000)) {
            $metadata.timed_out = $true
            $worker.Kill($true)
            $worker.WaitForExit()
            throw '快照客户端超过 30 秒，已只终止本次客户端；测试应用保持运行。'
        }
        $metadata.exited = $worker.HasExited
        $metadata.exit_code = $worker.ExitCode
        [IO.File]::WriteAllText($workerOutput + '.stdout.txt', $stdout.GetAwaiter().GetResult(), $utf8)
        [IO.File]::WriteAllText($workerOutput + '.stderr.txt', $stderr.GetAwaiter().GetResult(), $utf8)
        if (-not (Test-Path -LiteralPath $workerOutput)) { throw '快照客户端没有写出结构化结果。' }
        $result = ConvertFrom-SnapshotWorkerJson ([IO.File]::ReadAllText($workerOutput))
        foreach ($check in @($result.checks)) { Add-Check $check.name $check.passed $check.detail }
        if ($worker.ExitCode -ne 0 -or $null -ne $result.failure) { throw ('快照客户端失败：' + $result.failure) }
        $snapshot = $result.snapshot
        Assert-SnapshotWorkerIdentity $snapshot $testPid $mainStart $mainHwnd $Name
        Add-Check ($Name + ':snapshot_worker_exited') $metadata.exited '短命快照客户端已退出后才继续测试'
        $snapshot | Add-Member -NotePropertyName capture_source -NotePropertyValue 'short_lived_snapshot_worker'
        $snapshots.Add($snapshot)
        return $snapshot
    }
    catch { $metadata.failure = $_.Exception.ToString(); throw }
    finally {
        if ($null -ne $worker) {
            $metadata.exited = $worker.HasExited
            if ($worker.HasExited) { $metadata.exit_code = $worker.ExitCode }
            $worker.Dispose()
        }
        $metadata.elapsed_ms = $workerWatch.ElapsedMilliseconds
        $snapshotWorkers.Add($metadata)
        Write-JsonFile ($workerOutput + '.client.json') $metadata
    }
}

try {
    & $initializationPath
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class RemoteOpsNativeSessionProbe
{
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    public struct LogFont {
        public int Height, Width, Escapement, Orientation, Weight;
        public byte Italic, Underline, StrikeOut, CharacterSet, OutPrecision, ClipPrecision, Quality, PitchAndFamily;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst=32)] public string FaceName;
    }
    public class ControlRow {
        public int Id, Left, Top, Width, Height, FontHeight;
        public bool FitsClient;
        public string Font, Text;
    }
    public class WindowRow {
        public long Handle;
        public string Title;
        public uint Dpi;
        public bool Visible, PerMonitorV2;
        public int Left, Top, Width, Height, ClientWidth, ClientHeight;
        public List<ControlRow> Controls = new List<ControlRow>();
    }
    public class CaptureRow { public List<WindowRow> Windows = new List<WindowRow>(); }
    public class SessionRow { public int Id, State, Protocol; public string StateName; }
    private delegate bool EnumCallback(IntPtr hwnd, IntPtr parameter);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern bool EnumChildWindows(IntPtr parent, EnumCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint process);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowTextW(IntPtr hwnd, StringBuilder text, int length);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] private static extern bool ClientToScreen(IntPtr hwnd, ref Point point);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern int GetDlgCtrlID(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern IntPtr GetWindowDpiAwarenessContext(IntPtr hwnd);
    [DllImport("user32.dll")] private static extern bool AreDpiAwarenessContextsEqual(IntPtr first, IntPtr second);
    [DllImport("user32.dll")] private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern IntPtr SendMessageTimeoutW(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam, uint flags, uint timeout, out IntPtr result);
    [DllImport("gdi32.dll", CharSet=CharSet.Unicode)] private static extern int GetObjectW(IntPtr handle, int bytes, out LogFont font);
    [DllImport("wtsapi32.dll", CharSet=CharSet.Unicode)] private static extern bool WTSQuerySessionInformationW(IntPtr server, int session, int informationClass, out IntPtr buffer, out int bytes);
    [DllImport("wtsapi32.dll")] private static extern void WTSFreeMemory(IntPtr buffer);

    [DllImport("user32.dll", SetLastError=true)] private static extern bool SetWindowPos(IntPtr hwnd, IntPtr insertAfter, int x, int y, int width, int height, uint flags);

    /// <summary>只移动经 PID 核验的本次测试窗口，保留大小、层级和焦点。</summary>
    public static void Place(int process, long handle, int left, int top) {
        uint owner; IntPtr hwnd = new IntPtr(handle);
        GetWindowThreadProcessId(hwnd, out owner);
        if (owner != (uint)process) throw new InvalidOperationException("测试窗口所属进程发生变化。");
        IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            if (!SetWindowPos(hwnd, IntPtr.Zero, left, top, 0, 0, 0x215))
                throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        } finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
    }

    // 仅查询测试进程自身的会话，不读取用户名、客户端地址或其他会话。
    public static SessionRow Session(int session) {
        SessionRow row = new SessionRow { Id=session, State=-1, Protocol=-1, StateName="Unavailable" };
        IntPtr buffer; int bytes;
        if (WTSQuerySessionInformationW(IntPtr.Zero, session, 8, out buffer, out bytes)) {
            try { if (bytes >= 4) row.State = Marshal.ReadInt32(buffer); } finally { WTSFreeMemory(buffer); }
        }
        if (WTSQuerySessionInformationW(IntPtr.Zero, session, 16, out buffer, out bytes)) {
            try { if (bytes >= 2) row.Protocol = Marshal.ReadInt16(buffer); } finally { WTSFreeMemory(buffer); }
        }
        string[] names = {"Active","Connected","ConnectQuery","Shadow","Disconnected","Idle","Listen","Reset","Down","Init"};
        if (row.State >= 0 && row.State < names.Length) row.StateName = names[row.State];
        return row;
    }

    // 全程使用真实 HWND 的 DPI；坐标读取临时采用 PerMonitorV2，随后恢复测试线程上下文。
    public static CaptureRow Capture(int process) {
        CaptureRow result = new CaptureRow();
        IntPtr previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            EnumWindows(delegate(IntPtr hwnd, IntPtr unused) {
                uint owner; GetWindowThreadProcessId(hwnd, out owner);
                if (owner != (uint)process) return true;
                StringBuilder title = new StringBuilder(256); GetWindowTextW(hwnd, title, title.Capacity);
                if (title.ToString() != "RemoteOps · Windows 原生界面小样" && title.ToString() != "RemoteOps · 只读操作观察窗") return true;
                Rect outer, client; if (!GetWindowRect(hwnd, out outer) || !GetClientRect(hwnd, out client)) return true;
                Point origin = new Point(); ClientToScreen(hwnd, ref origin);
                WindowRow row = new WindowRow {
                    Handle=hwnd.ToInt64(), Title=title.ToString(), Dpi=GetDpiForWindow(hwnd), Visible=IsWindowVisible(hwnd),
                    PerMonitorV2=AreDpiAwarenessContextsEqual(GetWindowDpiAwarenessContext(hwnd), new IntPtr(-4)),
                    Left=outer.Left, Top=outer.Top, Width=outer.Right-outer.Left, Height=outer.Bottom-outer.Top,
                    ClientWidth=client.Right-client.Left, ClientHeight=client.Bottom-client.Top
                };
                EnumChildWindows(hwnd, delegate(IntPtr child, IntPtr ignored) {
                    int id = GetDlgCtrlID(child); if (id < 101 || id > 205) return true;
                    Rect bounds; if (!GetWindowRect(child, out bounds)) return true;
                    IntPtr fontHandle; SendMessageTimeoutW(child, 0x31, IntPtr.Zero, IntPtr.Zero, 2, 250, out fontHandle);
                    LogFont font; int copied = GetObjectW(fontHandle, Marshal.SizeOf(typeof(LogFont)), out font);
                    string controlText = null;
                    if (id == 103 || id == 118 || id == 203) {
                        StringBuilder caption = new StringBuilder(256); GetWindowTextW(child, caption, caption.Capacity); controlText = caption.ToString();
                    }
                    row.Controls.Add(new ControlRow {
                        Id=id, Left=bounds.Left-origin.X, Top=bounds.Top-origin.Y,
                        Width=bounds.Right-bounds.Left, Height=bounds.Bottom-bounds.Top,
                        FontHeight=copied > 0 ? font.Height : 0, Font=copied > 0 ? font.FaceName : null, Text=controlText,
                        FitsClient=bounds.Left>=origin.X && bounds.Top>=origin.Y && bounds.Right<=origin.X+row.ClientWidth && bounds.Bottom<=origin.Y+row.ClientHeight
                    });
                    return true;
                }, IntPtr.Zero);
                result.Windows.Add(row); return true;
            }, IntPtr.Zero);
        } finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
        return result;
    }
}
'@

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $exePath
    $startInfo.WorkingDirectory = $runPath
    $startInfo.UseShellExecute = $false
    $startInfo.WindowStyle = [Diagnostics.ProcessWindowStyle]::Normal
    foreach ($argument in @('--demo', '--report', $applicationReportPath, '--stress-lines', [string]$PressureLines)) { $startInfo.ArgumentList.Add($argument) }
    foreach ($environmentName in @($startInfo.Environment.Keys)) {
        if ($environmentName -match '^(REMOTEOPS|JLAI)_') { $startInfo.Environment.Remove($environmentName) | Out-Null }
    }
    foreach ($environmentName in @('LOCALAPPDATA', 'APPDATA', 'TEMP', 'TMP')) {
        $isolated = Join-Path $runPath ('isolated/' + $environmentName.ToLowerInvariant())
        New-Item -ItemType Directory -Force -Path $isolated | Out-Null
        $startInfo.Environment[$environmentName] = $isolated
    }
    $process = [Diagnostics.Process]::Start($startInfo)
    $testPid = $process.Id
    for ($attempt = 0; $attempt -lt 80; $attempt++) {
        $process.Refresh()
        if ($process.HasExited) { throw '测试小样在主窗口创建前退出。' }
        if ($process.MainWindowHandle -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 100
    }
    $mainHwnd = $process.MainWindowHandle.ToInt64()
    if ($mainHwnd -eq 0) { throw '测试小样没有创建主窗口。' }
    $mainStart = $process.StartTime.ToUniversalTime().ToString('o')
    Set-InitialTestWindowPositions
    $sessionId = $process.SessionId
    if ($RequiredReconnectCycles -gt 0) {
        $initialSession = [RemoteOpsNativeSessionProbe]::Session($sessionId)
        Add-Check 'rdp_protocol' ($initialSession.Protocol -eq 2) '要求重连时，本次测试进程确实位于 RDP 协议会话'
    }
    Write-JsonFile (Join-Path $runPath 'ready.json') ([pscustomobject]@{
        ready = $true; process_id = $process.Id; process_start_utc = $mainStart; main_handle = $mainHwnd
        session_id = $sessionId; exe_sha256 = $exeHash; version = $exeVersion.ProductVersion
        commands = (Join-Path $runPath 'commands'); report = $reportPath; pressure_lines = $PressureLines
        require_dpi_round_trip = [bool]$RequireDpiRoundTrip; snapshot_in_subprocess = [bool]$SnapshotInSubprocess
        initial_positions = [pscustomobject]@{ main_left = $InitialMainLeft; main_top = $InitialMainTop; observer_left = $InitialObserverLeft; observer_top = $InitialObserverTop }
        timeout_seconds = $TimeoutSeconds; required_dpi = $RequiredDpi; required_reconnect_cycles = $RequiredReconnectCycles; required_pressure_disconnects = $RequiredPressureDisconnects
    })
    while (-not $finished -and $watch.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        $process.Refresh()
        if ($process.HasExited) { throw '测试小样在 finish 操作前退出。' }
        Set-InitialTestWindowPositions
        $session = [RemoteOpsNativeSessionProbe]::Session($sessionId)
        $native = [RemoteOpsNativeSessionProbe]::Capture($process.Id)
        $key = [string]$session.State + '|' + ((@($native.Windows) | ForEach-Object { [string]$_.Handle + ':' + $_.Dpi + ':' + $_.Visible }) -join ',')
        $now = [DateTime]::UtcNow.ToString('o')
        $nativeMain = @($native.Windows | Where-Object { $_.Handle -eq $mainHwnd })
        $statusText = $null
        if ($nativeMain.Count -eq 1) {
            $statusControl = @($nativeMain[0].Controls | Where-Object { $_.Id -eq 103 })
            if ($statusControl.Count -eq 1) { $statusText = $statusControl[0].Text }
        }
        if ($null -ne $activePressure -and $null -ne $statusText) {
            if ($statusText.StartsWith('正在生成压力样例')) { $activePressure.saw_running = $true }
            if ($activePressure.saw_running -and $statusText -eq '本次任务已结束，日志已接收') {
                $activePressure.ended_utc = $now
                $activePressure.ended_ms = $watch.ElapsedMilliseconds
                $activePressure = $null
            }
        }
        if ($session.State -ne $previousState) {
            if ($session.State -eq 4 -and $previousState -eq 0) { $disconnectPending = $true }
            if ($session.State -eq 0 -and $disconnectPending) { $reconnectCycles++; $disconnectPending = $false }
            $transitions.Add([pscustomobject]@{ utc = $now; elapsed_ms = $watch.ElapsedMilliseconds; previous = $previousState; state = $session.State; name = $session.StateName; pressure_active = $null -ne $activePressure -and $activePressure.saw_running })
            $previousState = $session.State
        }
        if ($key -ne $previousKey) {
            $samples.Add([pscustomobject]@{ utc = $now; elapsed_ms = $watch.ElapsedMilliseconds; session = $session; windows = @($native.Windows) })
            $previousKey = $key
        }
        Write-JsonFile (Join-Path $runPath 'live.json') ([pscustomobject]@{
            utc = $now; session = $session; windows = @($native.Windows)
            pressure_active = $null -ne $activePressure; pressure_running_seen = $null -ne $activePressure -and $activePressure.saw_running
            status = $statusText; reconnect_cycles = $reconnectCycles
        })
        foreach ($commandFile in @(Get-ChildItem -LiteralPath (Join-Path $runPath 'commands') -Filter '*.json' -File | Sort-Object Name)) {
            $responsePath = Join-Path (Join-Path $runPath 'responses') $commandFile.Name
            $response = $null
            try {
                $command = Get-Content -LiteralPath $commandFile.FullName -Raw | ConvertFrom-Json
                $action = [string]$command.action
                $name = [string]$command.name
                if ([string]::IsNullOrWhiteSpace($name) -or $name.Length -gt 80) { throw 'name 必须为 1–80 字符。' }
                if ($action -notin @('snapshot', 'ssh', 'console', 'stress', 'finish')) { throw '不支持的 action。' }
                if ($action -ne 'finish' -and $session.State -ne 0) { throw '当前会话不是 Active，不执行 UIA 操作。' }
                switch ($action) {
                    'ssh' { Invoke-TestButton '111' }
                    'console' { Invoke-TestButton '112' }
                    'stress' {
                        if ($null -ne $activePressure) { throw '上一轮压力尚未结束。' }
                        Invoke-TestButton '113'
                        $activePressure = [pscustomobject]@{ name = $name; requested_lines = $PressureLines; started_utc = $now; started_ms = $watch.ElapsedMilliseconds; ended_utc = $null; ended_ms = $null; saw_running = $false }
                        $pressureRuns.Add($activePressure)
                    }
                    'finish' {
                        if ($RequireDpiRoundTrip) {
                            $roundTrip = Get-DpiRoundTrip @($snapshots) $testPid $mainStart $mainHwnd
                            if (-not $roundTrip.complete) { throw 'finish 前须以同一进程的显式快照完成两个窗口的 96→144→192→96 DPI 往返。' }
                        }
                        $finished = $true
                    }
                }
                if ($action -eq 'snapshot') {
                    $data = if ($SnapshotInSubprocess) { Get-SubprocessSnapshot $name } else { Get-WindowSnapshot $name }
                } else { $data = $null }
                $response = [pscustomobject]@{ action = $action; name = $name; passed = $true; utc = [DateTime]::UtcNow.ToString('o'); data = $data }
            }
            catch {
                Add-Check ('command:' + $commandFile.Name) $false $_.Exception.Message
                $response = [pscustomobject]@{ passed = $false; failure = $_.Exception.ToString() }
            }
            Write-JsonFile $responsePath $response
            Move-Item -LiteralPath $commandFile.FullName -Destination (Join-Path (Join-Path $runPath 'processed') $commandFile.Name)
        }
        if (-not $finished) { Start-Sleep -Milliseconds $PollMilliseconds }
    }
    if (-not $finished) { throw '验收达到超时上限，不能视为测试通过。' }
}
catch { $failure = $_.Exception.ToString(); Add-Check 'controller_exception' $false $failure }
finally {
    if ($null -ne $process) {
        try {
            for ($attempt = 0; $attempt -lt 2 -and -not $process.HasExited; $attempt++) {
                $process.Refresh()
                $null = $process.CloseMainWindow()
                $null = $process.WaitForExit(1500)
            }
            $cleanExit = $process.HasExited
            Add-Check 'clean_exit' $cleanExit '只关闭本次验收启动的进程'
            if (-not $cleanExit) { $process.Kill($true); $process.WaitForExit() }
            if ($cleanExit) { Add-Check 'zero_exit_code' ($process.ExitCode -eq 0) '正常退出码为零' }
        }
        catch { Add-Check 'cleanup_exception' $false $_.Exception.ToString() }
        finally { $process.Dispose() }
    }
    $testedWindows = @($snapshots | ForEach-Object { $_.windows } | Where-Object { $_.Visible })
    $testedDpi = @($testedWindows | ForEach-Object { [int]$_.Dpi } | Sort-Object -Unique)
    $mainDpi = @($testedWindows | Where-Object { $_.Handle -eq $mainHwnd } | ForEach-Object { [int]$_.Dpi } | Sort-Object -Unique)
    $observerDpi = @($testedWindows | Where-Object { $_.Title -eq 'RemoteOps · 只读操作观察窗' } | ForEach-Object { [int]$_.Dpi } | Sort-Object -Unique)
    foreach ($dpi in $RequiredDpi) { Add-Check ('actual_dpi_' + $dpi) (($dpi -in $mainDpi) -and ($dpi -in $observerDpi)) '主窗口和观察窗分别由显式快照覆盖真实 HWND DPI，不使用布局注入或位图缩放' }
    Add-Check 'real_reconnect_cycles' ($reconnectCycles -ge $RequiredReconnectCycles) ('采样到 Active→Disconnected→Active 次数：' + $reconnectCycles)
    $pressureDisconnects = @($transitions | Where-Object { $_.state -eq 4 -and $_.pressure_active }).Count
    Add-Check 'disconnect_during_pressure' ($pressureDisconnects -ge $RequiredPressureDisconnects) '真实断开状态出现在压力操作实际运行且尚未报告完成期间'
    if ($null -ne $InitialMainLeft) { Add-Check 'initial_main_position_applied' $mainPlaced '只移动本次启动的主窗口一次' }
    if ($null -ne $InitialObserverLeft) { Add-Check 'initial_observer_position_applied' $observerPlaced '只移动本次启动的观察窗一次' }
    $roundTrip = Get-DpiRoundTrip @($snapshots) $testPid $mainStart $mainHwnd
    if ($RequireDpiRoundTrip) { Add-Check 'actual_dpi_round_trip' ($roundTrip.complete -and $finished) '同一进程两个窗口的显式快照完成 96→144→192→96，之后才接受 finish' }
    $passed = $null -eq $failure -and @($checks | Where-Object { -not $_.passed }).Count -eq 0
    $result = [pscustomobject]@{
        schema_version = 2; passed = $passed; finished = $finished; failure = $failure
        executable = [pscustomobject]@{ path = $exePath; sha256 = $exeHash; version = $exeVersion.ProductVersion }
        process_id = $testPid
        process_start_utc = $mainStart; main_handle = $mainHwnd
        observed_dpi = $testedDpi; observed_main_dpi = $mainDpi; observed_observer_dpi = $observerDpi; required_dpi = $RequiredDpi; reconnect_cycles = $reconnectCycles; required_reconnect_cycles = $RequiredReconnectCycles; pressure_disconnects = $pressureDisconnects; required_pressure_disconnects = $RequiredPressureDisconnects
        checks = @($checks); snapshots = @($snapshots); samples = @($samples); transitions = @($transitions); pressure_lines = $PressureLines; pressure_runs = @($pressureRuns)
        require_dpi_round_trip = [bool]$RequireDpiRoundTrip; dpi_round_trip = $roundTrip
        snapshot_in_subprocess = [bool]$SnapshotInSubprocess; snapshot_workers = @($snapshotWorkers)
        initial_positions_applied = [pscustomobject]@{ main = $mainPlaced; observer = $observerPlaced }
        application_report = $applicationReportPath
        limitations = @('DPI 证据来自真实窗口 API；未采到目标 DPI 时明确失败。', '会话状态按间隔采样，短于采样周期的断开可能漏记；不伪造会话事件。', '本轮只测离线小样，不替换现场旧 Agent；SSH/Console 是演示。', '持续输出断开的判定依据压力开始/完成状态与 WTS 状态采样，不能证明断开期间每一帧都绘制。')
    }
    Write-JsonFile $reportPath $result
    Write-Output ('会话验收记录：' + $reportPath)
}
if (-not $result.passed) { throw '真实会话或高 DPI 验收未全部通过，详见 report.json。' }
