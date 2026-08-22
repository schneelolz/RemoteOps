[CmdletBinding()]
param(
    [string]$OutputRoot,
    [switch]$SkipLinux,
    [switch]$CleanLegacyArtifacts
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspaceRoot = Split-Path -Parent $PSScriptRoot
$artifactsRoot = Join-Path $workspaceRoot 'artifacts'

Push-Location $workspaceRoot
try {
    $packageId = (& cargo pkgid --locked -p remoteops-agent 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw '读取 remoteops-agent 版本失败。'
    }
}
finally {
    Pop-Location
}

$versionMatch = [regex]::Match(
    $packageId,
    '#(?:[^@#]+@)?(?<version>[^#\s]+)$'
)
if (-not $versionMatch.Success) {
    throw '无法确定 RemoteOps 正式发布版本。'
}
$releaseVersion = $versionMatch.Groups['version'].Value

if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $artifactsRoot "release\$releaseVersion"
}
$resolvedArtifactsRoot = [IO.Path]::GetFullPath($artifactsRoot)
$resolvedReleaseRoot = [IO.Path]::GetFullPath($OutputRoot)
if (-not $resolvedReleaseRoot.StartsWith(
        $resolvedArtifactsRoot + [IO.Path]::DirectorySeparatorChar,
        [StringComparison]::OrdinalIgnoreCase
    )) {
    throw '统一发布目录必须位于 artifacts 下。'
}

if (Test-Path -LiteralPath $resolvedReleaseRoot) {
    Remove-Item -LiteralPath $resolvedReleaseRoot -Recurse -Force
}

$windowsOutput = Join-Path $resolvedReleaseRoot 'windows-x64'
$linuxOutput = Join-Path $resolvedReleaseRoot 'linux-x64'
$mcpOutput = Join-Path $resolvedReleaseRoot 'mcp'
New-Item -ItemType Directory -Force -Path $resolvedReleaseRoot | Out-Null

& (Join-Path $PSScriptRoot 'Build-Windows.ps1') `
    -OutputDirectory $windowsOutput
if ($LASTEXITCODE -ne 0) {
    throw 'Windows 发布构建失败。'
}

& (Join-Path $PSScriptRoot 'Build-WindowsMcpPackage.ps1') `
    -OutputRoot $mcpOutput `
    -ArtifactDirectory $windowsOutput
if ($LASTEXITCODE -ne 0) {
    throw 'MCP 安装包构建失败。'
}

if (-not $SkipLinux) {
    & (Join-Path $PSScriptRoot 'Build-LinuxRelay.ps1') `
        -OutputDirectory $linuxOutput
    if ($LASTEXITCODE -ne 0) {
        throw 'Linux Relay 发布构建失败。'
    }
}

$readme = @'
# RemoteOps {0} 发布产物

- `windows-x64`：Windows Agent、Controller、MCP、Service、GUI 与测试工具。
- `linux-x64`：Linux x64 Relay 和构建清单。
- `mcp`：本机生成的 Codex MCP Windows x64 安装 ZIP；Apple Silicon macOS MCP 由 GitHub macOS runner 生成。
- `SHA256SUMS.txt`：本目录全部发布文件的 SHA-256。

`target` 是 Cargo 构建缓存，不属于发布产物。Token、证书私钥、控制码、状态文件和审计日志不得进入本目录。
'@
$readme = $readme -f $releaseVersion
[IO.File]::WriteAllText(
    (Join-Path $resolvedReleaseRoot 'README.md'),
    $readme.Trim() + "`r`n",
    [Text.UTF8Encoding]::new($false)
)

& (Join-Path $PSScriptRoot 'Test-ReleaseArtifacts.ps1') `
    -ArtifactRoot $resolvedReleaseRoot
if ($LASTEXITCODE -ne 0) {
    throw '发布产物路径脱敏检查失败。'
}

$checksumLines = Get-ChildItem -LiteralPath $resolvedReleaseRoot -Recurse -File |
    Where-Object Name -NE 'SHA256SUMS.txt' |
    Sort-Object FullName |
    ForEach-Object {
        $relativePath = $_.FullName.Substring($resolvedReleaseRoot.Length)
        $relativePath = $relativePath.TrimStart([char[]]@('\', '/')).Replace('\', '/')
        $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $relativePath"
    }
[IO.File]::WriteAllLines(
    (Join-Path $resolvedReleaseRoot 'SHA256SUMS.txt'),
    $checksumLines,
    [Text.UTF8Encoding]::new($false)
)

if ($CleanLegacyArtifacts) {
    foreach ($legacyName in @('windows-x64', 'linux-x64', 'packages')) {
        $legacyPath = [IO.Path]::GetFullPath((Join-Path $resolvedArtifactsRoot $legacyName))
        if (
            $legacyPath.StartsWith(
                $resolvedArtifactsRoot + [IO.Path]::DirectorySeparatorChar,
                [StringComparison]::OrdinalIgnoreCase
            ) -and
            -not $legacyPath.StartsWith(
                $resolvedReleaseRoot + [IO.Path]::DirectorySeparatorChar,
                [StringComparison]::OrdinalIgnoreCase
            ) -and
            (Test-Path -LiteralPath $legacyPath)
        ) {
            Remove-Item -LiteralPath $legacyPath -Recurse -Force
        }
    }
}

Write-Host "统一发布目录：$resolvedReleaseRoot"
Write-Host "正式版本：$releaseVersion"
Write-Host "SHA256 清单：$(Join-Path $resolvedReleaseRoot 'SHA256SUMS.txt')"
