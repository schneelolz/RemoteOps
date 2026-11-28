#requires -Version 7.2
<# .SYNOPSIS 不启动界面的短命快照客户端参数、同源实现和失败报告检查。 #>
[CmdletBinding()]
param([string]$SessionScript = (Join-Path $PSScriptRoot 'Test-NativeGuiSession.ps1'))
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$workerScript = Join-Path $PSScriptRoot 'Test-NativeGuiSessionSnapshot.ps1'
$initializationScript = Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1'
$checks = [Collections.Generic.List[object]]::new()
foreach ($path in @($SessionScript, $workerScript)) {
    $errors = $null; $tokens = $null
    $parsed = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path -LiteralPath $path).Path, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw ($errors | Out-String) }
    $checks.Add([pscustomobject]@{ name = ('parse:' + [IO.Path]::GetFileName($path)); passed = $true })
    if ($path -eq $SessionScript) { $sessionAst = $parsed }
}
foreach ($name in @('Get-WindowSnapshot', 'Get-Root', 'Find-Control', 'Add-Check')) {
    $definition = $sessionAst.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $true)
    if ($null -eq $definition) { throw ('worker 所需同源实现不存在：' + $name) }
}
foreach ($name in @('ConvertFrom-SnapshotWorkerJson', 'Assert-SnapshotWorkerIdentity')) {
    $definition = $sessionAst.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name }, $true)
    if ($null -eq $definition) { throw ('同源 JSON 身份函数不存在：' + $name) }
    . ([scriptblock]::Create($definition.Extent.Text))
}
# 七位小数和 Z 必须原样保留；不能把 DateTime 的本地格式误当成进程启动标识。
$startUtc = '2026-10-30T09:14:58.5507235Z'
$json = '{"snapshot":{"process_id":123,"process_start_utc":"2026-10-30T09:14:58.5507235Z","main_handle":456,"name":"中文快照","utc":"2026-10-30T09:15:00.1234567Z"},"checks":[],"failure":null,"passed":true}'
$decoded = ConvertFrom-SnapshotWorkerJson $json
if ($decoded.snapshot.process_start_utc -isnot [string] -or $decoded.snapshot.process_start_utc -cne $startUtc -or $decoded.snapshot.utc -cne '2026-10-30T09:15:00.1234567Z') { throw 'JSON 的精确 UTC 字符串未保留。' }
Assert-SnapshotWorkerIdentity $decoded.snapshot 123 $startUtc 456 '中文快照'
$checks.Add([pscustomobject]@{ name = 'seven_fraction_utc_identity_round_trip'; passed = $true })
foreach ($field in @('process_id', 'process_start_utc', 'main_handle', 'name')) {
    $changed = ConvertFrom-SnapshotWorkerJson $json
    $changed.snapshot.$field = switch ($field) {
        'process_start_utc' { '2026-10-30T09:14:58.5507236Z' }
        'name' { '其他快照' }
        default { 999 }
    }
    $rejected = $false
    try { Assert-SnapshotWorkerIdentity $changed.snapshot 123 $startUtc 456 '中文快照' } catch { $rejected = $true }
    if (-not $rejected) { throw ('JSON 还原后身份变更未拒绝：' + $field) }
    $checks.Add([pscustomobject]@{ name = ('identity_mismatch_rejected:' + $field); passed = $true })
}
$nullResult = ConvertFrom-SnapshotWorkerJson '{"snapshot":null,"checks":[],"failure":"expected failure","passed":false}'
if ($null -ne $nullResult.snapshot -or $nullResult.failure -cne 'expected failure') { throw '失败 JSON 被转换错误。' }
$checks.Add([pscustomobject]@{ name = 'null_snapshot_failure_preserved'; passed = $true })
$probe = $sessionAst.Find({ param($node) $node -is [Management.Automation.Language.StringConstantExpressionAst] -and $node.Value.Contains('public static class RemoteOpsNativeSessionProbe') }, $true)
Add-Type -TypeDefinition $probe.Value
$checks.Add([pscustomobject]@{ name = 'shared_functions_present_and_actual_probe_compiles'; passed = $true })
$directory = Join-Path $PSScriptRoot ('../artifacts/native-ui-prototype/snapshot-worker-tests/中文 空格 ' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $directory | Out-Null
$current = [Diagnostics.Process]::GetCurrentProcess()
foreach ($case in @('missing_process', 'mismatched_start')) {
    $output = Join-Path $directory ($case + '.json')
    $testTarget = if ($case -eq 'missing_process') { [int]::MaxValue } else { $current.Id }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = Join-Path $PSHOME 'pwsh.exe'
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in @('-NoProfile', '-STA', '-File', $workerScript,
        '-SessionScript', (Resolve-Path -LiteralPath $SessionScript).Path,
        '-InitializationScript', (Resolve-Path -LiteralPath $initializationScript).Path,
        '-ProcessId', [string]$testTarget, '-ProcessStart', 'deliberately-wrong-start', '-MainHandle', '0',
        '-SnapshotName', '中文 空格 "引号"', '-OutputFile', $output)) { $start.ArgumentList.Add($argument) }
    $child = [Diagnostics.Process]::Start($start)
    $stdout = $child.StandardOutput.ReadToEndAsync(); $stderr = $child.StandardError.ReadToEndAsync()
    try {
        if (-not $child.WaitForExit(15000)) { $child.Kill($true); $child.WaitForExit(); throw '非 GUI worker 测试超时。' }
        $result = Get-Content -LiteralPath $output -Raw | ConvertFrom-Json
        $stdoutResult = $stdout.GetAwaiter().GetResult() | ConvertFrom-Json
        if ($child.ExitCode -ne 1 -or $result.passed -or [string]::IsNullOrWhiteSpace($result.failure) -or $null -ne $result.snapshot -or $stdoutResult.worker_process_id -ne $child.Id) { throw ('worker 失败结果不正确：' + $case) }
        if (@($result.checks).Count -ne 0) { throw '身份校验失败前不应开始 UIA 快照。' }
        $checks.Add([pscustomobject]@{ name = $case; passed = $true; worker_exited = $child.HasExited; exit_code = $child.ExitCode })
    } finally { $child.Dispose() }
}
$current.Refresh()
if ($current.HasExited) { throw '只读客户端错误地结束了目标进程。' }
$checks.Add([pscustomobject]@{ name = 'target_process_preserved'; passed = $true })
$current.Dispose()
[pscustomobject]@{ passed = $true; checks = @($checks); evidence_directory = $directory } | ConvertTo-Json -Depth 5
