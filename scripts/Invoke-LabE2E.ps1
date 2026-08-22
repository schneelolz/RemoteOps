[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$LinuxHost,
    [Parameter(Mandatory)]
    [string]$LinuxUser,
    [Parameter(Mandatory)]
    [string]$LinuxSshKey,
    [Parameter(Mandatory)]
    [string]$ExpectedLinuxHostFingerprint,
    [Parameter(Mandatory)]
    [string[]]$WindowsComputers,
    [Parameter(Mandatory)]
    [string]$WindowsCredentialTarget,
    [switch]$SkipBuild,
    [switch]$KeepLabResources,
    [string]$CleanupRunToken
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$utf8NoBom = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = $utf8NoBom
$OutputEncoding = $utf8NoBom

if ($WindowsComputers.Count -ne 2) {
    throw '第一阶段实验室验收固定需要两台 Windows Agent'
}

$workspaceRoot = Split-Path -Parent $PSScriptRoot
$workspaceManifest = Get-Content -LiteralPath (Join-Path $workspaceRoot 'Cargo.toml') -Raw
$versionMatch = [regex]::Match(
    $workspaceManifest,
    '(?ms)^\[workspace\.package\]\s*.*?^version\s*=\s*"(?<version>[^"]+)"'
)
if (-not $versionMatch.Success) {
    throw '无法从 Cargo.toml 读取 Workspace 版本。'
}
$artifactRoot = Join-Path $workspaceRoot (
    'artifacts\release\{0}\windows-x64' -f $versionMatch.Groups['version'].Value
)
$agentExe = Join-Path $artifactRoot 'remoteops-agent.exe'
$controllerExe = Join-Path $artifactRoot 'remoteops-controller-cli.exe'
$mcpExe = Join-Path $artifactRoot 'remoteops-controller-mcp.exe'
$mcpSmokeExe = Join-Path $artifactRoot 'remoteops-mcp-smoke.exe'
$knownHosts = Join-Path $env:USERPROFILE '.ssh\known_hosts'
$runToken = if ($CleanupRunToken) {
    if ($CleanupRunToken -notmatch '^[0-9]{8}-[0-9]{6}-[0-9a-f]{8}$') {
        throw "清理运行标识格式无效：$CleanupRunToken"
    }
    $CleanupRunToken
}
else {
    '{0}-{1}' -f @(
        (Get-Date -Format 'yyyyMMdd-HHmmss'),
        ([guid]::NewGuid().ToString('N').Substring(0, 8))
    )
}
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) "remoteops-lab-$runToken"
$resolvedTestRoot = [System.IO.Path]::GetFullPath($testRoot)
$resolvedTempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$sshTestKey = Join-Path $resolvedTestRoot 'ssh-test-key'
$sshTestKnownHosts = Join-Path $resolvedTestRoot 'ssh-known-hosts'
$remoteLabRoot = "/home/$LinuxUser/remoteops-lab"
$remoteRunRoot = "$remoteLabRoot/$runToken"
$remoteWorkspace = "$remoteRunRoot/workspace"
$remoteArchive = "$remoteRunRoot/remoteops-source.tar.gz"
$remoteDockerConfig = "$remoteRunRoot/docker-config"
$remoteTemp = "$remoteRunRoot/tmp"
$remoteRelayData = "$remoteRunRoot/relay-data"
$remoteControllerEnv = "$remoteRunRoot/controller.env"
$relayHostPort = 17443
$relayCertificate = Join-Path $resolvedTestRoot 'relay-cert.pem'
$auditLog = Join-Path $resolvedTestRoot 'controller-audit.jsonl'
$firewallRuleName = "RemoteOpsLab-Temporary-Block-$runToken"
$taskName = "RemoteOpsLabAgent-$runToken"
$windowsRoot = "C:\ProgramData\RemoteOpsLab\$runToken"
$relayDockerfile = 'deploy/relay/Dockerfile'
$relayComposeFiles = "-f '$remoteWorkspace/deploy/relay/docker-compose.yml'"
$relayComposeProject = "remoteops-relay-$runToken"
$relayContainerName = "remoteops-relay-$runToken"
$relayVolumeName = "remoteops-relay-data-$runToken"
$labSshComposeProject = "remoteops-lab-ssh-$runToken"
$labSshContainerName = "remoteops-lab-ssh-$runToken"
$humanControllerToken = (
    [guid]::NewGuid().ToString('N') +
    [guid]::NewGuid().ToString('N')
)
$aiControllerToken = (
    [guid]::NewGuid().ToString('N') +
    [guid]::NewGuid().ToString('N')
)
$controllerOwnerId = [guid]::NewGuid().ToString()
if ($humanControllerToken -eq $aiControllerToken) {
    throw '人工与 AI Controller Token 必须不同'
}
$previousHumanControllerToken = $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN
$previousAiControllerToken = $env:REMOTEOPS_CONTROLLER_TOKEN
$previousControllerOwnerId = $env:REMOTEOPS_CONTROLLER_OWNER_ID
$relayComposeCommand = (
    "cd '$remoteWorkspace' && " +
    "REMOTEOPS_RELAY_CONTAINER_NAME='$relayContainerName' " +
    "REMOTEOPS_RELAY_VOLUME_NAME='$relayVolumeName' " +
    "REMOTEOPS_RELAY_DOCKERFILE='$relayDockerfile' " +
    "REMOTEOPS_RELAY_DATA_PATH='$remoteRelayData' " +
    "REMOTEOPS_RELAY_USER='1000:1000' " +
    "DOCKER_CONFIG='$remoteDockerConfig' " +
    "TMPDIR='$remoteTemp' TMP='$remoteTemp' TEMP='$remoteTemp' " +
    "REMOTEOPS_RELAY_PORT='$relayHostPort' REMOTEOPS_HEALTH_PORT=0 " +
    'REMOTEOPS_RESTART_POLICY=no REMOTEOPS_LEASE_SECONDS=30 ' +
    "docker compose --env-file '$remoteControllerEnv' " +
    "-p '$relayComposeProject' " +
    $relayComposeFiles
)
$labSshComposeCommand = (
    "cd '$remoteWorkspace/deploy/lab-ssh' && " +
    "REMOTEOPS_LAB_SSH_CONTAINER_NAME='$labSshContainerName' " +
    "DOCKER_CONFIG='$remoteDockerConfig' " +
    "TMPDIR='$remoteTemp' TMP='$remoteTemp' TEMP='$remoteTemp' " +
    'REMOTEOPS_LAB_SSH_PORT=0 REMOTEOPS_RESTART_POLICY=no ' +
    "docker compose -p '$labSshComposeProject' " +
    '-f docker-compose.yml'
)
$sessions = @()
$firewallInstalled = $false
$remoteRunCreated = $false
$relayComposeAttempted = $false
$labSshComposeAttempted = $false
$relayContainerId = $null
$labSshContainerId = $null
$relayNetworkName = $null
$relayPort = 0
$labSshPort = 0
$credential = $null
$resultPath = Join-Path `
    $workspaceRoot `
    "artifacts\lab-e2e-result-$runToken.json"
$latestResultPath = Join-Path $workspaceRoot 'artifacts\lab-e2e-result.json'
$resultJson = $null

if (-not $resolvedTestRoot.StartsWith(
    $resolvedTempRoot,
    [System.StringComparison]::OrdinalIgnoreCase
)) {
    throw "实验室临时目录不在系统临时目录内：$resolvedTestRoot"
}
if ($LinuxUser -notmatch '^[a-z_][a-z0-9_-]*\$?$') {
    throw "Linux 用户名包含不安全字符：$LinuxUser"
}
if ($KeepLabResources) {
    Write-Warning (
        '已启用 KeepLabResources。实验结束后将保留计划任务、Windows 工作目录、' +
        'Linux Compose 资源、远程目录和本地临时密钥，仅强制恢复临时防火墙规则。'
    )
}
if ($CleanupRunToken -and $KeepLabResources) {
    throw 'CleanupRunToken 与 KeepLabResources 不能同时使用'
}

function Invoke-Linux {
    param(
        [Parameter(Mandatory)]
        [string]$Command,
        [switch]$AllowFailure
    )

    $arguments = @(
        '-o', 'BatchMode=yes',
        '-o', 'StrictHostKeyChecking=yes',
        '-o', 'ConnectTimeout=10',
        '-i', $LinuxSshKey,
        "$LinuxUser@$LinuxHost",
        $Command
    )
    $output = & ssh.exe @arguments 2>&1
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0 -and -not $AllowFailure) {
        throw "Linux 命令执行失败，退出码 $exitCode：$($output -join "`n")"
    }
    [PSCustomObject]@{
        ExitCode = $exitCode
        Output = @($output)
    }
}

function Write-LinuxSecretFile {
    param(
        [Parameter(Mandatory)]
        [string]$RemotePath,
        [Parameter(Mandatory)]
        [string]$Content
    )

    $arguments = @(
        '-o', 'BatchMode=yes',
        '-o', 'StrictHostKeyChecking=yes',
        '-o', 'ConnectTimeout=10',
        '-i', $LinuxSshKey,
        "$LinuxUser@$LinuxHost",
        "umask 077 && cat > '$RemotePath' && chmod 0600 '$RemotePath'"
    )
    $output = $Content | & ssh.exe @arguments 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "写入 Linux 受限环境文件失败：$($output -join "`n")"
    }
}

function Copy-ToLinux {
    param(
        [Parameter(Mandatory)]
        [string]$LocalPath,
        [Parameter(Mandatory)]
        [string]$RemotePath
    )

    $output = & scp.exe `
        -o BatchMode=yes `
        -o StrictHostKeyChecking=yes `
        -i $LinuxSshKey `
        $LocalPath `
        "${LinuxUser}@${LinuxHost}:$RemotePath" 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "复制文件到 Linux 失败：$($output -join "`n")"
    }
}

function Copy-FromLinux {
    param(
        [Parameter(Mandatory)]
        [string]$RemotePath,
        [Parameter(Mandatory)]
        [string]$LocalPath
    )

    $output = & scp.exe `
        -o BatchMode=yes `
        -o StrictHostKeyChecking=yes `
        -i $LinuxSshKey `
        "${LinuxUser}@${LinuxHost}:$RemotePath" `
        $LocalPath 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "从 Linux 复制文件失败：$($output -join "`n")"
    }
}

function Get-LinuxComposeContainerId {
    param(
        [Parameter(Mandatory)]
        [string]$ComposeCommand,
        [Parameter(Mandatory)]
        [string]$ServiceName
    )

    $result = Invoke-Linux -Command "$ComposeCommand ps -aq '$ServiceName'"
    $firstLine = $result.Output |
        Where-Object { -not [string]::IsNullOrWhiteSpace([string]$_) } |
        Select-Object -First 1
    $containerId = if ($null -eq $firstLine) {
        ''
    }
    else {
        ([string]$firstLine).Trim()
    }
    if (-not $containerId) {
        throw "Compose 服务 $ServiceName 没有返回容器 ID"
    }
    $containerId
}

function Get-LinuxComposePublishedPort {
    param(
        [Parameter(Mandatory)]
        [string]$ComposeCommand,
        [Parameter(Mandatory)]
        [string]$ServiceName,
        [Parameter(Mandatory)]
        [int]$ContainerPort
    )

    $result = Invoke-Linux `
        -Command "$ComposeCommand port '$ServiceName' '$ContainerPort'"
    foreach ($line in $result.Output) {
        if ([string]$line -match ':(?<port>[0-9]{1,5})\s*$') {
            $publishedPort = [int]$Matches.port
            if ($publishedPort -ge 1 -and $publishedPort -le 65535) {
                return $publishedPort
            }
        }
    }
    throw "Compose 服务 $ServiceName 没有返回有效的宿主机端口"
}

function Wait-LinuxContainerHealthy {
    param(
        [Parameter(Mandatory)]
        [string]$ContainerReference,
        [int]$TimeoutSeconds = 180
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Seconds 2
        $result = Invoke-Linux `
            -Command "docker inspect --format '{{.State.Status}}|{{if .State.Health}}{{.State.Health.Status}}{{end}}' '$ContainerReference'" `
            -AllowFailure
        if ($result.ExitCode -eq 0) {
            $state = ($result.Output -join "`n").Trim()
            if ($state -eq 'running|healthy') {
                return
            }
            if ($state -match '^(exited|dead)\|') {
                $logs = Invoke-Linux `
                    -Command "docker logs --tail 100 '$ContainerReference'" `
                    -AllowFailure
                throw (
                    "容器 $ContainerReference 已停止，状态 $state：" +
                    ($logs.Output -join "`n")
                )
            }
        }
    } while ([DateTime]::UtcNow -lt $deadline)

    $logs = Invoke-Linux `
        -Command "docker logs --tail 100 '$ContainerReference'" `
        -AllowFailure
    throw "容器 $ContainerReference 未在期限内健康：$($logs.Output -join "`n")"
}

function Get-WindowsLabCredential {
    if (-not ('RemoteOpsCredentialReader' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class RemoteOpsCredentialReader {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct CREDENTIAL {
        public uint Flags;
        public uint Type;
        public string TargetName;
        public string Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public IntPtr CredentialBlob;
        public uint Persist;
        public uint AttributeCount;
        public IntPtr Attributes;
        public string TargetAlias;
        public string UserName;
    }

    [DllImport("advapi32.dll", EntryPoint = "CredReadW", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool CredRead(
        string target,
        uint type,
        int reservedFlag,
        out IntPtr credentialPtr
    );

    [DllImport("advapi32.dll", SetLastError = true)]
    public static extern void CredFree(IntPtr buffer);
}
'@
    }

    $pointer = [IntPtr]::Zero
    if (-not [RemoteOpsCredentialReader]::CredRead(
        $WindowsCredentialTarget,
        1,
        0,
        [ref]$pointer
    )) {
        $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        throw "无法读取 Windows 测试凭据，错误码 $errorCode"
    }

    try {
        $native = [Runtime.InteropServices.Marshal]::PtrToStructure(
            $pointer,
            [type][RemoteOpsCredentialReader+CREDENTIAL]
        )
        $secure = [Security.SecureString]::new()
        for ($index = 0; $index -lt $native.CredentialBlobSize; $index += 2) {
            $character = [char][Runtime.InteropServices.Marshal]::ReadInt16(
                $native.CredentialBlob,
                $index
            )
            $secure.AppendChar($character)
        }
        $secure.MakeReadOnly()
        [pscredential]::new($native.UserName, $secure)
    }
    finally {
        if ($pointer -ne [IntPtr]::Zero) {
            [RemoteOpsCredentialReader]::CredFree($pointer)
        }
    }
}

function Invoke-Controller {
    param(
        [Parameter(Mandatory)]
        [string[]]$Pairs,
        [Parameter(Mandatory)]
        [string[]]$Arguments,
        [switch]$AllowFailure
    )

    $common = @(
        '--relay', "${LinuxHost}:$relayPort",
        '--server-name', $LinuxHost,
        '--ca-cert', $relayCertificate,
        '--audit-log', $auditLog
    )
    foreach ($pair in $Pairs) {
        $common += @('--pair', $pair)
    }
    $maxAttempts = if ($AllowFailure) { 1 } else { 5 }
    for ($attempt = 1; $attempt -le $maxAttempts; $attempt++) {
        $stderrPath = Join-Path `
            $resolvedTestRoot `
            ("controller-stderr-{0}.log" -f [guid]::NewGuid().ToString('N'))
        try {
            $output = & $controllerExe @common @Arguments 2> $stderrPath
            $exitCode = $LASTEXITCODE
            $errorOutput = if (Test-Path -LiteralPath $stderrPath) {
                @(Get-Content -LiteralPath $stderrPath)
            }
            else {
                @()
            }
        }
        finally {
            Remove-Item -LiteralPath $stderrPath -Force -ErrorAction SilentlyContinue
        }
        $combined = @($output) + @($errorOutput)
        $combinedText = $combined -join "`n"
        if (
            $exitCode -ne 0 -and
            -not $AllowFailure -and
            $attempt -lt $maxAttempts -and
            $combinedText -match
                '(正在重新连接 Relay|连接 Relay 失败|已有 Controller|Controller.+占用)'
        ) {
            Start-Sleep -Milliseconds 750
            continue
        }
        if ($exitCode -ne 0 -and -not $AllowFailure) {
            throw (
                "Controller CLI 执行失败，退出码 $exitCode；命令：" +
                ($Arguments -join ' ') +
                "；输出：$combinedText"
            )
        }
        return [PSCustomObject]@{
            ExitCode = $exitCode
            Output = @($output)
            Error = @($errorOutput)
        }
    }
    throw 'Controller CLI 达到重试上限'
}

function Get-AgentRegistration {
    param(
        [Parameter(Mandatory)]
        [System.Management.Automation.Runspaces.PSSession]$Session,
        [string]$PreviousPairingCode,
        [string]$ExpectedPairingCode,
        [int]$AfterRegistrationCount = 0,
        [DateTime]$NotBeforeUtc = [DateTime]::MinValue,
        [int]$TimeoutSeconds = 60
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Milliseconds 500
        $log = Invoke-Command -Session $Session -ScriptBlock {
            param($root)
            $path = Join-Path $root 'agent.out'
            if (Test-Path -LiteralPath $path) {
                $item = Get-Item -LiteralPath $path
                [PSCustomObject]@{
                    Text = Get-Content -LiteralPath $path -Raw -Encoding utf8
                    LastWriteTimeUtc = $item.LastWriteTimeUtc
                }
            }
        } -ArgumentList $windowsRoot
        if ($null -eq $log) {
            continue
        }
        $text = [string]$log.Text
        $freshLog = (
            $NotBeforeUtc -eq [DateTime]::MinValue -or
            [DateTime]$log.LastWriteTimeUtc -ge $NotBeforeUtc
        )
        $codes = [regex]::Matches(
            [string]$text,
            '控制码：([0-9]{3}-[0-9]{3}-[0-9]{3})'
        )
        $guids = [regex]::Matches(
            [string]$text,
            'Agent GUID：([0-9a-fA-F-]{36})'
        )
        if ($codes.Count -gt 0 -and $guids.Count -gt 0) {
            $code = $codes[$codes.Count - 1].Groups[1].Value
            $isNewRegistration = $codes.Count -gt $AfterRegistrationCount
            $pairingCodeMatches = (
                (-not $ExpectedPairingCode -or $code -eq $ExpectedPairingCode) -and
                (-not $PreviousPairingCode -or $code -ne $PreviousPairingCode)
            )
            if ($freshLog -and $isNewRegistration -and $pairingCodeMatches) {
                return [PSCustomObject]@{
                    PairingCode = $code
                    AgentInstanceId = $guids[$guids.Count - 1].Groups[1].Value
                    RegistrationCount = $codes.Count
                    LogLastWriteTimeUtc = [DateTime]$log.LastWriteTimeUtc
                }
            }
        }
    } while ([DateTime]::UtcNow -lt $deadline)

    $diagnostic = Invoke-Command -Session $Session -ScriptBlock {
        param($root)
        $paths = @(
            (Join-Path $root 'agent.out'),
            (Join-Path $root 'agent.err')
        )
        $paths |
            ForEach-Object {
                if (Test-Path -LiteralPath $_) {
                    "[$_]`n" + (Get-Content -LiteralPath $_ -Tail 40 -ErrorAction SilentlyContinue -Raw)
                }
            }
    } -ArgumentList $windowsRoot -ErrorAction SilentlyContinue
    throw (
        "未在期限内读取到 $($Session.ComputerName) 的新 Agent 注册信息；" +
        "最近日志：$($diagnostic -join "`n")"
    )
}

    function Start-WindowsAgent {
    param(
        [Parameter(Mandatory)]
        [System.Management.Automation.Runspaces.PSSession]$Session,
        [Parameter(Mandatory)]
        [int]$RelayPort
    )

    Invoke-Command -Session $Session -ScriptBlock {
        param($root, $relayHost, $relayPort, $task)

        $agentPath = Join-Path $root 'remoteops-agent.exe'
        $certificatePath = Join-Path $root 'relay-cert.pem'
        $wrapperPath = Join-Path $root 'Start-Agent.cmd'
        $stdoutPath = Join-Path $root 'agent.out'
        $stderrPath = Join-Path $root 'agent.err'
        $runAsUser = [Security.Principal.WindowsIdentity]::GetCurrent().Name
        if ($runAsUser -eq 'NT AUTHORITY\SYSTEM') {
            throw '实验室 Agent 计划任务禁止使用 SYSTEM 身份'
        }

        Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue |
            Stop-ScheduledTask -ErrorAction SilentlyContinue
        Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue |
            Unregister-ScheduledTask -Confirm:$false -ErrorAction Stop

        $processDeadline = [DateTime]::UtcNow.AddSeconds(15)
        do {
            $agentProcesses = @(
                Get-CimInstance -ClassName Win32_Process -Filter "Name='remoteops-agent.exe'" |
                    Where-Object {
                        $_.ExecutablePath -and
                        $_.ExecutablePath.Equals(
                            $agentPath,
                            [System.StringComparison]::OrdinalIgnoreCase
                        )
                    }
            )
            if ($agentProcesses.Count -eq 0) {
                break
            }
            Start-Sleep -Milliseconds 250
        } while ([DateTime]::UtcNow -lt $processDeadline)
        if ($agentProcesses.Count -ne 0) {
            throw "旧 Agent 进程未退出：$agentPath"
        }

        foreach ($logPath in @($stdoutPath, $stderrPath)) {
            if (Test-Path -LiteralPath $logPath) {
                Remove-Item -LiteralPath $logPath -Force -ErrorAction Stop
                if (Test-Path -LiteralPath $logPath) {
                    throw "无法删除旧 Agent 日志：$logPath"
                }
            }
        }

        $wrapper = @"
@echo off
"$agentPath" --relay "${relayHost}:$relayPort" --server-name "$relayHost" --ca-cert "$certificatePath" --state-file "$root\agent-state.json" --transfer-root "$root" --retry-seconds 1 1>>"$stdoutPath" 2>>"$stderrPath"
"@
        Set-Content -LiteralPath $wrapperPath -Value $wrapper -Encoding ascii

        $action = New-ScheduledTaskAction `
            -Execute 'cmd.exe' `
            -Argument "/d /s /c `"`"$wrapperPath`"`""
        $principal = New-ScheduledTaskPrincipal `
            -UserId $runAsUser `
            -LogonType S4U `
            -RunLevel Highest
        $settings = New-ScheduledTaskSettingsSet `
            -AllowStartIfOnBatteries `
            -DontStopIfGoingOnBatteries `
            -ExecutionTimeLimit ([TimeSpan]::Zero) `
            -RestartCount 3 `
            -RestartInterval (New-TimeSpan -Minutes 1)
        Register-ScheduledTask `
            -TaskName $task `
            -Action $action `
            -Principal $principal `
            -Settings $settings |
            Out-Null
        $notBeforeUtc = [DateTime]::UtcNow
        Start-ScheduledTask -TaskName $task
        [PSCustomObject]@{
            NotBeforeUtc = $notBeforeUtc
        }
    } -ArgumentList $windowsRoot, $LinuxHost, $RelayPort, $taskName
}

function Stop-WindowsAgent {
    param(
        [Parameter(Mandatory)]
        [System.Management.Automation.Runspaces.PSSession]$Session
    )

    Invoke-Command -Session $Session -ScriptBlock {
        param($task, $root)
        Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue |
            Stop-ScheduledTask -ErrorAction SilentlyContinue
        $agentPath = Join-Path $root 'remoteops-agent.exe'
        Get-CimInstance -ClassName Win32_Process -Filter "Name='remoteops-agent.exe'" |
            Where-Object {
                $_.ExecutablePath -and
                $_.ExecutablePath.Equals(
                    $agentPath,
                    [System.StringComparison]::OrdinalIgnoreCase
                )
            } |
            ForEach-Object {
                Invoke-CimMethod -InputObject $_ -MethodName Terminate |
                    Out-Null
            }

        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        do {
            $remaining = @(
                Get-CimInstance -ClassName Win32_Process -Filter "Name='remoteops-agent.exe'" |
                    Where-Object {
                        $_.ExecutablePath -and
                        $_.ExecutablePath.Equals(
                            $agentPath,
                            [System.StringComparison]::OrdinalIgnoreCase
                        )
                    }
            )
            if ($remaining.Count -eq 0) {
                return
            }
            Start-Sleep -Milliseconds 250
        } while ([DateTime]::UtcNow -lt $deadline)
        throw "实验 Agent 进程未能停止：$agentPath"
    } -ArgumentList $taskName, $windowsRoot
}

function Clear-WindowsLabState {
    param(
        [Parameter(Mandatory)]
        [System.Management.Automation.Runspaces.PSSession]$Session,
        [Parameter(Mandatory)]
        [bool]$RemoveResources
    )

    Invoke-Command -Session $Session -ScriptBlock {
        param($root, $task, $rule, $removeResources)

        Get-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue |
            Remove-NetFirewallRule -ErrorAction SilentlyContinue
        if (Get-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue) {
            throw "临时防火墙规则仍然存在：$rule"
        }

        if (-not $removeResources) {
            return
        }

        Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue |
            Stop-ScheduledTask -ErrorAction SilentlyContinue
        Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue |
            Unregister-ScheduledTask -Confirm:$false -ErrorAction SilentlyContinue

        $agentPath = Join-Path $root 'remoteops-agent.exe'
        Get-CimInstance -ClassName Win32_Process -Filter "Name='remoteops-agent.exe'" |
            Where-Object {
                $_.ExecutablePath -and
                $_.ExecutablePath.Equals(
                    $agentPath,
                    [System.StringComparison]::OrdinalIgnoreCase
                )
            } |
            ForEach-Object {
                Invoke-CimMethod -InputObject $_ -MethodName Terminate |
                    Out-Null
            }

        $resolvedRoot = [System.IO.Path]::GetFullPath($root)
        $allowedParent = (
            [System.IO.Path]::GetFullPath('C:\ProgramData\RemoteOpsLab')
        ).TrimEnd('\') + '\'
        if (-not $resolvedRoot.StartsWith(
            $allowedParent,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
            throw "拒绝清理允许范围外的 Windows 目录：$resolvedRoot"
        }
        if (Test-Path -LiteralPath $resolvedRoot) {
            Remove-Item -LiteralPath $resolvedRoot -Recurse -Force
        }
        if (Get-ScheduledTask -TaskName $task -ErrorAction SilentlyContinue) {
            throw "实验计划任务仍然存在：$task"
        }
        if (
            Get-CimInstance -ClassName Win32_Process -Filter "Name='remoteops-agent.exe'" |
                Where-Object {
                    $_.ExecutablePath -and
                    $_.ExecutablePath.Equals(
                        $agentPath,
                        [System.StringComparison]::OrdinalIgnoreCase
                    )
                }
        ) {
            throw "实验 Agent 进程仍然运行：$agentPath"
        }
        if (Test-Path -LiteralPath $resolvedRoot) {
            throw "Windows 实验目录仍然存在：$resolvedRoot"
        }

        $parent = Split-Path -Parent $resolvedRoot
        if (
            (Test-Path -LiteralPath $parent) -and
            -not (Get-ChildItem -LiteralPath $parent -Force |
                Select-Object -First 1)
        ) {
            Remove-Item -LiteralPath $parent -Force
        }
    } -ArgumentList $windowsRoot, $taskName, $firewallRuleName, $RemoveResources
}

function Wait-ControllerPairing {
    param(
        [Parameter(Mandatory)]
        [string[]]$Pairs,
        [int]$TimeoutSeconds = 45,
        [string]$Description = 'Agent'
    )

    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $lastResultText = '尚未执行 Controller 查询'
    do {
        Start-Sleep -Seconds 1
        $result = Invoke-Controller `
            -Pairs $Pairs `
            -Arguments @('--json', 'list') `
            -AllowFailure
        $lastResultText = (
            @($result.Output) +
            @($result.Error)
        ) -join "`n"
        if ($result.ExitCode -eq 0) {
            try {
                $connections = @(($result.Output -join "`n") | ConvertFrom-Json)
                if (
                    $connections.Count -eq $Pairs.Count -and
                    @(
                        $connections |
                            Where-Object {
                                ([string]$_.state) -ne 'Online'
                            }
                    ).Count -eq 0
                ) {
                    return $connections
                }
            }
            catch {
                # 连接尚未形成完整 JSON，继续等待。
            }
        }
    } while ([DateTime]::UtcNow -lt $deadline)
    throw (
        "$Description 未在 $TimeoutSeconds 秒内恢复为 Online 且可配对状态；" +
        "最后一次 Controller 输出：$lastResultText"
    )
}

if ($CleanupRunToken) {
    $cleanupErrors = [System.Collections.Generic.List[string]]::new()
    $cleanupCredential = $null
    try {
        $cleanupCredential = Get-WindowsLabCredential
    }
    catch {
        $cleanupErrors.Add("无法读取 Windows 测试凭据：$($_.Exception.Message)")
    }

    if ($cleanupCredential) {
        foreach ($computer in $WindowsComputers) {
            $cleanupSession = $null
            try {
                $cleanupSession = New-PSSession `
                    -ComputerName $computer `
                    -Credential $cleanupCredential `
                    -Authentication Negotiate
                Clear-WindowsLabState `
                    -Session $cleanupSession `
                    -RemoveResources $true
            }
            catch {
                $cleanupErrors.Add(
                    "清理 $computer 的 $CleanupRunToken 资源失败：" +
                    $_.Exception.Message
                )
            }
            finally {
                if ($cleanupSession) {
                    Remove-PSSession `
                        -Session $cleanupSession `
                        -ErrorAction SilentlyContinue
                }
            }
        }
    }

    try {
        $linuxCleanup = Invoke-Linux `
            -Command (
                "docker rm -f '$relayContainerName' '$labSshContainerName' " +
                ">/dev/null 2>&1 || true; " +
                "docker volume rm '$relayVolumeName' >/dev/null 2>&1 || true; " +
                "docker network rm '${relayComposeProject}_default' " +
                "'${labSshComposeProject}_default' >/dev/null 2>&1 || true; " +
                "rm -rf -- '$remoteRunRoot'; " +
                "rmdir -- '$remoteLabRoot' 2>/dev/null || true; " +
                "if docker container inspect '$relayContainerName' " +
                ">/dev/null 2>&1 || " +
                "docker container inspect '$labSshContainerName' " +
                ">/dev/null 2>&1 || [ -e '$remoteRunRoot' ]; then " +
                "echo 'Linux 实验资源仍然存在'; exit 1; fi"
            ) `
            -AllowFailure
        if ($linuxCleanup.ExitCode -ne 0) {
            $cleanupErrors.Add(
                "清理 Linux 的 $CleanupRunToken 资源失败：" +
                ($linuxCleanup.Output -join "`n")
            )
        }
    }
    catch {
        $cleanupErrors.Add(
            "清理 Linux 的 $CleanupRunToken 资源失败：" +
            $_.Exception.Message
        )
    }

    $resolvedTempPrefix = $resolvedTempRoot.TrimEnd(
        [System.IO.Path]::DirectorySeparatorChar,
        [System.IO.Path]::AltDirectorySeparatorChar
    ) + [System.IO.Path]::DirectorySeparatorChar
    if (
        (Test-Path -LiteralPath $resolvedTestRoot) -and
        $resolvedTestRoot.StartsWith(
            $resolvedTempPrefix,
            [System.StringComparison]::OrdinalIgnoreCase
        )
    ) {
        try {
            Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force
        }
        catch {
            $cleanupErrors.Add(
                "清理本机的 $CleanupRunToken 临时目录失败：" +
                $_.Exception.Message
            )
        }
    }
    if (Test-Path -LiteralPath $resolvedTestRoot) {
        $cleanupErrors.Add(
            "本机临时目录仍然存在：$resolvedTestRoot"
        )
    }

    if ($cleanupErrors.Count -gt 0) {
        throw ($cleanupErrors -join '；')
    }
    [ordered]@{
        RunToken = $CleanupRunToken
        WindowsResourcesRemoved = $true
        LinuxResourcesRemoved = $true
        LocalTemporaryDirectoryRemoved = $true
    } |
        ConvertTo-Json
    return
}

New-Item -ItemType Directory -Path $resolvedTestRoot -Force | Out-Null

try {
    $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = $humanControllerToken
    $env:REMOTEOPS_CONTROLLER_OWNER_ID = $controllerOwnerId
    Remove-Item -LiteralPath $latestResultPath -Force -ErrorAction SilentlyContinue

    if (-not (Test-Path -LiteralPath $LinuxSshKey)) {
        throw "Linux 专用 SSH 私钥不存在：$LinuxSshKey"
    }
    if (-not (Test-Path -LiteralPath $knownHosts)) {
        throw "OpenSSH known_hosts 不存在：$knownHosts"
    }
    $knownLines = & ssh-keygen.exe -F $LinuxHost -f $knownHosts
    $fingerprints = $knownLines | & ssh-keygen.exe -lf - -E sha256
    if (
        ($fingerprints -join "`n") -notmatch
        [regex]::Escape($ExpectedLinuxHostFingerprint)
    ) {
        throw "Linux SSH 指纹与固定基线不一致：$($fingerprints -join "`n")"
    }
    $keyLogin = Invoke-Linux -Command 'printf REMOTEOPS_KEY_LOGIN_OK'
    if (($keyLogin.Output -join '') -notmatch 'REMOTEOPS_KEY_LOGIN_OK') {
        throw 'Linux SSH 密钥登录验证失败'
    }

    if (-not $SkipBuild) {
        & (Join-Path $PSScriptRoot 'Build-Windows.ps1') -OutputDirectory $artifactRoot
        if ($LASTEXITCODE -ne 0) {
            throw 'Windows Release 构建失败'
        }
    }
    foreach ($path in @($agentExe, $controllerExe, $mcpExe, $mcpSmokeExe)) {
        if (-not (Test-Path -LiteralPath $path)) {
            throw "缺少 Windows 构建产物：$path"
        }
    }
    if (-not (Test-Path -LiteralPath $sshTestKey)) {
        & ssh-keygen.exe `
            -q `
            -t ed25519 `
            -N '' `
            -C 'remoteops-ssh-e2e' `
            -f $sshTestKey
        if ($LASTEXITCODE -ne 0) {
            throw '生成实验室 SSH 测试密钥失败'
        }
    }

    $preexistingContainers = @(
        (
            Invoke-Linux -Command "docker ps --format '{{.ID}} {{.Names}}'"
        ).Output
    )
    $archive = Join-Path $resolvedTestRoot 'remoteops-source.tar.gz'
    Push-Location $workspaceRoot
    try {
        & tar.exe `
            -czf $archive `
            --exclude='./target' `
            --exclude='./artifacts' `
            .
        if ($LASTEXITCODE -ne 0) {
            throw '创建 Linux Docker 构建源归档失败'
        }
    }
    finally {
        Pop-Location
    }

    Invoke-Linux `
        -Command "install -d -m 0700 '$remoteWorkspace' '$remoteDockerConfig' '$remoteTemp' '$remoteRelayData'" |
        Out-Null
    $remoteRunCreated = $true
    $controllerEnvContent = @(
        "REMOTEOPS_HUMAN_CONTROLLER_TOKEN=$humanControllerToken"
        "REMOTEOPS_AI_CONTROLLER_TOKEN=$aiControllerToken"
        "REMOTEOPS_CONTROLLER_OWNER_ID=$controllerOwnerId"
    ) -join "`n"
    Write-LinuxSecretFile `
        -RemotePath $remoteControllerEnv `
        -Content $controllerEnvContent
    $controllerEnvContent = $null
    Copy-ToLinux -LocalPath $archive -RemotePath $remoteArchive
    Invoke-Linux -Command "tar -xzf '$remoteArchive' -C '$remoteWorkspace'" | Out-Null
    Copy-ToLinux `
        -LocalPath "$sshTestKey.pub" `
        -RemotePath "$remoteWorkspace/deploy/lab-ssh/authorized_keys"

    $relayComposeAttempted = $true
    Invoke-Linux -Command "$relayComposeCommand up -d --build" | Out-Null
    $relayContainerId = Get-LinuxComposeContainerId `
        -ComposeCommand $relayComposeCommand `
        -ServiceName 'remoteops-relay'
    Wait-LinuxContainerHealthy -ContainerReference $relayContainerId
    $relayPort = Get-LinuxComposePublishedPort `
        -ComposeCommand $relayComposeCommand `
        -ServiceName 'remoteops-relay' `
        -ContainerPort 7443
    if ($relayPort -ne $relayHostPort) {
        throw "Relay 没有使用预期固定宿主机端口：$relayPort"
    }
    $relayNetworks = (
        Invoke-Linux `
            -Command "docker inspect --format '{{json .NetworkSettings.Networks}}' '$relayContainerId'"
    ).Output -join "`n" |
        ConvertFrom-Json
    $relayNetworkName = $relayNetworks.PSObject.Properties.Name |
        Select-Object -First 1
    if (-not $relayNetworkName) {
        throw '无法确定 Relay 容器所在的 Docker 网络'
    }

    $labSshComposeAttempted = $true
    Invoke-Linux -Command "$labSshComposeCommand up -d --build" | Out-Null
    $labSshContainerId = Get-LinuxComposeContainerId `
        -ComposeCommand $labSshComposeCommand `
        -ServiceName 'remoteops-lab-ssh'
    Wait-LinuxContainerHealthy -ContainerReference $labSshContainerId
    $labSshPort = Get-LinuxComposePublishedPort `
        -ComposeCommand $labSshComposeCommand `
        -ServiceName 'remoteops-lab-ssh' `
        -ContainerPort 22

    Invoke-Linux -Command "docker cp '${relayContainerId}:/data/tls/cert.pem' '$remoteRunRoot/relay-cert.pem' && chmod 0644 '$remoteRunRoot/relay-cert.pem'" | Out-Null
    Copy-FromLinux `
        -RemotePath "$remoteRunRoot/relay-cert.pem" `
        -LocalPath $relayCertificate

    $sshScan = & ssh-keyscan.exe `
        -T 5 `
        -t ed25519 `
        -p $labSshPort `
        $LinuxHost 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $sshScan) {
        throw '无法读取实验室 SSH 容器主机密钥'
    }
    $hostKeyMatch = $null
    foreach ($line in $sshScan) {
        $match = [regex]::Match(
            [string]$line,
            '(?:^|\s)(ssh-ed25519)\s+([A-Za-z0-9+/=]+)(?:\s|$)'
        )
        if ($match.Success) {
            $hostKeyMatch = $match
            break
        }
    }
    if ($null -eq $hostKeyMatch) {
        throw '实验室 SSH 扫描结果没有 ED25519 主机密钥'
    }
    $knownHostLine = '[{0}]:{1} {2} {3}' -f @(
        $LinuxHost,
        $labSshPort,
        $hostKeyMatch.Groups[1].Value,
        $hostKeyMatch.Groups[2].Value
    )
    Set-Content `
        -LiteralPath $sshTestKnownHosts `
        -Value $knownHostLine `
        -Encoding ascii

    $credential = Get-WindowsLabCredential
    foreach ($computer in $WindowsComputers) {
        $sessions += New-PSSession `
            -ComputerName $computer `
            -Credential $credential `
            -Authentication Negotiate
    }
    if ($sessions.Count -ne 2) {
        throw '没有建立两条 Windows PSSession'
    }

    $computerNameA = (Invoke-Command -Session $sessions[0] -ScriptBlock { $env:COMPUTERNAME }).ToString()
    $computerNameB = (Invoke-Command -Session $sessions[1] -ScriptBlock { $env:COMPUTERNAME }).ToString()

    foreach ($session in $sessions) {
        Invoke-Command -Session $session -ScriptBlock {
            param($root)
            New-Item -ItemType Directory -Path $root -Force | Out-Null
            New-Item -ItemType Directory -Path (Join-Path $root 'ssh') -Force | Out-Null
            @(
                'file-a.txt',
                'file-b.txt',
                'mcp-file.txt',
                'ai-approved.txt'
            ) |
                ForEach-Object { Join-Path $root $_ } |
                Remove-Item -Force -ErrorAction SilentlyContinue
        } -ArgumentList $windowsRoot
        Copy-Item -LiteralPath $agentExe -Destination "$windowsRoot\remoteops-agent.exe" -ToSession $session -Force
        Copy-Item -LiteralPath $relayCertificate -Destination "$windowsRoot\relay-cert.pem" -ToSession $session -Force
        Copy-Item -LiteralPath $sshTestKey -Destination "$windowsRoot\ssh\id_ed25519" -ToSession $session -Force
        Copy-Item -LiteralPath $sshTestKnownHosts -Destination "$windowsRoot\ssh\known_hosts" -ToSession $session -Force
        Invoke-Command -Session $session -ScriptBlock {
            param($root)
            $identity = Join-Path $root 'ssh\id_ed25519'
            & icacls.exe $identity /inheritance:r /grant:r '*S-1-5-32-544:(R)' | Out-Null
            if ($LASTEXITCODE -ne 0) {
                throw '限制 SSH 私钥 ACL 失败'
            }
        } -ArgumentList $windowsRoot
    }

    $startA = Start-WindowsAgent `
        -Session $sessions[0] `
        -RelayPort $relayPort
    $startB = Start-WindowsAgent `
        -Session $sessions[1] `
        -RelayPort $relayPort
    $registrationA = Get-AgentRegistration `
        -Session $sessions[0] `
        -NotBeforeUtc $startA.NotBeforeUtc
    $registrationB = Get-AgentRegistration `
        -Session $sessions[1] `
        -NotBeforeUtc $startB.NotBeforeUtc
    $agentAId = [guid]$registrationA.AgentInstanceId
    $agentBId = [guid]$registrationB.AgentInstanceId
    if ($agentAId -eq $agentBId) {
        throw '两台 Windows Agent 生成了相同的实例 GUID'
    }

    $initialPairs = @(
        $registrationA.PairingCode,
        $registrationB.PairingCode
    )
    $defaultConnections = Wait-ControllerPairing `
        -Pairs $initialPairs `
        -Description 'Agent A 和 Agent B 初始连接'
    if ($defaultConnections.Count -ne 2) {
        throw 'Controller 没有同时看到两个连接'
    }
    $defaultIndexes = @(
        $defaultConnections |
            Sort-Object display_index |
            ForEach-Object display_index
    )
    if (
        $defaultIndexes.Count -ne 2 -or
        $defaultIndexes[0] -ne 1 -or
        $defaultIndexes[1] -ne 2 -or
        @($defaultConnections | Where-Object alias).Count -ne 0
    ) {
        throw '双 Agent 没有按“连接 1、连接 2”显示默认编号'
    }
    $defaultConnectionA = $defaultConnections |
        Where-Object agent_instance_id -EQ $agentAId.ToString()
    $defaultConnectionB = $defaultConnections |
        Where-Object agent_instance_id -EQ $agentBId.ToString()
    if (-not $defaultConnectionA -or -not $defaultConnectionB) {
        throw '默认连接列表没有按 Agent GUID 匹配到 Agent A 和 Agent B'
    }

    $aliasAResult = Invoke-Controller `
        -Pairs $initialPairs `
        -Arguments @(
            '--json',
            'alias',
            $defaultConnectionA.session_id,
            '客户 A'
        )
    $aliasBResult = Invoke-Controller `
        -Pairs $initialPairs `
        -Arguments @(
            '--json',
            'alias',
            $defaultConnectionB.session_id,
            '客户 B'
        )
    $aliasAConnections = ($aliasAResult.Output -join "`n") | ConvertFrom-Json
    $aliasBConnections = ($aliasBResult.Output -join "`n") | ConvertFrom-Json
    $aliasAConnection = @(
        $aliasAConnections |
            Where-Object { $_.session_id -eq $defaultConnectionA.session_id }
    )
    $aliasBConnection = @(
        $aliasBConnections |
            Where-Object { $_.session_id -eq $defaultConnectionB.session_id }
    )
    if (
        $aliasAConnection.Count -ne 1 -or
        $aliasBConnection.Count -ne 1 -or
        $aliasAConnection[0].alias -ne '客户 A' -or
        $aliasBConnection[0].alias -ne '客户 B'
    ) {
        throw (
            'Controller CLI 修改连接别名失败；客户 A 输出：' +
            ($aliasAConnections | ConvertTo-Json -Depth 5 -Compress) +
            '；客户 B 输出：' +
            ($aliasBConnections | ConvertTo-Json -Depth 5 -Compress)
        )
    }

    $pairA = "$($registrationA.PairingCode)=客户 A"
    $pairB = "$($registrationB.PairingCode)=客户 B"
    $allPairs = @($pairA, $pairB)
    $connections = Wait-ControllerPairing `
        -Pairs $allPairs `
        -Description 'Agent A 和 Agent B 别名连接'
    $connectionA = $connections | Where-Object alias -EQ '客户 A'
    $connectionB = $connections | Where-Object alias -EQ '客户 B'
    if (
        -not $connectionA -or
        -not $connectionB -or
        $connectionA.session_id -eq $connectionB.session_id -or
        $connectionA.session_id -ne $defaultConnectionA.session_id -or
        $connectionB.session_id -ne $defaultConnectionB.session_id
    ) {
        throw '双 Agent 的别名或 session_id 隔离失败'
    }

    $cmdA = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('run', '客户 A', '--shell', 'cmd', '--readonly', 'echo %COMPUTERNAME%')
    $cmdB = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('run', '客户 B', '--shell', 'cmd', '--readonly', 'echo %COMPUTERNAME%')
    if (
        ($cmdA.Output -join "`n") -notmatch [regex]::Escape($computerNameA) -or
        ($cmdB.Output -join "`n") -notmatch [regex]::Escape($computerNameB)
    ) {
        throw 'CMD 命令被发送到错误目标或输出不正确'
    }

    $powerShellA = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('run', '客户 A', '--shell', 'windows-power-shell', '--readonly', '$env:COMPUTERNAME')
    $powerShellB = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('run', '客户 B', '--shell', 'windows-power-shell', '--readonly', '$env:COMPUTERNAME')
    if (
        ($powerShellA.Output -join "`n") -notmatch [regex]::Escape($computerNameA) -or
        ($powerShellB.Output -join "`n") -notmatch [regex]::Escape($computerNameB)
    ) {
        throw 'Windows PowerShell 5.1 命令隔离失败'
    }

    $shellOpen = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('--json', 'shell-open', '客户 A', '--shell', 'cmd')
    $persistentShellId = (
        ($shellOpen.Output -join "`n") |
            ConvertFrom-Json
    ).details.shell_id
    Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @(
            'shell-run',
            '客户 A',
            $persistentShellId,
            '--shell',
            'cmd',
            'set REMOTEOPS_LAB_PERSIST=LAB-AGENT-A'
        ) |
        Out-Null
    $persistentRead = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @(
            'shell-run',
            '客户 A',
            $persistentShellId,
            '--shell',
            'cmd',
            'echo %REMOTEOPS_LAB_PERSIST%'
        )
    if (($persistentRead.Output -join "`n") -notmatch 'LAB-AGENT-A') {
        throw '真实持久 Shell 没有保持进程状态'
    }
    Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('shell-close', '客户 A', $persistentShellId) |
        Out-Null

    foreach ($alias in @('客户 A', '客户 B')) {
        $probe = Invoke-Controller `
            -Pairs $allPairs `
            -Arguments @('test-port', $alias, $LinuxHost, [string]$relayPort)
        if (($probe.Output -join "`n") -notmatch 'open') {
            throw "$alias 无法从现场网络访问 Relay 端口"
        }
    }

    $uploadA = Join-Path $resolvedTestRoot 'upload-a.txt'
    $uploadB = Join-Path $resolvedTestRoot 'upload-b.txt'
    $downloadA = Join-Path $resolvedTestRoot 'download-a.txt'
    $downloadB = Join-Path $resolvedTestRoot 'download-b.txt'
    Set-Content -LiteralPath $uploadA -Value 'REMOTEOPS_AGENT_A_FILE' -Encoding utf8NoBOM
    Set-Content -LiteralPath $uploadB -Value 'REMOTEOPS_AGENT_B_FILE' -Encoding utf8NoBOM
    Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('upload', '客户 A', $uploadA, 'file-a.txt') |
        Out-Null
    Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('upload', '客户 B', $uploadB, 'file-b.txt') |
        Out-Null
    Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('download', '客户 A', 'file-a.txt', $downloadA) |
        Out-Null
    Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('download', '客户 B', 'file-b.txt', $downloadB) |
        Out-Null
    if (
        (Get-FileHash $uploadA -Algorithm SHA256).Hash -ne
        (Get-FileHash $downloadA -Algorithm SHA256).Hash -or
        (Get-FileHash $uploadB -Algorithm SHA256).Hash -ne
        (Get-FileHash $downloadB -Algorithm SHA256).Hash
    ) {
        throw '双 Agent 文件上传下载 SHA-256 校验失败'
    }

    $remoteIdentity = 'ssh\id_ed25519'
    $remoteKnownHosts = 'ssh\known_hosts'
    foreach ($item in @(
        @{ Alias = '客户 A'; Marker = 'REMOTEOPS_SSH_AGENT_A' },
        @{ Alias = '客户 B'; Marker = 'REMOTEOPS_SSH_AGENT_B' }
    )) {
        $sshResult = Invoke-Controller `
            -Pairs $allPairs `
            -Arguments @(
                'ssh',
                $item.Alias,
                $LinuxHost,
                '--port',
                [string]$labSshPort,
                '--identity-file',
                $remoteIdentity,
                '--known-hosts-file',
                $remoteKnownHosts,
                '--readonly',
                'remoteops',
                "printf $($item.Marker)"
            )
        if (($sshResult.Output -join "`n") -notmatch $item.Marker) {
            $sshDetails = @($sshResult.Output) + @($sshResult.Error)
            throw (
                "$($item.Alias) 的 SSH 密钥认证测试失败：" +
                ($sshDetails -join "`n")
            )
        }
    }

    $serialA = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('--json', 'serial-list', '客户 A')
    $serialB = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('--json', 'serial-list', '客户 B')
    $serialAResult = ($serialA.Output -join "`n") | ConvertFrom-Json
    $serialBResult = ($serialB.Output -join "`n") | ConvertFrom-Json
    $serialADevices = @($serialAResult.details)
    $serialBDevices = @($serialBResult.details)
    $serialValidationSettings = [ordered]@{
        BaudRate = 115200
        DataBits = 'eight'
        StopBits = 'one'
        Parity = 'none'
        FlowControl = 'none'
        Writable = $false
    }

    $approvalTarget = "$windowsRoot\approval-executed.txt"
    $approval = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @(
            'run',
            '客户 A',
            '--shell',
            'windows-power-shell',
            '--approve',
            (
                "Start-Process -FilePath cmd.exe -ArgumentList " +
                "'/c','echo REMOTEOPS_APPROVED > $approvalTarget' -Wait"
            )
        )
    $approvalText = (@($approval.Output) + @($approval.Error)) -join "`n"
    if ($approvalText -notmatch '人工审批：[-0-9a-fA-F]{36}') {
        throw "人工高风险操作没有进入审批：$approvalText"
    }
    $approvalVerification = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @(
            '--json',
            'run',
            '客户 A',
            '--shell',
            'windows-power-shell',
            '--readonly',
            "Get-Content -LiteralPath '$approvalTarget'"
        )
    if (($approvalVerification.Output -join "`n") -notmatch 'REMOTEOPS_APPROVED') {
        throw '人工审批后的高风险实验命令没有产生预期结果'
    }

    $cancel = Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @(
            '--json',
            'cancel-after',
            '客户 A',
            '--shell',
            'cmd',
            '--delay-millis',
            '500',
            'ping 127.0.0.1 -n 30'
        )
    if (($cancel.Output -join "`n") -notmatch '"cancelled"\s*:\s*true') {
        throw '人工没有成功中断 AI 请求'
    }

    $mcpDownload = Join-Path $resolvedTestRoot 'mcp-download.txt'
    $mcpRemote = 'mcp-file.txt'
    $env:REMOTEOPS_CONTROLLER_TOKEN = $aiControllerToken
    $mcpOutput = & $mcpSmokeExe `
        --mcp-executable $mcpExe `
        --human-cli-executable $controllerExe `
        --relay "${LinuxHost}:$relayPort" `
        --server-name $LinuxHost `
        --ca-cert $relayCertificate `
        --audit-log $auditLog `
        --transfer-root $resolvedTestRoot `
        --pair $pairA `
        --pair $pairB `
        --upload-source $uploadA `
        --remote-file $mcpRemote `
        --download-target $mcpDownload `
        --probe-host $LinuxHost `
        --probe-port $relayPort 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "双连接 MCP STDIO 验收失败：$($mcpOutput -join "`n")"
    }
    if (
        (Get-FileHash $uploadA -Algorithm SHA256).Hash -ne
        (Get-FileHash $mcpDownload -Algorithm SHA256).Hash
    ) {
        throw 'MCP 文件传输 SHA-256 校验失败'
    }

    $auditExportA = Join-Path $resolvedTestRoot 'audit-agent-a.json'
    Invoke-Controller `
        -Pairs $allPairs `
        -Arguments @('audit-export', '客户 A', $auditExportA) |
        Out-Null
    $auditText = Get-Content -LiteralPath $auditExportA -Raw
    if (
        $auditText -notmatch '"source": "human"' -or
        $auditText -notmatch '"source": "ai"' -or
        $auditText -match '(?i)(password|api[_-]?key|authorization)\s*[:=]\s*(?!\[REDACTED\])'
    ) {
        throw '审计导出缺少人工/AI 事件或包含未脱敏秘密'
    }

    Invoke-Command -Session $sessions[0] -ScriptBlock {
        param($rule, $program, $remoteHost, $remotePort)
        Get-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue |
            Remove-NetFirewallRule
        New-NetFirewallRule `
            -DisplayName $rule `
            -Direction Outbound `
            -Action Block `
            -Program $program `
            -Protocol TCP `
            -RemoteAddress $remoteHost `
            -RemotePort $remotePort `
            -Profile Any |
            Out-Null
    } -ArgumentList `
        $firewallRuleName,
        "$windowsRoot\remoteops-agent.exe",
        $LinuxHost,
        $relayPort
    $firewallInstalled = $true
    Stop-WindowsAgent -Session $sessions[0]

    $offlineObserved = $false
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        Start-Sleep -Seconds 1
        $attempt = Invoke-Controller `
            -Pairs @($pairA) `
            -Arguments @('list') `
            -AllowFailure
        $offlineObserved = $attempt.ExitCode -ne 0
    } while (-not $offlineObserved -and [DateTime]::UtcNow -lt $deadline)
    if (-not $offlineObserved) {
        throw '没有观察到 Agent A 单独断线'
    }
    $agentBDuringAOffline = Invoke-Controller `
        -Pairs @($pairB) `
        -Arguments @('run', '客户 B', '--shell', 'cmd', '--readonly', 'echo AGENT_B_STILL_ONLINE')
    if (($agentBDuringAOffline.Output -join "`n") -notmatch 'AGENT_B_STILL_ONLINE') {
        throw 'Agent A 断线影响了 Agent B'
    }

    Invoke-Command -Session $sessions[0] -ScriptBlock {
        param($rule)
        Get-NetFirewallRule -DisplayName $rule -ErrorAction SilentlyContinue |
            Remove-NetFirewallRule
    } -ArgumentList $firewallRuleName
    $firewallInstalled = $false
    $networkRecoveryStartA = Start-WindowsAgent `
        -Session $sessions[0] `
        -RelayPort $relayPort
    $networkRecoveryRegistrationA = Get-AgentRegistration `
        -Session $sessions[0] `
        -ExpectedPairingCode $registrationA.PairingCode `
        -NotBeforeUtc $networkRecoveryStartA.NotBeforeUtc
    if (
        $networkRecoveryRegistrationA.AgentInstanceId -ne
        $agentAId.ToString()
    ) {
        throw 'Agent A 网络隔离恢复后没有恢复原 GUID'
    }
    $recoveredA = Wait-ControllerPairing `
        -Pairs @($pairA) `
        -TimeoutSeconds 25 `
        -Description 'Agent A 网络隔离恢复'
    if ($recoveredA[0].session_id -ne $connectionA.session_id) {
        throw 'Agent A 网络恢复后 session_id 没有保持'
    }

    Stop-WindowsAgent -Session $sessions[0]
    Start-Sleep -Seconds 2
    $processRestartStartA = Start-WindowsAgent `
        -Session $sessions[0] `
        -RelayPort $relayPort
    $registrationAAfterProcessRestart = Get-AgentRegistration `
        -Session $sessions[0] `
        -ExpectedPairingCode $registrationA.PairingCode `
        -NotBeforeUtc $processRestartStartA.NotBeforeUtc
    if (
        $registrationAAfterProcessRestart.AgentInstanceId -ne
        $agentAId.ToString()
    ) {
        throw 'Agent A 进程重启后没有恢复原 GUID'
    }
    $processRestartRecoveredA = Wait-ControllerPairing `
        -Pairs @($pairA) `
        -TimeoutSeconds 25 `
        -Description 'Agent A 进程重启恢复'
    if ($processRestartRecoveredA[0].session_id -ne $connectionA.session_id) {
        throw 'Agent A 进程重启后 session_id 没有保持'
    }

    Invoke-Linux `
        -Command "docker rm -f '$relayContainerId' >/dev/null 2>&1 || true; docker network rm '$relayNetworkName' >/dev/null 2>&1 || true" |
        Out-Null
    Start-Sleep -Seconds 2
    Invoke-Linux -Command "$relayComposeCommand up -d --no-build" |
        Out-Null
    $relayContainerId = (
        @(
            Invoke-Linux `
                -Command "$relayComposeCommand ps -q remoteops-relay"
        ).Output -join "`n"
    ).Trim()
    if ($relayContainerId -notmatch '^[0-9a-f]{12,64}$') {
        throw "Relay 网络故障恢复后没有获得新的容器 ID：$relayContainerId"
    }
    $relayContainerName = "remoteops-relay-$runToken"
    $relayPort = Get-LinuxComposePublishedPort `
        -ComposeCommand $relayComposeCommand `
        -ServiceName 'remoteops-relay' `
        -ContainerPort 7443
    if ($relayPort -ne $relayHostPort) {
        throw "Relay 网络故障恢复后宿主机端口发生变化：$relayPort"
    }
    $relayNetworks = (
        Invoke-Linux `
            -Command "docker inspect --format '{{json .NetworkSettings.Networks}}' '$relayContainerId'"
    ).Output -join "`n" |
        ConvertFrom-Json
    $relayNetworkName = $relayNetworks.PSObject.Properties.Name |
        Select-Object -First 1
    if (-not $relayNetworkName) {
        throw 'Relay 网络故障恢复后无法确定 Docker 网络'
    }
    Wait-LinuxContainerHealthy -ContainerReference $relayContainerId
    $networkRecovered = Wait-ControllerPairing `
        -Pairs $allPairs `
        -TimeoutSeconds 30 `
        -Description 'Relay Docker 网络重接并重启恢复'
    $networkA = $networkRecovered | Where-Object alias -EQ '客户 A'
    $networkB = $networkRecovered | Where-Object alias -EQ '客户 B'
    if (
        $networkA.session_id -ne $connectionA.session_id -or
        $networkB.session_id -ne $connectionB.session_id
    ) {
        throw 'Relay 网络断开恢复后 session_id 发生变化'
    }

    $beforeRelayRestartA = Get-AgentRegistration -Session $sessions[0]
    $beforeRelayRestartB = Get-AgentRegistration -Session $sessions[1]
    $relayStateStat = (
        Invoke-Linux `
            -Command "docker exec '$relayContainerId' stat -c '%a %s' /data/relay-state.json"
    ).Output -join "`n"
    if ($relayStateStat -notmatch '^600\s+[1-9][0-9]*\s*$') {
        throw "Relay 状态文件不存在、为空或权限不是 0600：$relayStateStat"
    }

    Invoke-Linux -Command "$relayComposeCommand restart remoteops-relay" | Out-Null
    Wait-LinuxContainerHealthy -ContainerReference $relayContainerId
    $registrationA2 = Get-AgentRegistration `
        -Session $sessions[0] `
        -ExpectedPairingCode $registrationA.PairingCode `
        -AfterRegistrationCount $beforeRelayRestartA.RegistrationCount
    $registrationB2 = Get-AgentRegistration `
        -Session $sessions[1] `
        -ExpectedPairingCode $registrationB.PairingCode `
        -AfterRegistrationCount $beforeRelayRestartB.RegistrationCount
    if (
        $registrationA2.AgentInstanceId -ne $agentAId.ToString() -or
        $registrationB2.AgentInstanceId -ne $agentBId.ToString()
    ) {
        throw 'Relay 容器重启后 Agent GUID 发生变化'
    }
    if (
        $registrationA2.PairingCode -ne $registrationA.PairingCode -or
        $registrationB2.PairingCode -ne $registrationB.PairingCode
    ) {
        throw 'Relay 容器重启后控制码发生变化'
    }
    $restoredConnections = Wait-ControllerPairing `
        -Pairs @($pairA, $pairB) `
        -Description 'Relay 容器重启后的双 Agent'
    $restoredConnectionA = $restoredConnections | Where-Object alias -EQ '客户 A'
    $restoredConnectionB = $restoredConnections | Where-Object alias -EQ '客户 B'
    if (
        $restoredConnectionA.session_id -ne $connectionA.session_id -or
        $restoredConnectionB.session_id -ne $connectionB.session_id
    ) {
        throw 'Relay 容器重启后 session_id 发生变化'
    }

    Stop-WindowsAgent -Session $sessions[1]
    Start-Sleep -Seconds 35
    $expiredPairAttempt = Invoke-Controller `
        -Pairs @($pairB) `
        -Arguments @('list') `
        -AllowFailure
    $expiredPairText = (
        @($expiredPairAttempt.Output) +
        @($expiredPairAttempt.Error)
    ) -join "`n"
    $expiredPairRejected = (
        $expiredPairAttempt.ExitCode -ne 0 -and
        $expiredPairText -match
            '(pairing_failed|控制码不存在或已经过期|控制码租约已经过期)'
    )
    if (-not $expiredPairRejected) {
        throw "Agent B 离线后的控制码未以租约错误被拒绝：$expiredPairText"
    }
    Invoke-Controller -Pairs @($pairA) -Arguments @('list') | Out-Null
    $leaseExpiryStartB = Start-WindowsAgent `
        -Session $sessions[1] `
        -RelayPort $relayPort
    $registrationB3 = Get-AgentRegistration `
        -Session $sessions[1] `
        -PreviousPairingCode $registrationB.PairingCode `
        -NotBeforeUtc $leaseExpiryStartB.NotBeforeUtc
    if ($registrationB3.AgentInstanceId -ne $agentBId.ToString()) {
        throw 'Agent B 租约过期后 Agent 进程重启没有恢复原 GUID'
    }
    $pairB3 = "$($registrationB3.PairingCode)=客户 B"
    $connectionB3 = Wait-ControllerPairing `
        -Pairs @($pairB3) `
        -Description 'Agent B 控制码租约过期后重新注册'
    if ($connectionB3[0].session_id -eq $connectionB.session_id) {
        throw 'Agent B 租约过期并重新注册后仍错误复用了旧 session_id'
    }

    $relayLogs = (
        Invoke-Linux -Command "$relayComposeCommand logs --tail 500 remoteops-relay"
    ).Output -join "`n"
    if (
        $relayLogs -match
        '(?i)(password|passwd|api[_-]?key|authorization|bearer)\s*[:=]\s*\S+'
    ) {
        throw 'Relay 日志包含疑似密码或 API Key'
    }
    $controllerAuditText = if (Test-Path -LiteralPath $auditLog) {
        Get-Content -LiteralPath $auditLog -Raw
    }
    else {
        ''
    }
    $tokenLeakSurface = @(
        $relayLogs,
        $controllerAuditText,
        $auditText,
        ($mcpOutput -join "`n")
    ) -join "`n"
    if (
        $tokenLeakSurface.Contains($humanControllerToken) -or
        $tokenLeakSurface.Contains($aiControllerToken)
    ) {
        throw 'Relay、Controller 或 MCP 输出中记录了 Controller Token'
    }

    $postContainers = (
        Invoke-Linux -Command "docker ps --format '{{.ID}} {{.Names}}'"
    ).Output
    foreach ($container in $preexistingContainers) {
        $containerId = ($container -split '\s+')[0]
        if (
            $containerId -and
            -not ($postContainers -match "^$([regex]::Escape($containerId))\s")
        ) {
            throw "Relay 部署影响了原有容器：$container"
        }
    }

    $result = [PSCustomObject]@{
        RunToken = $runToken
        CompletedAt = (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
        ResourceIsolation = @{
            WindowsRoot = $windowsRoot
            ScheduledTask = $taskName
            RelayComposeProject = $relayComposeProject
            RelayContainer = $relayContainerName
            RelayPort = $relayPort
            LabSshComposeProject = $labSshComposeProject
            LabSshContainer = $labSshContainerName
            LabSshPort = $labSshPort
            KeepLabResources = [bool]$KeepLabResources
        }
        LinuxRelay = @{
            SshKeyLogin = $true
            DockerBuild = $true
            Healthy = $true
            LogsChecked = $true
            Restarted = $true
            StateFile = '/data/relay-state.json'
            StateFileMode0600 = $true
            RestartPreservedSession = $true
            RestartPreservedPairingCode = $true
            ExistingContainersPreserved = $true
        }
        AgentA = @{
            AgentGuid = $agentAId.ToString()
            SessionId = $connectionA.session_id
            Cmd = $true
            WindowsPowerShell51 = $true
            PersistentShell = $true
            PortProbe = $true
            FileTransferHash = $true
            SshKeyAuthentication = $true
            SerialEnumerationCount = $serialADevices.Count
            DisconnectRecovery = $true
            AgentGuidPersistedAfterProcessRestart = $true
            SessionPersistedAfterProcessRestart = $true
        }
        AgentB = @{
            AgentGuid = $agentBId.ToString()
            InitialSessionId = $connectionB.session_id
            SessionIdAfterLeaseExpiry = $connectionB3[0].session_id
            Cmd = $true
            WindowsPowerShell51 = $true
            PortProbe = $true
            FileTransferHash = $true
            SshKeyAuthentication = $true
            SerialEnumerationCount = $serialBDevices.Count
            IndependentWhileAgentAOffline = $true
            ExpiredPairingCodeRejected = $true
            RestartedAfterExpiry = [bool]$registrationB3
            AgentGuidPersistedAfterLeaseExpiryRestart = $true
            NewSessionAfterLeaseExpiry = $true
        }
        Controller = @{
            MultiConnection = $true
            DefaultNumbering = $true
            AliasModification = $true
            AliasIsolation = $true
            SessionIsolation = $true
            HumanApproval = $true
            HumanCancelledAi = $true
            AuditExport = $true
            HumanAndAiTokensDistinct = $true
            HumanControllerAuthenticated = $true
            AiControllerAuthenticated = $true
            ControllerTokenLeakCheck = $true
        }
        Mcp = @{
            OfficialSdkSmoke = $true
            RuntimePairing = $true
            TwoConnections = $true
            PersistentShell = $true
            ApprovalBoundToSession = $true
            FileTransferHash = $true
        }
        Serial = @{
            ValidationSettings = $serialValidationSettings
            AgentAEnumeration = $serialADevices
            AgentBEnumeration = $serialBDevices
            EnumerationCompleted = $true
            RealHardwareValidated = $false
            OpenReadWriteValidated = $false
            Limitation = '仅完成两台 Agent 的真实系统串口枚举；没有现场 USB/COM 硬件，因此未声称打开、读写或断线恢复通过'
        }
        Cleanup = @{
            ResourcesRemoved = -not [bool]$KeepLabResources
            VerifiedBeforeResultWrite = -not [bool]$KeepLabResources
        }
        Remaining = @(
            '真实交换机 SSH 待现场设备验证',
            '真实 USB/COM 串口读写待现场硬件验证'
        )
    }
    $resultJson = $result | ConvertTo-Json -Depth 8
}
finally {
    $removeResources = -not [bool]$KeepLabResources
    $cleanupErrors = [System.Collections.Generic.List[string]]::new()

    foreach ($computer in $WindowsComputers) {
        $cleanupSession = $sessions |
            Where-Object {
                $_.ComputerName -eq $computer -and
                $_.State -eq [System.Management.Automation.Runspaces.RunspaceState]::Opened
            } |
            Select-Object -First 1
        $createdCleanupSession = $false
        if (-not $cleanupSession -and $credential) {
            try {
                $cleanupSession = New-PSSession `
                    -ComputerName $computer `
                    -Credential $credential `
                    -Authentication Negotiate
                $createdCleanupSession = $true
            }
            catch {
                $cleanupErrors.Add(
                    "无法为 $computer 建立清理会话：$($_.Exception.Message)"
                )
            }
        }

        if ($cleanupSession) {
            try {
                Clear-WindowsLabState `
                    -Session $cleanupSession `
                    -RemoveResources $removeResources
            }
            catch {
                $cleanupErrors.Add(
                    "清理 $computer 实验资源失败：$($_.Exception.Message)"
                )
            }
            finally {
                if ($createdCleanupSession) {
                    Remove-PSSession `
                        -Session $cleanupSession `
                        -ErrorAction SilentlyContinue
                }
            }
        }
    }

    if ($sessions.Count -gt 0) {
        $sessions | Remove-PSSession -ErrorAction SilentlyContinue
    }

    if ($removeResources) {
        if ($labSshComposeAttempted) {
            try {
                Invoke-Linux `
                    -Command "$labSshComposeCommand down --volumes --remove-orphans --timeout 10" `
                    -AllowFailure |
                    Out-Null
                Invoke-Linux `
                    -Command "docker rm -f '$labSshContainerName' >/dev/null 2>&1 || true; docker network rm '${labSshComposeProject}_default' >/dev/null 2>&1 || true" `
                    -AllowFailure |
                    Out-Null
                $verification = Invoke-Linux `
                    -Command "if docker container inspect '$labSshContainerName' >/dev/null 2>&1; then echo 'SSH 容器仍存在'; exit 1; fi; if docker network inspect '${labSshComposeProject}_default' >/dev/null 2>&1; then echo 'SSH 网络仍存在'; exit 1; fi" `
                    -AllowFailure
                if ($verification.ExitCode -ne 0) {
                    $cleanupErrors.Add(
                        "Linux SSH 资源清理验证失败：$($verification.Output -join "`n")"
                    )
                }
            }
            catch {
                $cleanupErrors.Add(
                    "清理 Linux SSH 容器失败：$($_.Exception.Message)"
                )
            }
        }
        if ($relayComposeAttempted) {
            try {
                Invoke-Linux `
                    -Command "$relayComposeCommand down --volumes --remove-orphans --timeout 10" `
                    -AllowFailure |
                    Out-Null
                Invoke-Linux `
                    -Command "docker rm -f '$relayContainerName' >/dev/null 2>&1 || true; docker volume rm '$relayVolumeName' >/dev/null 2>&1 || true; docker network rm '${relayComposeProject}_default' >/dev/null 2>&1 || true" `
                    -AllowFailure |
                    Out-Null
                $verification = Invoke-Linux `
                    -Command "if docker container inspect '$relayContainerName' >/dev/null 2>&1; then echo 'Relay 容器仍存在'; exit 1; fi; if docker volume inspect '$relayVolumeName' >/dev/null 2>&1; then echo 'Relay 卷仍存在'; exit 1; fi; if docker network inspect '${relayComposeProject}_default' >/dev/null 2>&1; then echo 'Relay 网络仍存在'; exit 1; fi" `
                    -AllowFailure
                if ($verification.ExitCode -ne 0) {
                    $cleanupErrors.Add(
                        "Linux Relay 资源清理验证失败：$($verification.Output -join "`n")"
                    )
                }
            }
            catch {
                $cleanupErrors.Add(
                    "清理 Linux Relay 容器失败：$($_.Exception.Message)"
                )
            }
        }
        if ($remoteRunCreated) {
            $allowedRemotePrefix = "$remoteLabRoot/"
            if (-not $remoteRunRoot.StartsWith(
                $allowedRemotePrefix,
                [System.StringComparison]::Ordinal
            )) {
                $cleanupErrors.Add(
                    "拒绝清理允许范围外的 Linux 目录：$remoteRunRoot"
                )
            }
            else {
                try {
                    $verification = Invoke-Linux `
                        -Command "rm -rf -- '$remoteRunRoot'; rmdir -- '$remoteLabRoot' 2>/dev/null || true; if [ -e '$remoteRunRoot' ]; then echo '远程实验目录仍存在'; exit 1; fi" `
                        -AllowFailure
                    if ($verification.ExitCode -ne 0) {
                        $cleanupErrors.Add(
                            "Linux 远程目录清理验证失败：$($verification.Output -join "`n")"
                        )
                    }
                }
                catch {
                    $cleanupErrors.Add(
                        "清理 Linux 远程目录失败：$($_.Exception.Message)"
                    )
                }
            }
        }

        $resolvedTempPrefix = $resolvedTempRoot.TrimEnd(
            [System.IO.Path]::DirectorySeparatorChar,
            [System.IO.Path]::AltDirectorySeparatorChar
        ) + [System.IO.Path]::DirectorySeparatorChar
        if (
            (Test-Path -LiteralPath $resolvedTestRoot) -and
            $resolvedTestRoot.StartsWith(
                $resolvedTempPrefix,
                [System.StringComparison]::OrdinalIgnoreCase
            )
        ) {
            try {
                Remove-Item -LiteralPath $resolvedTestRoot -Recurse -Force
                if (Test-Path -LiteralPath $resolvedTestRoot) {
                    throw "本地实验临时目录仍然存在：$resolvedTestRoot"
                }
            }
            catch {
                $cleanupErrors.Add(
                    "清理本地实验临时目录失败：$($_.Exception.Message)"
                )
            }
        }
    }
    else {
        Write-Warning "已保留实验资源，运行标识：$runToken"
        Write-Warning "Windows 目录：$windowsRoot；计划任务：$taskName"
        Write-Warning (
            "Linux 目录：$remoteRunRoot；Compose 项目：" +
            "$relayComposeProject、$labSshComposeProject"
        )
        Write-Warning "本地临时目录包含实验 SSH 私钥：$resolvedTestRoot"
    }

    if ($null -eq $previousHumanControllerToken) {
        Remove-Item Env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN -ErrorAction SilentlyContinue
    }
    else {
        $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = $previousHumanControllerToken
    }
    if ($null -eq $previousAiControllerToken) {
        Remove-Item Env:REMOTEOPS_CONTROLLER_TOKEN -ErrorAction SilentlyContinue
    }
    else {
        $env:REMOTEOPS_CONTROLLER_TOKEN = $previousAiControllerToken
    }
    if ($null -eq $previousControllerOwnerId) {
        Remove-Item Env:REMOTEOPS_CONTROLLER_OWNER_ID -ErrorAction SilentlyContinue
    }
    else {
        $env:REMOTEOPS_CONTROLLER_OWNER_ID = $previousControllerOwnerId
    }

    if ($cleanupErrors.Count -gt 0) {
        $cleanupMessage = $cleanupErrors -join '；'
        if ($resultJson) {
            throw "三机主流程通过，但资源清理未通过：$cleanupMessage"
        }
        Write-Warning "三机失败后的资源清理存在问题：$cleanupMessage"
    }
}

$resultJson |
    Set-Content -LiteralPath $resultPath -Encoding utf8NoBOM
$resultJson |
    Set-Content -LiteralPath $latestResultPath -Encoding utf8NoBOM
$resultJson

