#requires -Version 7.2
<#
.SYNOPSIS
验证真实 DPI 往返判定与测试窗口放置的进程边界，不启动界面或修改系统缩放。
#>
[CmdletBinding()]
param([string]$SessionScript = (Join-Path $PSScriptRoot 'Test-NativeGuiSession.ps1'))
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$parseErrors = $null
$tokens = $null
$ast = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path -LiteralPath $SessionScript).Path, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -gt 0) { throw ($parseErrors | Out-String) }
$function = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-DpiRoundTrip' }, $true)
if ($null -eq $function) { throw '未找到真实 DPI 往返判定函数。' }
. ([scriptblock]::Create($function.Extent.Text))
$checks = [Collections.Generic.List[object]]::new()
function Assert-RoundTrip {
    <# .SYNOPSIS 从实际脚本加载的函数检查每一类证据序列。 #>
    param([string]$Name, [object[]]$Snapshots, [bool]$Expected)
    $actual = Get-DpiRoundTrip $Snapshots 123 'same-start' 456
    if ($actual.complete -ne $Expected) { throw ('往返判定错误：' + $Name) }
    $checks.Add([pscustomobject]@{ name = $Name; passed = $true; sequence = $actual.observed })
}
function New-Snapshot {
    <# .SYNOPSIS 创建不涉及窗口 API 的序列边界测试数据。 #>
    param([int]$MainDpi, [int]$ObserverDpi = $MainDpi)
    return [pscustomobject]@{
        process_id = 123; process_start_utc = 'same-start'; main_handle = 456
        windows = @(
            [pscustomobject]@{ Handle = 456; Title = 'RemoteOps · Windows 原生界面小样'; Visible = $true; Dpi = $MainDpi },
            [pscustomobject]@{ Handle = 789; Title = 'RemoteOps · 只读操作观察窗'; Visible = $true; Dpi = $ObserverDpi })
    }
}
$full = @(96, 144, 192, 96 | ForEach-Object { New-Snapshot $_ })
Assert-RoundTrip 'complete_ordered_sequence' $full $true
Assert-RoundTrip 'repeated_dpi_is_not_an_extra_stage' @(96, 96, 144, 144, 192, 192, 96, 96 | ForEach-Object { New-Snapshot $_ }) $true
Assert-RoundTrip 'missing_restore' @($full[0..2]) $false
Assert-RoundTrip 'wrong_order' @(96, 192, 144, 96 | ForEach-Object { New-Snapshot $_ }) $false
Assert-RoundTrip 'extra_transition' @(96, 144, 96, 144, 192, 96 | ForEach-Object { New-Snapshot $_ }) $false
Assert-RoundTrip 'empty' @() $false
Assert-RoundTrip 'single_dpi_repeated' @(96, 96, 96, 96 | ForEach-Object { New-Snapshot $_ }) $false
$mixed = @(96, 144, 192, 96 | ForEach-Object { New-Snapshot $_ })
$mixed[2].windows[1].Dpi = 144
Assert-RoundTrip 'observer_did_not_reach_200' $mixed $false
$hidden = @(96, 144, 192, 96 | ForEach-Object { New-Snapshot $_ })
$hidden[3].windows[1].Visible = $false
Assert-RoundTrip 'hidden_restore_is_not_evidence' $hidden $false
foreach ($field in @('process_id', 'process_start_utc', 'main_handle')) {
    $changed = @(96, 144, 192, 96 | ForEach-Object { New-Snapshot $_ })
    $changed[3].$field = if ($field -eq 'process_start_utc') { 'other-start' } else { 999 }
    Assert-RoundTrip ('changed_identity_' + $field) $changed $false
}
$csharp = $ast.Find({ param($node) $node -is [Management.Automation.Language.StringConstantExpressionAst] -and $node.Value.Contains('public static class RemoteOpsNativeSessionProbe') }, $true)
if ($null -eq $csharp) { throw '未找到实际会话探针 C# 源码。' }
Add-Type -TypeDefinition $csharp.Value
$checks.Add([pscustomobject]@{ name = 'native_probe_compiles'; passed = $true })
if ($IsWindows) {
    $rejected = $false
    try { [RemoteOpsNativeSessionProbe]::Place($PID, 0, 0, 0) }
    catch { $rejected = $_.Exception.ToString().Contains('测试窗口所属进程发生变化') }
    if (-not $rejected) { throw 'Place 未拒绝不属于测试进程的无效窗口。' }
    $checks.Add([pscustomobject]@{ name = 'foreign_window_rejected_before_move'; passed = $true })
}
[pscustomobject]@{ passed = $true; checks = @($checks) } | ConvertTo-Json -Depth 5
