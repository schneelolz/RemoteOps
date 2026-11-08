#requires -Version 7.2
<# .SYNOPSIS 验证实际凭据窗口的双语文案、空输入、取消与记忆选择；仅输入合成密码。 #>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedHash,
    [switch]$Browser,
    [Parameter(Mandatory)][string]$OutputDirectory
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows -or [Threading.Thread]::CurrentThread.GetApartmentState() -ne 'STA') { throw '需要 Windows STA PowerShell。' }
$exe = (Resolve-Path -LiteralPath $Executable).Path
if ((Get-FileHash -LiteralPath $exe).Hash -ine $ExpectedHash) { throw '程序摘要不符。' }
$output = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $output) { throw '输出目录必须不存在。' }
New-Item -ItemType Directory -Path $output | Out-Null
. (Join-Path $PSScriptRoot 'NativeStyle-DesktopGate.ps1')
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
/// <summary>只操作本次安全提示器子进程的窗口，绝不读取密码框内容。</summary>
public static class CredentialWindowProbe {
 /// <summary>窗口与控件的脱敏属性。</summary>
 public sealed class Row { public long Handle; public string Class, Text; }
 /// <summary>原生窗口边界。</summary>
 [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left,Top,Right,Bottom; }
 [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr window,out Rect rect);
 /// <summary>绘制本次测试窗口到受控截图缓冲区。</summary>
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window,IntPtr dc,uint flags);
 /// <summary>验证归属后读取窗口边界。</summary>
 public static int[] Bounds(IntPtr window,uint expected) { Own(window,expected);Rect r;if(!GetWindowRect(window,out r))throw new InvalidOperationException("Window bounds unavailable");return new[]{r.Left,r.Top,r.Right-r.Left,r.Bottom-r.Top}; }
 delegate bool EnumWindow(IntPtr window, IntPtr data);
 [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindow callback, IntPtr data);
 [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr window, EnumWindow callback, IntPtr data);
 [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr window);
 [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder text, int length);
 [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr ReadText(IntPtr window,uint message,IntPtr length,StringBuilder text,uint flags,uint timeout,out IntPtr result);
 [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr SendText(IntPtr window, uint message, IntPtr wparam, string text, uint flags, uint timeout, out IntPtr result);
 [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", SetLastError=true)] static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam, uint flags, uint timeout, out IntPtr result);
 /// <summary>拒绝不属于本次提示器的窗口句柄。</summary>
 public static void Own(IntPtr window, uint expected) { uint actual; GetWindowThreadProcessId(window,out actual); if(actual!=expected) throw new InvalidOperationException("Test window identity changed"); }
 /// <summary>查找指定提示器进程的可见窗口。</summary>
 public static long Main(uint expected) { IntPtr found=IntPtr.Zero; EnumWindows((w,d)=>{uint pid;GetWindowThreadProcessId(w,out pid);if(pid==expected&&IsWindowVisible(w)){found=w;return false;}return true;},IntPtr.Zero);return found.ToInt64(); }
 static Row Read(IntPtr w) { var c=new StringBuilder(256);GetClassName(w,c,c.Capacity);var t=new StringBuilder(1024);if(c.ToString().IndexOf("EDIT",StringComparison.OrdinalIgnoreCase)<0){IntPtr result;if(ReadText(w,0xd,(IntPtr)t.Capacity,t,2,2000,out result)==IntPtr.Zero)throw new InvalidOperationException("Own label read timed out");}return new Row{Handle=w.ToInt64(),Class=c.ToString(),Text=t.ToString()}; }
 /// <summary>读取指定提示器的标签与控件句柄，跳过密码文本。</summary>
 public static Row[] Rows(IntPtr main,uint expected) { Own(main,expected);var rows=new List<Row>{Read(main)};EnumChildWindows(main,(w,d)=>{Own(w,expected);rows.Add(Read(w));return true;},IntPtr.Zero);return rows.ToArray(); }
 /// <summary>仅向本次测试按钮发送点击消息。</summary>
 public static void Click(IntPtr window,uint expected) { Own(window,expected);IntPtr result;if(SendMessage(window,0xf5,IntPtr.Zero,IntPtr.Zero,2,5000,out result)==IntPtr.Zero)throw new InvalidOperationException("Own button timed out"); }
 /// <summary>仅写入测试生成的合成密码。</summary>
 public static void SetSynthetic(IntPtr window,uint expected,string value) { Own(window,expected);IntPtr result;if(SendText(window,0xc,IntPtr.Zero,value,2,5000,out result)==IntPtr.Zero)throw new InvalidOperationException("Own field timed out"); }
}
'@
$checks = [Collections.Generic.List[object]]::new()
function Check([string]$Name, [bool]$Passed) {
    $checks.Add([pscustomobject]@{name=$Name;passed=$Passed})
    if (-not $Passed) { throw ('检查失败：' + $Name) }
}
$failure = $null
try {
    foreach ($language in @('zh-CN', 'en-US')) {
        $zh = $language -eq 'zh-CN'
        $title = if ($Browser) { 'RemoteOps 网页凭据 / Browser credential' } elseif ($zh) { 'RemoteOps SSH 凭据' } else { 'RemoteOps SSH credential' }
        $okTitle = if ($zh) { '仅本次使用' } else { 'Use once' }
        $cancelTitle = if ($zh) { '取消' } else { 'Cancel' }
        $rememberTitle = if ($zh) { '记住 10 分钟（仅保存在内存中）' } else { 'Remember for 10 minutes (stored in memory only)' }
        $emptyTitle = if ($Browser) { '网页密码不能为空 / Password is required' } elseif ($zh) { 'SSH 密码不能为空' } else { 'SSH password cannot be empty' }
        $heading = if ($Browser) { '填写网页密码 / Browser password' } elseif ($zh) { '需要 SSH 凭据' } else { 'SSH credential required' }
        $passwordTitle = if ($Browser) { '网页密码 / Browser password' } elseif ($zh) { 'SSH 密码' } else { 'SSH password' }
        foreach ($mode in $(if ($Browser) { @('cancel', 'use_once') } else { @('cancel', 'use_once', 'remember_10_minutes') })) {
            Assert-NativeStyleDesktopUnlocked
            $start = [Diagnostics.ProcessStartInfo]::new($exe)
            $start.UseShellExecute = $false
            $start.CreateNoWindow = $true
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            $start.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
            foreach ($arg in @('--host','example.invalid','--username','fixture','--command-sha256',('a'*64),'--language',$language)) { $start.ArgumentList.Add($arg) }
            if ($Browser) { foreach ($arg in @('--purpose','browser','--browser-target','fixture-agent | #password')) { $start.ArgumentList.Add($arg) } }
            $app = [Diagnostics.Process]::Start($start)
            $stdout = $app.StandardOutput.ReadToEndAsync()
            $stderr = $app.StandardError.ReadToEndAsync()
            try {
                $deadline = [DateTime]::UtcNow.AddSeconds(15)
                $main = 0L
                $childId = 0u
                do {
                    $children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($app.Id)" | Where-Object Name -eq 'powershell.exe')
                    if ($children.Count -eq 1) {
                        $childId = [uint32]$children[0].ProcessId
                        $main = [CredentialWindowProbe]::Main($childId)
                    }
                    if ($main -or $app.HasExited) { break }
                    Start-Sleep -Milliseconds 100
                } while ([DateTime]::UtcNow -lt $deadline)
                Check ($language+'_'+$mode+'_window') ($main -ne 0)
                $rows = [CredentialWindowProbe]::Rows([IntPtr]$main,$childId)
                Check ($language+'_'+$mode+'_title') ($rows[0].Text -ceq $title)
                foreach ($text in $(if ($Browser) { @($okTitle,$cancelTitle,$heading,$passwordTitle) } else { @($okTitle,$cancelTitle,$rememberTitle,$heading,$passwordTitle) })) {
                    Check ($language+'_'+$mode+'_localized_'+$text) (@($rows | Where-Object Text -CEQ $text).Count -eq 1)
                }
                $ok = @($rows | Where-Object Text -CEQ $okTitle)[0]
                $cancel = @($rows | Where-Object Text -CEQ $cancelTitle)[0]
                if ($mode -eq 'cancel') {
                    [CredentialWindowProbe]::Click([IntPtr]$cancel.Handle,$childId)
                } else {
                    [CredentialWindowProbe]::Click([IntPtr]$ok.Handle,$childId)
                    Start-Sleep -Milliseconds 150
                    Check ($language+'_'+$mode+'_empty_stays_open') (-not $app.HasExited)
                    $rows = [CredentialWindowProbe]::Rows([IntPtr]$main,$childId)
                    Check ($language+'_'+$mode+'_empty_inline') (@($rows | Where-Object Text -CEQ $emptyTitle).Count -eq 1)
                    if ($mode -eq 'use_once') {
                        $bounds = [CredentialWindowProbe]::Bounds([IntPtr]$main,$childId)
                        $bitmap = [Drawing.Bitmap]::new($bounds[2],$bounds[3])
                        $graphics = [Drawing.Graphics]::FromImage($bitmap)
                        $dc = $graphics.GetHdc()
                        try {
                            Check ($language+'_capture_empty_window') ([CredentialWindowProbe]::PrintWindow([IntPtr]$main,$dc,0))
                        } finally { $graphics.ReleaseHdc($dc); $graphics.Dispose() }
                        try { $bitmap.Save((Join-Path $output ($language+'.png')),[Drawing.Imaging.ImageFormat]::Png) } finally { $bitmap.Dispose() }
                    }
                    $password = @($rows | Where-Object Class -Match 'EDIT')[0]
                    [CredentialWindowProbe]::SetSynthetic([IntPtr]$password.Handle,$childId,'synthetic-fixture-only')
                    if ($mode -eq 'remember_10_minutes') {
                        $remember = @($rows | Where-Object Text -CEQ $rememberTitle)[0]
                        [CredentialWindowProbe]::Click([IntPtr]$remember.Handle,$childId)
                    }
                    [CredentialWindowProbe]::Click([IntPtr]$ok.Handle,$childId)
                }
                Check ($language+'_'+$mode+'_exited') ($app.WaitForExit(10000))
                Check ($language+'_'+$mode+'_exit_success') ($app.ExitCode -eq 0)
                $response = $stdout.GetAwaiter().GetResult() | ConvertFrom-Json
                Check ($language+'_'+$mode+'_action') ($response.action -ceq $mode)
                if ($mode -ne 'cancel') { Check ($language+'_'+$mode+'_synthetic_response') ($response.password -ceq 'synthetic-fixture-only') }
                Check ($language+'_'+$mode+'_no_stderr') ([string]::IsNullOrEmpty($stderr.GetAwaiter().GetResult()))
            } finally {
                if (-not $app.HasExited) { $app.Kill($true); $app.WaitForExit() }
                $app.Dispose()
            }
        }
    }
} catch { $failure = @{type=$_.Exception.GetType().Name;reason=$_.Exception.Message} }
$record = [ordered]@{passed=$null -eq $failure;checks=@($checks);failure=$failure;executable_sha256=$ExpectedHash.ToLowerInvariant();version=[Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion;scope='owned_windows_helper_synthetic_credentials_only'}
[IO.File]::WriteAllText((Join-Path $output 'report.json'),($record | ConvertTo-Json -Depth 5),[Text.UTF8Encoding]::new($false))
$record | ConvertTo-Json -Depth 5
if ($failure) { exit 1 }
