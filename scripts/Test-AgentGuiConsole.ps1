[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../artifacts/gui-console-check')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
if (-not (Test-Path -LiteralPath $resolvedExecutable -PathType Leaf)) {
    throw 'Executable 必须是已构建的 Agent EXE。'
}
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$resolvedOutput = (Resolve-Path -LiteralPath $OutputDirectory).Path
# 独立副本隔离诊断目录；只使用无效配置，绝不连接产品的真实 Relay。
$runDirectory = Join-Path $resolvedOutput ([Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runDirectory | Out-Null
$copiedExecutable = Join-Path $runDirectory ([IO.Path]::GetFileName($resolvedExecutable))
Copy-Item -LiteralPath $resolvedExecutable -Destination $copiedExecutable
$invalidConfig = Join-Path $runDirectory 'invalid-config.json'
[IO.File]::WriteAllText($invalidConfig, '{', [Text.UTF8Encoding]::new($false))
$cases = @(
    @{ Name = 'help'; Arguments = '--cli --help'; Success = $true; Pattern = '--cli' },
    @{ Name = 'version'; Arguments = '--cli --version'; Success = $true; Pattern = '\d+\.\d+\.\d+' },
    @{ Name = 'invalid-config'; Arguments = ('--cli --config "{0}"' -f $invalidConfig); Success = $false; Pattern = '启动失败|Error|error' }
)
$results = foreach ($case in $cases) {
    $stdout = Join-Path $runDirectory ($case.Name + '.stdout.txt')
    $stderr = Join-Path $runDirectory ($case.Name + '.stderr.txt')
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $copiedExecutable
    $info.WorkingDirectory = $runDirectory
    $info.Arguments = $case.Arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    $info.StandardErrorEncoding = [Text.UTF8Encoding]::new($false)
    # 定制产品把诊断写到用户目录；同时隔离其配置和日志，避免使用现场资料。
    foreach ($name in @($info.EnvironmentVariables.Keys)) {
        if ($name -match '^(REMOTEOPS|JLAI)_') { $info.EnvironmentVariables.Remove($name) }
    }
    $info.EnvironmentVariables['LOCALAPPDATA'] = Join-Path $runDirectory 'localappdata'
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $info
    try {
        $null = $process.Start()
        $readOutput = $process.StandardOutput.ReadToEndAsync()
        $readError = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(15000)) {
            $process.Kill()
            $process.WaitForExit()
            throw ('控制台启动超时，可能打开了图形错误对话框：' + $case.Name)
        }
        $process.WaitForExit()
        [IO.File]::WriteAllText($stdout, $readOutput.Result, [Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText($stderr, $readError.Result, [Text.UTF8Encoding]::new($false))
        $output = $readOutput.Result + $readError.Result
        $passed = (($process.ExitCode -eq 0) -eq $case.Success) -and ($output -match $case.Pattern)
        [PSCustomObject]@{ Case = $case.Name; Passed = $passed; ExitCode = $process.ExitCode }
    }
    finally {
        $process.Dispose()
    }
}
$runtimeLogs = @(Get-ChildItem -LiteralPath $runDirectory -Recurse -Filter 'agent-gui-runtime-*.log' -ErrorAction SilentlyContinue)
$diagnostics = ($runtimeLogs | ForEach-Object { [IO.File]::ReadAllText($_.FullName) }) -join "`n"
$consoleSelected = $diagnostics -match 'stage: console_mode_selected(?:\r?\n|$)'
$windowStarted = $diagnostics -match 'stage: (starting_native_window|ui_thread_started)(?:\r?\n|$)'
$results += [PSCustomObject]@{
    Case = 'no-graphics-startup'
    Passed = $consoleSelected -and -not $windowStarted
    ExitCode = $null
}
$results | ConvertTo-Json | Set-Content -Encoding UTF8 -LiteralPath (Join-Path $runDirectory 'results.json')
$results | Format-Table -AutoSize
Write-Output ('验收记录：' + $runDirectory)
if (@($results | Where-Object { -not $_.Passed }).Count -gt 0) {
    throw 'Agent GUI 控制台启动检查失败。'
}
