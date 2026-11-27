[CmdletBinding()]
param([Parameter(Mandatory)][string]$FixtureSource, [switch]$UseNetworkInterfaces)
$ErrorActionPreference = 'Stop'
# 只修改隔离构建中的上游源码。回归客户端校验证书链和主机名，系统证书库不需要安装测试证书。
$file = Join-Path $FixtureSource 'deps/submodules/minirtc/src/ws/ws_client_tls.cpp'
$source = [IO.File]::ReadAllText($file)
$needle = '  if (!LoadWindowsRootCertificates(ssl_ctx)) {'
$replacement = @'
  if (const char* pem = std::getenv("REMOTEOPS_CROSSDESK_FIXTURE_CA")) {
    if (SSL_CTX_load_verify_locations(ssl_ctx, pem, nullptr) != 1)
      throw std::runtime_error("Cannot load isolated fixture CA");
    return;
  }
  if (!LoadWindowsRootCertificates(ssl_ctx)) {
'@
if (!$source.Contains($replacement)) {
    if ($source.IndexOf($needle) -lt 0 -or $source.IndexOf($needle) -ne $source.LastIndexOf($needle)) {
        throw '上游 Windows 测试信任入口与锁定版本不一致'
    }
    [IO.File]::WriteAllText($file, $source.Replace($needle, $replacement))
}
if (!$UseNetworkInterfaces) {
$file = Join-Path $FixtureSource 'deps/submodules/minirtc/src/ice/ice_agent.cpp'
$source = [IO.File]::ReadAllText($file)
$needle = '      PreferUnscopedLocalAddresses(agent);'
# 回环 TURN 测试只有这个接口，避免向工作站的其他网卡或局域网发送探测。
$replacement = @'
      NiceAddress fixture_loopback{};
      if (!nice_address_set_from_string(&fixture_loopback, "127.0.0.1") ||
          !nice_agent_add_local_address(agent, &fixture_loopback))
        throw std::runtime_error("Cannot bind isolated ICE interface");
'@
if (!$source.Contains($replacement)) {
    if ($source.IndexOf($needle) -lt 0 -or $source.IndexOf($needle) -ne $source.LastIndexOf($needle)) {
        throw '上游 ICE 本地接口入口与锁定版本不一致'
    }
    [IO.File]::WriteAllText($file, $source.Replace($needle, $replacement))
}
}
$file = Join-Path $FixtureSource 'libs/controller/src/controller_session.cpp'
$source = [IO.File]::ReadAllText($file)
$needle = 'p.turn_mode = MiniRtcTurnAutoUdpTcp;'
$replacement = 'p.turn_mode = std::getenv("REMOTEOPS_CROSSDESK_FIXTURE_TURN") ? MiniRtcTurnForceUdp : MiniRtcTurnDisabled;'
if (!$source.Contains($replacement)) {
    if ($source.IndexOf($needle) -lt 0 -or $source.IndexOf($needle) -ne $source.LastIndexOf($needle)) {
        throw '上游控制端测试路由入口与锁定版本不一致'
    }
    [IO.File]::WriteAllText($file, $source.Replace($needle, $replacement))
}
