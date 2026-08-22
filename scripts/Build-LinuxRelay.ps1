[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [string]$Target = 'x86_64-unknown-linux-gnu.2.36',
    [string]$ZigPath = $env:REMOTEOPS_ZIG,
    [switch]$SkipClean
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

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
    '-ffile-prefix-map={0}=/remoteops' -f [IO.Path]::GetFullPath($workspaceRoot)
    '-fdebug-prefix-map={0}=/remoteops' -f [IO.Path]::GetFullPath($workspaceRoot)
    '-ffile-prefix-map={0}=/cargo' -f $cargoHome
    '-fdebug-prefix-map={0}=/cargo' -f $cargoHome
    '-ffile-prefix-map={0}=/user' -f $userProfile
    '-fdebug-prefix-map={0}=/user' -f $userProfile
) -join ' '
$env:CFLAGS = [string]::Join(' ', @(
    @($previousCFlags, $releaseNativeFlags) |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
))
$env:CXXFLAGS = [string]::Join(' ', @(
    @($previousCxxFlags, $releaseNativeFlags) |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
))
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $manifestText = Get-Content -LiteralPath (Join-Path $workspaceRoot 'Cargo.toml') -Raw
    $versionMatch = [regex]::Match(
        $manifestText,
        '(?ms)^\[workspace\.package\]\s*.*?^version\s*=\s*"(?<version>[^"]+)"'
    )
    if (-not $versionMatch.Success) {
        throw '无法从 Cargo.toml 读取 Workspace 版本。'
    }
    $OutputDirectory = Join-Path $workspaceRoot (
        'artifacts\release\{0}\linux-x64' -f $versionMatch.Groups['version'].Value
    )
}
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
$rustTarget = $Target -replace '\.[0-9]+\.[0-9]+$', ''

if (-not (Get-Command cargo-zigbuild.exe -ErrorAction SilentlyContinue)) {
    throw '缺少 cargo-zigbuild。请先执行：cargo install cargo-zigbuild --locked'
}

if (-not $ZigPath) {
    $zigCommand = Get-Command zig.exe -ErrorAction SilentlyContinue
    if ($zigCommand) {
        $ZigPath = $zigCommand.Source
    }
}
if (-not $ZigPath) {
    $candidate = Join-Path `
        $env:APPDATA `
        'Python\Python312\site-packages\ziglang\zig.exe'
    if (Test-Path -LiteralPath $candidate) {
        $ZigPath = $candidate
    }
}
if (-not $ZigPath -or -not (Test-Path -LiteralPath $ZigPath)) {
    throw (
        '缺少 Zig 编译器。可设置 REMOTEOPS_ZIG，或使用 Python 安装 ziglang。'
    )
}

$env:PATH = (Split-Path -Parent $ZigPath) + [IO.Path]::PathSeparator + $env:PATH
New-Item -ItemType Directory -Path $resolvedOutput -Force | Out-Null

Push-Location $workspaceRoot
try {
    & rustup.exe target add $rustTarget
    if ($LASTEXITCODE -ne 0) {
        throw "安装 Rust Linux 目标失败：$rustTarget"
    }

    if (-not $SkipClean) {
        & cargo.exe clean --release --locked --target $rustTarget
        if ($LASTEXITCODE -ne 0) {
            throw "清理 Linux Relay Release 构建缓存失败：$rustTarget"
        }
    }

    & cargo.exe zigbuild `
        --release `
        --locked `
        -p remoteops-relay `
        --target $Target
    if ($LASTEXITCODE -ne 0) {
        throw 'Linux Relay 交叉编译失败'
    }

    $packageId = (& cargo.exe pkgid --locked -p remoteops-relay 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw '读取 remoteops-relay 版本失败'
    }
    $packageVersionMatch = [regex]::Match(
        $packageId,
        '#(?:[^@#]+@)?(?<version>[^#\s]+)$'
    )
    if (-not $packageVersionMatch.Success) {
        throw '无法确定 remoteops-relay 版本'
    }
    $releaseVersion = $packageVersionMatch.Groups['version'].Value

    $source = Join-Path `
        $workspaceRoot `
        "target\$rustTarget\release\remoteops-relay"
    if (-not (Test-Path -LiteralPath $source)) {
        throw "Linux Relay 构建产物不存在：$source"
    }
    $targetPath = Join-Path $resolvedOutput 'remoteops-relay'
    Copy-Item -LiteralPath $source -Destination $targetPath -Force
    $item = Get-Item -LiteralPath $targetPath
    $hash = Get-FileHash -LiteralPath $targetPath -Algorithm SHA256
    $manifest = [PSCustomObject]@{
        File = $item.Name
        CargoVersion = $releaseVersion
        Target = $Target
        Length = $item.Length
        Sha256 = $hash.Hash.ToLowerInvariant()
    }
    $manifestJson = $manifest | ConvertTo-Json
    [IO.File]::WriteAllText(
        (Join-Path $resolvedOutput 'manifest.json'),
        $manifestJson + "`r`n",
        [Text.UTF8Encoding]::new($false)
    )
    & (Join-Path $PSScriptRoot 'New-ThirdPartyNotices.ps1') -OutputDirectory $resolvedOutput
    if ($LASTEXITCODE -ne 0) {
        throw '生成 Linux Relay 许可证和依赖清单失败'
    }
    & (Join-Path $PSScriptRoot 'Test-ReleaseArtifacts.ps1') `
        -ArtifactRoot $resolvedOutput `
        -RequireLegalFiles
    if ($LASTEXITCODE -ne 0) {
        throw 'Linux Relay 发布产物检查失败'
    }
    $manifest | Format-List
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
