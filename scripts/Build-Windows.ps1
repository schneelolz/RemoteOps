[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [switch]$SkipClean,
    [string]$PlinkSourcePath = 'D:\Tools\putty\PLINK.EXE',
    [switch]$IncludeNativePreview
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
$encodedReleaseFlags = if ([string]::IsNullOrWhiteSpace($previousEncodedRustFlags)) {
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
$combinedCFlags = [string]::Join(' ', @(
    @($previousCFlags, $releaseNativeFlags) |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
))
$combinedCxxFlags = [string]::Join(' ', @(
    @($previousCxxFlags, $releaseNativeFlags) |
        Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
))
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    Push-Location $workspaceRoot
    try {
        $packageId = (& cargo pkgid --locked -p remoteops-agent 2>&1 | Out-String).Trim()
        if ($LASTEXITCODE -ne 0) {
            throw '无法读取 remoteops-agent 发布版本。'
        }
    }
    finally {
        Pop-Location
    }
    $versionMatch = [regex]::Match($packageId, '#(?:[^@#]+@)?(?<version>[^#\s]+)$')
    if (-not $versionMatch.Success) {
        throw '无法从 Cargo 包标识读取 remoteops-agent 发布版本。'
    }
    $OutputDirectory = Join-Path $workspaceRoot (
        'artifacts\release\{0}\windows-x64' -f $versionMatch.Groups['version'].Value
    )
}
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputDirectory)
# 不把新版随包组件写入已有交付；需由调用者选择一个新的输出目录。
if (Test-Path -LiteralPath $resolvedOutput) {
    $existingOutput = Get-Item -LiteralPath $resolvedOutput -Force
    if (-not $existingOutput.PSIsContainer -or
        ($existingOutput.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        @(Get-ChildItem -LiteralPath $resolvedOutput -Force).Count -gt 0) {
        throw '输出目录必须是不存在或为空的普通目录；拒绝覆盖旧版交付。'
    }
}
for ($ancestor = Split-Path -Parent $resolvedOutput; -not [string]::IsNullOrEmpty($ancestor); $ancestor = Split-Path -Parent $ancestor) {
    if ((Test-Path -LiteralPath $ancestor) -and
        ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw '输出目录的父路径含重解析点，拒绝写入。'
    }
}
$resolvedPlink = (Resolve-Path -LiteralPath $PlinkSourcePath).Path
if ((Get-FileHash -LiteralPath $resolvedPlink -Algorithm SHA256).Hash -ine
    '50479953865b30775056441b10fdcb984126ba4f98af4f64756902a807b453e7') {
    throw 'PlinkSourcePath 必须是已核验的官方 x64 Plink 0.83；不自动下载或升级。'
}
New-Item -ItemType Directory -Path $resolvedOutput -Force | Out-Null

Push-Location $workspaceRoot
try {
    $env:CARGO_ENCODED_RUSTFLAGS = $encodedReleaseFlags
    $env:CFLAGS = $combinedCFlags
    $env:CXXFLAGS = $combinedCxxFlags
    $metadataJson = (& cargo metadata --locked --no-deps --format-version 1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw '读取 Cargo 构建目录失败'
    }
    $cargoMetadata = $metadataJson | ConvertFrom-Json
    $cargoTargetDirectory = [string]$cargoMetadata.target_directory
    if ([string]::IsNullOrWhiteSpace($cargoTargetDirectory)) {
        throw 'Cargo metadata 缺少实际构建目录'
    }

    if (-not $SkipClean) {
        cargo clean --release --locked
        if ($LASTEXITCODE -ne 0) {
            throw '清理 Windows Release 构建缓存失败'
        }
        cargo clean --profile release-small --locked
        if ($LASTEXITCODE -ne 0) {
            throw '清理 Windows Agent GUI 体积优化构建缓存失败'
        }
    }

    cargo build --release --locked `
        -p remoteops-agent `
        -p remoteops-agent-service `
        -p remoteops-ssh-askpass `
        -p remoteops-controller-cli `
        -p remoteops-controller-mcp `
        -p remoteops-credential-prompt `
        -p remoteops-controller-gui `
        -p remoteops-serial-demo `
        -p remoteops-mcp-smoke
    if ($LASTEXITCODE -ne 0) {
        throw 'Windows Release 构建失败'
    }

    # 仅现场 GUI 使用体积优化配置，其他发布程序保持原有性能配置。
    cargo build --profile release-small --locked -p remoteops-agent-gui
    if ($LASTEXITCODE -ne 0) {
        throw 'Windows Agent GUI 体积优化构建失败'
    }
    # 原生预览独立子目录且显式启用，旧 GUI 的文件名与默认发布选择保持兼容。
    if ($IncludeNativePreview) {
        cargo build --profile release-small --locked -p remoteops-agent-native-prototype
        if ($LASTEXITCODE -ne 0) { throw 'Windows 原生预览构建失败' }
    }

    $releasePackages = @(
        'remoteops-agent',
        'remoteops-agent-service',
        'remoteops-agent-gui',
        'remoteops-ssh-askpass',
        'remoteops-controller-cli',
        'remoteops-controller-mcp',
        'remoteops-credential-prompt',
        'remoteops-controller-gui',
        'remoteops-serial-demo',
        'remoteops-mcp-smoke'
    )
    $packageVersions = @{}
    if ($IncludeNativePreview) { $releasePackages += 'remoteops-agent-native-prototype' }
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
        @{ File = 'remoteops-credential-prompt.exe'; Package = 'remoteops-credential-prompt' },
        @{ File = 'remoteops-controller-gui.exe'; Package = 'remoteops-controller-gui' },
        @{ File = 'remoteops-serial-demo.exe'; Package = 'remoteops-serial-demo' },
        @{ File = 'remoteops-mcp-smoke.exe'; Package = 'remoteops-mcp-smoke' }
    )
    $sourceCommit = (& git -C $workspaceRoot rev-parse HEAD).Trim()
    if ($IncludeNativePreview) {
        $binaries += @{ File = 'remoteops-agent-native-prototype.exe'; Package = 'remoteops-agent-native-prototype' }
    }
    if ($LASTEXITCODE -ne 0) { throw '无法确定实际源码提交' }
    $sourceDirty = [bool](& git -C $workspaceRoot status --porcelain)
    $protocolSource = Get-Content -LiteralPath (Join-Path $workspaceRoot 'crates/remoteops-protocol/src/message.rs') -Raw
    if ($protocolSource -notmatch 'PROTOCOL_VERSION: u16 = (\d+)') { throw '无法读取实际协议版本' }
    $protocolVersion = [int]$Matches[1]
    $manifest = foreach ($binary in $binaries) {
        $releaseVersion = $packageVersions[$binary.Package]
        if ([string]::IsNullOrWhiteSpace($releaseVersion)) {
            throw "未找到交付程序对应的 Cargo 包版本：$($binary.Package)"
        }
        $buildProfile = if ($binary.Package -in @('remoteops-agent-gui', 'remoteops-agent-native-prototype')) {
            'release-small'
        }
        else {
            'release'
        }
        $source = Join-Path (Join-Path $cargoTargetDirectory $buildProfile) $binary.File
        $relativeTarget = if ($binary.Package -eq 'remoteops-agent-native-prototype') {
            Join-Path 'native-preview' $binary.File
        } else { $binary.File }
        $target = Join-Path $resolvedOutput $relativeTarget
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
        Copy-Item -LiteralPath $source -Destination $target
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
            File = $relativeTarget
            Package = $binary.Package
            SourceCommit = $sourceCommit
            SourceDirty = $sourceDirty
            ProtocolVersion = $protocolVersion
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
    $askpassSource = Join-Path $resolvedOutput 'remoteops-ssh-askpass.exe'
    $bundled = & (Join-Path $PSScriptRoot 'Copy-BundledPlink.ps1') `
        -SourcePath $resolvedPlink -DestinationDirectory $resolvedOutput -AskpassPath $askpassSource
    $puttyNotice = "`r`n----- PuTTY / Plink 0.83 (MIT) -----`r`n" +
        [IO.File]::ReadAllText((Join-Path $resolvedOutput 'tools/Plink-LICENCE.txt'))
    [IO.File]::AppendAllText((Join-Path $resolvedOutput 'THIRD_PARTY_LICENSES.txt'), $puttyNotice, [Text.UTF8Encoding]::new($false))
    $dependencyPath = Join-Path $resolvedOutput 'DEPENDENCIES.json'
    $allDependencies = @(Get-Content -LiteralPath $dependencyPath -Raw | ConvertFrom-Json) + @(
        [pscustomobject]@{ name = 'PuTTY / Plink'; version = '0.83'; license = 'MIT';
            source = 'https://the.earth.li/~sgtatham/putty/0.83/w64/plink.exe';
            repository = 'https://www.chiark.greenend.org.uk/~sgtatham/putty/'; sha256 = $bundled.sha256 }
    )
    [IO.File]::WriteAllText($dependencyPath,
        (ConvertTo-Json -InputObject $allDependencies -Depth 4) + "`r`n", [Text.UTF8Encoding]::new($false))
    $toolManifest = @(
        [pscustomobject]@{ File = 'tools/plink.exe'; Version = $bundled.version; Sha256 = $bundled.sha256 }
        [pscustomobject]@{ File = 'tools/Plink-LICENCE.txt'; Sha256 = $bundled.licence_sha256 }
        [pscustomobject]@{ File = 'remoteops-ssh-askpass.exe'; Protocol = $bundled.askpass_protocol; Sha256 = (Get-FileHash -LiteralPath $askpassSource).Hash.ToLowerInvariant() }
    )
    $toolsJson = ConvertTo-Json -InputObject $toolManifest -Depth 4
    [IO.File]::WriteAllText((Join-Path $resolvedOutput 'bundled-tools.json'), $toolsJson + "`r`n", [Text.UTF8Encoding]::new($false))
    if ($IncludeNativePreview) {
        $nativeDirectory = Join-Path $resolvedOutput 'native-preview'
        & (Join-Path $PSScriptRoot 'Copy-BundledPlink.ps1') `
            -SourcePath $resolvedPlink -DestinationDirectory $nativeDirectory -AskpassPath $askpassSource | Out-Null
        foreach ($legalFile in @('LICENSE', 'THIRD_PARTY_LICENSES.txt', 'DEPENDENCIES.json', 'bundled-tools.json')) {
            Copy-Item -LiteralPath (Join-Path $resolvedOutput $legalFile) -Destination (Join-Path $nativeDirectory $legalFile)
        }
        $nativeManifest = @($manifest | Where-Object { $_.Package -in @('remoteops-agent-native-prototype', 'remoteops-ssh-askpass') } |
            ForEach-Object {
                $entry = $_.PSObject.Copy()
                $entry.File = [IO.Path]::GetFileName($entry.File)
                $entry
            })
        [IO.File]::WriteAllText((Join-Path $nativeDirectory 'manifest.json'),
            (ConvertTo-Json -InputObject $nativeManifest -Depth 4) + "`r`n", [Text.UTF8Encoding]::new($false))
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
