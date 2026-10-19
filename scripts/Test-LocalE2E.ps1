[CmdletBinding()]
param(
    [switch]$KeepOnFailure
)

$ErrorActionPreference = 'Stop'

$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    'remoteops-e2e-' + [guid]::NewGuid().ToString('N')
)
$resolvedTestRoot = [System.IO.Path]::GetFullPath($testRoot)
$resolvedTempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())

if (-not $resolvedTestRoot.StartsWith($resolvedTempRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "测试目录不在系统临时目录内：$resolvedTestRoot"
}

$workspaceRoot = Split-Path -Parent $PSScriptRoot
$relayExe = $null
$agentExe = $null
$cliExe = $null
$mcpExe = $null
$mcpSmokeExe = $null
$relayPort = 0
$healthPort = 0

$cert = Join-Path $resolvedTestRoot 'tls\cert.pem'
$key = Join-Path $resolvedTestRoot 'tls\key.pem'
$relayState = Join-Path $resolvedTestRoot 'relay-state.json'
$relayOut = Join-Path $resolvedTestRoot 'relay.out'
$relayErr = Join-Path $resolvedTestRoot 'relay.err'
$relayRestartOut = Join-Path $resolvedTestRoot 'relay-restart.out'
$relayRestartErr = Join-Path $resolvedTestRoot 'relay-restart.err'
$agentOut = Join-Path $resolvedTestRoot 'agent.out'
$agentErr = Join-Path $resolvedTestRoot 'agent.err'
$agentResumeOut = Join-Path $resolvedTestRoot 'agent-resume.out'
$agentResumeErr = Join-Path $resolvedTestRoot 'agent-resume.err'
$agentState = Join-Path $resolvedTestRoot 'agent-state.json'
$auditLog = Join-Path $resolvedTestRoot 'audit.jsonl'
$agentTransferRoot = Join-Path $resolvedTestRoot 'agent-transfer'
$relayProcess = $null
$agentProcess = $null
$completed = $false
$humanControllerToken = (
    [guid]::NewGuid().ToString('N') +
    [guid]::NewGuid().ToString('N')
)
$aiControllerToken = (
    [guid]::NewGuid().ToString('N') +
    [guid]::NewGuid().ToString('N')
)
$controllerOwnerId = [guid]::NewGuid().ToString()
$previousHumanToken = $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN
$previousAiToken = $env:REMOTEOPS_AI_CONTROLLER_TOKEN
$previousControllerToken = $env:REMOTEOPS_CONTROLLER_TOKEN
$previousControllerOwnerId = $env:REMOTEOPS_CONTROLLER_OWNER_ID

function Stop-OwnedProcess {
    param(
        [System.Diagnostics.Process]$Process
    )

    if ($null -eq $Process) {
        return
    }

    try {
        if (-not $Process.HasExited) {
            Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
        }
        if (-not $Process.WaitForExit(5000)) {
            throw "子进程 $($Process.Id) 未在 5 秒内退出"
        }
        $Process.WaitForExit()
    }
    finally {
        $Process.Dispose()
    }
}

function Get-FreeTcpPort {
    $listener = [System.Net.Sockets.TcpListener]::new(
        [System.Net.IPAddress]::Loopback,
        0
    )
    try {
        $listener.Start()
        $listener.LocalEndpoint.Port
    }
    finally {
        $listener.Stop()
    }
}

function Invoke-NativeCapture {
    param(
        [Parameter(Mandatory = $true, Position = 0)]
        [string]$FilePath,

        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]]$Arguments
    )

    $standardErrorPath = [System.IO.Path]::GetTempFileName()
    $previousPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $standardOutput = @(& $FilePath @Arguments 2> $standardErrorPath)
        $nativeExitCode = $LASTEXITCODE
        if ($nativeExitCode -eq 0) {
            $output = $standardOutput
        }
        else {
            $standardError = Get-Content `
                -LiteralPath $standardErrorPath `
                -Raw `
                -ErrorAction SilentlyContinue
            $output = @($standardOutput)
            if (-not [string]::IsNullOrWhiteSpace($standardError)) {
                $output += $standardError
            }
        }
    }
    finally {
        $ErrorActionPreference = $previousPreference
        Remove-Item `
            -LiteralPath $standardErrorPath `
            -Force `
            -ErrorAction SilentlyContinue
    }

    $script:LASTEXITCODE = $nativeExitCode
    @($output | ForEach-Object { $_.ToString() })
}


# 同一次交互进程保留同一个Controller绑定；独立CLI退出会正确撤销其持久资源。
function Read-InteractivePrompt {
    param(
        [System.Diagnostics.Process]$Process,
        [System.Text.StringBuilder]$Transcript
    )
    $received = [System.Text.StringBuilder]::new()
    $buffer = [char[]]::new(1024)
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ($true) {
        $remaining = [int][Math]::Ceiling(($deadline - [DateTime]::UtcNow).TotalMilliseconds)
        if ($remaining -le 0) { throw '等待持久CLI提示符超时，请检查隔离诊断文件' }
        $read = $Process.StandardOutput.ReadAsync($buffer, 0, $buffer.Length)
        if (-not $read.Wait($remaining)) { throw '读取持久CLI响应超时，请检查隔离诊断文件' }
        $count = $read.GetAwaiter().GetResult()
        if ($count -eq 0) { throw '持久CLI提前退出，请检查隔离诊断文件' }
        [void]$received.Append($buffer, 0, $count)
        [void]$Transcript.Append($buffer, 0, $count)
        $text = $received.ToString()
        if ($text.EndsWith('remoteops> ', [StringComparison]::Ordinal)) {
            return $text.Substring(0, $text.Length - 'remoteops> '.Length)
        }
    }
}

function Test-PersistentCliShell {
    param([string]$Executable, [string[]]$CliArguments, [string]$DiagnosticRoot)
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Executable
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.StandardInputEncoding = [System.Text.UTF8Encoding]::new($false)
    $start.StandardOutputEncoding = [System.Text.UTF8Encoding]::new($false)
    $start.StandardErrorEncoding = [System.Text.UTF8Encoding]::new($false)
    foreach ($argument in $CliArguments) { $start.ArgumentList.Add($argument) }
    $start.ArgumentList.Add('interactive')
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $start
    $transcript = [System.Text.StringBuilder]::new()
    $errorRead = $null
    try {
        if (-not $process.Start()) { throw '无法启动隔离的交互CLI' }
        # 异步排空事件日志，避免stderr管道阻塞命令完成。
        $errorRead = $process.StandardError.ReadToEndAsync()
        $null = Read-InteractivePrompt -Process $process -Transcript $transcript
        $process.StandardInput.WriteLine('shell-open 本机测试 cmd')
        $process.StandardInput.Flush()
        $opened = Read-InteractivePrompt -Process $process -Transcript $transcript
        $match = [regex]::Match($opened, '"shell_id"\s*:\s*"([0-9a-f-]{36})"')
        if (-not $match.Success) { throw '交互CLI未返回持久Shell标识' }
        $persistentShellId = $match.Groups[1].Value
        $process.StandardInput.WriteLine("shell-run 本机测试 $persistentShellId cmd set REMOTEOPS_PERSIST_E2E=alpha")
        $process.StandardInput.Flush()
        $null = Read-InteractivePrompt -Process $process -Transcript $transcript
        $process.StandardInput.WriteLine("shell-run 本机测试 $persistentShellId cmd echo %REMOTEOPS_PERSIST_E2E%")
        $process.StandardInput.Flush()
        $readBack = Read-InteractivePrompt -Process $process -Transcript $transcript
        if ($readBack -notmatch 'alpha') { throw '同一CLI绑定内持久Shell未保留变量状态' }
        $process.StandardInput.WriteLine("shell-close 本机测试 $persistentShellId")
        $process.StandardInput.Flush()
        $null = Read-InteractivePrompt -Process $process -Transcript $transcript
        $process.StandardInput.WriteLine('quit')
        $process.StandardInput.Flush()
        $process.StandardInput.Close()
        if (-not $process.WaitForExit(10000)) { throw '交互CLI未在10秒内退出' }
        if ($process.ExitCode -ne 0) { throw '交互CLI持久Shell验收失败，请检查隔离诊断文件' }
    }
    finally {
        # 只结束本函数创建的进程；异常、超时也不遗留Controller或子Shell。
        Stop-OwnedProcess -Process $process
        [IO.File]::WriteAllText((Join-Path $DiagnosticRoot 'persistent-cli.out'), $transcript.ToString(), [Text.UTF8Encoding]::new($false))
        if ($null -ne $errorRead) {
            $stderr = $errorRead.GetAwaiter().GetResult()
            [IO.File]::WriteAllText((Join-Path $DiagnosticRoot 'persistent-cli.err'), $stderr, [Text.UTF8Encoding]::new($false))
        }
    }
}

try {
    New-Item -ItemType Directory -Path $resolvedTestRoot -Force | Out-Null
    Push-Location $workspaceRoot
    try {
        cargo build --workspace --locked
        if ($LASTEXITCODE -ne 0) {
            throw '本机 E2E 前置构建失败'
        }
    }
    finally {
        Pop-Location
    }

    $relayExe = (
        Resolve-Path (Join-Path $workspaceRoot 'target\debug\remoteops-relay.exe')
    ).Path
    $agentExe = (
        Resolve-Path (Join-Path $workspaceRoot 'target\debug\remoteops-agent.exe')
    ).Path
    $cliExe = (
        Resolve-Path (Join-Path $workspaceRoot 'target\debug\remoteops-controller-cli.exe')
    ).Path
    $mcpExe = (
        Resolve-Path (Join-Path $workspaceRoot 'target\debug\remoteops-controller-mcp.exe')
    ).Path
    $mcpSmokeExe = (
        Resolve-Path (Join-Path $workspaceRoot 'target\debug\remoteops-mcp-smoke.exe')
    ).Path
    $relayPort = Get-FreeTcpPort
    do {
        $healthPort = Get-FreeTcpPort
    } while ($healthPort -eq $relayPort)
    $relayEndpoint = "127.0.0.1:$relayPort"

    $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = $humanControllerToken
    $env:REMOTEOPS_AI_CONTROLLER_TOKEN = $aiControllerToken
    $env:REMOTEOPS_CONTROLLER_TOKEN = $aiControllerToken
    $env:REMOTEOPS_CONTROLLER_OWNER_ID = $controllerOwnerId
    New-Item -ItemType Directory -Path $agentTransferRoot -Force | Out-Null

    $relayArguments = @(
        '--bind', $relayEndpoint,
        '--health-bind', "127.0.0.1:$healthPort",
        '--tls-cert', $cert,
        '--tls-key', $key,
        '--state-file', $relayState,
        '--tls-sans', '127.0.0.1,localhost',
        '--lease-seconds', '4',
        '--heartbeat-seconds', '1',
        '--controller-owner-id', $controllerOwnerId
    )
    $agentArguments = @(
        '--relay', $relayEndpoint,
        '--server-name', '127.0.0.1',
        '--ca-cert', $cert,
        '--state-file', $agentState,
        '--transfer-root', $agentTransferRoot,
        '--retry-seconds', '1'
    )
    $relayProcess = Start-Process `
        -FilePath $relayExe `
        -ArgumentList $relayArguments `
        -RedirectStandardOutput $relayOut `
        -RedirectStandardError $relayErr `
        -WindowStyle Hidden `
        -PassThru

    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while (-not (Test-Path -LiteralPath $cert) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 200
    }
    if (-not (Test-Path -LiteralPath $cert)) {
        throw 'Relay 未生成 TLS 证书'
    }

    $agentProcess = Start-Process `
        -FilePath $agentExe `
        -ArgumentList $agentArguments `
        -RedirectStandardOutput $agentOut `
        -RedirectStandardError $agentErr `
        -WindowStyle Hidden `
        -PassThru

    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    $pairingCode = $null
    while (-not $pairingCode -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 250
        if (Test-Path -LiteralPath $agentOut) {
            $agentText = Get-Content -LiteralPath $agentOut -Raw -ErrorAction SilentlyContinue
            if ($null -eq $agentText) {
                $agentText = ''
            }
            $pairingMatch = [regex]::Match(
                $agentText,
                '([0-9]{3}-[0-9]{3}-[0-9]{3})'
            )
            if ($pairingMatch.Success) {
                $pairingCode = $pairingMatch.Groups[1].Value
            }
        }
    }
    if (-not $pairingCode) {
        $agentErrors = if (Test-Path -LiteralPath $agentErr) {
            Get-Content -LiteralPath $agentErr -Raw
        }
        else {
            ''
        }
        throw "Agent 未取得配对码。$agentErrors"
    }

    $pairSpec = "$pairingCode=本机测试"
    $baseArguments = @(
        '--relay', $relayEndpoint,
        '--server-name', '127.0.0.1',
        '--ca-cert', $cert,
        '--audit-log', $auditLog,
        '--pair', $pairSpec
    )

    $listOutput = Invoke-NativeCapture $cliExe @baseArguments list
    if ($LASTEXITCODE -ne 0) {
        throw "连接列表失败：$($listOutput -join "`n")"
    }
    $initialListJson = Invoke-NativeCapture $cliExe @baseArguments --json list
    if ($LASTEXITCODE -ne 0) {
        throw "读取初始连接 JSON 失败：$($initialListJson -join "`n")"
    }
    $initialConnections = @(($initialListJson -join "`n") | ConvertFrom-Json)
    $initialAgentIdentity = (Get-Content -LiteralPath $agentState -Raw | ConvertFrom-Json).agent_instance_id
    if (-not $initialAgentIdentity) { throw '初始 Agent 状态缺少长期身份' }
    $initialSessionId = $initialConnections[0].session_id
    if (-not $initialSessionId) {
        throw '初始连接没有 session_id'
    }

    $cmdOutput = Invoke-NativeCapture $cliExe @baseArguments run `
        '本机测试' `
        '--shell' 'cmd' `
        '--readonly' `
        'echo REMOTEOPS_LOCAL_E2E'
    if (
        $LASTEXITCODE -ne 0 -or
        (($cmdOutput -join "`n") -notmatch 'REMOTEOPS_LOCAL_E2E')
    ) {
        throw "CMD 测试失败：$($cmdOutput -join "`n")"
    }

    $powerShellOutput = Invoke-NativeCapture $cliExe @baseArguments run `
        '本机测试' `
        '--shell' 'windows-power-shell' `
        '--readonly' `
        'Get-Host'
    if (
        $LASTEXITCODE -ne 0 -or
        (($powerShellOutput -join "`n") -notmatch '\b5\.1(?:\.|\b)')
    ) {
        throw "Windows PowerShell 测试失败：$($powerShellOutput -join "`n")"
    }

    Test-PersistentCliShell -Executable $cliExe -CliArguments $baseArguments -DiagnosticRoot $resolvedTestRoot

    $portOutput = Invoke-NativeCapture $cliExe @baseArguments test-port `
        '本机测试' `
        '127.0.0.1' `
        $healthPort
    if (
        $LASTEXITCODE -ne 0 -or
        (($portOutput -join "`n") -notmatch 'open')
    ) {
        throw "端口探测失败：$($portOutput -join "`n")"
    }

    $uploadSource = Join-Path $resolvedTestRoot 'upload-source.txt'
    $remoteFile = 'agent-file.txt'
    $downloadTarget = Join-Path $resolvedTestRoot 'download-target.txt'
    [System.IO.File]::WriteAllText(
        $uploadSource,
        'REMOTEOPS_FILE_E2E',
        [System.Text.UTF8Encoding]::new($false)
    )

    $uploadOutput = Invoke-NativeCapture $cliExe @baseArguments upload `
        '本机测试' `
        $uploadSource `
        $remoteFile
    if ($LASTEXITCODE -ne 0) {
        throw "文件上传失败：$($uploadOutput -join "`n")"
    }

    $downloadOutput = Invoke-NativeCapture $cliExe @baseArguments download `
        '本机测试' `
        $remoteFile `
        $downloadTarget
    if ($LASTEXITCODE -ne 0) {
        throw "文件下载失败：$($downloadOutput -join "`n")"
    }

    $sourceHash = (Get-FileHash -LiteralPath $uploadSource -Algorithm SHA256).Hash
    $downloadHash = (Get-FileHash -LiteralPath $downloadTarget -Algorithm SHA256).Hash
    if ($sourceHash -ne $downloadHash) {
        throw '文件上传下载后的 SHA-256 不一致'
    }

    $mcpRemoteFile = 'mcp-agent-file.txt'
    $mcpDownloadTarget = Join-Path $resolvedTestRoot 'mcp-download-target.txt'
    $mcpOutput = Invoke-NativeCapture $mcpSmokeExe `
        '--mcp-executable' $mcpExe `
        '--human-cli-executable' $cliExe `
        '--relay' $relayEndpoint `
        '--server-name' '127.0.0.1' `
        '--ca-cert' $cert `
        '--audit-log' $auditLog `
        '--transfer-root' $resolvedTestRoot `
        '--controller-token' $aiControllerToken `
        '--human-controller-token' $humanControllerToken `
        '--pair' $pairSpec `
        '--upload-source' $uploadSource `
        '--remote-file' $mcpRemoteFile `
        '--download-target' $mcpDownloadTarget `
        '--probe-host' '127.0.0.1' `
        '--probe-port' $healthPort
    if ($LASTEXITCODE -ne 0) {
        throw "MCP STDIO 测试失败：$($mcpOutput -join "`n")"
    }
    $mcpDownloadHash = (Get-FileHash -LiteralPath $mcpDownloadTarget -Algorithm SHA256).Hash
    if ($sourceHash -ne $mcpDownloadHash) {
        throw 'MCP 上传下载后的 SHA-256 不一致'
    }

    $auditExport = Join-Path $resolvedTestRoot 'audit-export.json'
    $auditOutput = Invoke-NativeCapture $cliExe @baseArguments audit-export `
        '本机测试' `
        $auditExport
    if (
        $LASTEXITCODE -ne 0 -or
        -not (Test-Path -LiteralPath $auditExport)
    ) {
        throw "审计导出失败：$($auditOutput -join "`n")"
    }
    $auditText = Get-Content -LiteralPath $auditExport -Raw
    if (
        $auditText -notmatch 'run_command' -or
        $auditText -notmatch 'remote_response'
    ) {
        throw '审计导出缺少命令或响应记录'
    }

    $cancelOutput = Invoke-NativeCapture $cliExe @baseArguments --json cancel-after `
        '本机测试' `
        '--shell' 'cmd' `
        '--delay-millis' '500' `
        'ping 127.0.0.1 -n 30'
    if (
        $LASTEXITCODE -ne 0 -or
        (($cancelOutput -join "`n") -notmatch '"cancelled"\s*:\s*true')
    ) {
        throw "人工中断 AI 请求失败：$($cancelOutput -join "`n")"
    }

    $persistedState = Get-Content -LiteralPath $relayState -Raw | ConvertFrom-Json
    if (
        @($persistedState.agents).Count -ne 1 -or
        $persistedState.agents[0].session_id -ne $initialSessionId
    ) {
        throw 'Relay 状态文件没有持久化当前逻辑会话'
    }
    $agentTextBeforeRestart = Get-Content -LiteralPath $agentOut -Raw
    if ($null -eq $agentTextBeforeRestart) {
        $agentTextBeforeRestart = ''
    }
    $registrationCountBeforeRestart = [regex]::Matches(
        $agentTextBeforeRestart,
        '([0-9]{3}-[0-9]{3}-[0-9]{3})'
    ).Count

    Stop-OwnedProcess -Process $relayProcess
    $relayProcess = $null
    $relayProcess = Start-Process `
        -FilePath $relayExe `
        -ArgumentList $relayArguments `
        -RedirectStandardOutput $relayRestartOut `
        -RedirectStandardError $relayRestartErr `
        -WindowStyle Hidden `
        -PassThru

    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    $relayHealthyAfterRestart = $false
    do {
        Start-Sleep -Milliseconds 250
        $healthClient = [System.Net.Sockets.TcpClient]::new()
        try {
            $healthClient.Connect('127.0.0.1', $healthPort)
            $relayHealthyAfterRestart = $true
        }
        catch {
            $relayHealthyAfterRestart = $false
        }
        finally {
            $healthClient.Dispose()
        }
    } while (-not $relayHealthyAfterRestart -and [DateTime]::UtcNow -lt $deadline)
    if (-not $relayHealthyAfterRestart) {
        $restartErrors = if (Test-Path -LiteralPath $relayRestartErr) {
            Get-Content -LiteralPath $relayRestartErr -Raw
        }
        else {
            ''
        }
        throw "Relay 重启后未恢复健康：$restartErrors"
    }

    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    $pairingCodePreserved = $false
    do {
        Start-Sleep -Milliseconds 250
        $agentTextAfterRestart = Get-Content `
            -LiteralPath $agentOut `
            -Raw `
            -ErrorAction SilentlyContinue
        if ($null -eq $agentTextAfterRestart) {
            $agentTextAfterRestart = ''
        }
        $registrationsAfterRestart = [regex]::Matches(
            $agentTextAfterRestart,
            '([0-9]{3}-[0-9]{3}-[0-9]{3})'
        )
        if ($registrationsAfterRestart.Count -gt $registrationCountBeforeRestart) {
            $latestPairingCode = $registrationsAfterRestart[
                $registrationsAfterRestart.Count - 1
            ].Groups[1].Value
            $pairingCodePreserved = $latestPairingCode -eq $pairingCode
        }
    } while (-not $pairingCodePreserved -and [DateTime]::UtcNow -lt $deadline)
    if (-not $pairingCodePreserved) {
        throw 'Relay 重启后 Agent 没有使用原控制码恢复'
    }

    $restoredListJson = Invoke-NativeCapture $cliExe @baseArguments --json list
    if ($LASTEXITCODE -ne 0) {
        throw "Relay 重启后旧控制码无法重新绑定：$($restoredListJson -join "`n")"
    }
    $restoredConnections = @(($restoredListJson -join "`n") | ConvertFrom-Json)
    if ($restoredConnections[0].session_id -ne $initialSessionId) {
        throw 'Relay 重启后 session_id 发生变化'
    }

    Stop-OwnedProcess -Process $agentProcess
    $agentProcess = $null
    Start-Sleep -Seconds 6
    $expiredCodeOutput = Invoke-NativeCapture $cliExe @baseArguments list
    $expiredCodeText = $expiredCodeOutput -join "`n"
    $expiredCodeRejected = (
        $LASTEXITCODE -ne 0 -and
        $expiredCodeText -match
            '(pairing_failed|控制码不存在或已经过期|控制码租约已经过期)'
    )
    if (-not $expiredCodeRejected) {
        throw "旧控制码未以租约错误被拒绝：$expiredCodeText"
    }

    $agentProcess = Start-Process `
        -FilePath $agentExe `
        -ArgumentList $agentArguments `
        -RedirectStandardOutput $agentResumeOut `
        -RedirectStandardError $agentResumeErr `
        -WindowStyle Hidden `
        -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    $newBootCodeRotated = $false
    $newBootPairingCode = $null
    do {
        Start-Sleep -Milliseconds 250
        $agentResumeText = Get-Content `
            -LiteralPath $agentResumeOut `
            -Raw `
            -ErrorAction SilentlyContinue
        if ($null -eq $agentResumeText) {
            $agentResumeText = ''
        }
        $resumedPairingMatch = [regex]::Match(
            $agentResumeText,
            '([0-9]{3}-[0-9]{3}-[0-9]{3})'
        )
        if ($resumedPairingMatch.Success) {
            $newBootPairingCode = $resumedPairingMatch.Groups[1].Value
            $newBootCodeRotated = $newBootPairingCode -ne $pairingCode
        }
    } while (-not $newBootCodeRotated -and [DateTime]::UtcNow -lt $deadline)
    if (-not $newBootCodeRotated) {
        throw '新 Agent 进程没有生成新控制码，请检查隔离诊断文件'
    }

    $oldCodeAfterBootOutput = Invoke-NativeCapture $cliExe @baseArguments --json list
    $newBootOldCodeRejected = (
        $LASTEXITCODE -ne 0 -and
        (($oldCodeAfterBootOutput -join "`n") -match
            '(pairing_failed|控制码不存在或已经过期|控制码租约已经过期)')
    )
    if (-not $newBootOldCodeRejected) { throw '新 Agent 进程启动后旧控制码未被拒绝' }

    $newBootArguments = @(
        '--relay', $relayEndpoint,
        '--server-name', '127.0.0.1',
        '--ca-cert', $cert,
        '--audit-log', $auditLog,
        '--pair', "$newBootPairingCode=本机测试"
    )
    $newBootListJson = Invoke-NativeCapture $cliExe @newBootArguments --json list
    if ($LASTEXITCODE -ne 0) { throw '新 Agent 进程的新控制码无法配对' }
    $newBootConnections = @(($newBootListJson -join "`n") | ConvertFrom-Json)
    if (-not $newBootConnections[0].session_id -or
        $newBootConnections[0].session_id -eq $initialSessionId) {
        throw '新 Agent 进程没有创建新会话'
    }
    $newBootAgentIdentity = (Get-Content -LiteralPath $agentState -Raw | ConvertFrom-Json).agent_instance_id
    if ($newBootAgentIdentity -ne $initialAgentIdentity) {
        throw 'Agent 进程重启改变了长期设备身份'
    }

    $healthClient = [System.Net.Sockets.TcpClient]::new()
    try {
        $healthClient.Connect('127.0.0.1', $healthPort)
    }
    finally {
        $healthClient.Dispose()
    }
    $LASTEXITCODE = 0
    $global:LASTEXITCODE = 0

    $completed = $true
    [PSCustomObject]@{
        Pairing = '通过'
        ConnectionList = $listOutput -join "`n"
        Cmd = $cmdOutput -join "`n"
        WindowsPowerShell = $powerShellOutput -join "`n"
        PersistentShellOpen = '通过'
        PersistentShellState = '通过'
        PersistentShellClose = '通过'
        PortProbe = $portOutput -join "`n"
        Upload = $uploadOutput -join "`n"
        Download = $downloadOutput -join "`n"
        FileHashMatch = $true
        Mcp = $mcpOutput -join "`n"
        McpFileHashMatch = $true
        AuditExport = $auditOutput -join "`n"
        AuditContainsCommandAndResponse = $true
        HumanCancelledAi = $cancelOutput -join "`n"
        RelayRestartPreservedSession = $true
        RelayRestartPreservedPairingCode = $true
        RelayHealthyAfterRestart = $relayHealthyAfterRestart
        ExpiredPairingCodeRejected = $expiredCodeRejected
        NewBootCodeRotated = $newBootCodeRotated
        NewBootOldCodeRejected = $newBootOldCodeRejected
        NewBootCreatedNewSession = $true
        NewBootPreservedDeviceIdentity = $true
        RelayHealthyAfterLeaseExpiry = $true
    } | ConvertTo-Json -Depth 4
}
finally {
    $cleanupErrors = [System.Collections.Generic.List[string]]::new()
    $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = $previousHumanToken
    $env:REMOTEOPS_AI_CONTROLLER_TOKEN = $previousAiToken
    $env:REMOTEOPS_CONTROLLER_TOKEN = $previousControllerToken
    if ($null -eq $previousControllerOwnerId) {
        Remove-Item Env:REMOTEOPS_CONTROLLER_OWNER_ID -ErrorAction SilentlyContinue
    }
    else {
        $env:REMOTEOPS_CONTROLLER_OWNER_ID = $previousControllerOwnerId
    }

    try {
        Stop-OwnedProcess -Process $agentProcess
    }
    catch {
        $cleanupErrors.Add("Agent 清理失败：$($_.Exception.Message)")
    }
    finally {
        $agentProcess = $null
    }

    try {
        Stop-OwnedProcess -Process $relayProcess
    }
    catch {
        $cleanupErrors.Add("Relay 清理失败：$($_.Exception.Message)")
    }
    finally {
        $relayProcess = $null
    }

    if (
        (Test-Path -LiteralPath $resolvedTestRoot) -and
        $resolvedTestRoot.StartsWith(
            $resolvedTempRoot,
            [System.StringComparison]::OrdinalIgnoreCase
        ) -and
        ($completed -or -not $KeepOnFailure)
    ) {
        for ($attempt = 1; $attempt -le 20; $attempt++) {
            try {
                Remove-Item `
                    -LiteralPath $resolvedTestRoot `
                    -Recurse `
                    -Force `
                    -ErrorAction Stop
                break
            }
            catch {
                if ($attempt -eq 20) {
                    $cleanupErrors.Add(
                        "临时目录清理失败：$($_.Exception.Message)"
                    )
                    break
                }
                [GC]::Collect()
                [GC]::WaitForPendingFinalizers()
                Start-Sleep -Milliseconds 500
            }
        }
    }
    elseif (Test-Path -LiteralPath $resolvedTestRoot) {
        Write-Warning "本机 E2E 失败，已保留诊断目录：$resolvedTestRoot"
    }

    if ($cleanupErrors.Count -gt 0) {
        $cleanupMessage = $cleanupErrors -join '；'
        if ($completed) {
            throw "本机 E2E 主流程通过，但资源清理失败：$cleanupMessage"
        }
        Write-Warning "本机 E2E 清理存在问题：$cleanupMessage"
    }
}

