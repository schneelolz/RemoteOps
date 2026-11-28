#requires -Version 7.2
<# .SYNOPSIS 将已确认的 Win32 界面与现有 Agent 核心打包为独立新版，不覆盖旧版。 #>
[CmdletBinding()]
param([Parameter(Mandatory)][string]$OutputDirectory,[string]$PlinkSourcePath='D:\Tools\putty\PLINK.EXE')
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$root=Split-Path -Parent $PSScriptRoot
$output=[IO.Path]::GetFullPath($OutputDirectory)
if(Test-Path -LiteralPath $output){throw '交付目录必须不存在，拒绝覆盖旧版。'}
for($ancestor=Split-Path -Parent $output;$ancestor;$ancestor=Split-Path -Parent $ancestor){if((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor).Attributes -band [IO.FileAttributes]::ReparsePoint)){throw '交付路径不能经过重解析点。'}}
if(-not $env:CARGO_TARGET_DIR -or -not ([IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)).StartsWith('C:\',[StringComparison]::OrdinalIgnoreCase)){throw '构建缓存必须显式设置在C盘。'}
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
 & cargo build --offline --locked --profile release-small -p remoteops-agent-native-prototype -p remoteops-ssh-askpass
 if($LASTEXITCODE){throw 'Win32业务版本构建失败。'}
 $tree=(& cargo tree --offline --locked -p remoteops-agent-native-prototype -e normal|Out-String)
 if($LASTEXITCODE -or $tree -notmatch '(?im)remoteops-agent v' -or $tree -match '(?im)\bslint v|\bi-slint-'){throw '业务包必须包含Agent核心且不能引入Slint。'}
 $source=Join-Path $env:CARGO_TARGET_DIR 'release-small/remoteops-agent-native-prototype.exe'
 $version=[Diagnostics.FileVersionInfo]::GetVersionInfo($source)
 if($version.ProductVersion -cne $expectedVersion -or $version.ProductName -cne 'RemoteOps Native UI Prototype'){throw '实际生成物版本不匹配。'}
 New-Item -ItemType Directory -Path $output|Out-Null
 $target=Join-Path $output 'RemoteOps-Win32.exe'
 Copy-Item -LiteralPath $source -Destination $target
 Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination (Join-Path $output 'RemoteOps-AGPL-3.0.txt')
 $plink=& (Join-Path $PSScriptRoot 'Copy-BundledPlink.ps1') -SourcePath $PlinkSourcePath -DestinationDirectory $output -AskpassPath (Join-Path $env:CARGO_TARGET_DIR 'release-small/remoteops-ssh-askpass.exe')
 $files=@(Get-ChildItem -LiteralPath $output -File -Recurse|ForEach-Object{[ordered]@{file=[IO.Path]::GetRelativePath($output,$_.FullName);bytes=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName).Hash.ToLowerInvariant()}})
 $record=[ordered]@{version=$expectedVersion;demo_only=$false;agent_runtime_included=$true;default_mode='styled-product';profile='release-small';rust_flags=$flags;file=[IO.Path]::GetFileName($target);bytes=(Get-Item -LiteralPath $target).Length;sha256=(Get-FileHash -LiteralPath $target).Hash.ToLowerInvariant();files=$files;plink_version=$plink.version;old_version_replaced=$false;onsite_verified=$false;rdp_dpi_verified=$false}
 [IO.File]::WriteAllText((Join-Path $output 'manifest.json'),($record|ConvertTo-Json -Depth 6),[Text.UTF8Encoding]::new($false))
 [IO.File]::WriteAllText((Join-Path $output 'runtime-dependencies.txt'),$tree,[Text.UTF8Encoding]::new($false))
 $record
}finally{$env:CARGO_ENCODED_RUSTFLAGS=$savedFlags;Pop-Location}
