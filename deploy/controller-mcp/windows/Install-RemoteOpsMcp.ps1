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
    [switch]$ConfirmEnrollment,
    [switch]$UpgradeExisting
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
foreach ($manualParameter in @('RelayAddress', 'OwnerId', 'ServerName', 'CaCert', 'TlsFingerprint', 'SkipTokenPrompt')) {
    if ($PSBoundParameters.ContainsKey($manualParameter)) {
        throw '已停用 Token 手工安装；请使用 SetupFile、SetupCode 或 SetupStdin。'
    }
}
if ($UpgradeExisting -and ($setupMode -or $ConfirmEnrollment -or $PSBoundParameters.ContainsKey('CommandMode'))) {
    throw 'UpgradeExisting 不能与设置输入、ConfirmEnrollment 或 CommandMode 混用；升级保留原安装参数。'
}
if (-not $setupMode -and -not $UpgradeExisting) {
    throw '新安装必须提供 SetupFile、SetupCode 或 SetupStdin；已有安装请使用 UpgradeExisting。'
}

$mcpVersion = '0.2.0-preview.25'
$credentialPromptVersion = '0.2.0-preview.7'
$sourceExecutable = Join-Path $PSScriptRoot 'remoteops-controller-mcp.exe'
$sourceCredentialPrompt = Join-Path $PSScriptRoot 'remoteops-credential-prompt.exe'
# Use a managed worker for the complete transfer: .NET Framework's FlushAsync
# may synchronously flush its buffer before returning a Task. No PowerShell
# scriptblock runs on this worker, and setup material is never a process argument.
if (-not ('RemoteOps.SetupPipeTransfer' -as [type])) {
    Add-Type -TypeDefinition @'
using System.IO;
using System.Threading.Tasks;
namespace RemoteOps {
    public static class SetupPipeTransfer {
        public static Task SendAsync(Stream stream, byte[] bytes) {
            return Task.Run(() => {
                try {
                    stream.Write(bytes, 0, bytes.Length);
                    stream.Flush();
                }
                finally {
                    try { stream.Close(); }
                    finally { System.Array.Clear(bytes, 0, bytes.Length); }
                }
            });
        }
    }
}
'@
}

function Invoke-SetupHelper {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string[]]$Arguments,
        [Parameter(Mandatory)][AllowEmptyString()][string]$InputContent
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
    $processStarted = $false
    $inputBytes = $null
    $inputTransfer = $null
    try {
        # .NET Framework creates an AutoFlush StreamWriter during Start using
        # Console.InputEncoding, which can emit a BOM before BaseStream is used.
        # Capture a BOM-free encoding for this child only, then restore the host.
        $previousInputEncoding = [Console]::InputEncoding
        try {
            [Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
            $processStarted = $process.Start()
            if (-not $processStarted) { throw '无法启动一次性设置程序。' }
        }
        finally {
            [Console]::InputEncoding = $previousInputEncoding
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        # Write UTF-8 bytes directly for Windows PowerShell 5.1 as well as 7;
        # do not depend on the console code page or newer encoding properties.
        $inputBytes = [Text.Encoding]::UTF8.GetBytes($InputContent)
        # Bound the whole write/flush/close, including synchronous pipe calls.
        if ($inputBytes.Length -eq 0) {
            # 离线检查不读取输入，直接关闭管道，避免快速退出与占位写入竞争。
            $process.StandardInput.Close()
        }
        else {
            $inputTransfer = [RemoteOps.SetupPipeTransfer]::SendAsync($process.StandardInput.BaseStream, $inputBytes)
            if (-not $inputTransfer.Wait(10000)) {
                throw '设置输入传输超时；请保留设置文件并重试。'
            }
            $inputTransfer.GetAwaiter().GetResult()
        }
        # Preview only parses local input. Do not give a stuck preview the full
        # network-enrollment window, including Framework's silent pipe closure.
        $helperTimeoutMilliseconds = if ($Arguments -contains '--setup-preview') { 10000 } else { 180000 }
        if (-not $process.WaitForExit($helperTimeoutMilliseconds)) {
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
        # Dispose releases handles but does not stop a child. A broken stdin
        # pipe or another early failure must not leave enrollment running.
        try {
            if ($processStarted -and -not $process.HasExited) {
                $process.Kill()
                $process.WaitForExit()
            }
        }
        finally {
            # Kill first so any pending pipe write can finish before its buffer
            # is cleared. Never mutate bytes still owned by an async write.
            if ($null -ne $inputTransfer -and -not $inputTransfer.IsCompleted) {
                try { $null = $inputTransfer.Wait(5000) } catch { }
            }
            if ($null -ne $inputBytes -and ($null -eq $inputTransfer -or $inputTransfer.IsCompleted)) {
                [Array]::Clear($inputBytes, 0, $inputBytes.Length)
            }
            $process.Dispose()
        }
    }
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

$versionOutput = (& $sourceExecutable --version 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $versionOutput -notmatch ('^remoteops-controller-mcp\s+' + [regex]::Escape($mcpVersion) + '$')) {
    throw "MCP 可执行文件版本不正确：$versionOutput"
}
$promptVersionOutput = (& $sourceCredentialPrompt --version 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $promptVersionOutput -notmatch ('^remoteops-credential-prompt\s+' + [regex]::Escape($credentialPromptVersion) + '$')) {
    throw "SSH 密码安全输入程序版本不正确：$promptVersionOutput"
}

# 在登记、凭据或配置变更之前验证实际工具目录；此入口完全离线。
$schemaReport = (Invoke-SetupHelper -Executable $sourceExecutable `
    -Arguments @('--check-tool-schemas') -InputContent '') | ConvertFrom-Json
if ($schemaReport.passed -ne $true -or $schemaReport.tool_count -lt 1 -or @($schemaReport.failures).Count -ne 0) {
    throw 'MCP 工具 Schema 离线自检失败；尚未更改安装、配置或凭据。'
}

$configPath = Join-Path $CodexHome 'config.toml'
# Validate before changing any installation files or credentials.
& $sourceExecutable --validate-codex $configPath | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'Codex 配置校验失败；尚未更改安装或凭据。' }

$installDirectory = Join-Path $CodexHome 'remoteops'
$installedExecutable = Join-Path $installDirectory "remoteops-controller-mcp-$mcpVersion.exe"
$installedCredentialPrompt = Join-Path $installDirectory 'remoteops-credential-prompt.exe'
$connectionConfigPath = Join-Path $installDirectory 'controller-config.json'
if ($UpgradeExisting) {
    if (-not (Test-Path -LiteralPath $configPath -PathType Leaf) -or
        -not (Test-Path -LiteralPath $connectionConfigPath -PathType Leaf)) {
        throw '升级需要现有 Codex 配置和 controller-config.json；未更改安装或凭据。'
    }
    $connectionConfigHash = (Get-FileHash -LiteralPath $connectionConfigPath -Algorithm SHA256).Hash
    # JSON 路径可能包含中文，复用明确 UTF-8 的子进程读取，避免 PowerShell 5.1 OEM 解码。
    $inspection = (Invoke-SetupHelper -Executable $sourceExecutable `
        -Arguments @('--inspect-codex', $configPath) -InputContent '') | ConvertFrom-Json
    if ([string]::IsNullOrWhiteSpace($inspection.command) -or
        [string]::IsNullOrWhiteSpace($inspection.args_config) -or
        [IO.Path]::GetFullPath([string]$inspection.args_config) -ine [IO.Path]::GetFullPath($connectionConfigPath)) {
        throw '现有 RemoteOps 配置不属于此安装；未更改安装或凭据。'
    }
    $existingConnection = [IO.File]::ReadAllText($connectionConfigPath) | ConvertFrom-Json
    if ($null -ne $existingConnection.PSObject.Properties['credential_id'] -and
        -not [string]::IsNullOrWhiteSpace($existingConnection.credential_id)) {
        & $sourceExecutable --check-credential --config $connectionConfigPath | Out-Host
        if ($LASTEXITCODE -ne 0) { throw '现有安装凭据不可用；未进行登记或更换凭据。' }
    }
    $sourceHash = (Get-FileHash -LiteralPath $sourceExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
    $candidateRoot = Join-Path $installDirectory 'candidates'
    $packageFiles = @('remoteops-controller-mcp.exe', 'remoteops-credential-prompt.exe',
        'Install-RemoteOpsMcp.ps1', 'Test-RemoteOpsMcp.ps1', 'README.md', 'LICENSE',
        'THIRD_PARTY_NOTICES.md', 'THIRD_PARTY_LICENSES.txt', 'DEPENDENCIES.json', 'manifest.json', 'SHA256SUMS.txt')
    # 同一程序配套文档或脚本变化也生成独立目录，避免覆盖当前宿主文件。
    $packageEntries = foreach ($name in $packageFiles | Sort-Object) {
        $source = Join-Path $PSScriptRoot $name
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            $name + ':' + (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    $packageHasher = [Security.Cryptography.SHA256]::Create()
    try {
        $packageHash = [BitConverter]::ToString($packageHasher.ComputeHash(
            [Text.Encoding]::UTF8.GetBytes(($packageEntries -join "`n"))
        )).Replace('-', '').ToLowerInvariant()
    }
    finally { $packageHasher.Dispose() }
    $candidateDirectory = Join-Path $candidateRoot (
        "protocol20-preview25-" + $sourceHash.Substring(0, 12) + '-' + $packageHash.Substring(0, 12)
    )
    foreach ($directory in @($installDirectory, $candidateRoot, $candidateDirectory)) {
        if ((Test-Path -LiteralPath $directory) -and
            ((Get-Item -LiteralPath $directory -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw '升级目录不允许链接或重解析点。'
        }
    }
    # 完整配套并排安装，当前宿主的程序及 helper 均不覆盖、不清理。
    New-Item -ItemType Directory -Force -Path $candidateDirectory | Out-Null
    foreach ($name in $packageFiles) {
        $source = Join-Path $PSScriptRoot $name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { continue }
        $destination = Join-Path $candidateDirectory $name
        for ($parent = Split-Path -Parent $destination; $parent.Length -ge $candidateDirectory.Length; $parent = Split-Path -Parent $parent) {
            if ((Test-Path -LiteralPath $parent) -and
                ((Get-Item -LiteralPath $parent -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw '升级目录不允许链接或重解析点。'
            }
        }
        if (Test-Path -LiteralPath $destination) {
            if (((Get-Item -LiteralPath $destination -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -or
                (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash -ne
                (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash) {
                throw '并排升级目录已有不同内容；保留原目录，未切换配置。'
            }
        }
        else {
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
            Copy-Item -LiteralPath $source -Destination $destination
        }
    }
    $installedExecutable = Join-Path $candidateDirectory 'remoteops-controller-mcp.exe'
    & $installedExecutable --update-codex-command $configPath --mcp-command $installedExecutable | Out-Host
    if ($LASTEXITCODE -ne 0) { throw '升级配置未完成；原安装与凭据保留。' }
    if ((Get-FileHash -LiteralPath $connectionConfigPath -Algorithm SHA256).Hash -cne $connectionConfigHash) {
        throw '升级期间连接配置发生并发变化，请检查配置；本次未登记或写入凭据。'
    }
    Write-Host "RemoteOps MCP $mcpVersion 配置已更新，现有身份、凭据、命令模式和超时保持。"
    Write-Host "程序：$installedExecutable"
    Write-Host '当前 Codex/pi 进程未中断；完全重开 Codex，pi 重开或 /reload 后生效。'
    Write-Host 'Skill 独立安装和更新，本次未修改任何 Skill。'
    Write-Host '此安装器仅更新 Codex；pi 共用程序时由维护者单独结构化更新 pi 配置。'
    return
}

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

$setupInput = $null
try {
    if (-not [string]::IsNullOrWhiteSpace($SetupFile)) {
        $setupInput = [IO.File]::ReadAllText((Resolve-Path -LiteralPath $SetupFile).Path)
    }
    elseif ($SetupStdin) {
        # Read redirected setup as UTF-8 independently of the Windows OEM
        # console code page. The source is consumed only once.
        $setupReader = [IO.StreamReader]::new(
            [Console]::OpenStandardInput(), [Text.UTF8Encoding]::new($false, $true), $true
        )
        try { $setupInput = $setupReader.ReadToEnd() }
        finally { $setupReader.Dispose() }
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

# The helper parses TOML and preserves unrelated settings, quoted keys and
# multiline strings. Never edit Codex TOML with line-oriented substitutions.
$configurationArguments = @(
    '--configure-codex', $configPath,
    '--mcp-command', $installedExecutable,
    '--mcp-config', $connectionConfigPath,
    '--mcp-mode', $CommandMode
)
& $installedExecutable @configurationArguments | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'Codex MCP 配置未完成；请保留设置状态并重试。' }

# This enumeration is already scoped to the installation directory. Compare
# leaf names: FullName may expand a short (8.3) path or remove dot segments while
# installedExecutable retains that equivalent spelling, deleting the active exe.
$installedExecutableName = [IO.Path]::GetFileName($installedExecutable)
Get-ChildItem -LiteralPath $installDirectory -Filter 'remoteops-controller-mcp-*.exe' -File |
    Where-Object Name -NE $installedExecutableName |
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
if (Test-Path -LiteralPath $legacyCertificate -PathType Leaf) {
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
Write-Host 'Skill 独立安装和更新；本安装器不读取、覆盖或删除已安装的 Skill。'
Write-Host "命令模式：$CommandMode"
Write-Host '独立 Controller 凭据已保存到 Windows Credential Manager；登记与 Relay 身份自检通过。'
Write-Host '请完全退出并重新打开 Codex，然后输入 /mcp 检查 remoteops。'
Write-Host '安装脚本未输出、未写入 config.toml，也未打包 Controller Token。'
