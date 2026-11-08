#requires -Version 7.2
<# .SYNOPSIS 生成默认打开样式演示的独立 Win32 小样，不包含 Agent 运行时。 #>
[CmdletBinding()]
param([Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$root=Split-Path -Parent $PSScriptRoot
$output=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path -LiteralPath $output){throw '必须使用不存在的新交付目录，拒绝覆盖旧版。'}
for($ancestor=Split-Path -Parent $output;$ancestor;$ancestor=Split-Path -Parent $ancestor){
    if((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor).Attributes -band [IO.FileAttributes]::ReparsePoint)){throw '输出目录不能经过重解析点。'}
}
if(-not $env:CARGO_TARGET_DIR -or -not ([IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)).StartsWith('C:\',[StringComparison]::OrdinalIgnoreCase)){throw '必须显式使用 C 盘 CARGO_TARGET_DIR。'}
$savedFlags=$env:CARGO_ENCODED_RUSTFLAGS
$profileDirectory=[Environment]::GetFolderPath('UserProfile')
$cargoDirectory=if($env:CARGO_HOME){[IO.Path]::GetFullPath($env:CARGO_HOME)}else{Join-Path $profileDirectory '.cargo'}
$flags=@('-Ctarget-feature=+crt-static',"--remap-path-prefix=$root=/remoteops","--remap-path-prefix=$cargoDirectory=/cargo","--remap-path-prefix=$profileDirectory=/user")
Push-Location $root
try{
 $metadata=(& cargo metadata --offline --locked --no-deps --format-version 1 | ConvertFrom-Json)
 if($LASTEXITCODE){throw '无法读取构建包版本。'}
 $nativePackages=@($metadata.packages | Where-Object name -eq 'remoteops-agent-native-prototype')
 if($nativePackages.Count -ne 1){throw '原生宿主包必须唯一。'}
 $expectedVersion=[string]$nativePackages[0].version
    $env:CARGO_ENCODED_RUSTFLAGS=[string]::Join([char]0x1F,$flags)
    & cargo build --offline --locked --profile release-small -p remoteops-agent-native-prototype --no-default-features --features style-preview
    if($LASTEXITCODE){throw '样式预览构建失败。'}
    $tree=(& cargo tree --offline --locked -p remoteops-agent-native-prototype --no-default-features --features style-preview -e normal | Out-String)
    if($LASTEXITCODE -or $tree -match '(?im)\bremoteops-agent v|\bslint v|\bi-slint-'){throw '样式预览依赖不符合无 Agent 运行时／无 Slint 要求。'}
    $source=Join-Path $env:CARGO_TARGET_DIR 'release-small/remoteops-agent-native-prototype.exe'
    $version=[Diagnostics.FileVersionInfo]::GetVersionInfo($source)
    if($version.ProductVersion -cne $expectedVersion -or $version.ProductName -cne 'RemoteOps Native UI Prototype'){throw '实际生成物版本或产品标识不符。'}
    New-Item -ItemType Directory -Path $output|Out-Null
    $target=Join-Path $output 'RemoteOps-Win32-StylePreview.exe'
    Copy-Item -LiteralPath $source -Destination $target
    Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination (Join-Path $output 'RemoteOps-AGPL-3.0.txt')
    $record=[ordered]@{version=$expectedVersion;demo_only=$true;agent_runtime_included=$false;default_mode='style-preview';profile='release-small';rust_flags=$flags;file=[IO.Path]::GetFileName($target);bytes=(Get-Item -LiteralPath $target).Length;sha256=(Get-FileHash -LiteralPath $target).Hash.ToLowerInvariant();old_version_replaced=$false;onsite_verified=$false}
    [IO.File]::WriteAllText((Join-Path $output 'manifest.json'),($record|ConvertTo-Json -Depth 4),[Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText((Join-Path $output 'runtime-dependencies.txt'),$tree,[Text.UTF8Encoding]::new($false))
    $record
}finally{$env:CARGO_ENCODED_RUSTFLAGS=$savedFlags;Pop-Location}
