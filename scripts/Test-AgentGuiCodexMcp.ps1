[CmdletBinding()]
param(
    [string]$PairingCode = (Get-Clipboard -Raw),
    [string]$Alias = 'GUI-Agent-live-test',
    [int]$TimeoutSeconds = 300,
    [string]$Model = 'gpt-5.6-luna'
)

$ErrorActionPreference = 'Stop'

$pairingCodeValue = $PairingCode.Trim()
if ($pairingCodeValue -notmatch '^\d{3}-\d{3}-\d{3}$') {
    throw '没有取得有效的 RemoteOps 九位临时控制码'
}
if ($Alias -notmatch '^[A-Za-z0-9._-]{1,64}$') {
    throw '验收别名只能包含字母、数字、点、下划线和短横线'
}

$workspaceRoot = Split-Path -Parent $PSScriptRoot
$codexCommand = Get-Command codex -CommandType Application -ErrorAction Stop |
    Select-Object -First 1
$controllerToken = [Environment]::GetEnvironmentVariable(
    'REMOTEOPS_CONTROLLER_TOKEN',
    'User'
)
if ([string]::IsNullOrWhiteSpace($controllerToken)) {
    throw 'Windows 用户环境变量 REMOTEOPS_CONTROLLER_TOKEN 尚未设置'
}
$controllerOwnerId = [Environment]::GetEnvironmentVariable(
    'REMOTEOPS_CONTROLLER_OWNER_ID',
    'User'
)
$parsedOwnerId = [guid]::Empty
if (
    [string]::IsNullOrWhiteSpace($controllerOwnerId) -or
    -not [guid]::TryParse($controllerOwnerId, [ref]$parsedOwnerId) -or
    $parsedOwnerId -eq [guid]::Empty
) {
    throw 'Windows 用户环境变量 REMOTEOPS_CONTROLLER_OWNER_ID 尚未设置或格式无效'
}
$lastMessagePath = Join-Path (
    [System.IO.Path]::GetTempPath()
) ('remoteops-codex-result-' + [guid]::NewGuid().ToString('N') + '.txt')

$prompt = @"
这是 RemoteOps 控制码 $pairingCodeValue，请连接这台现场电脑并进行只读检查：告诉我主机名、PowerShell 版本，以及处于 Up 状态且不是环回地址的 IPv4 地址。连接别名使用 $Alias。不要修改远程电脑，也不要使用远程桌面或本机命令代替 RemoteOps。

最终只输出一行 JSON。成功格式为：{"success":true,"hostname":"...","shell":"...","powershell_version":"...","ipv4":["..."],"error":""}；失败格式为：{"success":false,"hostname":"","shell":"","powershell_version":"","ipv4":[],"error":"不含控制码和内部标识的失败阶段与原因"}。不要输出控制码、Token、session_id、approval_id 或恢复令牌。
"@

$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $codexCommand.Source
$startInfo.WorkingDirectory = $workspaceRoot
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardInput = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
$utf8 = [System.Text.UTF8Encoding]::new($false)
$startInfo.StandardInputEncoding = $utf8
$startInfo.StandardOutputEncoding = $utf8
$startInfo.StandardErrorEncoding = $utf8
$startInfo.Environment['REMOTEOPS_CONTROLLER_TOKEN'] = $controllerToken
$startInfo.Environment['REMOTEOPS_CONTROLLER_OWNER_ID'] = $controllerOwnerId
foreach ($argument in @(
    'exec',
    '-m',
    $Model,
    '-',
    '--ephemeral',
    '--skip-git-repo-check',
    '--color',
    'never',
    '-s',
    'read-only',
    '-c',
    'mcp_servers.remoteops.tools.open_shell.approval_mode="approve"',
    '-c',
    'mcp_servers.remoteops.tools.run_readonly_command.approval_mode="approve"',
    '-C',
    $workspaceRoot,
    '-o',
    $lastMessagePath
)) {
    $startInfo.ArgumentList.Add($argument)
}

$process = [System.Diagnostics.Process]::new()
$process.StartInfo = $startInfo
$processStarted = $false
try {
    if (-not $process.Start()) {
        throw '无法启动 Codex CLI 验收进程'
    }
    $processStarted = $true
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.StandardInput.Write($prompt)
    $process.StandardInput.Close()
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        $process.Kill($true)
        throw "Codex MCP 验收在 $TimeoutSeconds 秒内没有完成"
    }
    $stdoutText = $stdoutTask.GetAwaiter().GetResult()
    $stderrText = $stderrTask.GetAwaiter().GetResult()
    if ($process.ExitCode -ne 0) {
        $diagnostic = ($stderrText + [Environment]::NewLine + $stdoutText)
        $diagnostic = $diagnostic.Replace($controllerToken, '[redacted-token]')
        $diagnostic = [regex]::Replace(
            $diagnostic,
            '\b\d{3}-\d{3}-\d{3}\b|\b\d{9}\b',
            '[redacted-code]'
        )
        $diagnostic = [regex]::Replace(
            $diagnostic,
            '(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b',
            '[redacted-id]'
        )
        $diagnostic = [regex]::Replace(
            $diagnostic,
            '(?i)(token|secret|authorization|api[_-]?key)(\s*[:=]\s*)\S+',
            '$1$2[redacted]'
        )
        if ($diagnostic.Length -gt 4000) {
            $diagnostic = $diagnostic.Substring($diagnostic.Length - 4000)
        }
        throw "Codex MCP 验收失败，退出码 $($process.ExitCode)：`n$diagnostic"
    }
    if (-not (Test-Path -LiteralPath $lastMessagePath)) {
        throw 'Codex CLI 没有生成最终验收结果'
    }
    $result = (Get-Content -Raw -LiteralPath $lastMessagePath).Trim()
    $safeResult = $result.Replace($controllerToken, '[redacted-token]')
    $safeResult = [regex]::Replace(
        $safeResult,
        '\b\d{3}-\d{3}-\d{3}\b',
        '[redacted]'
    )
    $parsed = $safeResult | ConvertFrom-Json -ErrorAction Stop
    if ($parsed.success -ne $true) {
        throw "Codex MCP 返回了失败结果：$safeResult"
    }
    $safeResult
}
finally {
    if ($processStarted -and -not $process.HasExited) {
        $process.Kill($true)
    }
    $process.Dispose()
    if (Test-Path -LiteralPath $lastMessagePath) {
        Remove-Item -LiteralPath $lastMessagePath -Force
    }
    Remove-Variable pairingCodeValue -ErrorAction SilentlyContinue
    Remove-Variable controllerToken -ErrorAction SilentlyContinue
    Remove-Variable prompt -ErrorAction SilentlyContinue
    Remove-Variable stdoutText -ErrorAction SilentlyContinue
    Remove-Variable stderrText -ErrorAction SilentlyContinue
    Remove-Variable diagnostic -ErrorAction SilentlyContinue
    Remove-Variable result -ErrorAction SilentlyContinue
    Remove-Variable safeResult -ErrorAction SilentlyContinue
}
