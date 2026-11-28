[CmdletBinding()]
param([Parameter(Mandatory)][string]$NativeDll)
$ErrorActionPreference = 'Stop'
$library = (Resolve-Path -LiteralPath $NativeDll).Path
$runName = 'remoteops-agent-capture-' + [guid]::NewGuid().ToString('N')
$run = [IO.Directory]::CreateDirectory((Join-Path ([IO.Path]::GetTempPath()) $runName)).FullName
$previousHash = $env:REMOTEOPS_CROSSDESK_CAPTURE_SHA256
$previousChoice = $env:REMOTEOPS_CROSSDESK_CAPTURE
Push-Location (Join-Path $PSScriptRoot '../..')
try {
    $env:REMOTEOPS_CROSSDESK_CAPTURE_SHA256 = (Get-FileHash -LiteralPath $library).Hash
    $artifacts = & cargo test --locked -p remoteops-agent --features paired-human-desktop --lib --no-run --message-format=json |
        ForEach-Object { ConvertFrom-Json -InputObject $_ } |
        Where-Object { $_.reason -eq 'compiler-artifact' -and $_.profile.test -and $_.target.name -eq 'remoteops_agent' -and $_.executable }
    if ($LASTEXITCODE -or @($artifacts).Count -ne 1) { throw '未能定位本次编译的 Agent 测试程序' }
    $program = Join-Path $run 'agent-tests.exe'
    Copy-Item -LiteralPath $artifacts.executable -Destination $program
    Copy-Item -LiteralPath $library -Destination (Join-Path $run 'remoteops-crossdesk-native.dll')
    $env:REMOTEOPS_CROSSDESK_CAPTURE = '1'
    & $program 'observing_receives_a_decodable_frame_after_each_open' --ignored --nocapture --test-threads=1
    if ($LASTEXITCODE) { throw 'CrossDesk 现场采集和解码回归失败' }
} finally {
    Pop-Location
    $env:REMOTEOPS_CROSSDESK_CAPTURE_SHA256 = $previousHash
    $env:REMOTEOPS_CROSSDESK_CAPTURE = $previousChoice
    if ([IO.Path]::GetFullPath($run) -eq [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetTempPath()) $runName))) {
        Remove-Item -LiteralPath $run -Recurse -Force
    }
}
