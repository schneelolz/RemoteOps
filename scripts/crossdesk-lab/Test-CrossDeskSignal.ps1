[CmdletBinding()]
param(
    [string]$Xmake = 'xmake',
    [string]$OpenSsl = 'openssl',
    [string]$RtcFixture,
    [switch]$Turn,
    [string]$BuildRoot = (Join-Path ([IO.Path]::GetTempPath()) 'remoteops-crossdesk-signal')
)
$ErrorActionPreference = 'Stop'
$revision = '68534ccf13d04838bd852a2fdd9733efde4564b3'
$sha256 = '53be7be3eb12adc1395dadb26fed30774cc76168b1c62648882860f46b2bbfc5'
$root = (New-Item -ItemType Directory -Force -Path $BuildRoot).FullName
$zip = Join-Path $root "signal-$revision.zip"
if (!(Test-Path -LiteralPath $zip)) {
    Invoke-WebRequest "https://codeload.github.com/kunkundi/crossdesk-server/zip/$revision" -OutFile $zip
}
if ((Get-FileHash -LiteralPath $zip).Hash -ne $sha256) { throw '信令源码与锁定版本不匹配' }
$build = (New-Item -ItemType Directory -Path (Join-Path $root ([guid]::NewGuid().ToString('N')))).FullName
Expand-Archive -LiteralPath $zip -DestinationPath $build
$source = Join-Path $build "crossdesk-server-$revision"
$listen = Join-Path $source 'src/signal_server.cpp'
$before = 'acceptor_->bind(asio::ip::tcp::endpoint(asio::ip::tcp::v4(), port_));'
$code = [IO.File]::ReadAllText($listen)
if (($code.Split($before, [StringSplitOptions]::None)).Length -ne 2) { throw '无法定位唯一的测试监听端点' }
$after = 'acceptor_->bind(asio::ip::tcp::endpoint(asio::ip::address_v4::loopback(), port_));'
[IO.File]::WriteAllText($listen, $code.Replace($before, $after))
Push-Location $source
$originalCl = $env:CL
try {
    $env:CL = "$originalCl /DNOMINMAX /utf-8"
    & $Xmake f -p windows -a x64 -m release -y
    if ($LASTEXITCODE) { throw '信令依赖配置失败' }
    & $Xmake b credential_server_fixture
    if ($LASTEXITCODE) { throw '信令测试实例构建失败' }
} finally {
    $env:CL = $originalCl
    Pop-Location
}
$fixture = Join-Path $source 'build/windows/x64/release/credential_server_fixture.exe'
& (Join-Path $PSScriptRoot 'Invoke-SignalFixture.ps1') -Fixture $fixture -OpenSsl $OpenSsl -RtcFixture $RtcFixture -Turn:$Turn
