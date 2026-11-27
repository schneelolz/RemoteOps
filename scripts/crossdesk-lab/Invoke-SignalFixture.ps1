[CmdletBinding()]
param([Parameter(Mandatory)][string]$Fixture, [string]$OpenSsl = 'openssl', [string]$RtcFixture, [string]$MediaWorker, [switch]$Turn)
$ErrorActionPreference = 'Stop'
$temp = [IO.Path]::GetTempPath()
$runName = "remoteops-signal-test-$([guid]::NewGuid().ToString('N'))"
$run = [IO.Directory]::CreateDirectory((Join-Path $temp $runName)).FullName
$cert = Join-Path $run 'api.crossdesk.cn_bundle.crt'
$key = Join-Path $run 'api.crossdesk.cn.key'
$server = $null
$turnServer = $null
try {
    & $OpenSsl req -x509 -newkey rsa:2048 -nodes -days 1 -subj '/CN=localhost' -addext 'subjectAltName=IP:127.0.0.1' -keyout $key -out $cert 2>$null
    if ($LASTEXITCODE) { throw '未能为隔离实例生成一次性 TLS 证书' }
    $reservation = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $reservation.Start()
    try { $port = $reservation.LocalEndpoint.Port } finally { $reservation.Stop() }
    $start = [Diagnostics.ProcessStartInfo]::new((Resolve-Path -LiteralPath $Fixture).Path)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WorkingDirectory = $run
    $start.ArgumentList.Add([string]$port)
    $start.ArgumentList.Add($run)
    $start.Environment['CROSSDESK_ICE_SERVERS'] = '[]'
    if ($Turn) {
        if (!$RtcFixture -and !$MediaWorker) { throw 'TURN 验收需要原生 RTC 客户端' }
        $turnExe = Join-Path $run 'local-turn.exe'
        Push-Location (Join-Path $PSScriptRoot 'turn')
        try {
            & go build -mod=readonly -o $turnExe .
            if ($LASTEXITCODE) { throw '隔离 TURN 构建失败' }
        } finally { Pop-Location }
        $turnInfo = [Diagnostics.ProcessStartInfo]::new($turnExe)
        $turnInfo.UseShellExecute = $false
        $turnInfo.CreateNoWindow = $true
        $turnInfo.RedirectStandardInput = $true
        $turnInfo.RedirectStandardOutput = $true
        $secret = [Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
        $turnInfo.Environment['REMOTEOPS_FIXTURE_TURN_SECRET'] = $secret
        $turnServer = [Diagnostics.Process]::Start($turnInfo)
        $portLine = $turnServer.StandardOutput.ReadLineAsync()
        if (!$portLine.Wait(5000)) { throw '隔离 TURN 启动超时' }
        $turnPort = [int]$portLine.Result
        if ($turnPort -lt 1024 -or $turnPort -gt 65535) { throw '隔离 TURN 端口无效' }
        $start.Environment['CROSSDESK_ICE_SERVERS'] = '[{"urls":["turn:127.0.0.1:' + $turnPort + '?transport=udp"]}]'
        $start.Environment['COTURN_AUTH_SECRET'] = $secret
    }
    $start.Environment['ADMIN_USERNAME'] = ''
    $start.Environment['ADMIN_PASSWORD'] = ''
    $server = [Diagnostics.Process]::Start($start)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    do {
        if ($server.HasExited) { throw '隔离信令实例启动失败' }
        $listening = @(Get-NetTCPConnection -OwningProcess $server.Id -State Listen -ErrorAction SilentlyContinue)
        if ($listening.Count) {
            if ($listening.Count -ne 1 -or $listening[0].LocalAddress -ne '127.0.0.1' -or $listening[0].LocalPort -ne $port) {
                throw '隔离实例监听端点错误'
            }
            break
        }
        if ($clock.Elapsed.TotalSeconds -gt 15) { throw '隔离实例启动超时' }
        Start-Sleep -Milliseconds 80
    } while ($true)
    $node = Get-Command node -CommandType Application | Select-Object -First 1
    $test = [Diagnostics.ProcessStartInfo]::new($node.Source)
    $test.UseShellExecute = $false
    $test.Environment['NODE_EXTRA_CA_CERTS'] = $cert
    $test.Environment['REMOTEOPS_CROSSDESK_TEST_PORT'] = [string]$port
    $test.ArgumentList.Add('--test')
    $test.ArgumentList.Add((Join-Path $PSScriptRoot 'signal.test.mjs'))
    $probe = [Diagnostics.Process]::Start($test)
    try {
        if (!$probe.WaitForExit(55000)) { $probe.Kill($true); throw '信令回归超时' }
        if ($probe.ExitCode -ne 0) { throw '信令回归未通过' }
    } finally { $probe.Dispose() }
    if ($MediaWorker) {
        $test.ArgumentList.Clear()
        $test.ArgumentList.Add('--test')
        $test.ArgumentList.Add((Join-Path $PSScriptRoot 'worker.test.mjs'))
        $test.Environment['REMOTEOPS_CROSSDESK_WORKER'] = (Resolve-Path -LiteralPath $MediaWorker).Path
        $test.Environment['REMOTEOPS_CROSSDESK_FIXTURE_CA'] = $cert
        $test.Environment['REMOTEOPS_CROSSDESK_LAB_DIRECTORY'] = $run
        if ($Turn) { $test.Environment['REMOTEOPS_CROSSDESK_FIXTURE_TURN'] = '1' }
        else { $null = $test.Environment.Remove('REMOTEOPS_CROSSDESK_FIXTURE_TURN') }
        $check = [Diagnostics.Process]::Start($test)
        try {
            if (!$check.WaitForExit(65000)) { $check.Kill($true); throw '媒体进程联调超时' }
            if ($check.ExitCode) { throw '媒体进程联调失败' }
        } finally { $check.Dispose() }
    }
    if ($RtcFixture) {
        $cargo = Get-Command cargo -CommandType Application | Select-Object -First 1
        $rtc = [Diagnostics.ProcessStartInfo]::new($cargo.Source)
        $rtc.UseShellExecute = $false
        $rtc.WorkingDirectory = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
        $rtc.Environment['REMOTEOPS_CROSSDESK_FIXTURE_CA'] = $cert
        $rtc.Environment['REMOTEOPS_CROSSDESK_TEST_PORT'] = [string]$port
        $rtc.Environment['REMOTEOPS_CROSSDESK_RTC_FIXTURE'] = (Resolve-Path -LiteralPath $RtcFixture).Path
        $rtc.Environment['REMOTEOPS_CROSSDESK_LAB_DIRECTORY'] = $run
        $null = $rtc.Environment.Remove('REMOTEOPS_CROSSDESK_FIXTURE_TURN')
        if ($Turn) { $rtc.Environment['REMOTEOPS_CROSSDESK_FIXTURE_TURN'] = '1' }
        foreach ($arg in 'test','--locked','-p','remoteops-relay','--bin','remoteops-relay','crossdesk_native_media_and_handoff','--','--ignored','--nocapture') {
            $rtc.ArgumentList.Add($arg)
        }
        $check = [Diagnostics.Process]::Start($rtc)
        try {
            $check.WaitForExit()
            if ($check.ExitCode) { throw 'RemoteOps 配对和原生 RTC 联调失败' }
        } finally { $check.Dispose() }
    }
} finally {
    if ($turnServer) {
        $turnServer.StandardInput.Close()
        if (!$turnServer.WaitForExit(2000)) { $turnServer.Kill($true); $turnServer.WaitForExit() }
        $turnServer.Dispose()
    }
    if ($server) {
        if (!$server.HasExited) { $server.Kill($true) }
        $server.WaitForExit()
        $server.Dispose()
    }
    if ([IO.Path]::GetFullPath($run) -eq [IO.Path]::GetFullPath((Join-Path $temp $runName))) {
        Remove-Item -LiteralPath $run -Recurse -Force
    }
}
