[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$OutputDirectory,
    [string]$PackagePath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$version = '10.4.0.0p1-Preview'
$expectedArchiveSha256 = 'b67ff8becd203502190ec42ac7787c24c2cd3325ed7700485fe7a1c83d2a7957'
$sourceUrl = "https://github.com/PowerShell/Win32-OpenSSH/releases/download/$version/OpenSSH-Win64.zip"
$workspaceRoot = Split-Path -Parent $PSScriptRoot
$cache = Join-Path $workspaceRoot "artifacts/third-party/openssh-$version"
New-Item -ItemType Directory -Path $cache -Force | Out-Null
if ([string]::IsNullOrWhiteSpace($PackagePath)) {
    $PackagePath = Join-Path $cache 'OpenSSH-Win64.zip'
    if (-not (Test-Path -LiteralPath $PackagePath)) {
        Invoke-WebRequest -Uri $sourceUrl -OutFile $PackagePath
    }
}
$PackagePath = [IO.Path]::GetFullPath($PackagePath)
if ((Get-FileHash -LiteralPath $PackagePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expectedArchiveSha256) {
    throw '微软官方 OpenSSH 包 SHA-256 不匹配；停止构建。'
}
$unpack = Join-Path $cache ('verify-' + [Guid]::NewGuid().ToString('N'))
Expand-Archive -LiteralPath $PackagePath -DestinationPath $unpack
$source = Join-Path $unpack 'OpenSSH-Win64'
$files = @('ssh-keyscan.exe', 'libcrypto.dll', 'LICENSE.txt', 'NOTICE.txt')
$signatures = @{}
foreach ($name in @('ssh-keyscan.exe', 'libcrypto.dll')) {
    $signature = Get-AuthenticodeSignature -LiteralPath (Join-Path $source $name)
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation(?:,|$)') {
        throw "组件微软签名校验失败：$name；停止构建。"
    }
    $signatures[$name] = [ordered]@{
        status = [string]$signature.Status
        signer = $signature.SignerCertificate.Subject
        certificate_thumbprint = $signature.SignerCertificate.Thumbprint
    }
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw '缺少 vswhere，不能完成 PE 依赖核验。' }
$vsRoot = & $vswhere -latest -products '*' -property installationPath
$dumpbin = Get-ChildItem -Path (Join-Path $vsRoot 'VC/Tools/MSVC/*/bin/Hostx64/x64/dumpbin.exe') |
    Sort-Object FullName | Select-Object -Last 1 -ExpandProperty FullName
if (-not $dumpbin) { throw '缺少 dumpbin，不能完成 PE 依赖核验。' }
$dependencies = @{}
$allowed = @{
    'ssh-keyscan.exe' = @('libcrypto.dll', 'USERENV.dll', 'CRYPT32.dll', 'WS2_32.dll', 'KERNEL32.dll', 'USER32.dll', 'ADVAPI32.dll')
    'libcrypto.dll' = @('WS2_32.dll', 'ntdll.dll', 'bcrypt.dll', 'KERNEL32.dll')
}
foreach ($name in @('ssh-keyscan.exe', 'libcrypto.dll')) {
    $output = & $dumpbin /dependents (Join-Path $source $name) 2>&1
    if ($LASTEXITCODE -ne 0) { throw "读取 PE 依赖失败：$name" }
    $actual = @($output | ForEach-Object {
        if ([string]$_ -match '^\s+([A-Za-z0-9_.-]+\.dll)\s*$') { $Matches[1] }
    })
    if ($actual.Count -eq 0 -or @(Compare-Object ($actual | Sort-Object) ($allowed[$name] | Sort-Object)).Count -ne 0) {
        throw "PE 依赖集合与审核记录不一致：$name；停止构建。"
    }
    $dependencies[$name] = $actual
}
$resolvedOutput = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $resolvedOutput -Force | Out-Null
foreach ($name in $files) {
    if (-not (Test-Path -LiteralPath (Join-Path $source $name) -PathType Leaf)) { throw "官方包缺少组件：$name" }
    Copy-Item -LiteralPath (Join-Path $source $name) -Destination (Join-Path $resolvedOutput $name) -Force
}
# 仅给测试子进程设置隔离环境，不修改用户或系统 PATH。
$start = [Diagnostics.ProcessStartInfo]::new()
$start.FileName = Join-Path $resolvedOutput 'ssh-keyscan.exe'
$start.ArgumentList.Add('-?')
$start.WorkingDirectory = $resolvedOutput
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$start.Environment['PATH'] = Join-Path $env:SystemRoot 'System32'
$process = [Diagnostics.Process]::Start($start)
$stdout = $process.StandardOutput.ReadToEndAsync()
$stderr = $process.StandardError.ReadToEndAsync()
if (-not $process.WaitForExit(5000)) {
    $process.Kill($true)
    $process.WaitForExit()
    throw '官方扫描器隔离启动超时；停止构建。'
}
$diagnostic = $stdout.GetAwaiter().GetResult() + $stderr.GetAwaiter().GetResult()
if ($process.ExitCode -ne 1 -or $diagnostic -notmatch 'usage: ssh-keyscan') {
    throw "官方扫描器隔离启动失败，退出码：$($process.ExitCode)；停止构建。"
}
$manifest = [ordered]@{
    component = 'Microsoft Win32-OpenSSH host key scanner'
    version = $version
    preview = $true
    source_url = $sourceUrl
    archive_sha256 = $expectedArchiveSha256
    verified_at = [DateTime]::UtcNow.ToString('o')
    signatures = $signatures
    pe_dependencies = $dependencies
    isolated_startup = [ordered]@{ passed = $true; exit_code = $process.ExitCode; path = 'Windows/System32 only' }
    windows_version_tested = [Environment]::OSVersion.VersionString
    server_2016 = 'not_verified'
    files = @($files | ForEach-Object {
        [ordered]@{ name = $_; sha256 = (Get-FileHash -LiteralPath (Join-Path $resolvedOutput $_) -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $resolvedOutput 'scanner-manifest.json') -Encoding utf8NoBOM
Write-Output "微软官方扫描器门槛通过：$version；摘要、签名、PE 依赖、隔离启动均已验证。"
