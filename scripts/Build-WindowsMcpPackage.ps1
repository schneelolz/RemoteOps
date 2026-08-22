[CmdletBinding()]
param(
    [string]$OutputRoot,
    [string]$ArtifactDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspaceRoot = Split-Path -Parent $PSScriptRoot
$userProfile = [Environment]::GetFolderPath('UserProfile')
$cargoHome = if ([string]::IsNullOrWhiteSpace($env:CARGO_HOME)) {
    Join-Path $userProfile '.cargo'
}
else {
    [IO.Path]::GetFullPath($env:CARGO_HOME)
}
$previousEncodedRustFlags = $env:CARGO_ENCODED_RUSTFLAGS
$rustFlagSeparator = [char]0x1F
$releaseRustFlags = [string]::Join($rustFlagSeparator, @(
    '-Ctarget-feature=+crt-static'
    '--remap-path-prefix={0}=/remoteops' -f [IO.Path]::GetFullPath($workspaceRoot)
    '--remap-path-prefix={0}=/cargo' -f $cargoHome
    '--remap-path-prefix={0}=/user' -f $userProfile
))
$env:CARGO_ENCODED_RUSTFLAGS = if ([string]::IsNullOrWhiteSpace($previousEncodedRustFlags)) {
    $releaseRustFlags
}
else {
    "$previousEncodedRustFlags$rustFlagSeparator$releaseRustFlags"
}
$previousCFlags = $env:CFLAGS
$previousCxxFlags = $env:CXXFLAGS
$releaseNativeFlags = @(
    '/pathmap:{0}=/remoteops' -f [IO.Path]::GetFullPath($workspaceRoot)
    '/pathmap:{0}=/cargo' -f $cargoHome
    '/pathmap:{0}=/user' -f $userProfile
) -join ' '
$env:CFLAGS = [string]::Join(' ', @(
    @($previousCFlags, $releaseNativeFlags) |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
))
$env:CXXFLAGS = [string]::Join(' ', @(
    @($previousCxxFlags, $releaseNativeFlags) |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
))
$packageId = (& cargo pkgid `
    --manifest-path (Join-Path $workspaceRoot 'Cargo.toml') `
    --locked `
    -p remoteops-controller-mcp 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0) {
    throw '读取 remoteops-controller-mcp 版本失败。'
}
$versionMatch = [regex]::Match(
    $packageId,
    '#(?:[^@#]+@)?(?<version>[^#\s]+)$'
)
if (-not $versionMatch.Success) {
    throw '无法确定 remoteops-controller-mcp 版本。'
}
$version = $versionMatch.Groups['version'].Value
if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $workspaceRoot "artifacts\release\$version\mcp"
}
if ([string]::IsNullOrWhiteSpace($ArtifactDirectory)) {
    $ArtifactDirectory = Join-Path $workspaceRoot "artifacts\release\$version\windows-x64"
}
$packageName = "RemoteOps-MCP-Windows-x64-$version"
$resolvedOutputRoot = [IO.Path]::GetFullPath($OutputRoot)
$packageDirectory = [IO.Path]::GetFullPath((Join-Path $resolvedOutputRoot $packageName))
$archivePath = Join-Path $resolvedOutputRoot "$packageName.zip"
$templateDirectory = Join-Path $workspaceRoot 'deploy\controller-mcp\windows'
$releaseExecutable = Join-Path $workspaceRoot 'target\release\remoteops-controller-mcp.exe'
$resolvedArtifactDirectory = [IO.Path]::GetFullPath($ArtifactDirectory)
$artifactExecutable = Join-Path $resolvedArtifactDirectory 'remoteops-controller-mcp.exe'
$manifestPath = Join-Path $resolvedArtifactDirectory 'manifest.json'

if (-not $packageDirectory.StartsWith(
        $resolvedOutputRoot + [IO.Path]::DirectorySeparatorChar,
        [StringComparison]::OrdinalIgnoreCase
    )) {
    throw 'MCP 安装包目录必须位于指定输出目录内。'
}

Push-Location $workspaceRoot
try {
    cargo build --release --locked -p remoteops-controller-mcp
    if ($LASTEXITCODE -ne 0) {
        throw 'remoteops-controller-mcp Release 构建失败。'
    }
}
finally {
    Pop-Location
    if ($null -eq $previousEncodedRustFlags) {
        Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    }
    else {
        $env:CARGO_ENCODED_RUSTFLAGS = $previousEncodedRustFlags
    }
    if ($null -eq $previousCFlags) {
        Remove-Item Env:CFLAGS -ErrorAction SilentlyContinue
    }
    else {
        $env:CFLAGS = $previousCFlags
    }
    if ($null -eq $previousCxxFlags) {
        Remove-Item Env:CXXFLAGS -ErrorAction SilentlyContinue
    }
    else {
        $env:CXXFLAGS = $previousCxxFlags
    }
}

if (Test-Path -LiteralPath $packageDirectory) {
    Remove-Item -LiteralPath $packageDirectory -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $packageDirectory | Out-Null
New-Item -ItemType Directory -Force -Path $resolvedArtifactDirectory | Out-Null
Copy-Item -LiteralPath $releaseExecutable -Destination $artifactExecutable -Force
Copy-Item -LiteralPath $releaseExecutable -Destination (Join-Path $packageDirectory 'remoteops-controller-mcp.exe')
Copy-Item -LiteralPath (Join-Path $templateDirectory 'README.md') -Destination $packageDirectory
Copy-Item -LiteralPath (Join-Path $templateDirectory 'Install-RemoteOpsMcp.ps1') -Destination $packageDirectory
Copy-Item -LiteralPath (Join-Path $templateDirectory 'Test-RemoteOpsMcp.ps1') -Destination $packageDirectory
& (Join-Path $PSScriptRoot 'New-ThirdPartyNotices.ps1') -OutputDirectory $packageDirectory
if ($LASTEXITCODE -ne 0) {
    throw '生成 MCP 安装包许可证和依赖清单失败。'
}
$skillSource = Join-Path $templateDirectory 'skills\remoteops'
$skillDestination = Join-Path $packageDirectory 'skills\remoteops'
if (-not (Test-Path -LiteralPath (Join-Path $skillSource 'SKILL.md') -PathType Leaf)) {
    throw "MCP 安装模板缺少 RemoteOps skill：$skillSource"
}
New-Item -ItemType Directory -Force -Path $skillDestination | Out-Null
Copy-Item -LiteralPath (Join-Path $skillSource 'SKILL.md') -Destination $skillDestination
if (Test-Path -LiteralPath (Join-Path $skillSource 'agents') -PathType Container) {
    Copy-Item -LiteralPath (Join-Path $skillSource 'agents') -Destination $skillDestination -Recurse
}

$actualVersion = (& (Join-Path $packageDirectory 'remoteops-controller-mcp.exe') --version 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or $actualVersion -notmatch [regex]::Escape($version)) {
    throw "打包程序版本不正确：$actualVersion"
}

if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $manifestEntry = $manifest | Where-Object { $_.File -eq 'remoteops-controller-mcp.exe' }
    if ($null -eq $manifestEntry) {
        throw 'Windows 发布清单缺少 remoteops-controller-mcp.exe。'
    }
    $artifactItem = Get-Item -LiteralPath $artifactExecutable
    $manifestEntry.CargoVersion = $version
    $manifestEntry.FileVersion = $artifactItem.VersionInfo.FileVersion
    $manifestEntry.ProductVersion = $artifactItem.VersionInfo.ProductVersion
    $manifestEntry.Length = $artifactItem.Length
    $manifestEntry.Sha256 = (Get-FileHash -LiteralPath $artifactExecutable -Algorithm SHA256).Hash.ToLowerInvariant()
    $manifestJson = $manifest | ConvertTo-Json -Depth 5
    [IO.File]::WriteAllText($manifestPath, $manifestJson + "`r`n", [Text.UTF8Encoding]::new($false))
}

if (Test-Path -LiteralPath $archivePath) {
    Remove-Item -LiteralPath $archivePath -Force
}
& (Join-Path $PSScriptRoot 'Test-ReleaseArtifacts.ps1') `
    -ArtifactRoot $packageDirectory `
    -RequireLegalFiles
if ($LASTEXITCODE -ne 0) {
    throw 'MCP 安装包内容检查失败。'
}
Compress-Archive -Path (Join-Path $packageDirectory '*') -DestinationPath $archivePath -CompressionLevel Optimal

& (Join-Path $PSScriptRoot 'Test-ReleaseArtifacts.ps1') `
    -ArtifactRoot $packageDirectory `
    -RequireLegalFiles `
    -ReferenceMcpExecutable $artifactExecutable `
    -McpArchive $archivePath
if ($LASTEXITCODE -ne 0) {
    throw 'MCP 安装包与 Windows 发布目录一致性检查失败。'
}

$hash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host "安装包：$archivePath"
Write-Host "SHA256：$hash"

