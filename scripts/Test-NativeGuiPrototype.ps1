#requires -Version 7.2
<#
.SYNOPSIS
在隔离目录验收原生界面小样的离线压力输出和退出行为。
.DESCRIPTION
新版验收要求可见观察窗实际执行增量渲染，且最终文本与脱敏模型一致。
性能指标与全部原始报告保存在 summary.json；远程会话标志不代表已验证 RDP 重连。
.PARAMETER MaxUiTickMs
允许的最大单次 UI 批处理耗时，默认 100 毫秒；零表示只记录而不应用该门槛。
.PARAMETER MaxNormalDurationMs
完整十万行压力场景的墙钟预算，默认 15000 毫秒；零表示只记录而不应用该门槛。
.PARAMETER AllowLegacyReport
仅测量旧版基线，跳过新版渲染证据和性能门槛；摘要会明确标记不能作为新版验收。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Executable,
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../artifacts/native-ui-prototype'),
    [ValidateRange(0, 60000)]
    [int]$MaxUiTickMs = 100,
    [ValidateRange(0, 90000)]
    [int]$MaxNormalDurationMs = 15000,
    # 只用于测量旧版基线；明确跳过新版渲染证据及性能门槛，不能作为新版验收。
    [switch]$AllowLegacyReport
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw '原生界面小样验收只支持 Windows。' }
$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
if (-not (Test-Path -LiteralPath $resolvedExecutable -PathType Leaf)) {
    throw 'Executable 必须是已构建的小样 EXE。'
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$resolvedOutput = (Resolve-Path -LiteralPath $OutputDirectory).Path
$runDirectory = Join-Path $resolvedOutput ('中文目录 带空格/' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runDirectory -Force | Out-Null
$copiedExecutable = Join-Path $runDirectory ([IO.Path]::GetFileName($resolvedExecutable))
Copy-Item -LiteralPath $resolvedExecutable -Destination $copiedExecutable
$utf8 = [Text.UTF8Encoding]::new($false)

function Invoke-NativeSelfTest {
    <# .SYNOPSIS 运行一个独立小样进程并验证其结构化报告。 #>
    param([string]$Name, [long]$StressLines, [switch]$EarlyExit)

    $caseDirectory = Join-Path $runDirectory $Name
    New-Item -ItemType Directory -Path $caseDirectory | Out-Null
    $reportPath = Join-Path $caseDirectory '自测报告 带空格.json'
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $copiedExecutable
    $info.WorkingDirectory = $caseDirectory
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = $utf8
    $info.StandardErrorEncoding = $utf8
    foreach ($argument in @('--self-test', '--stress-lines', [string]$StressLines, '--report', $reportPath)) {
        $info.ArgumentList.Add($argument)
    }
    if ($EarlyExit) {
        $info.ArgumentList.Add('--exit-after-ms')
        $info.ArgumentList.Add('100')
    }
    # 自测不接受远程配置；环境和当前目录也隔离，避免读取现场配置或凭据。
    foreach ($environmentName in @($info.Environment.Keys)) {
        if ($environmentName -match '^(REMOTEOPS|JLAI)_') { $info.Environment.Remove($environmentName) | Out-Null }
    }
    foreach ($environmentName in @('LOCALAPPDATA', 'APPDATA', 'TEMP', 'TMP')) {
        $path = Join-Path $caseDirectory $environmentName.ToLowerInvariant()
        New-Item -ItemType Directory -Path $path | Out-Null
        $info.Environment[$environmentName] = $path
    }

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $started = $false
    $exitCode = $null
    $peakWorkingSet = 0L
    $report = $null
    $performance = $null
    $failure = $null
    try {
        $started = $process.Start()
        if (-not $started) { throw '无法启动小样进程。' }
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        while (-not $process.WaitForExit(100)) {
            try {
                $process.Refresh()
                $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
            }
            catch {
                # 进程可能刚好在采样间隔退出，内存采样失败不替代真实退出码判断。
                if (-not $process.HasExited) { throw }
            }
            if ($timer.Elapsed.TotalSeconds -ge 90) {
                throw '自测超过 90 秒；本次进程将终止，结果记为失败。'
            }
        }
        $process.WaitForExit()
        $exitCode = $process.ExitCode
        [IO.File]::WriteAllText((Join-Path $caseDirectory 'stdout.txt'), $stdoutTask.GetAwaiter().GetResult(), $utf8)
        [IO.File]::WriteAllText((Join-Path $caseDirectory 'stderr.txt'), $stderrTask.GetAwaiter().GetResult(), $utf8)
        if ($exitCode -ne 0) { throw "自测退出码非零：$exitCode" }
        if (-not (Test-Path -LiteralPath $reportPath -PathType Leaf)) { throw '缺少自测报告。' }
        $report = Get-Content -LiteralPath $reportPath -Raw | ConvertFrom-Json
        if ($report.schema_version -ne 1 -or $report.passed -isnot [bool] -or -not $report.passed) {
            throw '报告版本不符或 passed 不为 true。'
        }
        $checks = @($report.checks)
        if ($checks.Count -lt 10 -or @($checks | Where-Object { $_.passed -isnot [bool] -or -not $_.passed }).Count -gt 0) {
            throw '报告应至少包含 10 项检查，且全部通过。'
        }
        $metrics = $report.metrics
        $performance = $metrics
        foreach ($metricName in @('produced_lines', 'received_lines', 'retained_lines', 'trimmed_lines', 'dropped_lines', 'elapsed_ms')) {
            $value = $metrics.$metricName
            if ($null -eq $value -or [decimal]$value -lt 0 -or [decimal]$value -ne [Math]::Truncate([decimal]$value)) {
                throw "计数不符合非负整数约束：$metricName"
            }
        }
        if ([long]$metrics.retained_lines + [long]$metrics.trimmed_lines -ne [long]$metrics.received_lines) {
            throw '保留行数与裁剪行数之和不等于已接收行数。'
        }
        if ([long]$metrics.retained_lines -gt 2000) { throw '保留历史超过 2000 行。' }
        if (-not $EarlyExit -and [long]$metrics.produced_lines -ne $StressLines) {
            throw "实际生成行数不等于请求的 $StressLines 行。"
        }
        if (-not $EarlyExit -and ([long]$metrics.dropped_lines -ne 0 -or [long]$metrics.received_lines -ne $StressLines)) {
            throw '完整压力场景必须零传输丢行，且全部请求行均已接收。'
        }
        if ($EarlyExit -and ($metrics.cancelled_during_output -isnot [bool] -or -not $metrics.cancelled_during_output)) {
            throw '提前退出报告未声明输出期间取消。'
        }
        if ($EarlyExit -and ([long]$metrics.produced_lines -le 0 -or [long]$metrics.produced_lines -ge $StressLines)) {
            throw '提前退出未覆盖正在输出的场景：生成行数应大于零且小于请求总量。'
        }
        if (-not $AllowLegacyReport) {
            foreach ($metricName in @('max_tick_ms', 'p95_tick_ms', 'max_timer_gap_ms', 'rendered_utf16_units',
                    'full_text_replacements', 'incremental_text_updates', 'render_updates')) {
                $property = $metrics.PSObject.Properties[$metricName]
                if ($null -eq $property -or $null -eq $property.Value -or
                    [decimal]$property.Value -lt 0 -or
                    [decimal]$property.Value -ne [Math]::Truncate([decimal]$property.Value)) {
                    throw "缺少有效的渲染性能指标：$metricName"
                }
            }
            if ([long]$metrics.p95_tick_ms -gt [long]$metrics.max_tick_ms) {
                throw 'UI 批处理 P95 大于最大值，性能指标不一致。'
            }
            if ([long]$metrics.rendered_utf16_units -gt 262144) {
                throw '日志文本控件 实际呈现文本超过 262144 个 UTF-16 单元。'
            }
            if ([long]$metrics.full_text_replacements + [long]$metrics.incremental_text_updates -ne [long]$metrics.render_updates) {
                throw '完整替换与增量更新次数之和不等于实际渲染更新次数。'
            }
            if ($MaxUiTickMs -gt 0 -and [long]$metrics.max_tick_ms -gt $MaxUiTickMs) {
                throw "UI 单次批处理超过 $MaxUiTickMs 毫秒：$($metrics.max_tick_ms) 毫秒。"
            }
            if (-not $EarlyExit) {
                $visibleProperty = $metrics.PSObject.Properties['observer_visible_during_stress']
                if ($null -eq $visibleProperty -or $visibleProperty.Value -isnot [bool] -or -not $visibleProperty.Value) {
                    throw '完整压力场景缺少观察窗实际可见的证据，不能用隐藏窗口代替日志刷新验收。'
                }
                $renderChecks = @($checks | Where-Object { $_.name -eq 'rendered_transcript_matches_model' })
                if ($renderChecks.Count -ne 1 -or -not $renderChecks[0].passed) {
                    throw '完整压力场景缺少 日志文本控件 最终文本与脱敏日志模型一致的检查。'
                }
                if ([long]$metrics.incremental_text_updates -le 0 -or [long]$metrics.rendered_utf16_units -le 0) {
                    throw '完整压力场景没有实际增量渲染或没有呈现日志文本。'
                }
                if ($MaxNormalDurationMs -gt 0 -and $timer.ElapsedMilliseconds -gt $MaxNormalDurationMs) {
                    throw "完整压力场景超过 $MaxNormalDurationMs 毫秒：$($timer.ElapsedMilliseconds) 毫秒。"
                }
            }
        }
        # remote_session 只是运行环境标志，不能据此声称验证过 RDP 断开重连。
        if ($report.environment.remote_session -isnot [bool] -or [int]$report.environment.dpi -le 0) {
            throw '缺少有效的 DPI 或远程会话环境信息。'
        }
        $null = $report.environment.rdp_test_status
        $null = $report.limitations
    }
    catch {
        $failure = $_.Exception.Message
    }
    finally {
        if ($started -and -not $process.HasExited) {
            # 仅清理本脚本启动的进程，不能用进程名批量停止其他小样或产品。
            $process.Kill($true)
            $process.WaitForExit()
        }
        $timer.Stop()
        $process.Dispose()
    }
    [PSCustomObject]@{
        name = $Name
        passed = $null -eq $failure
        failure = $failure
        exit_code = $exitCode
        requested_lines = $StressLines
        early_exit_after_ms = $(if ($EarlyExit) { 100 } else { $null })
        wall_elapsed_ms = $timer.ElapsedMilliseconds
        observed_peak_working_set_bytes = $peakWorkingSet
        performance = $performance
        report_path = $reportPath
        report = $report
    }
}

$cases = @(
    Invoke-NativeSelfTest -Name '完整压力 100000行' -StressLines 100000
    foreach ($index in 1..5) {
        Invoke-NativeSelfTest -Name ('输出时退出 第{0}轮' -f $index) -StressLines 500000 -EarlyExit
    }
)

# 压缩口径只含一个 EXE，不包括报告、伴随程序或完整发布包。
$zipPath = Join-Path $runDirectory 'single-exe.zip'
$zipFile = [IO.File]::Open($zipPath, [IO.FileMode]::CreateNew)
try {
    $archive = [IO.Compression.ZipArchive]::new($zipFile, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $entry = $archive.CreateEntry([IO.Path]::GetFileName($copiedExecutable), [IO.Compression.CompressionLevel]::SmallestSize)
        $destination = $entry.Open()
        $source = [IO.File]::OpenRead($copiedExecutable)
        try { $source.CopyTo($destination) }
        finally { $source.Dispose(); $destination.Dispose() }
    }
    finally { $archive.Dispose() }
}
finally { $zipFile.Dispose() }
$summary = [PSCustomObject]@{
    schema_version = 1
    passed = @($cases | Where-Object { -not $_.passed }).Count -eq 0
    validation = [PSCustomObject]@{
        mode = $(if ($AllowLegacyReport) { '旧版基线测量，不代表新版验收' } else { '可见观察窗增量渲染验收' })
        legacy_report = [bool]$AllowLegacyReport
        max_ui_tick_ms = $(if ($AllowLegacyReport) { $null } else { $MaxUiTickMs })
        max_normal_duration_ms = $(if ($AllowLegacyReport) { $null } else { $MaxNormalDurationMs })
    }
    executable = [PSCustomObject]@{
        bytes = (Get-Item -LiteralPath $copiedExecutable).Length
        sha256 = (Get-FileHash -LiteralPath $copiedExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
        single_exe_zip_bytes = (Get-Item -LiteralPath $zipPath).Length
        zip_method = '.NET DEFLATE SmallestSize；仅一个 EXE'
    }
    cases = $cases
    limitations = @(
        'DPI 为小样逻辑布局注入，未修改或验证真实系统缩放、跨屏切换。',
        '远程会话标志不代表 RDP 断开重连验收；无实际会话时如实未测。',
        '进程内存为本机压力场景的采样观测，不是长期运行性能基准。',
        '自测不连接 Relay、SSH 或交换机，不代替真实远程能力验收。'
    )
}
$summaryPath = Join-Path $runDirectory 'summary.json'
[IO.File]::WriteAllText($summaryPath, ($summary | ConvertTo-Json -Depth 15), $utf8)
$cases | Select-Object name, passed, exit_code, wall_elapsed_ms, observed_peak_working_set_bytes,
    @{ Name = 'max_tick_ms'; Expression = {
        if ($null -ne $_.performance) { $value = $_.performance.PSObject.Properties['max_tick_ms']; if ($null -ne $value) { $value.Value } }
    } },
    @{ Name = 'p95_tick_ms'; Expression = {
        if ($null -ne $_.performance) { $value = $_.performance.PSObject.Properties['p95_tick_ms']; if ($null -ne $value) { $value.Value } }
    } } | Format-Table -AutoSize
Write-Output ('验收记录：' + $summaryPath)
if (-not $summary.passed) { throw 'Windows 原生界面小样验收失败，详见 summary.json。' }
