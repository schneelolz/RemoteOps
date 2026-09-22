param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '../artifacts/service-console-check')
)

$ErrorActionPreference = 'Stop'
$resolvedExecutable = (Resolve-Path -LiteralPath $Executable).Path
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$resolvedOutput = (Resolve-Path -LiteralPath $OutputDirectory).Path
# 每次使用独立目录，保留历史日志，避免覆盖仍在运行的验收。
$runDirectory = Join-Path $resolvedOutput ([Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $runDirectory | Out-Null
$missingConfig = Join-Path $runDirectory 'missing-config.json'
$cases = @(
    @{ Name = 'help'; Arguments = '--help'; Success = $true; Pattern = '--console' },
    @{ Name = 'version'; Arguments = '--version'; Success = $true; Pattern = '0.2.0-preview.8' },
    @{ Name = 'missing-config'; Arguments = ('--console --config "{0}"' -f $missingConfig); Success = $false; Pattern = 'Error' }
)
$results = foreach ($case in $cases) {
    $stdout = Join-Path $runDirectory ($case.Name + '.stdout.txt')
    $stderr = Join-Path $runDirectory ($case.Name + '.stderr.txt')
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $resolvedExecutable
    $info.Arguments = $case.Arguments
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $info
    $null = $process.Start()
    $readOutput = $process.StandardOutput.ReadToEndAsync()
    $readError = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(15000)) {
        $process.Kill()
        throw ('Test timeout: ' + $case.Name)
    }
    $process.WaitForExit()
    [IO.File]::WriteAllText($stdout, $readOutput.Result)
    [IO.File]::WriteAllText($stderr, $readError.Result)
    $output = $readOutput.Result + $readError.Result
    $passed = (($process.ExitCode -eq 0) -eq $case.Success) -and ($output -match [regex]::Escape($case.Pattern))
    [PSCustomObject]@{ Case = $case.Name; Passed = $passed; ExitCode = $process.ExitCode }
}
$results | ConvertTo-Json | Set-Content -Encoding UTF8 -LiteralPath (Join-Path $runDirectory 'results.json')
$results | Format-Table -AutoSize
Write-Output ('Logs: ' + $runDirectory)
if (@($results | Where-Object { -not $_.Passed }).Count -gt 0) { throw 'Service console checks failed' }
