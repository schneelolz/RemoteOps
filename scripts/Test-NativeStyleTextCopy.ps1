#requires -Version 7.2
<# .SYNOPSIS 验证自启只读 EDIT 的原生复制；完整保留可安全克隆的剪贴板格式，不记录个人内容。 #>
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedHash,[Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
if([Threading.Thread]::CurrentThread.GetApartmentState() -ne 'STA'){throw '复制验收必须使用 STA PowerShell。'}
$exe=(Resolve-Path -LiteralPath $Executable).Path;$output=[IO.Path]::GetFullPath($OutputDirectory)
$tempRoot=[IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp')).TrimEnd('\')
if(-not $IsWindows -or -not $tempRoot.StartsWith('C:\',[StringComparison]::OrdinalIgnoreCase) -or -not $output.StartsWith($tempRoot+'\',[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $output)){throw '必须使用全新的 C 盘本地 Temp 子目录。'}
for($ancestor=Split-Path -Parent $output;$ancestor;$ancestor=Split-Path -Parent $ancestor){if((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor).Attributes -band [IO.FileAttributes]::ReparsePoint)){throw '测试目录不能经过重解析点。'}}
if((Get-FileHash -LiteralPath $exe).Hash -ine $ExpectedHash -or [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion -notin @('0.3.6','0.3.7')){throw '测试只接受精确摘要对应的 0.3.6／0.3.7。'}
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;using System.Runtime.InteropServices;
public static class StyleCopyNative{
 [DllImport("user32.dll")]public static extern uint GetClipboardSequenceNumber();
 [DllImport("user32.dll")]static extern IntPtr GetClipboardOwner();
 [DllImport("user32.dll")]public static extern uint GetWindowThreadProcessId(IntPtr w,out uint pid);
 [DllImport("user32.dll",EntryPoint="SendMessageTimeoutW",SetLastError=true)]static extern IntPtr SendMessageTimeout(IntPtr w,uint m,IntPtr a,IntPtr b,uint flags,uint timeout,out IntPtr result);
 public static uint Owner(){uint pid;GetWindowThreadProcessId(GetClipboardOwner(),out pid);return pid;}
 public static void Send(IntPtr w,uint expected,uint m,int a,int b){uint actual;GetWindowThreadProcessId(w,out actual);if(actual!=expected)throw new InvalidOperationException("Own EDIT identity changed");IntPtr result;if(SendMessageTimeout(w,m,(IntPtr)a,(IntPtr)b,2,5000,out result)==IntPtr.Zero)throw new InvalidOperationException("Own EDIT message timed out");}
}
'@
$helper=Join-Path $PSScriptRoot 'Test-NativeStylePreview.ps1'
. (Join-Path $PSScriptRoot 'NativeStyle-DesktopGate.ps1')
New-Item -ItemType Directory -Path $output|Out-Null
$checks=[Collections.Generic.List[object]]::new();$failure=$null;$app=$null;$restored=$false;$copyRequested=$false;$externalChange=$false;$copySequence=$null
$backup=[Windows.Forms.DataObject]::new();$fingerprints=@{};$disposables=[Collections.Generic.List[IDisposable]]::new();$wasEmpty=$false
function Fingerprint($Value){
    $bytes=if($Value -is [string]){[Text.Encoding]::Unicode.GetBytes($Value)}elseif($Value -is [IO.MemoryStream]){$Value.ToArray()}elseif($Value -is [byte[]]){$Value}elseif($Value -is [string[]]){[Text.Encoding]::Unicode.GetBytes([string]::Join([char]0,$Value))}else{throw 'unsupported_clipboard_format'}
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([byte[]]$bytes))
}
function Check([string]$Name,[bool]$Passed){$checks.Add([pscustomobject]@{name=$Name;passed=$Passed});if(-not $Passed){throw $Name}}
function Client([string]$Action='Snapshot',[int]$Id=0){$json=& $helper -ExpectedVersion ([Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion) -Action $Action -ControlId $Id -ProcessId $app.Id -ProcessStartUtc $identity -ExpectedHash $ExpectedHash -FocusBeforeInvoke:($Action -eq 'Invoke');if(-not $? -or $LASTEXITCODE){throw 'own_client_failed'};return $json|ConvertFrom-Json}
try{
    Assert-NativeStyleDesktopUnlocked
    $sequence=[StyleCopyNative]::GetClipboardSequenceNumber();$original=[Windows.Forms.Clipboard]::GetDataObject();$wasEmpty=$null -eq $original -or @($original.GetFormats($false)).Count -eq 0
    if(-not $wasEmpty){foreach($format in $original.GetFormats($false)){
        $value=$original.GetData($format,$false)
        if($value -is [string]){$copy=$value}elseif($value -is [IO.MemoryStream]){$copy=[IO.MemoryStream]::new([byte[]]$value.ToArray());$disposables.Add($copy)}elseif($value -is [byte[]] -or $value -is [string[]]){$copy=$value.Clone()}else{throw 'unsupported_clipboard_format_no_write'}
        $fingerprints[$format]=Fingerprint $value;$backup.SetData($format,$false,$copy)
    }}
    Check 'clipboard_backup_stable_before_write' ($sequence -eq [StyleCopyNative]::GetClipboardSequenceNumber())
    $start=[Diagnostics.ProcessStartInfo]::new($exe);$start.UseShellExecute=$false;$start.WorkingDirectory=$output
    foreach($name in @('APPDATA','LOCALAPPDATA','TEMP','TMP')){$dir=Join-Path $output $name.ToLowerInvariant();New-Item -ItemType Directory -Path $dir|Out-Null;$start.Environment[$name]=$dir}
    foreach($key in @($start.Environment.Keys)){if($key -match '^(REMOTEOPS_|JLAI_)'){$null=$start.Environment.Remove($key)}}
    Assert-NativeStyleDesktopUnlocked
    $app=[Diagnostics.Process]::Start($start);$identity=$app.StartTime.ToUniversalTime().ToString('o');$null=$app.WaitForInputIdle(5000)
    $null=Client 'Invoke' 338;$state=Client;$row=@($state.windows.controls|Where-Object id -eq 202)[0];$handle=[IntPtr]([long]$row.hwnd)
    $owner=0u;$null=[StyleCopyNative]::GetWindowThreadProcessId($handle,[ref]$owner);Check 'readonly_edit_identity' ($owner -eq $app.Id -and $row.read_only -eq $true)
    $expected=$row.text.Substring(0,[math]::Min(24,$row.text.Length))
    Assert-NativeStyleDesktopUnlocked
    [StyleCopyNative]::Send($handle,[uint32]$app.Id,0xb1,0,$expected.Length)
    Check 'clipboard_unchanged_until_copy' ($sequence -eq [StyleCopyNative]::GetClipboardSequenceNumber())
    Assert-NativeStyleDesktopUnlocked
    $copyRequested=$true;[StyleCopyNative]::Send($handle,[uint32]$app.Id,0x301,0,0);$copySequence=[StyleCopyNative]::GetClipboardSequenceNumber()
    Check 'copied_clipboard_owned_by_test' ([StyleCopyNative]::Owner() -eq $app.Id)
    Check 'native_copy_exact_unicode_selection' ([Windows.Forms.Clipboard]::GetText([Windows.Forms.TextDataFormat]::UnicodeText) -ceq $expected)
    $state=Client;Check 'copy_does_not_modify_readonly_text' (@($state.windows.controls|Where-Object id -eq 202)[0].text -ceq $row.text)
}catch{$failure=@{category='text_copy_or_fixture_failure';code=$_.Exception.GetType().Name;step=if($checks.Count){$checks[-1].name}else{'backup'}}}
finally{
    if($copyRequested -and $null -ne $copySequence -and [StyleCopyNative]::GetClipboardSequenceNumber() -eq $copySequence){
        # 备份仅在内存；短暂占用时保持备份存活，直到还原或用户主动复制新内容。
        while(-not $restored){
            if([StyleCopyNative]::GetClipboardSequenceNumber() -ne $copySequence){$externalChange=$true;break}
            try{if($wasEmpty){[Windows.Forms.Clipboard]::Clear()}else{[Windows.Forms.Clipboard]::SetDataObject($backup,$true,5,100)};$restored=$true}catch{Start-Sleep -Milliseconds 500}
        }
        if($restored){$after=[Windows.Forms.Clipboard]::GetDataObject();$ok=if($wasEmpty){$null -eq $after}else{$true};if(-not $wasEmpty){foreach($format in $fingerprints.Keys){if(-not $after.GetDataPresent($format,$false) -or (Fingerprint ($after.GetData($format,$false))) -cne $fingerprints[$format]){$ok=$false}}};$checks.Add([pscustomobject]@{name='all_original_clipboard_formats_restored';passed=$ok});if(-not $ok){$failure??=@{category='clipboard_restore_verification_failed'}}}
    }elseif($copyRequested){$externalChange=$true}
    if($null -ne $app -and -not $app.HasExited){try{$null=Client 'Close';$null=Client;$null=Client 'Invoke' 463;$null=$app.WaitForExit(5000)}catch{};if(-not $app.HasExited){$app.Kill();$app.WaitForExit();$failure??=@{category='owned_test_forced_exit'}}}
    foreach($item in $disposables){$item.Dispose()}
    $record=[ordered]@{passed=$null -eq $failure;executable_sha256=$ExpectedHash.ToLowerInvariant();checks=@($checks);clipboard_restored=$restored;restored_format_count=$fingerprints.Count;external_clipboard_change_preserved=$externalChange;personal_clipboard_content_logged=$false;failure=$failure;application_exit_code=if($null -ne $app){$app.ExitCode}else{$null}}
    [IO.File]::WriteAllText((Join-Path $output 'report.json'),($record|ConvertTo-Json -Depth 5),[Text.UTF8Encoding]::new($false));if($null -ne $app){$app.Dispose()}
}
$record|ConvertTo-Json -Depth 5
if(-not $record.passed){exit 1}
