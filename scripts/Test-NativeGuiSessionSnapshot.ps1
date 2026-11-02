#requires -Version 7.2
<#
.SYNOPSIS
短命只读快照客户端；从会话验收脚本加载同一实现，不启动或结束测试应用。
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$SessionScript,
    [Parameter(Mandatory)] [string]$InitializationScript,
    [Parameter(Mandatory)] [int]$ProcessId,
    [Parameter(Mandatory)] [string]$ProcessStart,
    [Parameter(Mandatory)] [long]$MainHandle,
    [Parameter(Mandatory)] [string]$SnapshotName,
    [Parameter(Mandatory)] [string]$OutputFile,
    [long]$BaseElapsedMilliseconds = 0
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$utf8 = [Text.UTF8Encoding]::new($false)
$checks = [Collections.Generic.List[object]]::new()
$snapshots = [Collections.Generic.List[object]]::new()
$watch = [Diagnostics.Stopwatch]::StartNew()
$snapshotElapsedOffset = $BaseElapsedMilliseconds
$process = $null
$mainStart = $ProcessStart
$mainHwnd = $MainHandle
$snapshot = $null
$failure = $null
try {
    # 先验证目标进程身份，不因 PID 复用访问其他应用。
    $process = [Diagnostics.Process]::GetProcessById($ProcessId)
    if ($process.StartTime.ToUniversalTime().ToString('o') -ne $ProcessStart) { throw '快照目标进程启动时间不符。' }
    $parseErrors = $null
    $tokens = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path -LiteralPath $SessionScript).Path, [ref]$tokens, [ref]$parseErrors)
    if ($parseErrors.Count -gt 0) { throw '会话验收脚本存在语法错误。' }
    foreach ($functionName in @('Add-Check', 'Get-Root', 'Find-Control', 'Get-WindowSnapshot')) {
        $definition = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $functionName }, $true)
        if ($null -eq $definition) { throw ('缺少同源快照函数：' + $functionName) }
        . ([scriptblock]::Create($definition.Extent.Text))
    }
    $probe = $ast.Find({ param($node) $node -is [Management.Automation.Language.StringConstantExpressionAst] -and $node.Value.Contains('public static class RemoteOpsNativeSessionProbe') }, $true)
    if ($null -eq $probe) { throw '缺少同源会话探针。' }
    & (Resolve-Path -LiteralPath $InitializationScript).Path
    Add-Type -TypeDefinition $probe.Value
    $snapshot = Get-WindowSnapshot $SnapshotName
}
catch { $failure = $_.Exception.ToString() }
finally { if ($null -ne $process) { $process.Dispose() } }
$result = [pscustomobject]@{
    source = 'short_lived_snapshot_worker'; worker_process_id = $PID
    passed = $null -eq $failure -and @($checks | Where-Object { -not $_.passed }).Count -eq 0
    failure = $failure; snapshot = $snapshot; checks = @($checks)
    elapsed_ms = $watch.ElapsedMilliseconds
}
$json = $result | ConvertTo-Json -Depth 16
$temporary = $OutputFile + '.writing'
[IO.File]::WriteAllText($temporary, $json, $utf8)
[IO.File]::Move($temporary, $OutputFile, $true)
Write-Output $json
if ($null -ne $failure) { exit 1 }
