#requires -Version 7.2
<#
.SYNOPSIS
在 C 盘临时隔离目录验收产品布局；不读取旧版配置、不连接真实 Relay。
.DESCRIPTION
通过短命原生 UIA 客户端测试自己的新进程，截图只捕获该窗口。
Esc 项验证目标窗口按键消息路径，不改变系统缩放、RDP 或其他程序。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Executable,
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-fA-F]{64}$')] [string]$ExpectedHash,
    [string]$OutputDirectory,
    [string]$InitializationScript = (Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw '产品原生界面验收仅支持 Windows。' }
$executablePath = (Resolve-Path -LiteralPath $Executable).Path
if ([Diagnostics.FileVersionInfo]::GetVersionInfo($executablePath).ProductName -cne 'RemoteOps Native UI Prototype' -or
    (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash -ine $ExpectedHash) { throw '待测原生小样身份不匹配。' }
$cTemp = [IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('UserProfile')) 'AppData/Local/Temp'))
if (-not $cTemp.StartsWith('C:\', [StringComparison]::OrdinalIgnoreCase)) { throw '本项验收要求 C 盘 TEMP，不使用 D 盘缓存。' }
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) { $OutputDirectory = Join-Path $cTemp ('RemoteOps-native-product-' + [Guid]::NewGuid().ToString('N')) }
$runDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (-not $runDirectory.StartsWith($cTemp.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) { throw 'OutputDirectory 必须位于 C 盘用户 TEMP 内。' }
if (Test-Path -LiteralPath $runDirectory) { throw '必须使用新的独立输出目录，不能覆盖既有报告。' }
New-Item -ItemType Directory -Path $runDirectory | Out-Null
$clientPath = Join-Path $PSScriptRoot 'NativeGui-TestClient.ps1'
$checks = [Collections.Generic.List[object]]::new()
$processes = [Collections.Generic.List[object]]::new()
$failure = $null
$demo = $null
$copy = $null
$phase = 'none'

function Add-Check {
    <# .SYNOPSIS 只保存检查事实，不把窗口正文或配置写入报告。 #>
    param([string]$Name, [bool]$Passed, [string]$Detail)
    $checks.Add([pscustomobject]@{ name = $Name; passed = $Passed; detail = $Detail })
    if (-not $Passed) { throw ('验收失败：' + $Name) }
}

function Start-IsolatedApp {
    <# .SYNOPSIS 仅为本次进程提供新的空配置、缓存和临时目录。 #>
    param([string]$Name, [string[]]$Arguments)
    $directory = Join-Path $runDirectory $Name
    New-Item -ItemType Directory -Path $directory | Out-Null
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $executablePath; $start.UseShellExecute = $false; $start.WorkingDirectory = $directory
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $start.ArgumentList.Add('--report'); $start.ArgumentList.Add((Join-Path $directory 'application-report.json'))
    foreach ($name in @($start.Environment.Keys)) {
        if ($name -match '^(REMOTEOPS|JLAI)_') { $start.Environment.Remove($name) | Out-Null }
    }
    foreach ($name in @('LOCALAPPDATA', 'APPDATA', 'TEMP', 'TMP')) {
        $path = Join-Path $directory $name.ToLowerInvariant()
        New-Item -ItemType Directory -Path $path | Out-Null
        $start.Environment[$name] = $path
    }
    $process = [Diagnostics.Process]::Start($start)
    $identity = [pscustomobject]@{ process = $process; start = $process.StartTime.ToUniversalTime().ToString('o'); directory = $directory }
    $processes.Add($identity)
    $wait = [Diagnostics.Stopwatch]::StartNew()
    while ($wait.Elapsed.TotalSeconds -lt 10) {
        $process.Refresh()
        if ($process.HasExited) { throw ('测试程序提前退出：' + $Name) }
        if ($process.MainWindowHandle -ne [IntPtr]::Zero) { return $identity }
        Start-Sleep -Milliseconds 50
    }
    throw ('没有出现测试主窗：' + $Name)
}

function Client {
    <# .SYNOPSIS 使用参数列表启动短命 UIA 客户端，并限制每次操作的等待时间。 #>
    param($Identity, [string]$Action = 'Snapshot', [int]$Id = 0, [string]$Value, [string]$Output)
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path $PSHOME 'pwsh.exe'; $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $start.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    foreach ($argument in @('-NoProfile', '-STA', '-File', $clientPath,
        '-ProcessId', [string]$Identity.process.Id, '-ProcessStartUtc', $Identity.start,
        '-ExpectedHash', $ExpectedHash, '-InitializationScript', $InitializationScript,
        '-Action', $Action, '-ControlId', [string]$Id)) { $start.ArgumentList.Add($argument) }
    if ($PSBoundParameters.ContainsKey('Value')) { $start.ArgumentList.Add('-Value'); $start.ArgumentList.Add($Value) }
    if ($Output) { $start.ArgumentList.Add('-OutputPath'); $start.ArgumentList.Add($Output) }
    $client = [Diagnostics.Process]::Start($start)
    $stdout = $client.StandardOutput.ReadToEndAsync(); $stderr = $client.StandardError.ReadToEndAsync()
    try {
        if (-not $client.WaitForExit(15000)) { $client.Kill($true); $client.WaitForExit(); throw '原生测试客户端超时。' }
        $text = $stdout.GetAwaiter().GetResult(); $errorText = $stderr.GetAwaiter().GetResult()
        if ($client.ExitCode -ne 0) { throw ('原生客户端失败：' + $errorText) }
        return $text | ConvertFrom-Json
    } finally { $client.Dispose() }
}

function Row {
    <# .SYNOPSIS 在当前快照中按固定原生 ID 取得唯一控件。 #>
    param($Snapshot, [int]$Id)
    $rows = @($Snapshot.main.controls | Where-Object id -eq $Id)
    if ($rows.Count -ne 1) { throw ('当前页面没有唯一控件：' + $Id) }
    return $rows[0]
}

function Wait-Page {
    <# .SYNOPSIS 不改变焦点，等待页面中的已知控件出现。 #>
    param($Identity, [int]$Id)
    for ($attempt = 0; $attempt -lt 8; $attempt++) {
        $snapshot = Client $Identity
        if (@($snapshot.main.controls | Where-Object id -eq $Id).Count -eq 1) { return $snapshot }
        Start-Sleep -Milliseconds 80
    }
    throw ('等待页面控件超时：' + $Id)
}

try {
    $phase = 'demo'
    $demo = Start-IsolatedApp 'product-demo' @('--product-demo')
    $snapshot = Wait-Page $demo 303
    $dpi = [double]$snapshot.main.dpi
    Add-Check 'main_500x375_actual_dpi' ($snapshot.main.client_width -eq [math]::Round(500*$dpi/96) -and $snapshot.main.client_height -eq [math]::Round(375*$dpi/96)) '真实客户区与当前系统 DPI 下的 500×375 逻辑尺寸一致'
    Add-Check 'pairing_code' ((Row $snapshot 303).text -eq '123-456-789') '离线演示控制码按三位分组展示'
    foreach ($id in @(301,304,306,307,308,309)) {
        $row = Row $snapshot $id
        Add-Check ('button_uia_' + $id) ($row.control_type -eq 'ControlType.Button' -and $row.invoke) '标准按钮有类型与 InvokePattern'
    }
    $null = Client $demo 'Capture' 0 -Output (Join-Path $runDirectory 'main.png')
    $copy = Client $demo 'Invoke' 304
    Add-Check 'copy_feedback' ($copy.feedback_text -eq '✓' -and $copy.feedback_status -eq 'success') '同一客户端在真实 Invoke 后读取复制成功反馈，接受调用不等于复制成功'

    $null = Client $demo 'Invoke' 307
    $snapshot = Wait-Page $demo 323
    Add-Check 'details_read_only' ((Row $snapshot 311).value_read_only -eq $true -and (Row $snapshot 311).text.Contains('demo.invalid')) '连接详情是只读，展示独立演示地址'
    $foregroundBefore = $snapshot.foreground_handle
    $null = Client $demo 'Invoke' 337
    $snapshot = Client $demo
    Add-Check 'observer_read_only' ($null -ne $snapshot.observer -and $snapshot.observer.text_pattern -and $snapshot.observer.read_only) '观察窗具有 TextPattern，实际 EDIT 只读'
    Add-Check 'observer_open_no_focus' ($snapshot.foreground_handle -eq $foregroundBefore) '打开观察窗前后前台窗口不变'
    $null = Client $demo 'Capture' 202 -Output (Join-Path $runDirectory 'observer.png')
    $null = Client $demo 'Invoke' 310
    $null = Wait-Page $demo 303

    $null = Client $demo 'Invoke' 301
    $null = Wait-Page $demo 318
    $null = Client $demo 'Invoke' 318
    $snapshot = Client $demo
    Add-Check 'english_preferences' ((Row $snapshot 300).text -eq 'Preferences' -and (Row $snapshot 310).text -eq 'Back') '英语即时更新原生页面和按钮'
    $null = Client $demo 'Invoke' 321
    $snapshot = Client $demo
    Add-Check 'dark_selected' ((Row $snapshot 321).text.StartsWith('✓')) '深色选项实际生效且有选中标记'
    $null = Client $demo 'Capture' 0 -Output (Join-Path $runDirectory 'preferences-dark-en.png')
    Add-Type -AssemblyName System.Drawing
    $darkImage = [Drawing.Bitmap]::new((Join-Path $runDirectory 'preferences-dark-en.png'))
    try {
        $darkPixel = $darkImage.GetPixel([int]$snapshot.main.client_origin_x+5, [int]$snapshot.main.client_origin_y+5)
        Add-Check 'dark_actual_background' ($darkPixel.R -eq 24 -and $darkPixel.G -eq 28 -and $darkPixel.B -eq 35) '只读取自身 PrintWindow 图片，核对实际深色背景'
    } finally { $darkImage.Dispose() }
    $null = Client $demo 'Invoke' 320
    $snapshot = Client $demo
    Add-Check 'light_selected' ((Row $snapshot 320).text.StartsWith('✓')) '浅色选项有选中标记'
    $null = Client $demo 'Invoke' 319
    $snapshot = Client $demo
    Add-Check 'system_selected' ((Row $snapshot 319).text.StartsWith('✓')) '跟随系统选项可恢复'
    $null = Client $demo 'Invoke' 322
    $snapshot = Wait-Page $demo 330
    Add-Check 'control_disabled_without_controller' (-not (Row $snapshot 330).enabled -and -not (Row $snapshot 331).enabled) '无控制端时当前会话权限按钮均禁用'
    $null = Client $demo 'Invoke' 310
    $null = Wait-Page $demo 303

    $null = Client $demo 'Invoke' 308
    $snapshot = Wait-Page $demo 324
    Add-Check 'logs_read_only' ((Row $snapshot 311).value_read_only -eq $true) '日志正文使用实际只读标准 EDIT'
    foreach ($id in @(324,325,326,327,328)) {
        $null = Client $demo 'Invoke' $id
        $snapshot = Client $demo
        $expectedCount = if ($id -eq 324) { 4 } else { 1 }
        Add-Check ('log_filter_' + $id) ((Row $snapshot $id).text.StartsWith('✓') -and
            (Row $snapshot 306).text.StartsWith(([string]$expectedCount + ' / 4'))) '实际筛选后的记录数量正确'
        $labels = @('Running','Success','Error','Info')
        $body = (Row $snapshot 311).text
        foreach ($label in $labels) {
            $shouldContain = $id -eq 324 -or $label -eq $labels[$id-325]
            Add-Check ('log_filter_content_' + $id + '_' + $label) ($body.Contains('[演示] ' + $label) -eq $shouldContain) '正文真实保留目标级别、排除其他级别'
        }
    }
    $null = Client $demo 'Invoke' 329
    $snapshot = Client $demo
    Add-Check 'logs_clear' ((Row $snapshot 306).text.StartsWith('0 / 0')) '清空显示记录后总数为零'
    $null = Client $demo 'Escape'
    $snapshot = Wait-Page $demo 303
    Add-Check 'escape_message_returns_main' ((Row $snapshot 303).text -eq '123-456-789') '目标窗口 Esc 消息通过对话框按键处理返回主界面'
    $null = Client $demo 'Close' 0 'Confirm'
    Add-Check 'normal_demo_exit' ($demo.process.WaitForExit(8000) -and $demo.process.ExitCode -eq 0) '正常关闭请求与同进程确认完成，有界退出'

    # 独立不存在的配置文件，既不读取旧配置，也不点击有效地址的连接。
    $phase = 'first_setup'
    $setupPath = Join-Path $runDirectory '不存在的首次配置 中文.json'
    $setup = Start-IsolatedApp 'first-setup' @('--agent-config', $setupPath)
    $snapshot = Wait-Page $setup 314
    $dpi = [double]$snapshot.main.dpi
    Add-Check 'first_setup_640x330' ($snapshot.main.client_width -eq [math]::Round(640*$dpi/96) -and $snapshot.main.client_height -eq [math]::Round(330*$dpi/96)) '首次设置真实客户区为 640×330 逻辑尺寸'
    $null = Client $setup 'SetValue' 314 '无效 Relay 中文 空格'
    $null = Client $setup 'Invoke' 318
    $snapshot = Client $setup
    Add-Check 'setup_language_keeps_draft' ((Row $snapshot 314).text -eq '无效 Relay 中文 空格') '切换语言保留未保存的中文 Relay 草稿'
    $null = Client $setup 'Invoke' 316
    $snapshot = Client $setup
    Add-Check 'invalid_setup_stays_visible' ((Row $snapshot 314).text -eq '无效 Relay 中文 空格' -and -not [string]::IsNullOrWhiteSpace((Row $snapshot 312).text)) '无效地址在连接前被拒绝，设置和错误保留'
    Add-Check 'invalid_setup_not_saved' (-not (Test-Path -LiteralPath $setupPath)) '无效 Relay 未生成配置文件'
    $null = Client $setup 'Capture' 0 -Output (Join-Path $runDirectory 'first-setup.png')
    $null = Client $setup 'Close' 0 'Confirm'
    Add-Check 'normal_setup_exit' ($setup.process.WaitForExit(8000) -and $setup.process.ExitCode -eq 0) '首次设置通过正常关闭有界退出'
}
catch {
    $failure = $_.Exception.Message
    # 诊断只允许离线 demo，不能把真实环回/现场窗口正文带进失败附件。
    if ($phase -eq 'demo' -and $null -ne $demo -and -not $demo.process.HasExited) {
        if ($null -ne $copy) {
            $units = if ($null -eq $copy.feedback_text) { @() } else {
                @(([string]$copy.feedback_text).ToCharArray() | Select-Object -First 64 | ForEach-Object { [int]$_ })
            }
            $diagnostic = @{ demo_only = $true; copy_feedback_utf16_units = @($units) }
            [IO.File]::WriteAllText((Join-Path $runDirectory 'demo-copy-codepoints.json'),
                ($diagnostic | ConvertTo-Json -Depth 4), [Text.UTF8Encoding]::new($false))
        }
        try { $null = Client $demo 'Snapshot' -Output (Join-Path $runDirectory 'demo-failure-snapshot.json') } catch { }
        try { $null = Client $demo 'Capture' 0 -Output (Join-Path $runDirectory 'demo-failure.png') } catch { }
    }
}
finally {
    foreach ($identity in $processes) {
        try {
            if (-not $identity.process.HasExited) {
                $null = Client $identity 'Close' 0 'Confirm'
                if (-not $identity.process.WaitForExit(8000)) { throw '本次测试程序未正常退出，保留现场，不终止其他进程。' }
            }
        }
        catch { if (-not $failure) { $failure = $_.Exception.Message } }
        finally { $identity.process.Dispose() }
    }
    $report = [pscustomobject]@{ passed = $null -eq $failure -and @($checks | Where-Object { -not $_.passed }).Count -eq 0;
        executable_sha256 = $ExpectedHash.ToLowerInvariant(); checks = @($checks); failure = $failure;
        note = '仅离线产品演示和无效配置；真实 Relay/TLS/权限确认和业务观察链路由环回测试补充。' }
    [IO.File]::WriteAllText((Join-Path $runDirectory 'report.json'), ($report | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
}
$report | ConvertTo-Json -Depth 8
if (-not $report.passed) { exit 1 }
