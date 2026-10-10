[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$CodexHome = $(if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }),
    [string]$RelayAddress,
    [guid]$OwnerId,
    [string]$ServerName,
    [string]$CaCert,
    [string]$TlsFingerprint,
    [ValidateSet('readonly', 'approval', 'agent-controlled', 'full-access')]
    [string]$CommandMode = 'agent-controlled',
    [switch]$SkipTokenPrompt,
    [string]$SetupFile,
    [switch]$SetupStdin,
    [switch]$SetupCode,
    [switch]$ConfirmEnrollment
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Never include setup material in diagnostic tracing or native process arguments.
Set-PSDebug -Off
$setupMode = -not [string]::IsNullOrWhiteSpace($SetupFile) -or $SetupStdin -or $SetupCode
$setupSourceCount = [int](-not [string]::IsNullOrWhiteSpace($SetupFile)) + [int]$SetupStdin.IsPresent + [int]$SetupCode.IsPresent
if ($setupSourceCount -gt 1) {
    throw 'SetupFile、SetupStdin 和 SetupCode 只能选择一种。'
}
if ($setupMode) {
    foreach ($manualParameter in @('RelayAddress', 'OwnerId', 'ServerName', 'CaCert', 'TlsFingerprint', 'SkipTokenPrompt')) {
        if ($PSBoundParameters.ContainsKey($manualParameter)) {
            throw '一次性设置不能与手工 Relay、Owner、证书或 Token 参数混用。'
        }
    }
}
elseif ($ConfirmEnrollment) {
    throw 'ConfirmEnrollment 只能用于一次性设置。'
}
elseif ([string]::IsNullOrWhiteSpace($RelayAddress) -or $OwnerId -eq [guid]::Empty) {
    throw '手工安装必须提供 RelayAddress 和非全零 OwnerId；推荐改用 SetupFile 或 SetupCode。'
}

$mcpVersion = '0.2.0-preview.13'
$credentialPromptVersion = '0.2.0-preview.5'
$tokenVariable = 'REMOTEOPS_CONTROLLER_TOKEN'
$ownerVariable = 'REMOTEOPS_CONTROLLER_OWNER_ID'
$sourceExecutable = Join-Path $PSScriptRoot 'remoteops-controller-mcp.exe'
$sourceCredentialPrompt = Join-Path $PSScriptRoot 'remoteops-credential-prompt.exe'
$sourceSkill = Join-Path $PSScriptRoot 'skills\remoteops'
$defaultCodexHome = Join-Path $env:USERPROFILE '.codex'
$isDefaultCodexHome = [IO.Path]::GetFullPath($CodexHome).TrimEnd('\') -eq
    [IO.Path]::GetFullPath($defaultCodexHome).TrimEnd('\')

if (-not $setupMode) {
    if ([string]::IsNullOrWhiteSpace($ServerName)) {
        if ($RelayAddress -match '^\[(?<host>[^\]]+)\]:(?<port>\d+)$') {
            $ServerName = $Matches.host
        }
        elseif ($RelayAddress -match '^(?<host>[^:]+):(?<port>\d+)$') {
            $ServerName = $Matches.host
        }
        else {
            throw 'RelayAddress 必须使用 host:port 格式。'
        }
    }

    if (
        -not [string]::IsNullOrWhiteSpace($CaCert) -and
        -not [string]::IsNullOrWhiteSpace($TlsFingerprint)
    ) {
        throw 'CaCert 和 TlsFingerprint 只能选择一种信任方式。'
    }
    $normalizedTlsFingerprint = $null
    if (-not [string]::IsNullOrWhiteSpace($TlsFingerprint)) {
        $compactFingerprint = $TlsFingerprint.Trim() -replace '^(?i:sha256:)', ''
        $compactFingerprint = $compactFingerprint -replace '[:\-\s]', ''
        if ($compactFingerprint -notmatch '^[0-9A-Fa-f]{64}$') {
            throw 'TlsFingerprint 必须是 64 位 SHA-256 十六进制值。'
        }
        $normalizedTlsFingerprint = (
            0..31 |
                ForEach-Object {
                    $compactFingerprint.Substring($_ * 2, 2).ToUpperInvariant()
                }
        ) -join ':'
    }

}

function Invoke-SetupHelper {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string[]]$Arguments,
        [Parameter(Mandatory)][string]$InputContent
    )

    # .Arguments works on Windows PowerShell 5.1 as well as PowerShell 7.
    # Quote only non-secret switches/paths; setup material always uses stdin.
    $quotedArguments = foreach ($argument in $Arguments) {
        '"' + ([regex]::Replace(
            [regex]::Replace($argument, '(\\*)"', '$1$1\"'),
            '(\\+)$', '$1$1'
        )) + '"'
    }
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.Arguments = $quotedArguments -join ' '
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardInput = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $startInfo.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) { throw '无法启动一次性设置程序。' }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        # Write UTF-8 bytes directly for Windows PowerShell 5.1 as well as 7;
        # do not depend on the console code page or newer encoding properties.
        $inputBytes = [Text.Encoding]::UTF8.GetBytes($InputContent)
        try {
            $process.StandardInput.BaseStream.Write($inputBytes, 0, $inputBytes.Length)
            $process.StandardInput.BaseStream.Flush()
            $process.StandardInput.BaseStream.Close()
        }
        finally {
            [Array]::Clear($inputBytes, 0, $inputBytes.Length)
        }
        if (-not $process.WaitForExit(180000)) {
            $process.Kill()
            $process.WaitForExit()
            throw '登记超时；请保留 setup-state.json 并使用同一设置重试。'
        }
        $outputText = $stdout.GetAwaiter().GetResult()
        $errorText = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) {
            throw "一次性设置未完成：$errorText"
        }
        return $outputText.Trim()
    }
    finally {
        $process.Dispose()
    }
}

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Content
    )

    [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($false))
}

if (-not [Environment]::Is64BitOperatingSystem) {
    throw '此安装包仅支持 Windows x64。'
}
if (-not (Test-Path -LiteralPath $sourceExecutable -PathType Leaf)) {
    throw "安装包缺少 $sourceExecutable"
}
if (-not (Test-Path -LiteralPath $sourceCredentialPrompt -PathType Leaf)) {
    throw "安装包缺少 $sourceCredentialPrompt"
}
if (-not (Test-Path -LiteralPath (Join-Path $sourceSkill 'SKILL.md') -PathType Leaf)) {
    throw "安装包缺少 RemoteOps skill：$sourceSkill"
}

$versionOutput = (& $sourceExecutable --version 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $versionOutput -notmatch [regex]::Escape($mcpVersion)) {
    throw "MCP 可执行文件版本不正确：$versionOutput"
}
$promptVersionOutput = (& $sourceCredentialPrompt --version 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $promptVersionOutput -notmatch [regex]::Escape($credentialPromptVersion)) {
    throw "SSH 密码安全输入程序版本不正确：$promptVersionOutput"
}

$configPath = Join-Path $CodexHome 'config.toml'
# Validate before changing any installation files or credentials.
& $sourceExecutable --validate-codex $configPath | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'Codex 配置校验失败；尚未更改安装或凭据。' }

if (-not $setupMode) {
    $userToken = [Environment]::GetEnvironmentVariable($tokenVariable, 'User')
    if ([string]::IsNullOrWhiteSpace($userToken)) {
        $processToken = [Environment]::GetEnvironmentVariable($tokenVariable, 'Process')
        if (-not [string]::IsNullOrWhiteSpace($processToken)) {
            if ($processToken.Length -lt 32) {
                throw "$tokenVariable 至少需要 32 个字符。"
            }
            [Environment]::SetEnvironmentVariable($tokenVariable, $processToken, 'User')
            $userToken = $processToken
        }
    }
    if ([string]::IsNullOrWhiteSpace($userToken)) {
        if ($SkipTokenPrompt) {
            throw "未找到当前用户环境变量 $tokenVariable。请先安全设置 Token，或不带 -SkipTokenPrompt 重新运行安装脚本。"
        }
        $secureToken = Read-Host '请输入 AI Controller Token（输入内容不会显示）' -AsSecureString
        $tokenPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureToken)
        try {
            $plainToken = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($tokenPointer)
            if ([string]::IsNullOrWhiteSpace($plainToken) -or $plainToken.Length -lt 32) {
                throw 'AI Controller Token 至少需要 32 个字符。'
            }
            [Environment]::SetEnvironmentVariable($tokenVariable, $plainToken, 'User')
        }
        finally {
            [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($tokenPointer)
            Remove-Variable plainToken -ErrorAction SilentlyContinue
        }
    }

    $ownerValue = $OwnerId.ToString('D')
    [Environment]::SetEnvironmentVariable($ownerVariable, $ownerValue, 'User')

}

$installDirectory = Join-Path $CodexHome 'remoteops'
$installedExecutable = Join-Path $installDirectory "remoteops-controller-mcp-$mcpVersion.exe"
$installedCredentialPrompt = Join-Path $installDirectory 'remoteops-credential-prompt.exe'
$connectionConfigPath = Join-Path $installDirectory 'controller-config.json'
$standardSkillDirectory = if ($isDefaultCodexHome) {
    Join-Path $env:USERPROFILE '.agents\skills\remoteops'
}
else {
    Join-Path $CodexHome 'skills\remoteops'
}
$compatSkillDirectory = Join-Path $CodexHome 'skills\remoteops'
$configPath = Join-Path $CodexHome 'config.toml'
New-Item -ItemType Directory -Force -Path $installDirectory | Out-Null
Copy-Item -LiteralPath $sourceCredentialPrompt -Destination $installedCredentialPrompt -Force
try {
    Copy-Item -LiteralPath $sourceExecutable -Destination $installedExecutable -Force
}
catch {
    $sourceHash = (Get-FileHash -LiteralPath $sourceExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
    $installedExecutable = Join-Path $installDirectory (
        "remoteops-controller-mcp-$mcpVersion-$($sourceHash.Substring(0, 12)).exe"
    )
    if (Test-Path -LiteralPath $installedExecutable -PathType Leaf) {
        $installedHash = (
            Get-FileHash -LiteralPath $installedExecutable -Algorithm SHA256
        ).Hash.ToLowerInvariant()
        if ($installedHash -ne $sourceHash) {
            throw "MCP 旁路安装文件哈希冲突：$installedExecutable"
        }
    }
    else {
        Copy-Item -LiteralPath $sourceExecutable -Destination $installedExecutable
    }
    Write-Warning '当前版本 MCP 正被 Codex 使用，已按构建哈希旁路安装；重启 Codex 后切换到新程序。'
}

$installedCaCert = $null
if ($setupMode) {
    $setupInput = $null
    try {
        if (-not [string]::IsNullOrWhiteSpace($SetupFile)) {
            $setupInput = [IO.File]::ReadAllText((Resolve-Path -LiteralPath $SetupFile).Path)
        }
        elseif ($SetupStdin) {
            $setupInput = [Console]::In.ReadToEnd()
        }
        else {
            $secureSetup = Read-Host '请粘贴一次性设置码（输入内容不会显示）' -AsSecureString
            $setupPointer = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secureSetup)
            try {
                $setupInput = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($setupPointer)
            }
            finally {
                [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($setupPointer)
                $secureSetup.Dispose()
            }
        }
        # Preview and redemption use the same in-memory snapshot.
        $preview = Invoke-SetupHelper -Executable $installedExecutable `
            -Arguments @('--setup-stdin', '--setup-preview') -InputContent $setupInput
        Write-Host '请核对登记目标（以下不包含设置密钥）：'
        Write-Host $preview
        if (-not $ConfirmEnrollment) {
            if ($SetupStdin) {
                throw 'SetupStdin 需要 ConfirmEnrollment；请先核对管理员提供的登记目标。'
            }
            $confirmation = Read-Host '确认连接此登记地址并保存本机凭据？输入 yes 继续'
            if ($confirmation -cne 'yes') { throw '已取消，未进行登记。' }
        }
        $summary = Invoke-SetupHelper -Executable $installedExecutable -Arguments @(
            '--setup-stdin', '--setup-enroll',
            '--setup-state', (Join-Path $installDirectory 'setup-state.json'),
            '--setup-output', $connectionConfigPath
        ) -InputContent $setupInput
        if (-not (Test-Path -LiteralPath $connectionConfigPath -PathType Leaf)) {
            throw '登记程序未生成连接配置。'
        }
        Write-Host $summary
    }
    finally {
        Remove-Variable setupInput -ErrorAction SilentlyContinue
    }
}
else {
    if (-not [string]::IsNullOrWhiteSpace($CaCert)) {
        $resolvedCaCert = (Resolve-Path -LiteralPath $CaCert).Path
        $installedCaCert = Join-Path $installDirectory 'relay-ca.pem'
        Copy-Item -LiteralPath $resolvedCaCert -Destination $installedCaCert -Force
    }
    $connectionConfig = [ordered]@{
        relay = $RelayAddress
        server_name = $ServerName
        ca_cert = $installedCaCert
        tls_fingerprint = $normalizedTlsFingerprint
        reconnect_seconds = 2
    } | ConvertTo-Json
    Write-Utf8NoBom -Path $connectionConfigPath -Content ($connectionConfig + "`r`n")
}

foreach ($skillDirectory in @($standardSkillDirectory, $compatSkillDirectory) | Select-Object -Unique) {
    New-Item -ItemType Directory -Force -Path $skillDirectory | Out-Null
    Copy-Item -LiteralPath (Join-Path $sourceSkill 'SKILL.md') -Destination $skillDirectory -Force
    $sourceSkillAgents = Join-Path $sourceSkill 'agents'
    if (Test-Path -LiteralPath $sourceSkillAgents -PathType Container) {
        $skillAgentsDirectory = Join-Path $skillDirectory 'agents'
        New-Item -ItemType Directory -Force -Path $skillAgentsDirectory | Out-Null
        Copy-Item -LiteralPath (Join-Path $sourceSkillAgents 'openai.yaml') -Destination $skillAgentsDirectory -Force
    }
}

# The helper parses TOML and preserves unrelated settings, quoted keys and
# multiline strings. Never edit Codex TOML with line-oriented substitutions.
$configurationArguments = @(
    '--configure-codex', $configPath,
    '--mcp-command', $installedExecutable,
    '--mcp-config', $connectionConfigPath,
    '--mcp-mode', $CommandMode
)
if (-not $setupMode) { $configurationArguments += '--legacy-env' }
& $installedExecutable @configurationArguments | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'Codex MCP 配置未完成；请保留设置状态并重试。' }

Get-ChildItem -LiteralPath $installDirectory -Filter 'remoteops-controller-mcp-*.exe' -File |
    Where-Object FullName -NE $installedExecutable |
    ForEach-Object {
        $oldExecutable = $_.FullName
        try {
            Remove-Item -LiteralPath $oldExecutable -Force
        }
        catch {
            Write-Warning "旧版 MCP 正在使用，暂时无法删除：$oldExecutable。完全退出 Codex 后重新运行安装器即可清理。"
        }
    }
$legacyCertificate = Join-Path $installDirectory 'relay-cert.pem'
if (
    (Test-Path -LiteralPath $legacyCertificate -PathType Leaf) -and
    $legacyCertificate -ne $installedCaCert
) {
    try {
        Remove-Item -LiteralPath $legacyCertificate -Force
    }
    catch {
        Write-Warning "暂时无法删除已停用的旧证书副本：$legacyCertificate"
    }
}

Write-Host ''
Write-Host "RemoteOps MCP $mcpVersion 已安装。"
Write-Host "程序：$installedExecutable"
Write-Host "配置：$configPath"
Write-Host "Relay 配置：$connectionConfigPath"
Write-Host "RemoteOps skill（标准目录）：$standardSkillDirectory"
if ($compatSkillDirectory -ne $standardSkillDirectory) {
    Write-Host "RemoteOps skill（兼容目录）：$compatSkillDirectory"
}
Write-Host "命令模式：$CommandMode"
if ($setupMode) {
    Write-Host '独立 Controller 凭据已保存到 Windows Credential Manager；登记与 Relay 身份自检通过。'
}
else {
    Write-Host '统一 Controller Owner 已保存到当前 Windows 用户环境变量（不会显示其值）。'
}
Write-Host '请完全退出并重新打开 Codex，然后输入 /mcp 检查 remoteops。'
Write-Host '安装脚本未输出、未写入 config.toml，也未打包 Controller Token。'
