#requires -Version 7.2
<#
.SYNOPSIS
复现会话验收报告被短暂读锁占用时的原子替换重试，不启动任何界面。
#>
[CmdletBinding()]
param(
    [string]$SessionScript = (Join-Path $PSScriptRoot 'Test-NativeGuiSession.ps1'),
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../artifacts/native-ui-prototype/json-writer-tests')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'Windows 文件共享锁回归测试只支持 Windows。' }
$parseErrors = $null
$tokens = $null
$ast = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path -LiteralPath $SessionScript).Path, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -gt 0) { throw '会话验收脚本存在语法错误。' }
$function = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Write-JsonFile' }, $true)
if ($null -eq $function) { throw '未找到实际 Write-JsonFile 函数。' }
# 只加载目标函数，不能执行会启动小样的脚本入口。
. ([scriptblock]::Create($function.Extent.Text))
$utf8 = [Text.UTF8Encoding]::new($false)
$caseDirectory = Join-Path $OutputDirectory ([Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $caseDirectory | Out-Null
$reportFile = Join-Path (Resolve-Path -LiteralPath $caseDirectory).Path 'live.json'

Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Threading;
using System.Threading.Tasks;

/// <summary>在独立线程中占用本次测试文件的读共享锁。</summary>
public static class RemoteOpsJsonReadLock
{
    /// <summary>确认读锁已取得后返回，指定时限后自动释放句柄。</summary>
    public static Task Hold(string path, int milliseconds)
    {
        ManualResetEventSlim ready = new ManualResetEventSlim(false);
        Task task = Task.Run(() => {
            using (FileStream stream = File.Open(path, FileMode.Open, FileAccess.Read, FileShare.Read)) {
                ready.Set();
                Thread.Sleep(milliseconds);
            }
        });
        if (!ready.Wait(2000)) throw new TimeoutException("read_lock_not_acquired");
        ready.Dispose();
        return task;
    }
}
'@

Write-JsonFile $reportFile ([pscustomobject]@{ generation = 1 })
if ((Get-Content -LiteralPath $reportFile -Raw | ConvertFrom-Json).generation -ne 1) { throw '初次写入未成功。' }

$shortLock = [RemoteOpsJsonReadLock]::Hold($reportFile, 200)
$shortTimer = [Diagnostics.Stopwatch]::StartNew()
try { Write-JsonFile $reportFile ([pscustomobject]@{ generation = 2 }) }
finally { $shortLock.GetAwaiter().GetResult() }
$shortTimer.Stop()
if ($shortTimer.ElapsedMilliseconds -lt 150 -or $shortTimer.ElapsedMilliseconds -ge 1400) { throw '短读锁测试没有覆盖有界等待。' }
if ((Get-Content -LiteralPath $reportFile -Raw | ConvertFrom-Json).generation -ne 2) { throw '读锁释放后未完成原子替换。' }
if (Test-Path -LiteralPath ($reportFile + '.writing')) { throw '成功替换后仍残留临时文件。' }

$longLock = [RemoteOpsJsonReadLock]::Hold($reportFile, 1700)
$longTimer = [Diagnostics.Stopwatch]::StartNew()
$originalError = $null
$failureElapsed = $null
try { Write-JsonFile $reportFile ([pscustomobject]@{ generation = 3 }) }
catch { $originalError = $_.Exception.GetBaseException(); $failureElapsed = $longTimer.ElapsedMilliseconds }
finally { $longLock.GetAwaiter().GetResult() }
$longTimer.Stop()
if ($null -eq $originalError -or ($originalError -isnot [IO.IOException] -and $originalError -isnot [UnauthorizedAccessException])) {
    throw '持续锁未保留底层文件异常。'
}
if ($failureElapsed -lt 900 -or $failureElapsed -ge 1400) { throw '持续锁重试没有在约一秒内结束。' }
if ((Get-Content -LiteralPath $reportFile -Raw | ConvertFrom-Json).generation -ne 2) { throw '失败替换损坏了旧记录。' }
if (-not (Test-Path -LiteralPath ($reportFile + '.writing'))) { throw '失败时未保留待替换的临时记录。' }

Write-JsonFile $reportFile ([pscustomobject]@{ generation = 4 })
if ((Get-Content -LiteralPath $reportFile -Raw | ConvertFrom-Json).generation -ne 4) { throw '解除锁后无法再次写入。' }
$result = [pscustomobject]@{
    passed = $true; short_lock_elapsed_ms = $shortTimer.ElapsedMilliseconds
    persistent_lock_error = $originalError.GetType().FullName
    persistent_lock_failure_ms = $failureElapsed
    old_file_preserved = $true; recovery_succeeded = $true; test_file = $reportFile
}
$result | ConvertTo-Json
