#requires -Version 7.2
<# .SYNOPSIS 从已验证 JLAI 公共底座快照构建独立 Bridge 候选，不修改原快照或现有交付。 #>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$JlaiWorkDirectory,
    [Parameter(Mandatory)][string]$WorkDirectory,
    [Parameter(Mandatory)][string]$TargetDirectory,
    [Parameter(Mandatory)][string]$OutputDirectory
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$sourceRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$jlai = (Resolve-Path -LiteralPath $JlaiWorkDirectory).Path
$originalBase = Join-Path $jlai 'base'
$work = [IO.Path]::GetFullPath($WorkDirectory)
$target = [IO.Path]::GetFullPath($TargetDirectory)
$output = [IO.Path]::GetFullPath($OutputDirectory)
if (-not $work.StartsWith('C:\', [StringComparison]::OrdinalIgnoreCase) -or -not $target.StartsWith('C:\', [StringComparison]::OrdinalIgnoreCase)) { throw '快照和编译缓存必须在C盘。' }
if ((Test-Path -LiteralPath $work) -or (Test-Path -LiteralPath $output)) { throw '拒绝覆盖已有快照或交付。' }
$originalRecord = Get-Content -LiteralPath (Join-Path $jlai 'source-inputs.json') -Raw | ConvertFrom-Json
foreach ($file in @($originalRecord.base_inputs.files | Where-Object { $_.file -match '^(crates/|apps/remoteops-agent/|build/|Cargo\.(toml|lock)$)' -and $_.file -notmatch '^crates/remoteops-bridge-protocol/' })) {
    $actual = (Get-FileHash -LiteralPath (Join-Path $sourceRoot $file.file)).Hash.ToLowerInvariant()
    if ($actual -cne $file.sha256) { throw "公共核心与Agent构建快照不同：$($file.file)" }
}
New-Item -ItemType Directory -Path $work | Out-Null
$snapshot = Join-Path $work 'source'
Copy-Item -LiteralPath $originalBase -Destination $snapshot -Recurse
foreach ($relative in @('apps/remoteops-bridge', 'crates/remoteops-bridge-protocol', 'apps/remoteops-relay')) {
    foreach ($file in @(Get-ChildItem -LiteralPath (Join-Path $sourceRoot $relative) -File -Recurse)) {
        $destination = Join-Path $snapshot ([IO.Path]::GetRelativePath($sourceRoot, $file.FullName))
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
        Copy-Item -LiteralPath $file.FullName -Destination $destination -Force
    }
}
$inputs = @(Get-ChildItem -LiteralPath $snapshot -Recurse -File | Sort-Object FullName | ForEach-Object { [ordered]@{ file = [IO.Path]::GetRelativePath($snapshot, $_.FullName).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $_.FullName).Hash.ToLowerInvariant() } })
$saved = @{}
foreach ($name in @('CARGO_TARGET_DIR','CARGO_ENCODED_RUSTFLAGS','CFLAGS','CXXFLAGS')) { $saved[$name] = [Environment]::GetEnvironmentVariable($name) }
try {
    $userDirectory = [Environment]::GetFolderPath('UserProfile')
    $cargoDirectory = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $userDirectory '.cargo' }
    $env:CARGO_TARGET_DIR = $target
    # 复用同批JLAI的第三方依赖缓存；用户目录映射同时覆盖本次独立源码快照。
    $env:CARGO_ENCODED_RUSTFLAGS = [string]::Join([char]0x1F, @('-Ctarget-feature=+crt-static', "--remap-path-prefix=$originalBase=/remoteops", "--remap-path-prefix=$(Join-Path $jlai 'product')=/jlai", "--remap-path-prefix=$cargoDirectory=/cargo", "--remap-path-prefix=$userDirectory=/user"))
    $env:CFLAGS = "/pathmap:$originalBase=/remoteops /pathmap:$(Join-Path $jlai 'product')=/jlai /pathmap:$cargoDirectory=/cargo /pathmap:$userDirectory=/user"
    $env:CXXFLAGS = $env:CFLAGS
    & cargo build --offline --locked --manifest-path (Join-Path $snapshot 'Cargo.toml') --profile release-small -p remoteops-bridge -p remoteops-relay *> (Join-Path $work 'build.log')
    if ($LASTEXITCODE) { throw 'Bridge候选编译失败，详见独立工作目录build.log。' }
    foreach ($file in $inputs) { if ((Get-FileHash -LiteralPath (Join-Path $snapshot $file.file)).Hash.ToLowerInvariant() -cne $file.sha256) { throw '构建期间源码快照发生变化。' } }
    New-Item -ItemType Directory -Path $output | Out-Null
    $program = Join-Path $output 'RemoteOpsBridge.exe'
    Copy-Item -LiteralPath (Join-Path $target 'release-small/RemoteOpsBridge.exe') -Destination $program
    Copy-Item -LiteralPath (Join-Path $target 'release-small/remoteops-relay.exe') -Destination (Join-Path $output 'remoteops-relay.exe')
    $pe = [Diagnostics.FileVersionInfo]::GetVersionInfo($program)
    $version = [regex]::Match([IO.File]::ReadAllText((Join-Path $snapshot 'apps/remoteops-bridge/Cargo.toml')), '(?m)^version = "([^"\r\n]+)"').Groups[1].Value
    if ($pe.ProductVersion -cne $version -or $pe.FileVersion -cne $version) { throw 'Bridge PE版本不匹配。' }
    $relayProgram = Join-Path $output 'remoteops-relay.exe'
    $relayVersion = [regex]::Match([IO.File]::ReadAllText((Join-Path $snapshot 'apps/remoteops-relay/Cargo.toml')), '(?m)^version = "([^"\r\n]+)"').Groups[1].Value
    if ([Diagnostics.FileVersionInfo]::GetVersionInfo($relayProgram).ProductVersion -cne $relayVersion) { throw 'Relay PE版本不匹配。' }
    [ordered]@{ component='RemoteOpsBridge';version=$version;local_protocol=1;relay_protocol=22;default_agent_protocol=20;source_commit=$originalRecord.base_commit;dirty=$true;agent_base_content_sha256=$originalRecord.base_inputs.content_sha256;common_core_equal=$true;file='RemoteOpsBridge.exe';sha256=(Get-FileHash -LiteralPath $program).Hash.ToLowerInvariant();relay=[ordered]@{file='remoteops-relay.exe';version=$relayVersion;sha256=(Get-FileHash -LiteralPath $relayProgram).Hash.ToLowerInvariant()};source_inputs=$inputs;unsigned=$true;production_installed=$false } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $output 'bridge-build-manifest.json') -Encoding utf8NoBOM
    Write-Output "Bridge候选构建完成：$output"
} finally { foreach ($name in $saved.Keys) { [Environment]::SetEnvironmentVariable($name, $saved[$name]) } }
