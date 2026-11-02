#requires -Version 7.2
<#
.SYNOPSIS
仅测试已校验原生预览进程的复制；自己持锁或延迟 WM_DESTROYCLIPBOARD，不读取他人剪贴板。
.DESCRIPTION
独立模式使用 --product-demo；附着模式仅供同一轮本机环回 fixture 调用。
报告只有固定检查名、状态、数值。真实剪贴板仅在 owner 属于本测试/目标 PID 时比较已知值。
#>
[CmdletBinding()]
param(
    [string]$Executable,
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-fA-F]{64}$')] [string]$ExpectedHash,
    [int]$ProcessId = 0,
    [string]$ProcessStartUtc,
    [Parameter(Mandatory)] [string]$OutputDirectory,
    [ValidatePattern('^[A-Z0-9_]{1,80}$')] [string]$FirstMarker,
    [ValidatePattern('^[A-Z0-9_]{1,80}$')] [string]$LastMarker
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$temporary = [IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp'))
$run = [IO.Path]::GetFullPath($OutputDirectory)
if (-not $IsWindows -or -not $temporary.StartsWith('C:\', [StringComparison]::OrdinalIgnoreCase) -or
    -not $run.StartsWith($temporary.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase) -or
    (Test-Path -LiteralPath $run)) { throw '剪贴板验收必须使用 C 盘全新独立临时目录。' }
New-Item -ItemType Directory -Path $run | Out-Null
$clientScript = Join-Path $PSScriptRoot 'NativeGui-TestClient.ps1'
$attached = $ProcessId -ne 0
$checks = [Collections.Generic.List[object]]::new()
$clients = [Collections.Generic.List[object]]::new()
$metrics = @{}
$failure = $null
$step = 'initialization'
$application = $null
$lease = $null
$mainHandle = [IntPtr]::Zero
$expectedCode = $null

# 类型只持有自己创建的窗口和成功取得的锁；没有强制关闭其他 owner 的 API。
Add-Type -TypeDefinition @'
using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public sealed class RemoteOpsClipboardLease : IDisposable {
    delegate IntPtr WindowProcedure(IntPtr h,uint m,IntPtr w,IntPtr l);
    [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)] struct WindowClass {
        public uint style; public WindowProcedure procedure; public int classExtra,windowExtra;
        public IntPtr instance,icon,cursor,background; public string menu,className;
    }
    [StructLayout(LayoutKind.Sequential)] struct Message {public IntPtr h;public uint m;public UIntPtr w;public IntPtr l;public uint time;public int x,y;public uint extra;}
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern ushort RegisterClass(ref WindowClass c);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern bool UnregisterClass(string c,IntPtr i);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern IntPtr CreateWindowEx(uint e,string c,string t,uint s,int x,int y,int w,int h,IntPtr p,IntPtr menu,IntPtr i,IntPtr a);
    [DllImport("user32.dll")] static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr DefWindowProc(IntPtr h,uint m,IntPtr w,IntPtr l);
    [DllImport("user32.dll")] static extern int GetMessage(out Message m,IntPtr h,uint a,uint b);
    [DllImport("user32.dll")] static extern bool TranslateMessage(ref Message m);
    [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref Message m);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h,uint m,IntPtr w,IntPtr l);
    [DllImport("user32.dll")] static extern void PostQuitMessage(int c);
    [DllImport("user32.dll")] static extern bool OpenClipboard(IntPtr h);
    [DllImport("user32.dll")] static extern bool CloseClipboard();
    [DllImport("user32.dll")] static extern bool EmptyClipboard();
    [DllImport("user32.dll")] static extern IntPtr SetClipboardData(uint f,IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetClipboardData(uint f);
    [DllImport("user32.dll")] static extern bool IsClipboardFormatAvailable(uint f);
    [DllImport("user32.dll")] static extern IntPtr GetClipboardOwner();
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);
    [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr h,StringBuilder s,int c);
    [DllImport("user32.dll")] static extern IntPtr SendMessageTimeout(IntPtr h,uint m,UIntPtr w,IntPtr l,uint flags,uint timeout,out UIntPtr r);
    [DllImport("kernel32.dll")] static extern IntPtr GlobalAlloc(uint f,UIntPtr size);
    [DllImport("kernel32.dll")] static extern IntPtr GlobalLock(IntPtr h);
    [DllImport("kernel32.dll")] static extern bool GlobalUnlock(IntPtr h);
    [DllImport("kernel32.dll")] static extern IntPtr GlobalFree(IntPtr h);
    [DllImport("kernel32.dll")] static extern UIntPtr GlobalSize(IntPtr h);
    [DllImport("kernel32.dll",CharSet=CharSet.Unicode)] static extern IntPtr GetModuleHandle(string n);
    readonly ManualResetEventSlim ready=new ManualResetEventSlim(false), release=new ManualResetEventSlim(false);
    readonly ManualResetEventSlim entered=new ManualResetEventSlim(false), delayFinished=new ManualResetEventSlim(false);
    readonly Thread thread; readonly bool delayed; readonly int delayMs; readonly WindowProcedure callback;
    IntPtr window; bool acquired,valid; string className; int disposed;
    public bool Entered {get{return entered.IsSet;}}
    public bool DelayFinished {get{return delayFinished.IsSet;}}
    public const string Synthetic="REMOTEOPS_CLIPBOARD_SYNTHETIC_ONLY";
    public RemoteOpsClipboardLease(bool delayedOwner,int milliseconds) {
        delayed=delayedOwner;delayMs=milliseconds;callback=Procedure;
        thread=new Thread(Run);thread.IsBackground=true;thread.SetApartmentState(ApartmentState.STA);thread.Start();
        if(!ready.Wait(2500)||!valid) {Dispose();throw new InvalidOperationException("fixture_clipboard_acquire_failed");}
    }
    IntPtr Procedure(IntPtr h,uint m,IntPtr w,IntPtr l) {
        if(m==0x307 && delayed) {entered.Set();release.Wait(delayMs);delayFinished.Set();return IntPtr.Zero;}
        if(m==0x10) {DestroyWindow(h);return IntPtr.Zero;}
        if(m==2) {PostQuitMessage(0);return IntPtr.Zero;}
        return DefWindowProc(h,m,w,l);
    }
    void Run() {
        IntPtr instance=GetModuleHandle(null);className="RemoteOpsClipboardFixture-"+Guid.NewGuid().ToString("N");
        try {
            WindowClass c=new WindowClass();c.procedure=callback;c.instance=instance;c.className=className;
            if(RegisterClass(ref c)==0)return;
            window=CreateWindowEx(0,className,"",0,0,0,0,0,new IntPtr(-3),IntPtr.Zero,instance,IntPtr.Zero);
            if(window==IntPtr.Zero)return;
            Stopwatch wait=Stopwatch.StartNew();
            while(!(acquired=OpenClipboard(window))&&wait.ElapsedMilliseconds<500)Thread.Sleep(10);
            if(!acquired)return;
            if(delayed) {
                byte[] data=Encoding.Unicode.GetBytes(Synthetic+"\0");IntPtr memory=GlobalAlloc(2,(UIntPtr)data.Length);
                if(memory==IntPtr.Zero)return;IntPtr pointer=GlobalLock(memory);
                if(pointer==IntPtr.Zero){GlobalFree(memory);return;}
                Marshal.Copy(data,0,pointer,data.Length);GlobalUnlock(memory);
                if(!EmptyClipboard()||SetClipboardData(13,memory)==IntPtr.Zero){GlobalFree(memory);return;}
                CloseClipboard();acquired=false;
            }
            valid=true;ready.Set();
            if(!delayed)release.Wait(15000);
            else {Message message;while(GetMessage(out message,IntPtr.Zero,0,0)>0){TranslateMessage(ref message);DispatchMessage(ref message);}}
        } finally {
            if(acquired){CloseClipboard();acquired=false;}
            if(window!=IntPtr.Zero)DestroyWindow(window);
            UnregisterClass(className,instance);ready.Set();
        }
    }
    public void ReleaseAfter(int milliseconds) {ThreadPool.QueueUserWorkItem(_=>{Thread.Sleep(milliseconds);release.Set();});}
    public void Dispose() {
        if(Interlocked.Exchange(ref disposed,1)!=0)return;
        release.Set();if(window!=IntPtr.Zero)PostMessage(window,0x10,IntPtr.Zero,IntPtr.Zero);
        if(!thread.Join(4500))throw new InvalidOperationException("fixture_clipboard_release_timeout");
    }
    public static bool Ping(IntPtr window,int process) {
        uint owner;GetWindowThreadProcessId(window,out owner);if(owner!=process)return false;
        StringBuilder c=new StringBuilder(128);GetClassName(window,c,c.Capacity);if(c.ToString()!="RemoteOpsNativeMain")return false;
        UIntPtr result;return SendMessageTimeout(window,0,UIntPtr.Zero,IntPtr.Zero,2,250,out result)!=IntPtr.Zero;
    }
    public sealed class Comparison {public bool Opened,OwnerKnown,HasText,Matches,MatchesSynthetic;}
    public static bool MatchesControlCode(string value,string expected) {
        return value!=null&&expected!=null
            &&System.Text.RegularExpressions.Regex.IsMatch(expected,"\\A[0-9]{9}\\z")
            &&System.Text.RegularExpressions.Regex.IsMatch(value,"\\A(?:[0-9]{9}|[0-9]{3}-[0-9]{3}-[0-9]{3})\\z")
            &&value.Replace("-","")==expected;
    }
    public static Comparison Compare(string expected,int target) {
        Comparison result=new Comparison();if(!OpenClipboard(IntPtr.Zero))return result;result.Opened=true;
        try {
            uint owner;GetWindowThreadProcessId(GetClipboardOwner(),out owner);
            result.OwnerKnown=owner==target||owner==Process.GetCurrentProcess().Id;
            result.HasText=IsClipboardFormatAvailable(13);if(!result.HasText||!result.OwnerKnown)return result;
            IntPtr memory=GetClipboardData(13);if(memory==IntPtr.Zero)return result;
            ulong bytes=GlobalSize(memory).ToUInt64();if(bytes<2||bytes>4096)return result;
            IntPtr pointer=GlobalLock(memory);if(pointer==IntPtr.Zero)return result;
            try {string raw=Marshal.PtrToStringUni(pointer,(int)bytes/2);int end=raw.IndexOf('\0');if(end<0)return result;string value=raw.Substring(0,end);result.Matches=MatchesControlCode(value,expected);result.MatchesSynthetic=value==Synthetic;}
            finally{GlobalUnlock(memory);}return result;
        }finally{CloseClipboard();}
    }
}
'@

function Check([string]$Name,[bool]$Passed) {
    $script:step=$Name;$checks.Add([pscustomobject]@{name=$Name;passed=$Passed})
    if(-not $Passed){throw 'clipboard_assertion_failed'}
}
function Start-Client([string]$Action='Snapshot',[int]$Id=0,[bool]$Gated=$false,[int]$FeedbackWait=2200,[bool]$RequireIdle=$true) {
    $start=[Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'))
    $start.UseShellExecute=$false;$start.CreateNoWindow=$true;$start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
    $start.StandardOutputEncoding=[Text.UTF8Encoding]::new($false);$start.StandardErrorEncoding=[Text.UTF8Encoding]::new($false)
    foreach($arg in @('-NoProfile','-STA','-File',$clientScript,'-ProcessId',[string]$ProcessId,'-ProcessStartUtc',$ProcessStartUtc,
        '-ExpectedHash',$ExpectedHash,'-Action',$Action,'-ControlId',[string]$Id,'-CopyFeedbackWaitMilliseconds',[string]$FeedbackWait)){$start.ArgumentList.Add($arg)}
    if($Action -eq 'Close'){$start.ArgumentList.Add('-Value');$start.ArgumentList.Add('Confirm')}
    if($Action -eq 'Invoke' -and $Id -eq 304 -and $RequireIdle){$start.ArgumentList.Add('-RequireCopyIdle')}
    $gate=$null
    if($Gated){$gate=Join-Path $run ('gate-'+[Guid]::NewGuid().ToString('N'));New-Item -ItemType Directory -Path $gate|Out-Null;$start.ArgumentList.Add('-InvokeGateDirectory');$start.ArgumentList.Add($gate)}
    $process=[Diagnostics.Process]::Start($start)
    $state=[pscustomobject]@{process=$process;stdout=$process.StandardOutput.ReadToEndAsync();stderr=$process.StandardError.ReadToEndAsync();gate=$gate}
    $clients.Add($state);return $state
}
function Join-Client($Client) {
    if(-not $Client.process.WaitForExit(15000)){$Client.process.Kill();$Client.process.WaitForExit();throw 'uia_client_timeout'}
    if($Client.process.ExitCode -ne 0){throw 'uia_client_failed'}
    return $Client.stdout.GetAwaiter().GetResult()|ConvertFrom-Json
}
function Snapshot {return Join-Client (Start-Client)}
function Ready($Client) {
    $wait=[Diagnostics.Stopwatch]::StartNew()
    while(-not (Test-Path -LiteralPath (Join-Path $Client.gate 'ready'))){if($wait.ElapsedMilliseconds -ge 10000 -or $Client.process.HasExited){throw 'uia_gate_not_ready'};Start-Sleep -Milliseconds 15}
}
function Go($Client){[IO.File]::WriteAllText((Join-Path $Client.gate 'go'),'go')}
function Fast($Result,[string]$Name){$metrics[$Name+'_invoke_ms']=$Result.invoke_elapsed_ms;$metrics[$Name+'_feedback_ms']=$Result.feedback_wait_ms;$metrics[$Name+'_idle_ms']=$Result.copy_idle_wait_ms;Check $Name ($Result.accepted -and $Result.invoke_elapsed_ms -le 1000)}
function Row($Snapshot,[int]$Id){return @($Snapshot.main.controls|Where-Object id -eq $Id)[0]}
function Ping([string]$Name){Check $Name ([RemoteOpsClipboardLease]::Ping($mainHandle,$ProcessId))}
function Wait-OwnerDelay($Lease){$wait=[Diagnostics.Stopwatch]::StartNew();while(-not $Lease.Entered){if($wait.ElapsedMilliseconds -gt 1000){throw 'destroyclipboard_not_entered'};Start-Sleep -Milliseconds 10}}
function Output-Sequence([string]$Text){$highest=-1;foreach($match in [regex]::Matches($Text,'CLIPBOARD_PARALLEL_ROW_(\d+)_(\d+)')){$highest=[math]::Max($highest,100*[int]$match.Groups[1].Value+[int]$match.Groups[2].Value)};return $highest}
function Test-ControlCodeComparison {
    <# .SYNOPSIS 仅比较合成字符串，不访问剪贴板。 #>
    foreach($sample in @('123456789','123-456-789')){if(-not [RemoteOpsClipboardLease]::MatchesControlCode($sample,'123456789')){throw 'control_code_positive_self_test_failed'}}
    foreach($sample in @('prefix123456789','123456789suffix','123 456 789','123-45-6789','12345678','1234567890',"123456789`n",'１２３４５６７８９','123-456-780','')){if([RemoteOpsClipboardLease]::MatchesControlCode($sample,'123456789')){throw 'control_code_negative_self_test_failed'}}
    if([RemoteOpsClipboardLease]::MatchesControlCode('123-456-789','123-456-789')){throw 'control_code_expected_format_self_test_failed'}
}
function Test-CopyFeedbackClassification {
    <# .SYNOPSIS 从同一客户端提取纯状态函数回归旧终态，不加载 UIA 或操作窗口。 #>
    $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile($clientScript,[ref]$tokens,[ref]$errors)
    if(@($errors).Count){throw 'feedback_client_parse_failed'}
    $definition=$ast.Find({param($node)$node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-CopyFeedbackStatus'},$true)
    if($null -eq $definition){throw 'feedback_function_missing'}
    Invoke-Expression $definition.Extent.Text
    $errorText='复制失败：剪贴板被占用或响应超时，请重试。'
    foreach($sample in @(@('✓','',$false,'unknown'),@('✓','',$true,'success'),@('复制',$errorText,$false,'unknown'),@('复制',$errorText,$true,'failure'),@('…','',$false,'pending'),@('复制','',$false,'unsettled'),@('复制','unrelated error',$true,'unsettled'))){
        if((Get-CopyFeedbackStatus $sample[0] $sample[1] $sample[2]) -cne $sample[3]){throw 'feedback_classification_self_test_failed'}
    }
}

try {
    Test-ControlCodeComparison
    Test-CopyFeedbackClassification
    if(-not $attached){
        if(-not $Executable){throw 'executable_required'}
        $exe=(Resolve-Path -LiteralPath $Executable).Path
        if((Get-FileHash -LiteralPath $exe).Hash -ine $ExpectedHash -or [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductName -cne 'RemoteOps Native UI Prototype'){throw 'application_identity_mismatch'}
        $start=[Diagnostics.ProcessStartInfo]::new($exe);$start.UseShellExecute=$false;$start.WorkingDirectory=$run
        $start.ArgumentList.Add('--product-demo');$start.ArgumentList.Add('--report');$start.ArgumentList.Add((Join-Path $run 'application-report.json'))
        foreach($name in @($start.Environment.Keys)){if($name -match '^(REMOTEOPS|JLAI)_'){$start.Environment.Remove($name)|Out-Null}}
        foreach($name in @('LOCALAPPDATA','APPDATA','TEMP','TMP')){$directory=Join-Path $run $name.ToLowerInvariant();New-Item -ItemType Directory -Path $directory|Out-Null;$start.Environment[$name]=$directory}
        $application=[Diagnostics.Process]::Start($start);$ProcessId=$application.Id;$ProcessStartUtc=$application.StartTime.ToUniversalTime().ToString('o')
        $wait=[Diagnostics.Stopwatch]::StartNew();while($application.MainWindowHandle -eq [IntPtr]::Zero){if($wait.ElapsedMilliseconds -gt 10000 -or $application.HasExited){throw 'main_window_not_ready'};Start-Sleep -Milliseconds 50;$application.Refresh()}
    }elseif(-not $ProcessStartUtc -or -not $FirstMarker -or -not $LastMarker){throw 'attached_identity_and_markers_required'}
    $initial=Snapshot;$mainHandle=[IntPtr]$initial.main_handle;$displayCode=[string](Row $initial 303).text;$expectedCode=$displayCode.Replace('-','')
    Check 'own_fixture_copy_enabled' ([RemoteOpsClipboardLease]::MatchesControlCode($displayCode,$expectedCode) -and (Row $initial 304).enabled)
    if($attached){Check 'real_parallel_output_first_marker' ($initial.observer.visible -and $initial.observer.read_only -and $initial.observer.text.Contains($FirstMarker));$initialOutputSequence=Output-Sequence $initial.observer.text}

    $step='short_busy_case'
    $copy=Start-Client 'Invoke' 304 $true;Ready $copy;$lease=[RemoteOpsClipboardLease]::new($false,0);Go $copy;$lease.ReleaseAfter(350)
    $result=Join-Client $copy;Fast $result 'short_busy_invoke_returns_quickly';Check 'short_busy_then_release_copies' ($result.feedback_status -eq 'success' -and $result.feedback_text -eq '✓')
    $lease.Dispose();$lease=$null;$comparison=[RemoteOpsClipboardLease]::Compare($expectedCode,$ProcessId)
    Check 'real_clipboard_equals_only_fixture_expected_code' ($comparison.Opened -and $comparison.OwnerKnown -and $comparison.Matches)

    $step='persistent_busy_case'
    $copy=Start-Client 'Invoke' 304 $true;Ready $copy;$lease=[RemoteOpsClipboardLease]::new($false,0);Go $copy;Start-Sleep -Milliseconds 300;Ping 'busy_ui_message_loop_responds'
    $result=Join-Client $copy;Fast $result 'persistent_busy_invoke_returns_quickly'
    Check 'persistent_busy_has_bounded_explicit_failure' ($result.feedback_status -eq 'failure' -and $result.feedback_text -ne '✓' -and $result.feedback_wait_ms -ge 1000 -and $result.feedback_wait_ms -le 2000)
    $lease.Dispose();$lease=$null;$retry=Join-Client (Start-Client 'Invoke' 304);Check 'failed_copy_button_can_retry' ($retry.feedback_status -eq 'success')

    $step='repeated_copy_case'
    $repeats=@(Start-Client 'Invoke' 304 $true;Start-Client 'Invoke' 304 $true 2200 $false;Start-Client 'Invoke' 304 $true 2200 $false)
    foreach($copy in $repeats){Ready $copy};$lease=[RemoteOpsClipboardLease]::new($false,0)
    Go $repeats[0];Start-Sleep -Milliseconds 450;Go $repeats[1];Ping 'repeat_copy_ui_remains_responsive';Start-Sleep -Milliseconds 450;Go $repeats[2]
    for($index=0;$index -lt $repeats.Count;$index++){$result=Join-Client $repeats[$index];Fast $result ('repeat_invoke_fast_'+$index);Check ('repeat_deadline_not_extended_'+$index) ($result.feedback_status -eq 'failure')}
    $lease.Dispose();$lease=$null

    $step='destroyclipboard_block_case'
    $copy=Start-Client 'Invoke' 304 $true;Ready $copy;$lease=[RemoteOpsClipboardLease]::new($true,3000);Go $copy;Wait-OwnerDelay $lease
    Check 'actual_destroyclipboard_block_entered' $lease.Entered;Ping 'blocked_emptyclipboard_ui_responds'
    $result=Join-Client $copy;Fast $result 'blocked_emptyclipboard_invoke_returns_quickly'
    Check 'blocked_emptyclipboard_has_explicit_timeout' ($result.feedback_status -eq 'failure' -and $result.feedback_wait_ms -le 2000)
    $wait=[Diagnostics.Stopwatch]::StartNew();while(-not $lease.DelayFinished){if($wait.ElapsedMilliseconds -gt 4000){throw 'destroyclipboard_delay_not_finished'};Start-Sleep -Milliseconds 20}
    Start-Sleep -Milliseconds 200;$after=Snapshot;$comparison=[RemoteOpsClipboardLease]::Compare($expectedCode,$ProcessId)
    Check 'timed_out_copy_never_reports_late_success' ((Row $after 304).text -ne '✓')
    Check 'timed_out_copy_does_not_late_set_clipboard' ($comparison.Opened -and -not $comparison.Matches -and (-not $comparison.HasText -or ($comparison.OwnerKnown -and $comparison.MatchesSynthetic)))
    $lease.Dispose();$lease=$null

    if($attached){
        $during=Snapshot;$duringSequence=Output-Sequence $during.observer.text
        Check 'real_output_progressed_during_copy_contention' ($during.observer.visible -and $during.observer.read_only -and $during.observer.text -cne $initial.observer.text -and $duringSequence -gt $initialOutputSequence)
        $metrics.output_sequence_before=$initialOutputSequence;$metrics.output_sequence_after=$duringSequence
        [IO.File]::WriteAllText((Join-Path $run 'copies-complete'),'ready')
        $wait=[Diagnostics.Stopwatch]::StartNew();do{$last=Snapshot;if($last.observer.text.Contains($LastMarker)){break};if($wait.ElapsedMilliseconds -gt 20000){throw 'parallel_last_marker_timeout'}}while($true)
        Check 'real_parallel_output_reaches_last_marker' $last.observer.text.Contains($LastMarker)
    }else{
        $step='exit_during_blocked_copy_case'
        $copy=Start-Client 'Invoke' 304 $true 0;$closing=Start-Client 'Close' 0 $true;Ready $copy;Ready $closing
        $lease=[RemoteOpsClipboardLease]::new($true,3000);Go $copy;Wait-OwnerDelay $lease;$pending=Join-Client $copy;Fast $pending 'exit_case_copy_invoke_fast'
        $step='exit_during_blocked_copy_close'
        $exitWatch=[Diagnostics.Stopwatch]::StartNew();Go $closing;$nativeExited=$application.WaitForExit(2000);$exitWatch.Stop()
        $metrics.native_exited=$nativeExited;$metrics.exit_milliseconds=$exitWatch.ElapsedMilliseconds
        $metrics.native_exit_code=if($nativeExited){$application.ExitCode}else{$null}
        $null=Join-Client $closing
        Check 'exit_during_blocked_copy_is_bounded' ($nativeExited -and $metrics.exit_milliseconds -le 2000 -and $metrics.native_exit_code -eq 0)
        $wait=[Diagnostics.Stopwatch]::StartNew();while(-not $lease.DelayFinished){if($wait.ElapsedMilliseconds -gt 4000){throw 'exit_owner_delay_timeout'};Start-Sleep -Milliseconds 20}
        $comparison=[RemoteOpsClipboardLease]::Compare($expectedCode,$ProcessId)
        Check 'exited_copy_worker_cannot_late_write' ($application.HasExited -and $comparison.Opened -and -not $comparison.Matches -and (-not $comparison.HasText -or ($comparison.OwnerKnown -and $comparison.MatchesSynthetic)))
        $lease.Dispose();$lease=$null
    }
}catch{
    $category='clipboard_fixture_or_product_failure'
    foreach($known in @('uia_client_timeout','uia_client_failed','uia_gate_not_ready','destroyclipboard_not_entered','destroyclipboard_delay_not_finished','fixture_clipboard_acquire_failed','fixture_clipboard_release_timeout')){if($_.Exception.Message.Contains($known)){$category=$known;break}}
    $failure=[pscustomobject]@{step=$step;category=$category}
}
finally{
    if($null -ne $lease){try{$lease.Dispose()}catch{$failure??=[pscustomobject]@{step='cleanup';category='own_clipboard_release_failed'}}}
    if($null -ne $application){if(-not $application.HasExited){try{$null=Join-Client (Start-Client 'Close');if(-not $application.WaitForExit(8000)){$failure??=[pscustomobject]@{step='cleanup';category='normal_exit_failed'}}}catch{$failure??=[pscustomobject]@{step='cleanup';category='normal_exit_failed'}}};$application.Dispose()}
    foreach($client in $clients){if(-not $client.process.HasExited){$client.process.Kill();$null=$client.process.WaitForExit(3000)};$client.process.Dispose()}
    $report=[pscustomobject]@{passed=$null -eq $failure -and @($checks|Where-Object{-not $_.passed}).Count -eq 0;attached=$attached;executable_sha256=$ExpectedHash.ToLowerInvariant();checks=@($checks);metrics=$metrics;failure=$failure;normal_exit_verified_here=-not $attached}
    [IO.File]::WriteAllText((Join-Path $run 'report.json'),($report|ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
}
$report|ConvertTo-Json -Depth 8
if(-not $report.passed){exit 1}
