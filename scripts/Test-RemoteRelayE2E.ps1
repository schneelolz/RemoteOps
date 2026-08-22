[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$SshTarget,
    [Parameter(Mandatory)]
    [string]$SshKey,
    [Parameter(Mandatory)]
    [string]$RelayServerName,
    [string]$RemoteCertificatePath,
    [Parameter(Mandatory)]
    [string]$RemoteEnvironmentPath,
    [switch]$DirectRelay,
    [string]$DirectRelayHost = '',
    [int]$RemoteProxyPort = 7443,
    [int]$LocalTunnelPort = 27443,
    [switch]$KeepOnFailure
)

$ErrorActionPreference = 'Stop'

$workspaceRoot = Split-Path -Parent $PSScriptRoot
$workspaceManifest = Get-Content -LiteralPath (Join-Path $workspaceRoot 'Cargo.toml') -Raw
$versionMatch = [regex]::Match(
    $workspaceManifest,
    '(?ms)^\[workspace\.package\]\s*.*?^version\s*=\s*"(?<version>[^"]+)"'
)
if (-not $versionMatch.Success) {
    throw '无法从 Cargo.toml 读取 Workspace 版本。'
}
$releaseVersion = $versionMatch.Groups['version'].Value
$windowsArtifactRoot = Join-Path $workspaceRoot (
    'artifacts\release\{0}\windows-x64' -f $releaseVersion
)
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
    'remoteops-remote-relay-e2e-' + [guid]::NewGuid().ToString('N')
)
$resolvedTestRoot = [System.IO.Path]::GetFullPath($testRoot)
$resolvedTempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())

if (-not $resolvedTestRoot.StartsWith(
        $resolvedTempRoot,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
    throw "测试目录不在系统临时目录内：$resolvedTestRoot"
}
if ($SshTarget -notmatch '^[A-Za-z0-9._-]+@[A-Za-z0-9.-]+$') {
    throw 'SshTarget 格式无效'
}

$tunnelProcess = $null
$agentProcess = $null
$previousControllerToken = $env:REMOTEOPS_CONTROLLER_TOKEN
$previousHumanToken = $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN
$remoteEnvironment = $null
$completed = $false

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
        $Process.WaitForExit(5000) | Out-Null
    }
    finally {
        $Process.Dispose()
    }
}

function Test-TcpEndpoint {
    param(
        [string]$HostName,
        [int]$Port
    )

    $client = [System.Net.Sockets.TcpClient]::new()
    try {
        $task = $client.ConnectAsync($HostName, $Port)
        return $task.Wait(500) -and $client.Connected
    }
    catch {
        return $false
    }
    finally {
        $client.Dispose()
    }
}

function Restore-EnvironmentValue {
    param(
        [string]$Name,
        [AllowNull()]
        [string]$Value
    )

    if ($null -eq $Value) {
        Remove-Item "Env:$Name" -ErrorAction SilentlyContinue
    }
    else {
        Set-Item "Env:$Name" $Value
    }
}

try {
    New-Item -ItemType Directory -Path $resolvedTestRoot -Force | Out-Null

    $resolvedSshKey = (Resolve-Path -LiteralPath $SshKey).Path
    $certificate = $null
    $remoteEnvironment = Join-Path $resolvedTestRoot 'relay.env'

    if (-not [string]::IsNullOrWhiteSpace($RemoteCertificatePath)) {
        $certificate = Join-Path $resolvedTestRoot 'relay-cert.pem'
        & scp.exe -q -i $resolvedSshKey `
            "${SshTarget}:$RemoteCertificatePath" $certificate
        if ($LASTEXITCODE -ne 0) {
            throw '下载 Relay CA 证书失败'
        }
    }
    & scp.exe -q -i $resolvedSshKey `
        "${SshTarget}:$RemoteEnvironmentPath" $remoteEnvironment
    if ($LASTEXITCODE -ne 0) {
        throw '读取 Relay 测试凭据失败'
    }
    & icacls.exe $remoteEnvironment /inheritance:r /grant:r "$env:USERNAME`:F" |
        Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw '无法收紧临时凭据文件权限'
    }

    $settings = @{}
    foreach ($line in Get-Content -LiteralPath $remoteEnvironment) {
        if ($line -match '^([^#=]+)=(.*)$') {
            $settings[$matches[1]] = $matches[2]
        }
    }
    $env:REMOTEOPS_CONTROLLER_TOKEN = $settings['REMOTEOPS_AI_CONTROLLER_TOKEN']
    $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = (
        $settings['REMOTEOPS_HUMAN_CONTROLLER_TOKEN']
    )
    Remove-Item -LiteralPath $remoteEnvironment -Force

    if (
        [string]::IsNullOrWhiteSpace($env:REMOTEOPS_CONTROLLER_TOKEN) -or
        [string]::IsNullOrWhiteSpace($env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN)
    ) {
        throw 'Relay Token 缺失'
    }

    if ($DirectRelay) {
        $relayHost = if ([string]::IsNullOrWhiteSpace($DirectRelayHost)) {
            $RelayServerName
        }
        else {
            $DirectRelayHost
        }
        $relayEndpoint = "${relayHost}:$RemoteProxyPort"
        $probeHost = $relayHost
        $probePort = $RemoteProxyPort
        $transport = 'Direct TLS -> Docker Relay'
        if (-not (Test-TcpEndpoint -HostName $relayHost -Port $RemoteProxyPort)) {
            throw '直连 Relay 端口不可达'
        }
    }
    else {
        $relayEndpoint = "127.0.0.1:$LocalTunnelPort"
        $probeHost = '127.0.0.1'
        $probePort = $LocalTunnelPort
        $transport = 'SSH tunnel -> Docker Relay'
        $tunnelOutput = Join-Path $resolvedTestRoot 'tunnel.out'
        $tunnelError = Join-Path $resolvedTestRoot 'tunnel.err'
        $tunnelProcess = Start-Process `
            -FilePath 'ssh.exe' `
            -ArgumentList @(
                '-i', $resolvedSshKey,
                '-o', 'BatchMode=yes',
                '-o', 'ExitOnForwardFailure=yes',
                '-N',
                '-L', "127.0.0.1:${LocalTunnelPort}:127.0.0.1:${RemoteProxyPort}",
                $SshTarget
            ) `
            -RedirectStandardOutput $tunnelOutput `
            -RedirectStandardError $tunnelError `
            -WindowStyle Hidden `
            -PassThru

        $deadline = [DateTime]::UtcNow.AddSeconds(20)
        while (
            -not (Test-TcpEndpoint -HostName '127.0.0.1' -Port $LocalTunnelPort) -and
            -not $tunnelProcess.HasExited -and
            [DateTime]::UtcNow -lt $deadline
        ) {
            Start-Sleep -Milliseconds 250
        }
        if (-not (Test-TcpEndpoint -HostName '127.0.0.1' -Port $LocalTunnelPort)) {
            throw 'SSH 测试隧道未建立'
        }
    }

    $artifactRoot = $windowsArtifactRoot
    $agentExe = (Resolve-Path (Join-Path $artifactRoot 'remoteops-agent.exe')).Path
    $mcpExe = (
        Resolve-Path (Join-Path $artifactRoot 'remoteops-controller-mcp.exe')
    ).Path
    $cliExe = (
        Resolve-Path (Join-Path $artifactRoot 'remoteops-controller-cli.exe')
    ).Path
    $smokeExe = (
        Resolve-Path (Join-Path $artifactRoot 'remoteops-mcp-smoke.exe')
    ).Path

    $agentOutput = Join-Path $resolvedTestRoot 'agent.out'
    $agentError = Join-Path $resolvedTestRoot 'agent.err'
    $agentState = Join-Path $resolvedTestRoot 'agent-state.json'
    $agentTransferRoot = Join-Path $resolvedTestRoot 'agent-transfer'
    New-Item -ItemType Directory -Path $agentTransferRoot -Force | Out-Null
    $agentArguments = @(
        '--relay', $relayEndpoint,
        '--server-name', $RelayServerName,
        '--state-file', $agentState,
        '--transfer-root', $agentTransferRoot,
        '--retry-seconds', '1'
    )
    if ($certificate) {
        $agentArguments += @('--ca-cert', $certificate)
    }
    $agentProcess = Start-Process `
        -FilePath $agentExe `
        -ArgumentList $agentArguments `
        -RedirectStandardOutput $agentOutput `
        -RedirectStandardError $agentError `
        -WindowStyle Hidden `
        -PassThru

    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    $pairingCode = $null
    while (
        -not $pairingCode -and
        -not $agentProcess.HasExited -and
        [DateTime]::UtcNow -lt $deadline
    ) {
        Start-Sleep -Milliseconds 250
        if (Test-Path -LiteralPath $agentOutput) {
            $agentText = Get-Content -LiteralPath $agentOutput -Raw -ErrorAction SilentlyContinue
            if ($null -eq $agentText) {
                $agentText = ''
            }
            $pairingMatch = [regex]::Match(
                $agentText,
                '控制码：([0-9]{3}-[0-9]{3}-[0-9]{3})'
            )
            if ($pairingMatch.Success) {
                $pairingCode = $pairingMatch.Groups[1].Value
            }
        }
    }
    if (-not $pairingCode) {
        throw 'Agent 未取得配对码'
    }

    $controllerTransferRoot = Join-Path $resolvedTestRoot 'controller-transfer'
    New-Item -ItemType Directory -Path $controllerTransferRoot -Force | Out-Null
    $uploadSource = Join-Path $controllerTransferRoot 'upload.txt'
    $downloadTarget = Join-Path $controllerTransferRoot 'download.txt'
    $auditLog = Join-Path $resolvedTestRoot 'audit.jsonl'
    [System.IO.File]::WriteAllText(
        $uploadSource,
        'REMOTEOPS_REMOTE_RELAY_E2E',
        [System.Text.UTF8Encoding]::new($false)
    )

    $smokeArguments = @(
        '--mcp-executable', $mcpExe,
        '--human-cli-executable', $cliExe,
        '--relay', $relayEndpoint,
        '--server-name', $RelayServerName,
        '--audit-log', $auditLog,
        '--transfer-root', $controllerTransferRoot,
        '--pair', "$pairingCode=远端Relay测试",
        '--upload-source', $uploadSource,
        '--remote-file', 'mcp-remote-relay.txt',
        '--download-target', $downloadTarget,
        '--probe-host', $probeHost,
        '--probe-port', $probePort
    )
    if ($certificate) {
        $smokeArguments += @('--ca-cert', $certificate)
    }
    $smokeOutput = & $smokeExe @smokeArguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "MCP Smoke 失败：$($smokeOutput -join [Environment]::NewLine)"
    }

    $result = ($smokeOutput -join [Environment]::NewLine) | ConvertFrom-Json
    foreach ($check in @(
            'approval_command_mode_verified',
            'command_approval_exact_binding_verified',
            'command_approval_consumed_once_verified',
            'persistent_power_shell_verified',
            'default_step_by_step_verified',
            'full_access_without_nested_confirmation_verified',
            'same_owner_takeover_verified',
            'takeover_resets_full_access_verified',
            'close_clears_connection_list_verified',
            'same_mcp_self_approval_rejected'
        )) {
        if ($result.$check -ne $true) {
            throw "MCP Smoke 检查未通过：$check"
        }
    }
    if (
        (Get-FileHash -LiteralPath $uploadSource -Algorithm SHA256).Hash -ne
        (Get-FileHash -LiteralPath $downloadTarget -Algorithm SHA256).Hash
    ) {
        throw 'MCP 文件往返哈希不一致'
    }

    [pscustomobject]@{
        Status = 'passed'
        Relay = "Docker remoteops-relay:$releaseVersion"
        Transport = $transport
        PowerShell = 'Persistent PowerShell 7'
        ApprovalExactBinding = $true
        ApprovalSingleUse = $true
        TamperRejected = $true
        FileHashVerified = $true
    }
    $completed = $true
}
finally {
    Restore-EnvironmentValue `
        -Name 'REMOTEOPS_CONTROLLER_TOKEN' `
        -Value $previousControllerToken
    Restore-EnvironmentValue `
        -Name 'REMOTEOPS_HUMAN_CONTROLLER_TOKEN' `
        -Value $previousHumanToken
    if ($remoteEnvironment -and (Test-Path -LiteralPath $remoteEnvironment)) {
        Remove-Item -LiteralPath $remoteEnvironment -Force
    }
    Stop-OwnedProcess -Process $agentProcess
    Stop-OwnedProcess -Process $tunnelProcess
    if (Test-Path -LiteralPath $resolvedTestRoot) {
        if ($KeepOnFailure -and -not $completed) {
            Write-Warning "远程 Relay E2E 诊断目录已保留：$resolvedTestRoot"
        }
        else {
            for ($attempt = 1; $attempt -le 20; $attempt++) {
                try {
                    Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force
                    break
                }
                catch {
                    if ($attempt -eq 20) {
                        throw
                    }
                    Start-Sleep -Milliseconds 250
                }
            }
        }
    }
}
