[CmdletBinding()]
param(
    [string]$CodexHome = $(if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }),
    [switch]$SkipNetwork,
    [switch]$CredentialPromptSmokeTest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$expectedMcpVersion = '0.2.0-preview.13'
$expectedCredentialPromptVersion = '0.2.0-preview.5'
$expectedToolTimeoutSec = 360
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

function Read-CodexInspection {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string]$CodexConfig
    )

    # Rust always emits UTF-8. A native PowerShell pipeline instead uses the
    # console output code page, corrupting non-ASCII installation paths.
    $quotedPath = '"' + ([regex]::Replace(
        [regex]::Replace($CodexConfig, '(\\*)"', '$1$1\"'),
        '(\\+)$', '$1$1'
    )) + '"'
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.Arguments = '--inspect-codex ' + $quotedPath
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $startInfo.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) { throw '无法启动 Codex 配置检查。' }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            $process.WaitForExit()
            throw 'Codex 配置检查超时。'
        }
        $outputText = $stdout.GetAwaiter().GetResult()
        $null = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw 'Codex remoteops 配置解析失败。' }
        return $outputText
    }
    finally {
        $process.Dispose()
    }
}

if (-not (Test-Path -LiteralPath $credentialPromptPath -PathType Leaf)) {
    $failures.Add("未找到 SSH 密码安全输入程序：$credentialPromptPath")
}
else {
    Write-Host '[通过] SSH 密码安全输入程序已安装。'
    $promptVersionOutput = (& $credentialPromptPath --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $promptVersionOutput -notmatch [regex]::Escape($expectedCredentialPromptVersion)) {
        $failures.Add("SSH 密码安全输入程序版本不正确：$promptVersionOutput")
    }
    else {
        Write-Host "[通过] SSH 密码安全输入程序版本：$promptVersionOutput"
    }
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
$usesCredentialStore = $null -ne $connectionConfig -and
    $null -ne $connectionConfig.PSObject.Properties['credential_id'] -and
    -not [string]::IsNullOrWhiteSpace($connectionConfig.credential_id)
if (-not (Test-Path -LiteralPath $configPath -PathType Leaf)) {
    $failures.Add("未找到 Codex 配置：$configPath")
}
else {
    # Inspect using only a known package/installation path. Never execute a path
    # taken from Codex config before constraining it to this installation.
    $inspectionExecutable = Join-Path $PSScriptRoot 'remoteops-controller-mcp.exe'
    if (-not (Test-Path -LiteralPath $inspectionExecutable -PathType Leaf)) {
        $inspectionExecutable = Join-Path $installDirectory "remoteops-controller-mcp-$expectedMcpVersion.exe"
    }
    if (-not (Test-Path -LiteralPath $inspectionExecutable -PathType Leaf)) {
        $failures.Add('缺少可信的当前版本 MCP 程序，无法解析 Codex 配置。')
    }
    else {
        try {
            $inspectionText = Read-CodexInspection -Executable $inspectionExecutable -CodexConfig $configPath
            $inspection = $inspectionText | ConvertFrom-Json
            $candidate = [IO.Path]::GetFullPath([string]$inspection.command)
            $expectedDirectory = [IO.Path]::GetFullPath($installDirectory).TrimEnd('\')
            if ([IO.Path]::GetDirectoryName($candidate).TrimEnd('\') -ne $expectedDirectory -or
                [IO.Path]::GetFileName($candidate) -notmatch
                "^remoteops-controller-mcp-$([regex]::Escape($expectedMcpVersion))(?:-[0-9a-f]{12})?\.exe$") {
                $failures.Add('Codex remoteops 命令不属于当前安装目录；不会执行未知命令。')
            }
            else {
                $installedExecutable = $candidate
            }
            if ([string]::IsNullOrWhiteSpace($inspection.args_config) -or
                [IO.Path]::GetFullPath([string]$inspection.args_config) -ne [IO.Path]::GetFullPath($connectionConfigPath)) {
                $failures.Add('RemoteOps MCP 连接配置参数不匹配。')
            }
            if ($null -eq $inspection.legacy_env -or [bool]$inspection.legacy_env -eq $usesCredentialStore) {
                $failures.Add('RemoteOps MCP 凭据模式与旧环境变量转发配置不匹配。')
            }
            if ($null -eq $inspection.tool_timeout_sec -or [long]$inspection.tool_timeout_sec -lt $expectedToolTimeoutSec) {
                $failures.Add("RemoteOps MCP tool_timeout_sec 必须至少为 $expectedToolTimeoutSec 秒。")
            }
            if ($inspection.default_tools_approval_mode -ne 'approve' -or
                $inspection.set_control_mode_approval_mode -ne 'prompt') {
                $failures.Add('RemoteOps 工具审批或 set_control_mode 独立确认配置不完整。')
            }
            else {
                Write-Host '[通过] RemoteOps 工具审批已配置；全局 Codex 策略由用户管理。'
            }
        }
        catch {
            $failures.Add('无法安全解析 Codex remoteops 配置。')
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
        "^remoteops-controller-mcp-$([regex]::Escape($expectedMcpVersion))(?:-[0-9a-f]{12})?\.exe$"
) {
    $failures.Add('RemoteOps MCP 配置没有指向当前版本程序。')
}
else {
    $versionOutput = (& $installedExecutable --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $versionOutput -notmatch [regex]::Escape($expectedMcpVersion)) {
        $failures.Add("MCP 版本不正确：$versionOutput")
    }
    else {
        Write-Host "[通过] MCP 版本：$versionOutput"
    }
}

if ($usesCredentialStore) {
    if ($installedExecutable -and (Test-Path -LiteralPath $installedExecutable -PathType Leaf)) {
        & $installedExecutable --check-credential --config $connectionConfigPath | Out-Null
        if ($LASTEXITCODE -ne 0) {
            $failures.Add('无法读取此安装的 Windows Credential Manager 凭据；请保留设置状态并重新运行安装器。')
        }
        else {
            Write-Host '[通过] 独立 Controller 凭据存在于 Windows Credential Manager（未读取到脚本）。'
        }
    }
    $parsedOwner = [guid]::Empty
    if ($null -eq $connectionConfig.PSObject.Properties['owner_id'] -or
        -not [guid]::TryParse([string]$connectionConfig.owner_id, [ref]$parsedOwner) -or
        $parsedOwner -eq [guid]::Empty) {
        $failures.Add('连接配置缺少有效的非空 Owner UUID。')
    }
}
else {
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

if ($CredentialPromptSmokeTest) {
    $smokeHash = 'a' * 64
    $psi = [Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $credentialPromptPath
    $psi.Arguments = '--host 127.0.0.1 --port 22 --username smoke-test --command-sha256 ' + $smokeHash
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $smoke = [Diagnostics.Process]::new()
    $smoke.StartInfo = $psi
    if (-not $smoke.Start()) {
        throw '无法启动 SSH 密码安全输入 UI 烟测。'
    }
    $smoke.StandardInput.Close()
    Write-Host '请在 60 秒内点击 RemoteOps SSH credential 窗口的 Cancel 按钮。'
    if (-not $smoke.WaitForExit(60000)) {
        $smoke.Kill()
        $smoke.WaitForExit()
        throw 'SSH 密码安全输入 UI 烟测超时；未收到 Cancel。'
    }
    $smokeOutput = $smoke.StandardOutput.ReadToEnd().Trim()
    $smokeError = $smoke.StandardError.ReadToEnd().Trim()
    if ($smoke.ExitCode -ne 0 -or $smokeOutput -notmatch '"action"\s*:\s*"cancel"') {
        throw "SSH 密码安全输入 UI 烟测失败：$smokeOutput $smokeError"
    }
    Write-Host '[通过] SSH 密码安全输入 UI 可见且 Cancel 路径正常。'
}

Write-Host ''
Write-Host 'RemoteOps MCP 安装检查通过。完全重启 Codex 后输入 /mcp 查看 remoteops。'
