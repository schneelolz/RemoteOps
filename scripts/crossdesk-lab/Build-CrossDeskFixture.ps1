[CmdletBinding()]
param(
    [string]$Xmake = 'xmake',
    [string]$BuildRoot = (Join-Path ([IO.Path]::GetTempPath()) 'remoteops-crossdesk-lab'),
    [string]$VisualStudioVersion,
    [switch]$NativeCapture,
    [switch]$DesktopFixture,
    [switch]$MediaWorker,
    [switch]$ProductBuild
)
$ErrorActionPreference = 'Stop'
if ($ProductBuild -and (!$MediaWorker -and !$NativeCapture -or $DesktopFixture)) { throw '产品模式仅用于原生媒体或采集组件' }
$root = [IO.Directory]::CreateDirectory($BuildRoot).FullName
$build = [IO.Directory]::CreateDirectory((Join-Path $root ([guid]::NewGuid().ToString('N')))).FullName
function Expand-VerifiedSource([string]$Project, [string]$Revision, [string]$Hash, [string]$Directory) {
    $zip = Join-Path $root "$Project-$Revision.zip"
    if (!(Test-Path -LiteralPath $zip)) {
        $download = Join-Path $build "$Project.partial"
        Invoke-WebRequest "https://codeload.github.com/kunkundi/$Project/zip/$Revision" -OutFile $download
        if ((Get-FileHash -LiteralPath $download).Hash -ne $Hash) { throw "$Project 源码摘要不匹配" }
        [IO.File]::Copy($download, $zip, $false)
    }
    if ((Get-FileHash -LiteralPath $zip).Hash -ne $Hash) { throw "$Project 缓存源码摘要不匹配" }
    Expand-Archive -LiteralPath $zip -DestinationPath $Directory
    $expanded = (Resolve-Path -LiteralPath (Join-Path $Directory "$Project-$Revision")).Path
    $short = [IO.Path]::GetFullPath((Join-Path $Directory $Project))
    if ([IO.Path]::GetDirectoryName($expanded) -ne $build -or [IO.Path]::GetDirectoryName($short) -ne $build) {
        throw '解包路径与本次构建目录不符'
    }
    [IO.Directory]::Move($expanded, $short)
    $short
}
$source = Expand-VerifiedSource 'crossdesk' 'c3916910653a093588c8a85ca3b6f886720edd49' 'ee961bae5b5d91f3590a12f440b39db2cd3f3b250eefe82fd697be5e9193faa8' $build
$mini = Expand-VerifiedSource 'minirtc' '5d4f24c42831234035be3c79cd31467753e06609' 'e8d0484b1233c0e1d5f6e03530ced0ee934d629e38fdc828ce1af3155ec60b1a' $build
$submodule = [IO.Directory]::CreateDirectory((Join-Path $source 'deps/submodules/minirtc')).FullName
Get-ChildItem -LiteralPath $mini -Force | Copy-Item -Destination $submodule -Recurse
& (Join-Path $PSScriptRoot 'Set-CrossDeskUiaCompatibility.ps1') -Source $source
if (!$ProductBuild) { & (Join-Path $PSScriptRoot 'Set-FixtureTrust.ps1') -FixtureSource $source }
$fixture = [IO.Directory]::CreateDirectory((Join-Path $source 'remoteops_fixture')).FullName
foreach ($name in 'rtc_fixture.cpp','desktop_fixture.cpp','fixture.lua','../../crates/remoteops-desktop-media/crossdesk/host_grant.hpp',
    '../../crates/remoteops-desktop-media/crossdesk/uia.cpp','../../crates/remoteops-desktop-media/crossdesk/capture.cpp','../../crates/remoteops-desktop-media/crossdesk/capture.lua',
    '../../crates/remoteops-desktop-media/crossdesk/host_peer.hpp','../../crates/remoteops-desktop-media/crossdesk/worker.cpp') {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination $fixture
}
[IO.File]::AppendAllText((Join-Path $source 'xmake.lua'), "`nincludes(`"remoteops_fixture/fixture.lua`")`n")
[IO.File]::AppendAllText((Join-Path $source 'xmake.lua'), "`nincludes(`"remoteops_fixture/capture.lua`")`n")
$oldCl = $env:CL
Push-Location $source
try {
    $env:CL = "$oldCl /DNOMINMAX /utf-8"
    $options = @('f','-p','windows','-a','x64','-m','release','--CROSSDESK_BUILD_DESKTOP=n','--CROSSDESK_BUILD_CLI=y','-y')
    if ($VisualStudioVersion) { $options += "--vs=$VisualStudioVersion" }
    & $Xmake @options | Out-Host
    if ($LASTEXITCODE) { throw 'CrossDesk 测试组件配置失败' }
    $target = if ($DesktopFixture) { 'remoteops-crossdesk-desktop-fixture' } elseif ($NativeCapture) { 'remoteops-crossdesk-native' } elseif ($MediaWorker) { 'remoteops-crossdesk' } else { 'remoteops-crossdesk-fixture' }
    & $Xmake b $target | Out-Host
    if ($LASTEXITCODE) { throw 'CrossDesk 测试组件构建失败' }
    $extension = if ($target -eq 'remoteops-crossdesk-native') { 'dll' } else { 'exe' }
    (Resolve-Path -LiteralPath "build/windows/x64/release/$target.$extension").Path
} finally {
    Pop-Location
    $env:CL = $oldCl
}
