[CmdletBinding()]
param(
    [string]$CodexHome = (Join-Path $env:USERPROFILE '.codex'),
    [Parameter(Mandatory)]
    [string]$RelayAddress,
    [Parameter(Mandatory)]
    [ValidateScript({ $_ -ne [guid]::Empty })]
    [guid]$OwnerId,
    [string]$ServerName,
    [string]$CaCert,
    [string]$TlsFingerprint,
    [ValidateSet('readonly', 'approval', 'agent-controlled', 'full-access')]
    [string]$CommandMode = 'agent-controlled',
    [switch]$SkipTokenPrompt
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$packageVersion = '0.2.0-preview.4'
$tokenVariable = 'REMOTEOPS_CONTROLLER_TOKEN'
$ownerVariable = 'REMOTEOPS_CONTROLLER_OWNER_ID'
$sourceExecutable = Join-Path $PSScriptRoot 'remoteops-controller-mcp.exe'
$sourceSkill = Join-Path $PSScriptRoot 'skills\remoteops'
$defaultCodexHome = Join-Path $env:USERPROFILE '.codex'
$isDefaultCodexHome = [IO.Path]::GetFullPath($CodexHome).TrimEnd('\') -eq
    [IO.Path]::GetFullPath($defaultCodexHome).TrimEnd('\')

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

function ConvertTo-TomlBasicString {
    param([Parameter(Mandatory)][string]$Value)

    return '"' + $Value.Replace('\', '\\').Replace('"', '\"') + '"'
}

function Remove-RemoteOpsConfigSections {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Content)

    $result = [System.Collections.Generic.List[string]]::new()
    $skipSection = $false
    foreach ($line in [regex]::Split($Content, '\r?\n')) {
        if ($line -match '^\s*\[(?<name>[^\]]+)\]\s*(?:#.*)?$') {
            $sectionName = $Matches.name.Trim()
            $skipSection = $sectionName -eq 'mcp_servers.remoteops' -or
                $sectionName.StartsWith('mcp_servers.remoteops.', [StringComparison]::Ordinal) -or
                $sectionName -eq 'mcp_servers."remoteops"' -or
                $sectionName.StartsWith('mcp_servers."remoteops".', [StringComparison]::Ordinal)
        }
        if (-not $skipSection) {
            $result.Add($line)
        }
    }
    while ($result.Count -gt 0 -and [string]::IsNullOrWhiteSpace($result[$result.Count - 1])) {
        $result.RemoveAt($result.Count - 1)
    }
    return $result
}

function Set-CodexGranularApprovalPolicy {
    param(
        [Parameter(Mandatory)]
        [AllowEmptyCollection()]
        [AllowEmptyString()]
        [string[]]$Lines
    )

    $result = [System.Collections.Generic.List[string]]::new()
    $skipLegacyGranular = $false
    $approvalPolicyWritten = $false
    foreach ($line in $Lines) {
        if ($line -match '^\s*\[(?<name>[^\]]+)\]\s*(?:#.*)?$') {
            $skipLegacyGranular = $Matches.name.Trim() -eq 'approval_policy.granular'
            if ($skipLegacyGranular) {
                continue
            }
        }
        if ($skipLegacyGranular) {
            continue
        }
        if ($line -match '^\s*approval_policy\s*=') {
            if (-not $approvalPolicyWritten) {
                $result.Add(
                    'approval_policy = { granular = { sandbox_approval = true, rules = true, ' +
                    'mcp_elicitations = true, request_permissions = false, skill_approval = false } }'
                )
                $approvalPolicyWritten = $true
            }
            continue
        }
        $result.Add($line)
    }
    if (-not $approvalPolicyWritten) {
        $insertAt = 0
        while ($insertAt -lt $result.Count -and [string]::IsNullOrWhiteSpace($result[$insertAt])) {
            $insertAt++
        }
        $result.Insert(
            $insertAt,
            'approval_policy = { granular = { sandbox_approval = true, rules = true, ' +
            'mcp_elicitations = true, request_permissions = false, skill_approval = false } }'
        )
    }
    return $result
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
if (-not (Test-Path -LiteralPath (Join-Path $sourceSkill 'SKILL.md') -PathType Leaf)) {
    throw "安装包缺少 RemoteOps skill：$sourceSkill"
}

$versionOutput = (& $sourceExecutable --version 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $versionOutput -notmatch [regex]::Escape($packageVersion)) {
    throw "MCP 可执行文件版本不正确：$versionOutput"
}

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

$installDirectory = Join-Path $CodexHome 'remoteops'
$installedExecutable = Join-Path $installDirectory "remoteops-controller-mcp-$packageVersion.exe"
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
try {
    Copy-Item -LiteralPath $sourceExecutable -Destination $installedExecutable -Force
}
catch {
    $sourceHash = (Get-FileHash -LiteralPath $sourceExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
    $installedExecutable = Join-Path $installDirectory (
        "remoteops-controller-mcp-$packageVersion-$($sourceHash.Substring(0, 12)).exe"
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

$originalContent = if (Test-Path -LiteralPath $configPath -PathType Leaf) {
    [IO.File]::ReadAllText($configPath)
}
else {
    ''
}
$newline = if ($originalContent.Contains("`r`n")) { "`r`n" } else { "`n" }
$retainedLines = Remove-RemoteOpsConfigSections -Content $originalContent
$retainedLines = Set-CodexGranularApprovalPolicy -Lines $retainedLines
$configLines = [System.Collections.Generic.List[string]]::new()
foreach ($line in $retainedLines) {
    $configLines.Add($line)
}
if ($configLines.Count -gt 0) {
    $configLines.Add('')
}
$configLines.Add('[mcp_servers.remoteops]')
$configLines.Add('command = ' + (ConvertTo-TomlBasicString -Value $installedExecutable))
$configLines.Add(
    'args = ["--config", ' +
    (ConvertTo-TomlBasicString -Value $connectionConfigPath) +
    ', "--command-mode", ' +
    (ConvertTo-TomlBasicString -Value $CommandMode) +
    ']'
)
$configLines.Add(
    'env_vars = ["REMOTEOPS_CONTROLLER_TOKEN", "REMOTEOPS_CONTROLLER_OWNER_ID"]'
)
$configLines.Add('startup_timeout_sec = 15')
$configLines.Add('tool_timeout_sec = 180')
$configLines.Add('enabled = true')
$configLines.Add('required = false')
$configLines.Add('default_tools_approval_mode = "approve"')
$configLines.Add('')
$configLines.Add('[mcp_servers.remoteops.tools.set_control_mode]')
$configLines.Add('approval_mode = "prompt"')
$newContent = ($configLines -join $newline) + $newline

if ($newContent -ne $originalContent) {
    if (Test-Path -LiteralPath $configPath -PathType Leaf) {
        $backupPath = "$configPath.remoteops-backup-$(Get-Date -Format 'yyyyMMdd-HHmmss-fff')"
        Copy-Item -LiteralPath $configPath -Destination $backupPath
        Write-Host "已备份原配置：$backupPath"
    }
    Write-Utf8NoBom -Path $configPath -Content $newContent
}

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
Write-Host "RemoteOps MCP $packageVersion 已安装。"
Write-Host "程序：$installedExecutable"
Write-Host "配置：$configPath"
Write-Host "Relay 配置：$connectionConfigPath"
Write-Host "RemoteOps skill（标准目录）：$standardSkillDirectory"
if ($compatSkillDirectory -ne $standardSkillDirectory) {
    Write-Host "RemoteOps skill（兼容目录）：$compatSkillDirectory"
}
Write-Host "命令模式：$CommandMode"
Write-Host '统一 Controller Owner 已保存到当前 Windows 用户环境变量（不会显示其值）。'
Write-Host '请完全退出并重新打开 Codex，然后输入 /mcp 检查 remoteops。'
Write-Host '安装脚本未输出、未写入 config.toml，也未打包 Controller Token。'

