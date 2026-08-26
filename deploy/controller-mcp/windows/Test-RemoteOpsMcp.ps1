[CmdletBinding()]
param(
    [string]$CodexHome = (Join-Path $env:USERPROFILE '.codex'),
    [switch]$SkipNetwork
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$expectedVersion = '0.2.0-preview.5'
$configPath = Join-Path $CodexHome 'config.toml'
$installDirectory = Join-Path $CodexHome 'remoteops'
$installedExecutable = $null
$connectionConfigPath = Join-Path $installDirectory 'controller-config.json'
$credentialPromptPath = Join-Path $installDirectory 'remoteops-credential-prompt.exe'
$defaultCodexHome = Join-Path $env:USERPROFILE '.codex'
$isDefaultCodexHome = [IO.Path]::GetFullPath($CodexHome).TrimEnd('\') -eq
    [IO.Path]::GetFullPath($defaultCodexHome).TrimEnd('\')
$standardSkillPath = if ($isDefaultCodexHome) {
    Join-Path $env:USERPROFILE '.agents\skills\remoteops\SKILL.md'
}
else {
    Join-Path $CodexHome 'skills\remoteops\SKILL.md'
}
$compatSkillPath = Join-Path $CodexHome 'skills\remoteops\SKILL.md'
$failures = [System.Collections.Generic.List[string]]::new()

if (-not (Test-Path -LiteralPath $credentialPromptPath -PathType Leaf)) {
    $failures.Add("未找到 SSH 密码安全输入程序：$credentialPromptPath")
}
else {
    Write-Host '[通过] SSH 密码安全输入程序已安装。'
}

foreach ($skillPath in @($standardSkillPath, $compatSkillPath) | Select-Object -Unique) {
    if (-not (Test-Path -LiteralPath $skillPath -PathType Leaf)) {
        $failures.Add("未找到 RemoteOps skill：$skillPath")
    }
    else {
        $skillContent = [IO.File]::ReadAllText($skillPath)
        $description = [regex]::Match(
            $skillContent,
            '(?m)^description:\s*(?<value>.+)$'
        )
        if (
            $skillContent -notmatch '(?m)^name:\s*remoteops\s*$' -or
            $skillContent -notmatch '九位控制码' -or
            $skillContent -notmatch '当前任务未加载 RemoteOps MCP' -or
            -not $description.Success -or
            $description.Groups['value'].Value -notmatch '看不到 RemoteOps MCP 工具时' -or
            $description.Groups['value'].Value -notmatch '禁止改用 Computer Use'
        ) {
            $failures.Add("RemoteOps skill 缺少发现阶段的自然语言触发或 MCP 不可用保护规则：$skillPath")
        }
        else {
            Write-Host "[通过] RemoteOps skill 已安装且路由规则完整：$skillPath"
        }
    }
}

$connectionConfig = $null
if (-not (Test-Path -LiteralPath $connectionConfigPath -PathType Leaf)) {
    $failures.Add("未找到 Relay 配置：$connectionConfigPath")
}
else {
    try {
        $connectionConfig = Get-Content -LiteralPath $connectionConfigPath -Raw | ConvertFrom-Json
        if ([string]::IsNullOrWhiteSpace($connectionConfig.relay)) {
            $failures.Add('Relay 配置缺少 relay。')
        }
        elseif ([string]::IsNullOrWhiteSpace($connectionConfig.server_name) -or $connectionConfig.server_name -eq 'remoteops') {
            $failures.Add('Relay 配置缺少有效的 TLS server_name，不能使用默认占位值 remoteops。')
        }
        else {
            Write-Host '[通过] Relay 地址和 TLS server_name 已配置（不会显示其内容）。'
        }
    }
    catch {
        $failures.Add("Relay 配置不是有效 JSON：$connectionConfigPath")
    }
}
if (-not (Test-Path -LiteralPath $configPath -PathType Leaf)) {
    $failures.Add("未找到 Codex 配置：$configPath")
}
else {
    $config = [IO.File]::ReadAllText($configPath)
    $section = [regex]::Match(
        $config,
        '(?ms)^\[mcp_servers\.remoteops\]\s*\r?\n(?<body>.*?)(?=^\[|\z)'
    )
    if (-not $section.Success) {
        $failures.Add('config.toml 缺少 [mcp_servers.remoteops]。')
    }
    else {
        $command = [regex]::Match(
            $section.Groups['body'].Value,
            '(?m)^command\s*=\s*(?<literal>"(?:\\.|[^"\\])*")\s*$'
        )
        if (-not $command.Success) {
            $failures.Add('RemoteOps MCP 配置缺少有效的 command。')
        }
        else {
            try {
                $installedExecutable = $command.Groups['literal'].Value | ConvertFrom-Json
            }
            catch {
                $failures.Add('RemoteOps MCP command 不是有效的 TOML 基本字符串。')
            }
        }
        if (
            $section.Groups['body'].Value -notmatch
                '(?m)^env_vars\s*=\s*\["REMOTEOPS_CONTROLLER_TOKEN",\s*"REMOTEOPS_CONTROLLER_OWNER_ID"\]\s*$'
        ) {
            $failures.Add('RemoteOps MCP 配置没有安全转发 Controller Token 和统一 Owner 环境变量。')
        }
        else {
            Write-Host '[通过] Codex MCP 配置存在，且 Token 与统一 Owner 仅通过环境变量转发。'
        }
        if (
            $section.Groups['body'].Value -notmatch
                '(?m)^default_tools_approval_mode\s*=\s*"approve"\s*$'
        ) {
            $failures.Add('RemoteOps MCP 未配置为由自身统一处理交互确认。')
        }
        elseif (
            $config -notmatch
                '(?m)^approval_policy\s*=\s*\{\s*granular\s*=\s*\{[^}]*mcp_elicitations\s*=\s*true[^}]*\}\s*\}\s*$'
        ) {
            $failures.Add('Codex 未启用 MCP elicitation，无法显示 RemoteOps 逐项确认。')
        }
        else {
            Write-Host '[通过] RemoteOps MCP 交互确认已启用，且不会重复触发 Codex 静态工具审批。'
        }
        if (
            $config -notmatch
                '(?ms)^\[mcp_servers\.remoteops\.tools\.set_control_mode\]\s*\r?\napproval_mode\s*=\s*"prompt"\s*$'
        ) {
            $failures.Add('set_control_mode 未配置独立 Codex 工具确认。')
        }
        else {
            Write-Host '[通过] 完全控制仅通过 set_control_mode 的 Codex 工具确认启用。'
        }
    }
}

if ([string]::IsNullOrWhiteSpace($installedExecutable)) {
    $failures.Add('无法从 Codex 配置确定 MCP 程序路径。')
}
elseif (-not (Test-Path -LiteralPath $installedExecutable -PathType Leaf)) {
    $failures.Add("未找到 MCP 程序：$installedExecutable")
}
elseif (
    (Split-Path -Leaf $installedExecutable) -notmatch
        "^remoteops-controller-mcp-$([regex]::Escape($expectedVersion))(?:-[0-9a-f]{12})?\.exe$"
) {
    $failures.Add('RemoteOps MCP 配置没有指向当前版本程序。')
}
else {
    $versionOutput = (& $installedExecutable --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $versionOutput -notmatch [regex]::Escape($expectedVersion)) {
        $failures.Add("MCP 版本不正确：$versionOutput")
    }
    else {
        Write-Host "[通过] MCP 版本：$versionOutput"
    }
}

$userToken = [Environment]::GetEnvironmentVariable('REMOTEOPS_CONTROLLER_TOKEN', 'User')
if ([string]::IsNullOrWhiteSpace($userToken)) {
    $failures.Add('当前用户尚未设置 REMOTEOPS_CONTROLLER_TOKEN。')
}
elseif ($userToken.Length -lt 32) {
    $failures.Add('REMOTEOPS_CONTROLLER_TOKEN 长度不足 32 个字符。')
}
else {
    Write-Host '[通过] Controller Token 已设置（不会显示其内容）。'
}

$userOwner = [Environment]::GetEnvironmentVariable(
    'REMOTEOPS_CONTROLLER_OWNER_ID',
    'User'
)
$parsedOwner = [guid]::Empty
if ([string]::IsNullOrWhiteSpace($userOwner)) {
    $failures.Add('当前用户尚未设置 REMOTEOPS_CONTROLLER_OWNER_ID。')
}
elseif (-not [guid]::TryParse($userOwner, [ref]$parsedOwner) -or $parsedOwner -eq [guid]::Empty) {
    $failures.Add('REMOTEOPS_CONTROLLER_OWNER_ID 不是有效的非空 UUID。')
}
else {
    Write-Host '[通过] 统一 Controller Owner 已设置（不会显示其值）。'
}

if (-not $SkipNetwork) {
    if ($null -eq $connectionConfig -or $connectionConfig.relay -notmatch '^(?<host>.+):(?<port>\d+)$') {
        $failures.Add('无法从 Relay 配置解析网络地址。')
    }
    else {
        $reachable = Test-NetConnection $Matches.host -Port ([int]$Matches.port) -InformationLevel Quiet -WarningAction SilentlyContinue
        if ($reachable) {
            Write-Host "[通过] $($connectionConfig.relay) TCP 可达。"
        }
        else {
            $failures.Add("无法连接 $($connectionConfig.relay)，请检查网络、防火墙和 Relay 状态。")
        }
    }
}

if ($failures.Count -gt 0) {
    foreach ($failure in $failures) {
        Write-Error "[失败] $failure"
    }
    exit 1
}

Write-Host ''
Write-Host 'RemoteOps MCP 安装检查通过。完全重启 Codex 后输入 /mcp 查看 remoteops。'
