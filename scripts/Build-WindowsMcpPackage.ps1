[CmdletBinding()]
param(
    [string]$OutputRoot,
    [string]$ArtifactDirectory,
    [ValidateSet('release', 'release-small')][string]$BuildProfile = 'release-small'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'RemoteOps-Package.ps1')
$workspaceRoot = Split-Path -Parent $PSScriptRoot
Push-Location $workspaceRoot
try {
    $metadataJson = (& cargo metadata --locked --no-deps --format-version 1 | Out-String)
    if ($LASTEXITCODE -ne 0) { throw '读取 Cargo 元数据失败。' }
    $metadata = $metadataJson | ConvertFrom-Json
    $version = [string]($metadata.packages | Where-Object name -EQ 'remoteops-controller-mcp').version
    $promptVersion = [string]($metadata.packages | Where-Object name -EQ 'remoteops-credential-prompt').version
    if (-not $version -or -not $promptVersion) { throw '无法确定组件版本。' }
    if (-not $OutputRoot) { $OutputRoot = Join-Path $workspaceRoot "artifacts\release\$version\mcp" }
    if (-not $ArtifactDirectory) { $ArtifactDirectory = Join-Path $workspaceRoot "artifacts\release\$version\windows-x64" }
    $OutputRoot = [IO.Path]::GetFullPath($OutputRoot)
    $ArtifactDirectory = [IO.Path]::GetFullPath($ArtifactDirectory)
    $packageName = "RemoteOps-MCP-Windows-x64-$version"
    $packageDirectory = Join-Path $OutputRoot $packageName
    $archivePath = Join-Path $OutputRoot "$packageName.zip"
    if ((Test-Path -LiteralPath $packageDirectory) -or (Test-Path -LiteralPath $archivePath)) {
        throw '输出已存在；请使用新的输出目录，保留已有发布产物。'
    }
    New-Item -ItemType Directory -Force -Path $ArtifactDirectory, $OutputRoot | Out-Null
    # 摘要覆盖编译输入和发布脚本；构建前后再次核对，避免并行改动混入产物。
    function Get-SourceInputs {
        $files = @('Cargo.toml', 'Cargo.lock', 'LICENSE', '.gitattributes', '.editorconfig', 'rust-toolchain.toml') | ForEach-Object { $path = Join-Path $workspaceRoot $_; if (Test-Path -LiteralPath $path -PathType Leaf) { Get-Item -LiteralPath $path } }
        foreach ($directory in @('apps', 'crates', 'build', 'assets', 'deploy', 'scripts', 'examples', 'tests', 'third-party', '.cargo')) {
            if (-not (Test-Path -LiteralPath (Join-Path $workspaceRoot $directory) -PathType Container)) { continue }
            $files += @(Get-ChildItem -LiteralPath (Join-Path $workspaceRoot $directory) -Recurse -File |
                Where-Object { $_.FullName -notmatch '[\\/]__pycache__[\\/]' -and $_.Extension -notin @('.pyc', '.log') })
        }
        @($files | Sort-Object FullName | ForEach-Object {
            [ordered]@{ file=$_.FullName.Substring($workspaceRoot.Length+1).Replace('\','/'); sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
        }) | ConvertTo-Json -Depth 5 -Compress
    }
    $sourceInputs = Get-SourceInputs
    $previous = @{}
    foreach ($name in @('CARGO_ENCODED_RUSTFLAGS', 'CFLAGS', 'CXXFLAGS')) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
    $userProfile = [Environment]::GetFolderPath('UserProfile')
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $userProfile '.cargo' }
    $separator = [char]0x1f
    $rustFlags = @('-Ctarget-feature=+crt-static', "--remap-path-prefix=$workspaceRoot=/remoteops", "--remap-path-prefix=$cargoHome=/cargo", "--remap-path-prefix=$userProfile=/user") -join $separator
    $env:CARGO_ENCODED_RUSTFLAGS = if ($previous.CARGO_ENCODED_RUSTFLAGS) { $previous.CARGO_ENCODED_RUSTFLAGS + $separator + $rustFlags } else { $rustFlags }
    $nativeFlags = "/pathmap:$workspaceRoot=/remoteops /pathmap:$cargoHome=/cargo /pathmap:$userProfile=/user"
    $env:CFLAGS = "$($previous.CFLAGS) $nativeFlags".Trim()
    $env:CXXFLAGS = "$($previous.CXXFLAGS) $nativeFlags".Trim()
    try {
        & cargo build --profile $BuildProfile --locked -p remoteops-controller-mcp -p remoteops-credential-prompt
        if ($LASTEXITCODE -ne 0) { throw 'MCP Release 构建失败。' }
    }
    finally {
        foreach ($name in $previous.Keys) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
    }
    if ((Get-SourceInputs) -cne $sourceInputs) { throw '构建期间源码发生变化；未生成可发布安装包，请在源码稳定后重新构建。' }
    $inputsPath = Join-Path $ArtifactDirectory 'source-inputs.json'
    [IO.File]::WriteAllText($inputsPath, $sourceInputs + "`r`n", [Text.UTF8Encoding]::new($false))
    $inputsHash = (Get-FileHash -LiteralPath $inputsPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-RemoteOpsJson (Join-Path $ArtifactDirectory 'build-info.json') ([ordered]@{ cargo=(& cargo --version | Out-String).Trim(); rustc=(& rustc --version | Out-String).Trim(); profile=$BuildProfile; source_inputs_sha256=$inputsHash; lockfile_sha256=(Get-FileHash -LiteralPath (Join-Path $workspaceRoot 'Cargo.lock') -Algorithm SHA256).Hash.ToLowerInvariant() })
    New-Item -ItemType Directory -Path $packageDirectory | Out-Null
    $templateDirectory = Join-Path $workspaceRoot 'deploy\controller-mcp\windows'
    foreach ($name in @('remoteops-controller-mcp.exe', 'remoteops-credential-prompt.exe')) {
        $source = Join-Path $metadata.target_directory "$BuildProfile\$name"
        Copy-Item -LiteralPath $source -Destination $packageDirectory
        Copy-Item -LiteralPath $source -Destination $ArtifactDirectory -Force
    }
    foreach ($name in @('README.md', 'Install-RemoteOpsMcp.ps1', 'Test-RemoteOpsMcp.ps1')) {
        Copy-Item -LiteralPath (Join-Path $templateDirectory $name) -Destination $packageDirectory
    }
    & (Join-Path $PSScriptRoot 'New-ThirdPartyNotices.ps1') -OutputDirectory $packageDirectory
    if ($LASTEXITCODE -ne 0) { throw '许可证清单生成失败。' }
    foreach ($component in @(@('remoteops-controller-mcp.exe', $version), @('remoteops-credential-prompt.exe', $promptVersion))) {
        $output = (& (Join-Path $packageDirectory $component[0]) --version 2>&1 | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or -not $output.EndsWith(' ' + $component[1])) { throw '实际组件版本不匹配。' }
    }
    if ((Get-SourceInputs) -cne $sourceInputs) { throw '打包期间源码发生变化；保留本批证据，请在新目录重新构建。' }
    $manifest = [ordered]@{ schema_version=1; component='remoteops-mcp'; version=$version; credential_prompt_version=$promptVersion; protocol=20; build_profile=$BuildProfile; source_inputs_sha256=$inputsHash; contains_credentials=$false; files=@(Get-RemoteOpsPackageFiles $packageDirectory) }
    Write-RemoteOpsJson (Join-Path $packageDirectory 'manifest.json') $manifest
    Write-RemoteOpsChecksums $packageDirectory
    $null = Test-RemoteOpsComponentPackage $packageDirectory 'remoteops-mcp'
    & (Join-Path $PSScriptRoot 'Test-ReleaseArtifacts.ps1') -ArtifactRoot $packageDirectory -RequireLegalFiles
    if ($LASTEXITCODE -ne 0) { throw 'MCP 安装包内容检查失败。' }
    Compress-Archive -Path (Join-Path $packageDirectory '*') -DestinationPath $archivePath -CompressionLevel Optimal
    Test-RemoteOpsArchive $packageDirectory $archivePath
    & (Join-Path $PSScriptRoot 'Test-ReleaseArtifacts.ps1') -ArtifactRoot $packageDirectory -RequireLegalFiles -ReferenceMcpExecutable (Join-Path $ArtifactDirectory 'remoteops-controller-mcp.exe') -McpArchive $archivePath
    if ($LASTEXITCODE -ne 0) { throw 'MCP ZIP 一致性检查失败。' }
    Write-Host "MCP 独立包：$archivePath"
    Write-Host "SHA256：$((Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant())"
}
finally { Pop-Location }
