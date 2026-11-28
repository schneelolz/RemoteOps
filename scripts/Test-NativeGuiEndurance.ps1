#requires -Version 7.2
<#
.SYNOPSIS
同一原生小样进程反复执行完整 UIA 快照和压力输出，不更改显示或 RDP 设置。
.DESCRIPTION
SSH 完成后和每轮压力结束后都读取完整快照；默认五轮十万行。
最大批处理固定 100 毫秒；每十万行固定 15000 毫秒，不提供放宽门槛参数。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$Executable,
    [Parameter(Mandatory)] [string]$OutputDirectory,
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-fA-F]{64}$')] [string]$ExpectedHash,
    [string]$InitializationScript = (Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1'),
    [ValidateRange(1, 100)] [int]$Rounds = 5,
    [ValidateRange(1, 1000000)] [int]$PressureLines = 100000,
    [ValidateRange(30, 900)] [int]$TimeoutSeconds = 600
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw '原生小样耐久验收仅支持 Windows。' }
$utf8 = [Text.UTF8Encoding]::new($false)
$controller = $null
$ready = $null
$failure = $null
$finishRequested = $false
$commandSequence = 0
$observations = [Collections.Generic.List[object]]::new()
$cleanup = [Collections.Generic.List[string]]::new()
$validation = $null
$sessionReport = $null
$applicationReport = $null
$watch = [Diagnostics.Stopwatch]::StartNew()
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$runPath = (Resolve-Path -LiteralPath $OutputDirectory).Path
$sessionPath = Join-Path $runPath 'session'
$summaryPath = Join-Path $runPath 'endurance-summary.json'
if ((Test-Path -LiteralPath $summaryPath) -or (Test-Path -LiteralPath $sessionPath)) { throw '输出目录已有验收数据，请使用新目录，不能覆盖失败记录。' }

function Read-EnduranceText {
    <# .SYNOPSIS 共享读取原子替换文件，只对 Windows 共享锁冲突作有界重试。 #>
    param([string]$Path)
    $retryWatch = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        $stream = $null
        $reader = $null
        try {
            $sharing = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
            $stream = [IO.FileStream]::new($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, $sharing)
            $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8, $true)
            return $reader.ReadToEnd()
        }
        catch {
            $errorCause = $_.Exception
            while ($null -ne $errorCause.InnerException) { $errorCause = $errorCause.InnerException }
            $code = $errorCause.HResult -band 0xffff
            if ($errorCause -isnot [IO.IOException] -or $code -notin @(32, 33) -or $retryWatch.ElapsedMilliseconds -ge 1000) { throw }
            Start-Sleep -Milliseconds 25
        }
        finally {
            if ($null -ne $reader) { $reader.Dispose() }
            elseif ($null -ne $stream) { $stream.Dispose() }
        }
    }
}

function Read-EnduranceJson {
    <# .SYNOPSIS 读取原子 JSON，保留根级进程启动时间的精确字符串。 #>
    param([string]$Path)
    $json = Read-EnduranceText $Path
    $value = $json | ConvertFrom-Json
    if ($null -ne $value.PSObject.Properties['process_start_utc']) {
        $document = [System.Text.Json.JsonDocument]::Parse($json)
        try { $value.process_start_utc = $document.RootElement.GetProperty('process_start_utc').GetString() }
        finally { $document.Dispose() }
    }
    return $value
}

function Wait-EnduranceCondition {
    <# .SYNOPSIS 在整轮时间上限内等待真实状态，不把超时当成功。 #>
    param([scriptblock]$Condition, [string]$Description)
    while ($watch.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        $value = & $Condition
        if ($value) { return $value }
        if ($null -ne $controller) {
            $controller.Refresh()
            if ($controller.HasExited) { throw ('控制器提前退出，未完成：' + $Description) }
        }
        Start-Sleep -Milliseconds 100
    }
    throw ('耐久验收达到时间上限：' + $Description)
}

function Send-EnduranceCommand {
    <# .SYNOPSIS 用独立标记文件提交操作并等待对应响应。 #>
    param([string]$Action, [string]$Name)
    $script:commandSequence++
    $fileName = '{0:D3}-{1}.json' -f $commandSequence, $Action
    $commandPath = Join-Path (Join-Path $sessionPath 'commands') $fileName
    $temporary = $commandPath + '.writing'
    [IO.File]::WriteAllText($temporary, ([pscustomobject]@{ action = $Action; name = $Name } | ConvertTo-Json), $utf8)
    [IO.File]::Move($temporary, $commandPath)
    $responsePath = Join-Path (Join-Path $sessionPath 'responses') $fileName
    $response = Wait-EnduranceCondition { if (Test-Path -LiteralPath $responsePath) { Read-EnduranceJson $responsePath } } ('命令响应 ' + $Name)
    if (-not $response.passed) { throw ('操作失败：' + $Name + '；' + $response.failure) }
    return $response
}

function Get-EnduranceLive {
    <# .SYNOPSIS 只读取本轮控制器的采样文件，不直接操作应用控件。 #>
    $path = Join-Path $sessionPath 'live.json'
    if (Test-Path -LiteralPath $path) { return Read-EnduranceJson $path }
    return $null
}

function Test-EnduranceReports {
    <# .SYNOPSIS 按固定性能及完整输出标准验证原始报告，不修改报告。 #>
    param($Session, $Application, [int]$Count, [int]$Lines)
    $checks = [Collections.Generic.List[object]]::new()
    $rows = [Collections.Generic.List[object]]::new()
    $checks.Add([pscustomobject]@{ name = 'session_passed'; passed = $Session.passed -eq $true -and $Session.finished -eq $true -and @($Session.checks | Where-Object { -not $_.passed }).Count -eq 0 })
    $checks.Add([pscustomobject]@{ name = 'application_passed'; passed = $Application.passed -eq $true -and @($Application.checks | Where-Object { -not $_.passed }).Count -eq 0 })
    $checks.Add([pscustomobject]@{ name = 'max_tick_100ms'; passed = $Application.metrics.max_tick_ms -le 100 })
    $checks.Add([pscustomobject]@{ name = 'pending_zero'; passed = $Application.metrics.pending_units -eq 0 })
    # pending 为零不能证明已呈现；必须同时核对呈现游标和关闭前实际控件全文。
    $caughtUp = $Application.metrics.PSObject.Properties['view_caught_up']
    $renderedTotal = $Application.metrics.PSObject.Properties['rendered_total_lines']
    $actualTextMatches = $Application.metrics.PSObject.Properties['rendered_text_matches_model_on_close']
    $checks.Add([pscustomobject]@{ name = 'view_caught_up'; passed = $null -ne $caughtUp -and $caughtUp.Value -is [bool] -and $caughtUp.Value -eq $true })
    $checks.Add([pscustomobject]@{ name = 'rendered_total_matches_received'; passed = $null -ne $renderedTotal -and $null -ne $renderedTotal.Value -and $renderedTotal.Value -eq $Application.metrics.received_lines })
    $checks.Add([pscustomobject]@{ name = 'actual_text_matches_model_on_close'; passed = $null -ne $actualTextMatches -and $actualTextMatches.Value -is [bool] -and $actualTextMatches.Value -eq $true })
    $checks.Add([pscustomobject]@{ name = 'all_lines_received'; passed = $Application.metrics.received_lines -eq ([long]$Count * $Lines + 5) })
    $checks.Add([pscustomobject]@{ name = 'zero_dropped'; passed = $Application.metrics.dropped_lines -eq 0 })
    $checks.Add([pscustomobject]@{ name = 'all_pressure_runs_present'; passed = @($Session.pressure_runs).Count -eq $Count })
    $budget = [double]$Lines * 15000 / 100000
    for ($index = 1; $index -le $Count; $index++) {
        $name = 'round-{0:D2}' -f $index
        $matching = @($Session.pressure_runs | Where-Object { $_.name -eq $name })
        $duration = $null
        $passed = $false
        if ($matching.Count -eq 1) {
            $round = $matching[0]
            if ($null -ne $round.ended_ms) { $duration = [long]$round.ended_ms - [long]$round.started_ms }
            $passed = $round.saw_running -eq $true -and $round.requested_lines -eq $Lines -and $null -ne $duration -and $duration -ge 0 -and $duration -le $budget
        }
        $rows.Add([pscustomobject]@{ name = $name; passed = $passed; duration_ms = $duration; budget_ms = $budget; requested_lines = $Lines })
        $checks.Add([pscustomobject]@{ name = ($name + ':running_and_duration'); passed = $passed })
    }
    return [pscustomobject]@{ passed = @($checks | Where-Object { -not $_.passed }).Count -eq 0; checks = @($checks); rounds = @($rows) }
}

try {
    $exePath = (Resolve-Path -LiteralPath $Executable).Path
    $initializationPath = (Resolve-Path -LiteralPath $InitializationScript).Path
    $sessionScript = Join-Path $PSScriptRoot 'Test-NativeGuiSession.ps1'
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path $PSHOME 'pwsh.exe'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-STA', '-File', $sessionScript, '-Executable', $exePath,
        '-OutputDirectory', $sessionPath, '-ExpectedHash', $ExpectedHash, '-InitializationScript', $initializationPath,
        '-PressureLines', [string]$PressureLines, '-TimeoutSeconds', [string]$TimeoutSeconds,
        '-RequiredDpi', '96', '-RequiredReconnectCycles', '0', '-RequiredPressureDisconnects', '0')) { $start.ArgumentList.Add($argument) }
    $controller = [Diagnostics.Process]::Start($start)
    $stdout = $controller.StandardOutput.ReadToEndAsync()
    $stderr = $controller.StandardError.ReadToEndAsync()
    $readyPath = Join-Path $sessionPath 'ready.json'
    $ready = Wait-EnduranceCondition { if (Test-Path -LiteralPath $readyPath) { Read-EnduranceJson $readyPath } } 'ready.json'
    $null = Send-EnduranceCommand 'ssh' 'ssh-initial'
    $null = Wait-EnduranceCondition {
        $live = Get-EnduranceLive
        if ($null -eq $live -or $live.status -ne '本次任务已结束，日志已接收') { return $null }
        $main = @($live.windows | Where-Object { $_.Handle -eq $ready.main_handle })
        if ($main.Count -ne 1) { return $null }
        $recent = @($main[0].Controls | Where-Object { $_.Id -eq 118 })
        if ($recent.Count -eq 1 -and $recent[0].Text -eq '[演示] SSH 场景展示结束') { return $live }
    } 'SSH 状态和最近输出均确认完成'
    $null = Send-EnduranceCommand 'snapshot' 'ssh-full-uia'
    for ($index = 1; $index -le $Rounds; $index++) {
        $name = 'round-{0:D2}' -f $index
        $null = Send-EnduranceCommand 'stress' $name
        $running = Wait-EnduranceCondition {
            $live = Get-EnduranceLive
            if ($null -ne $live -and $live.pressure_active -and $live.pressure_running_seen) { return $live }
        } ($name + ' 实际运行')
        $ended = Wait-EnduranceCondition {
            $live = Get-EnduranceLive
            if ($null -ne $live -and -not $live.pressure_active -and $live.status -eq '本次任务已结束，日志已接收') { return $live }
        } ($name + ' 输出全部结束')
        $observations.Add([pscustomobject]@{ name = $name; running_utc = $running.utc; ended_utc = $ended.utc; saw_running = $true })
        $null = Send-EnduranceCommand 'snapshot' ($name + '-full-uia')
    }
    $null = Send-EnduranceCommand 'finish' 'endurance-finish'
    $finishRequested = $true
    $null = Wait-EnduranceCondition { $controller.Refresh(); if ($controller.HasExited) { return $true } } '控制器正常退出'
}
catch { $failure = $_.Exception.ToString() }
finally {
    if ($null -ne $controller) {
        try {
            $controller.Refresh()
            if (-not $controller.HasExited -and -not $finishRequested -and $null -ne $ready) {
                # 错误路径也尽量通过正常 finish 留下完整原始报告。
                $script:commandSequence++
                $finishPath = Join-Path (Join-Path $sessionPath 'commands') ('{0:D3}-cleanup-finish.json' -f $commandSequence)
                $temporary = $finishPath + '.writing'
                [IO.File]::WriteAllText($temporary, '{"action":"finish","name":"cleanup-finish"}', $utf8)
                [IO.File]::Move($temporary, $finishPath)
                $cleanup.Add('已提交正常 finish 清理请求。')
            }
            if (-not $controller.WaitForExit(5000)) {
                if ($null -ne $ready) {
                    $owned = Get-Process -Id $ready.process_id -ErrorAction SilentlyContinue
                    if ($null -ne $owned) {
                        try {
                            if ($owned.StartTime.ToUniversalTime().ToString('o') -ceq $ready.process_start_utc -and $owned.Path -ieq $exePath) {
                                $null = $owned.CloseMainWindow()
                                if (-not $owned.WaitForExit(1500)) { $owned.Kill($true); $owned.WaitForExit() }
                                $cleanup.Add('仅关闭本轮 ready 身份匹配的小样。')
                            } else { $cleanup.Add('小样身份不匹配，未操作该进程。') }
                        } finally { $owned.Dispose() }
                    }
                }
                if (-not $controller.WaitForExit(2000)) { $controller.Kill($false); $controller.WaitForExit(); $cleanup.Add('只终止本轮控制器。') }
            }
            [IO.File]::WriteAllText((Join-Path $runPath 'controller.stdout.txt'), $stdout.GetAwaiter().GetResult(), $utf8)
            [IO.File]::WriteAllText((Join-Path $runPath 'controller.stderr.txt'), $stderr.GetAwaiter().GetResult(), $utf8)
        } catch { $cleanup.Add($_.Exception.ToString()); if ($null -eq $failure) { $failure = $_.Exception.ToString() } }
    }
    try {
        $sessionReportPath = Join-Path $sessionPath 'report.json'
        $applicationReportPath = Join-Path $sessionPath 'application-report.json'
        if (Test-Path -LiteralPath $sessionReportPath) { $sessionReport = Read-EnduranceJson $sessionReportPath }
        if (Test-Path -LiteralPath $applicationReportPath) { $applicationReport = Read-EnduranceJson $applicationReportPath }
        if ($null -ne $sessionReport -and $null -ne $applicationReport) { $validation = Test-EnduranceReports $sessionReport $applicationReport $Rounds $PressureLines }
    } catch { if ($null -eq $failure) { $failure = $_.Exception.ToString() } }
    $exitCode = if ($null -ne $controller -and $controller.HasExited) { $controller.ExitCode } else { $null }
    $passed = $null -eq $failure -and $exitCode -eq 0 -and $null -ne $validation -and $validation.passed -and $observations.Count -eq $Rounds
    $summary = [pscustomobject]@{
        schema_version = 1; passed = $passed; failure = $failure; elapsed_ms = $watch.ElapsedMilliseconds
        rounds_requested = $Rounds; pressure_lines = $PressureLines; expected_received_lines = [long]$Rounds * $PressureLines + 5
        thresholds = [pscustomobject]@{ max_tick_ms = 100; duration_ms_per_100000_lines = 15000; pending_units = 0; dropped_lines = 0; view_caught_up = $true; rendered_total_equals_received = $true; rendered_text_matches_model_on_close = $true }
        controller_exit_code = $exitCode; observations = @($observations); validation = $validation; cleanup = @($cleanup)
        ready = $ready; session_report = $sessionReportPath; application_report = $applicationReportPath
        metrics = $(if ($null -ne $applicationReport) { $applicationReport.metrics } else { $null })
        limitations = @('只验证同一进程反复完整 UIA 快照与压力；不更改系统缩放，不执行 RDP 断开重连。', 'SSH 是演示；不代表真实交换机链路。')
    }
    [IO.File]::WriteAllText($summaryPath, ($summary | ConvertTo-Json -Depth 12), $utf8)
    if ($null -ne $controller) { $controller.Dispose() }
    Write-Output ('耐久验收汇总：' + $summaryPath)
}
if (-not $summary.passed) { throw '原生界面耐久验收未全部通过，详见 endurance-summary.json。' }
