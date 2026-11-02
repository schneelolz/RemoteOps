#requires -Version 7.2
<# .SYNOPSIS 将固定版本 Plink 及许可复制到独立新版目录，不修改旧版。 #>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$SourcePath,
    [Parameter(Mandatory)] [string]$DestinationDirectory,
    [string]$AskpassPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$expected = '50479953865b30775056441b10fdcb984126ba4f98af4f64756902a807b453e7'
$licenceHash = '914288c99f1b97fdfba323ad56cd935b0f195ce399a650ea6b1039abb0e2d879'
$source = (Resolve-Path -LiteralPath $SourcePath).Path
if ((Get-FileHash -LiteralPath $source).Hash -ine $expected) { throw 'Plink 必须匹配已核验的官方 Windows x64 0.83 SHA-256。' }
$destination = [IO.Path]::GetFullPath($DestinationDirectory)
$licence = Join-Path $PSScriptRoot '../third-party/putty/LICENCE.txt'
if ((Get-FileHash -LiteralPath $licence).Hash -ine $licenceHash) { throw 'PuTTY 许可证不匹配已核验的官方文本。' }
$files = [Collections.Generic.List[object]]::new()
$files.Add(@{ Source = $source; Relative = 'tools/plink.exe'; Hash = $expected })
$files.Add(@{ Source = $licence; Relative = 'tools/Plink-LICENCE.txt'; Hash = $licenceHash })
if (-not [string]::IsNullOrWhiteSpace($AskpassPath)) {
    $helper = (Resolve-Path -LiteralPath $AskpassPath).Path
    if ([IO.Path]::GetFileName($helper) -ine 'remoteops-ssh-askpass.exe') { throw 'AskpassPath 必须是本次构建的 remoteops-ssh-askpass.exe。' }
    $start = [Diagnostics.ProcessStartInfo]::new($helper)
    $start.ArgumentList.Add('--protocol-version')
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($start)
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(5000)) {
            $process.Kill($true)
            $process.WaitForExit()
            throw 'Askpass 协议核验超时。'
        }
        if ($process.ExitCode -ne 0 -or $stdout.GetAwaiter().GetResult() -cne 'remoteops-askpass-file-v1' -or
            -not [string]::IsNullOrEmpty($stderr.GetAwaiter().GetResult())) {
            throw 'Askpass 不支持当前安全文件协议，拒绝打包。'
        }
    }
    finally {
        $process.Dispose()
    }
    $files.Add(@{ Source = $helper; Relative = 'remoteops-ssh-askpass.exe'; Hash = (Get-FileHash -LiteralPath $helper).Hash })
}

# 先检查全部目的文件，再写入；不沿链接写到其他安装目录，不覆盖不同组件。
foreach ($file in $files) {
    $target = Join-Path $destination $file.Relative
    for ($ancestor = $target; -not [string]::IsNullOrEmpty($ancestor); $ancestor = Split-Path -Parent $ancestor) {
        if ((Test-Path -LiteralPath $ancestor) -and
            ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw '打包目的路径含链接或重解析点，拒绝写入。'
        }
    }
    if ((Test-Path -LiteralPath $target) -and (Get-FileHash -LiteralPath $target).Hash -ine $file.Hash) {
        throw "目标已存在不同文件，拒绝覆盖：$($file.Relative)"
    }
}
foreach ($file in $files) {
    $target = Join-Path $destination $file.Relative
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $target) | Out-Null
    if (-not (Test-Path -LiteralPath $target)) { Copy-Item -LiteralPath $file.Source -Destination $target }
    if ((Get-FileHash -LiteralPath $target).Hash -ine $file.Hash) { throw '复制后组件摘要不一致。' }
}
$target = Join-Path $destination 'tools/plink.exe'
[pscustomobject]@{
    path = $target; version = '0.83'; sha256 = $expected; bytes = (Get-Item -LiteralPath $target).Length
    licence_sha256 = $licenceHash
    askpass_protocol = if ($AskpassPath) { 'remoteops-askpass-file-v1' } else { $null }
}
