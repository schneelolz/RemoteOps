[CmdletBinding()]
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('remoteops-control-' + [guid]::NewGuid().ToString('N'))
$relayProcess = $null
$envNames = @('REMOTEOPS_HUMAN_CONTROLLER_TOKEN','REMOTEOPS_AI_CONTROLLER_TOKEN','REMOTEOPS_CONTROLLER_OWNER_ID','CONTROL_SMOKE_ADDRESS','CONTROL_SMOKE_CERT','REMOTEOPS_ADMIN_TOKEN','REMOTEOPS_ADMIN_USERNAME','REMOTEOPS_ADMIN_PASSWORD','REMOTEOPS_VISUAL_PROVIDER_ENABLED','CONTROL_SMOKE_MCP_EXECUTABLE')
$previous = @{}
foreach ($name in $envNames) { $previous[$name] = [Environment]::GetEnvironmentVariable($name) }
function Get-FreePort {
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try { $listener.Start(); return $listener.LocalEndpoint.Port } finally { $listener.Stop() }
}
try {
    if (-not $SkipBuild) {
        Push-Location $workspace
        try {
            cargo build --locked -p remoteops-relay -p remoteops-controller-mcp -p remoteops-control-smoke
            if ($LASTEXITCODE -ne 0) { throw '控制权限专项构建失败' }
        } finally { Pop-Location }
    }
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = [guid]::NewGuid().ToString('N') + [guid]::NewGuid().ToString('N')
    $env:REMOTEOPS_AI_CONTROLLER_TOKEN = [guid]::NewGuid().ToString('N') + [guid]::NewGuid().ToString('N')
    $env:REMOTEOPS_CONTROLLER_OWNER_ID = [guid]::NewGuid().ToString()
    $env:REMOTEOPS_ADMIN_TOKEN = $null
    $env:REMOTEOPS_ADMIN_USERNAME = $null
    $env:REMOTEOPS_ADMIN_PASSWORD = $null
    $env:REMOTEOPS_VISUAL_PROVIDER_ENABLED = $null
    $env:CONTROL_SMOKE_MCP_EXECUTABLE = Join-Path $workspace 'target/debug/remoteops-controller-mcp.exe'
    $env:CONTROL_SMOKE_ADDRESS = '127.0.0.1:' + (Get-FreePort)
    $env:CONTROL_SMOKE_CERT = Join-Path $testRoot 'cert.pem'
    $healthAddress = '127.0.0.1:' + (Get-FreePort)
    $relayExe = Join-Path $workspace 'target/debug/remoteops-relay.exe'
    $smokeExe = Join-Path $workspace 'target/debug/remoteops-control-smoke.exe'
    $liveExe = Join-Path $workspace 'target/debug/remoteops-control-live.exe'
    # 只启动本次专属的回环 Relay；凭据不放入参数或输出。
    $arguments = @('--bind',$env:CONTROL_SMOKE_ADDRESS,'--health-bind',$healthAddress,'--tls-cert',('"'+$env:CONTROL_SMOKE_CERT+'"'),'--tls-key',('"'+(Join-Path $testRoot 'key.pem')+'"'),'--state-file',('"'+(Join-Path $testRoot 'state.json')+'"'))
    $relayProcess = Start-Process -FilePath $relayExe -ArgumentList $arguments -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $testRoot 'relay.out') -RedirectStandardError (Join-Path $testRoot 'relay.err')
    $ready = $false
    for ($attempt=0; $attempt -lt 50; $attempt++) {
        if ($relayProcess.HasExited) { throw '专项 Relay 提前退出；未输出可能含运行数据的日志' }
        if (Test-Path -LiteralPath $env:CONTROL_SMOKE_CERT) {
            $tcp = [Net.Sockets.TcpClient]::new()
            try { $tcp.Connect('127.0.0.1',[int]($env:CONTROL_SMOKE_ADDRESS.Split(':')[1])); $ready=$true; break } catch {} finally { $tcp.Dispose() }
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not $ready) { throw '专项 Relay 启动超时' }
    & $smokeExe
    if ($LASTEXITCODE -ne 0) { throw '控制权限 TLS 专项测试失败' }
    & $liveExe
    if ($LASTEXITCODE -ne 0) { throw '真实 MCP 与 Agent 控制权限测试失败' }
}
finally {
    if ($null -ne $relayProcess) {
        try {
            if (-not $relayProcess.HasExited) { Stop-Process -Id $relayProcess.Id -Force }
            if (-not $relayProcess.WaitForExit(5000)) { throw '专项 Relay 退出超时' }
        } finally { $relayProcess.Dispose() }
    }
    foreach ($name in $envNames) { [Environment]::SetEnvironmentVariable($name,$previous[$name]) }
    if (Test-Path -LiteralPath $testRoot) {
        $resolved = [IO.Path]::GetFullPath($testRoot)
        $tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
        if (-not $resolved.StartsWith($tempBase,[StringComparison]::OrdinalIgnoreCase) -or (Split-Path -Leaf $resolved) -notlike 'remoteops-control-*') { throw '拒绝清理非本次临时目录' }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
