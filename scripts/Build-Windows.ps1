[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [switch]$SkipClean
)

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
        'artifacts\release\{0}\windows-x64' -f $versionMatch.Groups['version'].Value
    )
}
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $resolvedOutput -Force | Out-Null

Push-Location $workspaceRoot
try {
    if (-not $SkipClean) {
        cargo clean --release --locked
        if ($LASTEXITCODE -ne 0) {
            throw '清理 Windows Release 构建缓存失败'
        }
    }

    cargo build --release --locked `
        -p remoteops-agent `
        -p remoteops-agent-service `
        -p remoteops-agent-gui `
        -p remoteops-ssh-askpass `
        -p remoteops-controller-cli `
        -p remoteops-controller-mcp `
        -p remoteops-controller-gui `
        -p remoteops-serial-demo `
        -p remoteops-mcp-smoke
    if ($LASTEXITCODE -ne 0) {
        throw 'Windows Release 构建失败'
    }

    $releasePackages = @(
        'remoteops-agent',
        'remoteops-agent-service',
        'remoteops-agent-gui',
        'remoteops-ssh-askpass',
        'remoteops-controller-cli',
        'remoteops-controller-mcp',
        'remoteops-controller-gui',
        'remoteops-serial-demo',
        'remoteops-mcp-smoke'
    )
    $packageVersions = @{}
    foreach ($package in $releasePackages) {
        $packageId = (& cargo pkgid --locked -p $package 2>&1 | Out-String).Trim()
        if ($LASTEXITCODE -ne 0) {
            throw "读取 Cargo 包版本失败：$package"
        }
        $packageVersionMatch = [regex]::Match(
            $packageId,
            '#(?:[^@#]+@)?(?<version>[^#\s]+)$'
        )
        if (-not $packageVersionMatch.Success) {
            throw "无法从 Cargo 包标识读取版本：$package"
        }
        $packageVersions[$package] = $packageVersionMatch.Groups['version'].Value
    }

    $binaries = @(
        @{ File = 'remoteops-agent.exe'; Package = 'remoteops-agent' },
        @{ File = 'remoteops-agent-service.exe'; Package = 'remoteops-agent-service' },
        @{ File = 'remoteops-agent-gui.exe'; Package = 'remoteops-agent-gui' },
        @{ File = 'remoteops-ssh-askpass.exe'; Package = 'remoteops-ssh-askpass' },
        @{ File = 'remoteops-controller-cli.exe'; Package = 'remoteops-controller-cli' },
        @{ File = 'remoteops-controller-mcp.exe'; Package = 'remoteops-controller-mcp' },
        @{ File = 'remoteops-controller-gui.exe'; Package = 'remoteops-controller-gui' },
        @{ File = 'remoteops-serial-demo.exe'; Package = 'remoteops-serial-demo' },
        @{ File = 'remoteops-mcp-smoke.exe'; Package = 'remoteops-mcp-smoke' }
    )
    $manifest = foreach ($binary in $binaries) {
        $releaseVersion = $packageVersions[$binary.Package]
        if ([string]::IsNullOrWhiteSpace($releaseVersion)) {
            throw "未找到交付程序对应的 Cargo 包版本：$($binary.Package)"
        }
        $source = Join-Path $workspaceRoot "target\release\$($binary.File)"
        $target = Join-Path $resolvedOutput $binary.File
        Copy-Item -LiteralPath $source -Destination $target -Force
        $item = Get-Item -LiteralPath $target
        $hash = Get-FileHash -LiteralPath $target -Algorithm SHA256
        $fileVersion = $item.VersionInfo.FileVersion
        $productVersion = $item.VersionInfo.ProductVersion
        $baseVersion = ($releaseVersion -split '-', 2)[0]
        $versionPattern = '^(?:{0}|{1}(?:\.0)?)$' -f `
            [regex]::Escape($releaseVersion),
            [regex]::Escape($baseVersion)
        if (
            $fileVersion -notmatch $versionPattern -or
            $productVersion -notmatch $versionPattern
        ) {
            throw (
                "Windows 文件版本不正确：$binary；Cargo=$releaseVersion；" +
                "FileVersion=$fileVersion；ProductVersion=$productVersion"
            )
        }
        [PSCustomObject]@{
            File = $item.Name
            Package = $binary.Package
            CargoVersion = $releaseVersion
            FileVersion = $fileVersion
            ProductVersion = $productVersion
            Length = $item.Length
            Sha256 = $hash.Hash.ToLowerInvariant()
        }
    }

    $manifestJson = $manifest | ConvertTo-Json -Depth 4
    [IO.File]::WriteAllText(
        (Join-Path $resolvedOutput 'manifest.json'),
        $manifestJson + "`r`n",
        [Text.UTF8Encoding]::new($false)
    )
    & (Join-Path $PSScriptRoot 'New-ThirdPartyNotices.ps1') -OutputDirectory $resolvedOutput
    if ($LASTEXITCODE -ne 0) {
        throw '生成 Windows 发布许可证和依赖清单失败'
    }
    Copy-Item `
        -LiteralPath (Join-Path $PSScriptRoot 'Enable-AgentGuiCrashDumps.ps1') `
        -Destination (Join-Path $resolvedOutput 'Enable-AgentGuiCrashDumps.ps1') `
        -Force
    & (Join-Path $PSScriptRoot 'Test-ReleaseArtifacts.ps1') `
        -ArtifactRoot $resolvedOutput `
        -RequireLegalFiles
    if ($LASTEXITCODE -ne 0) {
        throw 'Windows 发布产物检查失败'
    }
    $manifest | Format-Table -AutoSize
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
