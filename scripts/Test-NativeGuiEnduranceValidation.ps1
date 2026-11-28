#requires -Version 7.2
<#
.SYNOPSIS
复跑耐久验收的真实文件锁与严格报告门槛回归，不启动 GUI。
.DESCRIPTION
只从实际耐久脚本 AST 加载读取与验证函数；持锁文件均位于本次新建目录。
#>
[CmdletBinding()]
param(
    [string]$EnduranceScript = (Join-Path $PSScriptRoot 'Test-NativeGuiEndurance.ps1'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../artifacts/native-ui-prototype/endurance-validation-tests')
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw '文件共享锁回归仅支持 Windows。' }
$errors = $null
$tokens = $null
$ast = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path -LiteralPath $EnduranceScript).Path, [ref]$tokens, [ref]$errors)
if ($errors.Count -gt 0) { throw ($errors | Out-String) }
foreach ($name in @('Read-EnduranceText', 'Read-EnduranceJson', 'Test-EnduranceReports')) {
    $definition = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $true)
    if ($null -eq $definition) { throw ('缺少实际耐久验收函数：' + $name) }
    # 不能执行会启动控制器和小样的脚本入口。
    . ([scriptblock]::Create($definition.Extent.Text))
}
$utf8 = [Text.UTF8Encoding]::new($false)
$checks = [Collections.Generic.List[object]]::new()
$casePath = Join-Path $OutputDirectory ([Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $casePath | Out-Null
$casePath = (Resolve-Path -LiteralPath $casePath).Path
$filePath = Join-Path $casePath '中文 live.json'
Add-Type -TypeDefinition @"
using System;
using System.IO;
using System.Threading;
using System.Threading.Tasks;

/// <summary>仅对调用方新建的测试文件取得排他锁。</summary>
public static class RemoteOpsEnduranceValidationLock
{
    /// <summary>确认取得锁后返回，并在指定时间后释放。</summary>
    public static Task Hold(string path, int milliseconds)
    {
        ManualResetEventSlim ready = new ManualResetEventSlim(false);
        Task task = Task.Run(() => {
            using (FileStream stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.None)) {
                ready.Set(); Thread.Sleep(milliseconds);
            }
        });
        if (!ready.Wait(3000)) throw new TimeoutException("排他锁未取得。");
        ready.Dispose();
        return task;
    }
}
"@
[IO.File]::WriteAllText($filePath, '{"passed":true,"process_start_utc":"2026-10-30T09:14:58.5507235Z"}', $utf8)
$lock = [RemoteOpsEnduranceValidationLock]::Hold($filePath, 250)
$timer = [Diagnostics.Stopwatch]::StartNew()
try { $value = Read-EnduranceJson $filePath; $shortElapsed = $timer.ElapsedMilliseconds }
finally { $lock.GetAwaiter().GetResult() }
if (-not $value.passed -or $value.process_start_utc -cne '2026-10-30T09:14:58.5507235Z' -or $shortElapsed -lt 150 -or $shortElapsed -gt 1800) { throw '短锁释放后未正确读取完整 JSON。' }
$checks.Add([pscustomobject]@{ name = 'short_exclusive_lock_recovers'; passed = $true; elapsed_ms = $shortElapsed })
$lock = [RemoteOpsEnduranceValidationLock]::Hold($filePath, 2200)
$timer.Restart()
$lockError = $null
$failureElapsed = $null
try { $null = Read-EnduranceJson $filePath }
catch { $lockError = $_.Exception.GetBaseException(); $failureElapsed = $timer.ElapsedMilliseconds }
finally { $lock.GetAwaiter().GetResult() }
if ($null -eq $lockError -or $lockError -isnot [IO.IOException] -or ($lockError.HResult -band 0xffff) -notin @(32, 33) -or $failureElapsed -lt 1000 -or $failureElapsed -gt 1800) { throw '持续排他锁未在有界时间内保留实际锁异常。' }
$checks.Add([pscustomobject]@{ name = 'persistent_exclusive_lock_fails'; passed = $true; elapsed_ms = $failureElapsed; error = $lockError.GetType().FullName })
if (-not (Read-EnduranceJson $filePath).passed) { throw '解锁后未恢复读取。' }
$checks.Add([pscustomobject]@{ name = 'read_recovers_after_unlock'; passed = $true })
[IO.File]::WriteAllText($filePath, 'broken JSON', $utf8)
$timer.Restart()
$badJsonFailed = $false
try { $null = Read-EnduranceJson $filePath } catch { $badJsonFailed = $true }
$jsonElapsed = $timer.ElapsedMilliseconds
if (-not $badJsonFailed -or $jsonElapsed -ge 900) { throw '坏 JSON 被吞掉或错误地按共享锁重试。' }
$checks.Add([pscustomobject]@{ name = 'bad_json_fails_immediately'; passed = $true; elapsed_ms = $jsonElapsed })
$timer.Restart()
$missingError = $null
try { $null = Read-EnduranceJson (Join-Path $casePath 'absent.json') } catch { $missingError = $_.Exception.GetBaseException() }
$missingElapsed = $timer.ElapsedMilliseconds
if ($missingError -isnot [IO.FileNotFoundException] -or $missingElapsed -ge 900) { throw '不存在文件被吞掉或错误地按共享锁重试。' }
$checks.Add([pscustomobject]@{ name = 'missing_file_fails_immediately'; passed = $true; elapsed_ms = $missingElapsed })

function New-ValidationReports {
    <# .SYNOPSIS 创建处于所有原始门槛边界的独立数据，不模拟 UIA 检查执行。 #>
    return [pscustomobject]@{
        session = [pscustomobject]@{ passed = $true; finished = $true; checks = @([pscustomobject]@{ passed = $true }); pressure_runs = @([pscustomobject]@{ name = 'round-01'; saw_running = $true; requested_lines = 100000; started_ms = 500; ended_ms = 15500 }) }
        application = [pscustomobject]@{ passed = $true; checks = @([pscustomobject]@{ passed = $true }); metrics = [pscustomobject]@{ max_tick_ms = 100; pending_units = 0; received_lines = 100005; dropped_lines = 0; view_caught_up = $true; rendered_total_lines = 100005; rendered_text_matches_model_on_close = $true } }
    }
}
function Assert-ValidationResult {
    <# .SYNOPSIS 使用 AST 加载的真实报告验证函数检查允许或拒绝。 #>
    param([string]$Name, $Reports, [bool]$Expected, [int]$Lines = 100000)
    $result = Test-EnduranceReports $Reports.session $Reports.application 1 $Lines
    if ($result.passed -ne $Expected) { throw ('报告门槛判定错误：' + $Name) }
    $checks.Add([pscustomobject]@{ name = $Name; passed = $true; rejected_checks = @($result.checks | Where-Object { -not $_.passed } | ForEach-Object name) })
}
Assert-ValidationResult 'correct_boundary_report_passes' (New-ValidationReports) $true
foreach ($field in @('view_caught_up', 'rendered_text_matches_model_on_close')) {
    foreach ($case in @('false', 'null', 'string_true')) {
        $reports = New-ValidationReports
        $reports.application.metrics.$field = switch ($case) { 'false' { $false }; 'null' { $null }; 'string_true' { 'true' } }
        Assert-ValidationResult ($field + ':' + $case + ':rejected_with_pending_zero') $reports $false
    }
}
foreach ($field in @('view_caught_up', 'rendered_total_lines', 'rendered_text_matches_model_on_close')) {
    $reports = New-ValidationReports
    $reports.application.metrics.PSObject.Properties.Remove($field)
    Assert-ValidationResult ('old_report_missing:' + $field) $reports $false
}
foreach ($case in @('null', 'wrong_total')) {
    $reports = New-ValidationReports
    $reports.application.metrics.rendered_total_lines = if ($case -eq 'null') { $null } else { 100004 }
    Assert-ValidationResult ('rendered_total:' + $case) $reports $false
}
foreach ($field in @('max_tick_ms', 'pending_units', 'received_lines', 'dropped_lines')) {
    $reports = New-ValidationReports
    $reports.application.metrics.$field++
    Assert-ValidationResult ('over_limit:' + $field) $reports $false
}
$reports = New-ValidationReports
$reports.application.metrics.received_lines = 200005
$reports.application.metrics.rendered_total_lines = 200005
Assert-ValidationResult 'extra_round_count_rejected_even_if_rendered_equal' $reports $false
$reports = New-ValidationReports
$reports.session.pressure_runs[0].ended_ms = 15501
Assert-ValidationResult 'duration_15001ms_rejected' $reports $false
$reports = New-ValidationReports
$reports.session.pressure_runs[0].saw_running = $false
Assert-ValidationResult 'missing_actual_running_rejected' $reports $false
$reports = New-ValidationReports
$reports.session.finished = $false
Assert-ValidationResult 'unfinished_session_rejected' $reports $false
$reports = New-ValidationReports
$reports.session.pressure_runs[0].requested_lines = 1000000
$reports.session.pressure_runs[0].ended_ms = 150500
$reports.application.metrics.received_lines = 1000005
$reports.application.metrics.rendered_total_lines = 1000005
Assert-ValidationResult 'million_lines_proportional_boundary_passes' $reports $true 1000000
$result = [pscustomobject]@{ passed = $true; gui_invoked = $false; checks = @($checks); checked_script = (Resolve-Path -LiteralPath $EnduranceScript).Path; evidence_directory = $casePath }
[IO.File]::WriteAllText((Join-Path $casePath 'validation-summary.json'), ($result | ConvertTo-Json -Depth 8), $utf8)
$result | ConvertTo-Json -Depth 8
