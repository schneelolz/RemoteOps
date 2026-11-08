#requires -Version 7.2
<# .SYNOPSIS 仅对本脚本自启的离线产品演示验证观察窗状态与生命周期；运行前须告知用户。 #>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedHash,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$ExpectedVersion,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(7,300)][int]$InitialFocusWaitSeconds = 7,
    [switch]$WaitForManualStart,
    [switch]$RunVisible
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
if (-not $RunVisible) { throw '本脚本会显示一组离线窗口；先告知用户后显式传入 -RunVisible。' }
if (-not $IsWindows -or [Threading.Thread]::CurrentThread.GetApartmentState() -ne 'STA') { throw '需要 Windows / STA。' }
$exe = (Resolve-Path -LiteralPath $Executable).Path
$output = [IO.Path]::GetFullPath($OutputDirectory)
$temporaryRoot = [IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp')).TrimEnd('\')
if (-not $temporaryRoot.StartsWith('C:\', [StringComparison]::OrdinalIgnoreCase) -or
    -not $output.StartsWith($temporaryRoot + '\', [StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $output)) { throw '输出必须是全新 C 盘 Temp 子目录。' }
for ($ancestor = Split-Path -Parent $output; $ancestor; $ancestor = Split-Path -Parent $ancestor) {
    if ((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw '输出路径不能经过链接。' }
}
$version = [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion
if ((Get-FileHash -LiteralPath $exe).Hash -ine $ExpectedHash -or $version -cne $ExpectedVersion) { throw '只接受指定摘要的本轮原生产品。' }
. (Join-Path $PSScriptRoot 'NativeStyle-DesktopGate.ps1')
Assert-NativeStyleDesktopUnlocked
& (Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1') | Out-Null
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
/// <summary>仅操作已校验进程的原生主窗、观察窗及其只读 EDIT。</summary>
public static class ProductObserverProbe {
 delegate bool Callback(IntPtr window, IntPtr data);
 [StructLayout(LayoutKind.Sequential)] struct Scroll { public uint Size,Mask; public int Min,Max; public uint Page; public int Position,Track; }
 /// <summary>只读窗口状态；滚动坐标以原生返回值记录。</summary>
 public sealed class View { public int FirstLine,Horizontal,Vertical,VerticalMaximum,Start,End; public bool AtBottom; public string Text; }
 [DllImport("user32.dll")] static extern bool EnumWindows(Callback callback,IntPtr data);
 [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr window,StringBuilder text,int capacity);
 [DllImport("user32.dll")] static extern bool IsWindow(IntPtr window);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
 [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
 [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr window);
 [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr window,uint flags);
 [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr window,uint command);
 [DllImport("user32.dll")] static extern IntPtr GetDlgItem(IntPtr window,int id);
 [DllImport("user32.dll")] static extern int GetDlgCtrlID(IntPtr window);
 [DllImport("user32.dll")] static extern IntPtr GetParent(IntPtr window);
 [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr window);
 [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr window,int command);
 [DllImport("user32.dll")] static extern IntPtr GetWindowLongPtrW(IntPtr window,int index);
 [DllImport("user32.dll")] static extern bool GetScrollInfo(IntPtr window,int bar,ref Scroll scroll);
 [DllImport("user32.dll",EntryPoint="SendMessageTimeoutW")] static extern IntPtr Send(IntPtr window,uint message,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
 [DllImport("user32.dll",CharSet=CharSet.Unicode,EntryPoint="SendMessageTimeoutW")] static extern IntPtr SendText(IntPtr window,uint message,IntPtr w,string text,uint flags,uint timeout,out IntPtr result);
 [DllImport("user32.dll",CharSet=CharSet.Unicode,EntryPoint="SendMessageTimeoutW")] static extern IntPtr ReadText(IntPtr window,uint message,IntPtr capacity,StringBuilder text,uint flags,uint timeout,out IntPtr result);
 [DllImport("user32.dll")] static extern bool PostMessageW(IntPtr window,uint message,IntPtr w,IntPtr l);
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
 static string Class(IntPtr window) { var text=new StringBuilder(128);GetClassName(window,text,text.Capacity);return text.ToString(); }
 /// <summary>枚举精确 PID 的原生顶层窗口，包括最小化或隐藏窗。</summary>
 public static IntPtr[] Windows(int expected,string type,bool visible) {
  var result=new List<IntPtr>();EnumWindows((window,data)=>{uint pid;GetWindowThreadProcessId(window,out pid);if(pid==expected&&Class(window)==type&&(!visible||IsWindowVisible(window)))result.Add(window);return true;},IntPtr.Zero);return result.ToArray();
 }
 /// <summary>拒绝进程或窗口类别不符的句柄。</summary>
 public static void Own(IntPtr window,int expected,string type) {
  uint pid;GetWindowThreadProcessId(window,out pid);if(!IsWindow(window)||pid!=expected||Class(window)!=type||GetAncestor(window,2)!=window)throw new InvalidOperationException("Own root identity mismatch");
 }
 /// <summary>每次返回当前有效只读正文控件，不能复用主题重建前的句柄。</summary>
 public static IntPtr Edit(IntPtr observer,int expected) {
  Own(observer,expected,"RemoteOpsStylePreviewObserver");var edit=GetDlgItem(observer,202);uint pid;GetWindowThreadProcessId(edit,out pid);
  if(!IsWindow(edit)||pid!=expected||GetParent(edit)!=observer||GetDlgCtrlID(edit)!=202||!Class(edit).Equals("Edit",StringComparison.OrdinalIgnoreCase)||(GetWindowLongPtrW(edit,-16).ToInt64()&0x800)==0)throw new InvalidOperationException("Own readonly EDIT mismatch");return edit;
 }
 static long Message(IntPtr window,uint message,IntPtr w,IntPtr l) {IntPtr result;if(Send(window,message,w,l,2,2000,out result)==IntPtr.Zero)throw new InvalidOperationException("Own message timeout");return result.ToInt64();}
 static int TextLength(IntPtr edit) {long units=Message(edit,0xe,IntPtr.Zero,IntPtr.Zero);if(units<0||units>200000)throw new InvalidOperationException("Fixture length outside bound");return (int)units;}
 static Scroll ReadScroll(IntPtr edit,int bar,long style) {var info=new Scroll{Size=(uint)Marshal.SizeOf<Scroll>(),Mask=23};long flag=bar==0?0x100000:0x200000;if((style&flag)==0)return info;if(!GetScrollInfo(edit,bar,ref info))throw new InvalidOperationException("Visible scroll state unavailable");return info;}
 /// <summary>通过精确原生控件句柄发送按钮通知，不触及全局输入。</summary>
 public static void Button(IntPtr root,int expected,string type,int id) {
  Own(root,expected,type);var button=GetDlgItem(root,id);uint pid;GetWindowThreadProcessId(button,out pid);
  if(!IsWindow(button)||pid!=expected||GetParent(button)!=root||GetDlgCtrlID(button)!=id||!Class(button).Equals("Button",StringComparison.OrdinalIgnoreCase))throw new InvalidOperationException("Own button mismatch");
  Message(root,0x111,(IntPtr)id,button);
 }
 /// <summary>只允许本次测试使用的主题及后台自动显示菜单命令。</summary>
 public static void Command(IntPtr main,int expected,int command) {Own(main,expected,"RemoteOpsStylePreviewMain");if(command!=5010&&command!=5011&&command!=5012&&command!=5043)throw new InvalidOperationException("Command not allowed");Message(main,0x111,(IntPtr)command,IntPtr.Zero);}
 /// <summary>把本次主窗带到前台，建立自动显示前的可验证焦点基线。</summary>
 public static void FocusMain(IntPtr main,int expected) {Own(main,expected,"RemoteOpsStylePreviewMain");ShowWindow(main,9);SetForegroundWindow(main);}
 /// <summary>前台必须属于指定 PID，且其原生根窗口恰好为预期窗口。</summary>
 public static bool Foreground(IntPtr root,int expected) {var current=GetForegroundWindow();uint pid;GetWindowThreadProcessId(current,out pid);return IsWindow(current)&&pid==expected&&GetAncestor(current,2)==root;}
 /// <summary>查询前台 PID，仅用于前后景证据。</summary>
 public static uint ForegroundPid() {uint pid;GetWindowThreadProcessId(GetForegroundWindow(),out pid);return pid;}
 /// <summary>只发送最小化、还原或最大化系统命令。</summary>
 public static void State(IntPtr root,int expected,string type,int state) {Own(root,expected,type);if(state!=0xf020&&state!=0xf120&&state!=0xf030)throw new InvalidOperationException("State not allowed");if(!PostMessageW(root,0x112,(IntPtr)state,IntPtr.Zero))throw new InvalidOperationException("Own state post failed");}
 /// <summary>关闭指定自启窗口；主窗退出仍经过程序原有确认框。</summary>
 public static void Close(IntPtr root,int expected,string type) {Own(root,expected,type);if(!PostMessageW(root,0x10,IntPtr.Zero,IntPtr.Zero))throw new InvalidOperationException("Own close failed");}
 /// <summary>返回窗口归属和扩展样式，不能据此声称已看到任务栏缩略图。</summary>
 public static long[] Styles(IntPtr observer,int expected) {Own(observer,expected,"RemoteOpsStylePreviewObserver");return new[]{GetWindow(observer,4).ToInt64(),GetWindowLongPtrW(observer,-16).ToInt64(),GetWindowLongPtrW(observer,-20).ToInt64()};}
 /// <summary>只向本次暂停的演示正文写入明确合成的原生测试夹具。</summary>
 public static void Fixture(IntPtr observer,int expected,string text) {var edit=Edit(observer,expected);if(text==null||!text.StartsWith("[离线合成夹具]",StringComparison.Ordinal)||text.Length>200000)throw new InvalidOperationException("Fixture guard rejected");IntPtr result;if(SendText(edit,0xc,IntPtr.Zero,text,2,2000,out result)==IntPtr.Zero||result==IntPtr.Zero)throw new InvalidOperationException("Fixture text failed");}
 /// <summary>建立历史阅读位置与 UTF-16 选区；只滚动自身 EDIT。</summary>
 public static void Position(IntPtr observer,int expected,int first,int horizontalCharacters,int start,int end) {var edit=Edit(observer,expected);Message(edit,0xb1,(IntPtr)start,(IntPtr)end);int current=(int)Message(edit,0xce,IntPtr.Zero,IntPtr.Zero);Message(edit,0xb6,(IntPtr)horizontalCharacters,(IntPtr)(first-current));}
 /// <summary>将夹具滚动到末尾，用于暂停正文的尾部位置回归。</summary>
 public static void Bottom(IntPtr observer,int expected) {var edit=Edit(observer,expected);int length=TextLength(edit);Message(edit,0xb1,(IntPtr)length,(IntPtr)length);Message(edit,0xb7,IntPtr.Zero,IntPtr.Zero);Message(edit,0x115,(IntPtr)7,IntPtr.Zero);}
 /// <summary>读取正文、选区与两个滚动方向的原生状态。</summary>
 public static View Read(IntPtr observer,int expected) {
  var edit=Edit(observer,expected);int length=TextLength(edit);var text=new StringBuilder(length+1);IntPtr copied;if(ReadText(edit,0xd,(IntPtr)(length+1),text,2,2000,out copied)==IntPtr.Zero)throw new InvalidOperationException("Own text read timeout");if(copied.ToInt64()<0||copied.ToInt64()>length||copied.ToInt64()!=text.Length)throw new InvalidOperationException("Own text read outside bound");
  var memory=Marshal.AllocHGlobal(8);int start,end;try{Message(edit,0xb0,memory,IntPtr.Add(memory,4));start=Marshal.ReadInt32(memory);end=Marshal.ReadInt32(memory,4);}finally{Marshal.FreeHGlobal(memory);}
  long style=GetWindowLongPtrW(edit,-16).ToInt64();var horizontal=ReadScroll(edit,0,style);var vertical=ReadScroll(edit,1,style);
  int maximum=Math.Max(vertical.Min,vertical.Max-(vertical.Page>0?(int)vertical.Page-1:0));return new View{Text=text.ToString(),Start=start,End=end,FirstLine=(int)Message(edit,0xce,IntPtr.Zero,IntPtr.Zero),Horizontal=horizontal.Position,Vertical=vertical.Position,VerticalMaximum=maximum,AtBottom=vertical.Position>=maximum};
 }
}
'@
$oldDpi = [ProductObserverProbe]::SetThreadDpiAwarenessContext([IntPtr](-4))
New-Item -ItemType Directory -Path $output | Out-Null
$helper = Join-Path $PSScriptRoot 'Test-NativeStylePreview.ps1'
$mainClass = 'RemoteOpsStylePreviewMain'; $observerClass = 'RemoteOpsStylePreviewObserver'
$app = $null; $main = [IntPtr]::Zero; $observer = [IntPtr]::Zero; $identity = $null
$checks = [Collections.Generic.List[object]]::new(); $views = [Collections.Generic.List[object]]::new(); $focus = [Collections.Generic.List[object]]::new()
$controlIdentities = [Collections.Generic.List[object]]::new(); $controlBaseline = $null
$failure = $null; $step = 'startup'; $applicationReport = $null; $applicationError = $null; $initialFocusAttempted = $false; $applicationReportReadError = $null
function Assert-Own {
    Assert-NativeStyleDesktopUnlocked
    $app.Refresh()
    if ($app.HasExited -or $app.SessionId -ne [Diagnostics.Process]::GetCurrentProcess().SessionId -or $app.StartTime.ToUniversalTime().ToString('o') -cne $identity -or (Get-FileHash -LiteralPath $app.MainModule.FileName).Hash -ine $ExpectedHash) { throw 'Own process identity changed' }
    if ($main -ne [IntPtr]::Zero) { [ProductObserverProbe]::Own($main, $app.Id, $mainClass) }
    if ($observer -ne [IntPtr]::Zero) { [ProductObserverProbe]::Own($observer, $app.Id, $observerClass) }
}
function Check([string]$Name, [bool]$Passed) { $script:step=$Name; $checks.Add([pscustomobject]@{name=$Name;passed=$Passed}); if (-not $Passed) { throw "检查失败：$Name" } }
function Wait-Own([string]$Name, [scriptblock]$Condition, [int]$TimeoutMilliseconds = 7000) {
    $script:step=$Name; $watch=[Diagnostics.Stopwatch]::StartNew()
    do { Assert-Own; if (& $Condition) { return }; if ($watch.ElapsedMilliseconds -ge $TimeoutMilliseconds) { throw "等待超时：$Name" }; Start-Sleep -Milliseconds 80 } while ($true)
}
function Snapshot {
    Assert-Own
    $start=[Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe')); $start.UseShellExecute=$false; $start.CreateNoWindow=$true; $start.RedirectStandardOutput=$true; $start.RedirectStandardError=$true; $start.StandardOutputEncoding=[Text.UTF8Encoding]::new($false)
    foreach ($arg in @('-NoProfile','-STA','-File',$helper,'-ExpectedVersion',$ExpectedVersion,'-Action','Snapshot','-ProcessId',[string]$app.Id,'-ProcessStartUtc',$identity,'-ExpectedHash',$ExpectedHash)) { $start.ArgumentList.Add($arg) }
    $worker=[Diagnostics.Process]::Start($start); $stdout=$worker.StandardOutput.ReadToEndAsync(); $stderr=$worker.StandardError.ReadToEndAsync()
    try { if (-not $worker.WaitForExit(12000)) { $worker.Kill(); $worker.WaitForExit(); throw 'Own snapshot worker timeout' }; if ($worker.ExitCode) { throw ('Own snapshot failed: '+$stderr.GetAwaiter().GetResult().Trim()) }; return $stdout.GetAwaiter().GetResult() | ConvertFrom-Json } finally { $worker.Dispose() }
}
function Row($State,[int]$Id) { $rows=@($State.windows.controls | Where-Object id -eq $Id); if ($rows.Count -ne 1) { throw "控件不唯一：$Id" }; return $rows[0] }
# Snapshot 自身已验证 PID/StartTime/摘要；这里只比较本次窗口的原生子控件身份。
function Control-Identity($State) {
    $handles=[ordered]@{}
    foreach($id in @(301,338,202,204)){
        $row=Row $State $id
        if($null -eq $row.PSObject.Properties['hwnd'] -or [long]$row.hwnd -eq 0){throw "原生控件句柄不可用：$id"}
        $handles[[string]$id]=[long]$row.hwnd
    }
    return $handles
}
function Control-Reused([string]$Phase,$State) {
    $handles=Control-Identity $State
    $same=$null -ne $controlBaseline -and $handles.Count -eq $controlBaseline.Count
    foreach($key in $handles.Keys){if($null -eq $controlBaseline -or -not $controlBaseline.Contains($key) -or $handles[$key] -ne $controlBaseline[$key]){$same=$false}}
    $controlIdentities.Add([pscustomobject]@{phase=$Phase;native_handles=$handles;matches_baseline=$same})
    return $same
}
function Button([IntPtr]$Root,[string]$Class,[int]$Id) { Assert-Own; [ProductObserverProbe]::Button($Root,$app.Id,$Class,$Id) }
function Command([int]$Id) { Assert-Own; [ProductObserverProbe]::Command($main,$app.Id,$Id) }
function State([IntPtr]$Root,[string]$Class,[int]$Value) { Assert-Own; [ProductObserverProbe]::State($Root,$app.Id,$Class,$Value) }
function Focus-Main {
    Assert-Own
    $first=-not $script:initialFocusAttempted;$script:initialFocusAttempted=$true
    [ProductObserverProbe]::FocusMain($main,$app.Id)
    if($first){
        # 仅记录本次自启主窗身份；用户点击后无需切回聊天。
        $ready=Join-Path $output 'initial-focus-ready.json'
        [IO.File]::WriteAllText($ready,(@{process_id=$app.Id;main_window_handle=$main.ToInt64();state='waiting_for_own_main_foreground'}|ConvertTo-Json),[Text.UTF8Encoding]::new($false))
        Wait-Own 'own_main_foreground' { [ProductObserverProbe]::Foreground($main,$app.Id) } ($InitialFocusWaitSeconds*1000)
        [IO.File]::WriteAllText($ready,(@{process_id=$app.Id;main_window_handle=$main.ToInt64();state='own_main_foreground_confirmed'}|ConvertTo-Json),[Text.UTF8Encoding]::new($false))
    }else{
        Wait-Own 'own_main_foreground' { [ProductObserverProbe]::Foreground($main,$app.Id) }
    }
}
function Record-Focus([string]$Name) { $focus.Add([pscustomobject]@{phase=$Name;foreground_pid=[ProductObserverProbe]::ForegroundPid();main_foreground=[ProductObserverProbe]::Foreground($main,$app.Id);observer_foreground=if($observer -ne [IntPtr]::Zero){[ProductObserverProbe]::Foreground($observer,$app.Id)}else{$false}}) }
function Read-View([string]$Name) { Assert-Own; $value=[ProductObserverProbe]::Read($observer,$app.Id); $views.Add([pscustomobject]@{phase=$Name;first_line=$value.FirstLine;horizontal=$value.Horizontal;vertical=$value.Vertical;vertical_maximum=$value.VerticalMaximum;selection_start=$value.Start;selection_end=$value.End;at_bottom=$value.AtBottom;text_sha256=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::Unicode.GetBytes($value.Text))).ToLowerInvariant()}); return $value }
function Same-View($Before,$After) { return $Before.Text -ceq $After.Text -and $Before.Start -eq $After.Start -and $Before.End -eq $After.End -and $Before.FirstLine -eq $After.FirstLine -and $Before.Horizontal -eq $After.Horizontal }

$taskbarWorker = Join-Path $output 'taskbar-readonly-worker.ps1'
@'
param([int]$ProcessId,[string]$ProcessStartUtc,[string]$ExpectedHash,[long]$MainHandle,[long]$ObserverHandle)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$p=[Diagnostics.Process]::GetProcessById($ProcessId)
if($p.StartTime.ToUniversalTime().ToString('o') -cne $ProcessStartUtc -or (Get-FileHash -LiteralPath $p.MainModule.FileName).Hash -ine $ExpectedHash){throw 'Taskbar own process mismatch'}
Add-Type -AssemblyName UIAutomationClient,UIAutomationTypes
Add-Type -TypeDefinition @"
using System;using System.Collections.Generic;using System.Text;using System.Runtime.InteropServices;
public static class TaskbarReadonlyProbe {
 public delegate bool Callback(IntPtr window,IntPtr data);
 [DllImport("user32.dll")]static extern bool EnumWindows(Callback callback,IntPtr data);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)]static extern int GetClassName(IntPtr window,StringBuilder text,int length);
 [DllImport("user32.dll")]static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
 public static bool Own(IntPtr window,int expected){uint pid;GetWindowThreadProcessId(window,out pid);return pid==expected;}
 public static IntPtr[] Taskbars(){var result=new List<IntPtr>();EnumWindows((window,data)=>{var name=new StringBuilder(128);GetClassName(window,name,name.Capacity);if(name.ToString()=="Shell_TrayWnd"||name.ToString()=="Shell_SecondaryTrayWnd")result.Add(window);return true;},IntPtr.Zero);return result.ToArray();}
}
"@
if(-not [TaskbarReadonlyProbe]::Own([IntPtr]$MainHandle,$ProcessId) -or -not [TaskbarReadonlyProbe]::Own([IntPtr]$ObserverHandle,$ProcessId)){throw 'Taskbar own HWND mismatch'}
$rows=@(foreach($bar in [TaskbarReadonlyProbe]::Taskbars()){
 $root=[System.Windows.Automation.AutomationElement]::FromHandle($bar)
 $buttons=$root.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,[System.Windows.Automation.ControlType]::Button))
 foreach($button in $buttons){$id=$button.Current.AutomationId;if($id -match '^Window:\s*0x([0-9a-fA-F]+)$'){$handle=[Convert]::ToInt64($Matches[1],16);if($handle -in @($MainHandle,$ObserverHandle)){$name=$button.Current.Name;$count=$null;if($name -match '(\d+)\s*(?:个运行窗口|running windows?)'){$count=[int]$Matches[1]};[pscustomobject]@{taskbar_handle=$bar.ToInt64();automation_id=$id;name=$name;bound_own_handle=$handle;window_count=$count}}}}
})
@{matched=@($rows);read_only=$true;actual_taskbar_queried=$true}|ConvertTo-Json -Depth 5
'@ | Set-Content -LiteralPath $taskbarWorker -Encoding utf8NoBOM
function Taskbar([string]$Phase) {
    Assert-Own
    $start=[Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'));$start.UseShellExecute=$false;$start.CreateNoWindow=$true;$start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true;$start.StandardOutputEncoding=[Text.UTF8Encoding]::new($false)
    foreach($arg in @('-NoProfile','-STA','-File',$taskbarWorker,'-ProcessId',[string]$app.Id,'-ProcessStartUtc',$identity,'-ExpectedHash',$ExpectedHash,'-MainHandle',[string]$main.ToInt64(),'-ObserverHandle',[string]$observer.ToInt64())){$start.ArgumentList.Add($arg)}
    $worker=[Diagnostics.Process]::Start($start);$stdout=$worker.StandardOutput.ReadToEndAsync();$stderr=$worker.StandardError.ReadToEndAsync()
    try{if(-not $worker.WaitForExit(12000)){$worker.Kill();$worker.WaitForExit();return @{phase=$Phase;verified=$false;reason='taskbar_uia_timeout'}};if($worker.ExitCode){return @{phase=$Phase;verified=$false;reason='taskbar_uia_unavailable'}};$data=$stdout.GetAwaiter().GetResult()|ConvertFrom-Json;return @{phase=$Phase;verified=$data.matched.Count -gt 0;matched=@($data.matched)}}catch{return @{phase=$Phase;verified=$false;reason='taskbar_uia_unavailable'}}finally{$worker.Dispose()}
}
$taskbarEvidence=[Collections.Generic.List[object]]::new()
try {
    $start=[Diagnostics.ProcessStartInfo]::new($exe);$start.UseShellExecute=$false;$start.CreateNoWindow=$true;$start.WorkingDirectory=$output;$start.RedirectStandardError=$true
    foreach($arg in @('--product-demo','--report',(Join-Path $output 'application-report.json'))){$start.ArgumentList.Add($arg)}
    foreach($key in @('APPDATA','LOCALAPPDATA','TEMP','TMP')){$dir=Join-Path $output $key.ToLowerInvariant();New-Item -ItemType Directory -Path $dir|Out-Null;$start.Environment[$key]=$dir}
    foreach($key in @($start.Environment.Keys)){if($key -match '^(REMOTEOPS_|JLAI_)'){$null=$start.Environment.Remove($key)}}
    $app=[Diagnostics.Process]::Start($start);$applicationError=$app.StandardError.ReadToEndAsync();$identity=$app.StartTime.ToUniversalTime().ToString('o')
    Wait-Own 'main_created' { [ProductObserverProbe]::Windows($app.Id,$mainClass,$true).Count -eq 1 }
    $main=[ProductObserverProbe]::Windows($app.Id,$mainClass,$true)[0]
    $initial=Snapshot;Check 'offline_demo_has_main_and_no_setup' ($initial.windows.controls.id -contains 303 -and $initial.windows.controls.id -notcontains 502)
    if($WaitForManualStart){
        # 只等待本次测试目录中的显式开闸标记，不读取其他应用或全局输入。
        $gate=Join-Path $output 'manual-start.ready'
        [IO.File]::WriteAllText((Join-Path $output 'manual-start-waiting.json'),(@{process_id=$app.Id;main_window_handle=$main.ToInt64();snapshot=$initial}|ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
        Wait-Own 'manual_start_pending' { Test-Path -LiteralPath $gate -PathType Leaf } 300000
    }
    Focus-Main;Record-Focus 'before_automatic_show';Command 5043
    Wait-Own 'observer_created' { [ProductObserverProbe]::Windows($app.Id,$observerClass,$true).Count -eq 1 }
    $observer=[ProductObserverProbe]::Windows($app.Id,$observerClass,$true)[0]
    Record-Focus 'after_automatic_show'
    if([ProductObserverProbe]::ForegroundPid() -ne $app.Id){$script:step='external_foreground_interrupted';throw '其他应用变为前台，本次焦点断言无法验收。'}
    Check 'automatic_show_does_not_activate' ([ProductObserverProbe]::Foreground($main,$app.Id))
    $controlBaseline=Control-Identity (Snapshot)
    $controlIdentities.Add([pscustomobject]@{phase='baseline';native_handles=$controlBaseline;matches_baseline=$true})
    $styles=[ProductObserverProbe]::Styles($observer,$app.Id)
    Check 'observer_has_no_owner' ($styles[0] -eq 0)
    Check 'observer_has_appwindow_style' (($styles[2] -band 0x40000) -ne 0)
    Check 'observer_has_no_toolwindow_style' (($styles[2] -band 0x80) -eq 0)
    Check 'observer_can_minimize_maximize_resize' (($styles[1] -band 0x70000) -eq 0x70000)
    $taskbarEvidence.Add((Taskbar 'observer_visible'))
    State $main $mainClass 0xf020;Wait-Own 'main_minimized' { [ProductObserverProbe]::IsIconic($main) }
    Check 'main_minimize_does_not_hide_observer' ([ProductObserverProbe]::IsWindowVisible($observer) -and -not [ProductObserverProbe]::IsIconic($observer))
    State $main $mainClass 0xf120;Wait-Own 'main_restored' { -not [ProductObserverProbe]::IsIconic($main) }
    Check 'main_restore_reuses_control_hwnds' (Control-Reused 'main_restore' (Snapshot))
    State $observer $observerClass 0xf020;Wait-Own 'observer_minimized' { [ProductObserverProbe]::IsIconic($observer) }
    Check 'observer_minimize_does_not_minimize_main' (-not [ProductObserverProbe]::IsIconic($main))
    Focus-Main;Command 5043;Start-Sleep -Milliseconds 250
    Check 'automatic_event_respects_observer_minimized_choice' ([ProductObserverProbe]::IsIconic($observer) -and [ProductObserverProbe]::Foreground($main,$app.Id))
    Button $main $mainClass 338;Wait-Own 'manual_restore_observer' { -not [ProductObserverProbe]::IsIconic($observer) -and [ProductObserverProbe]::Foreground($observer,$app.Id) }
    Record-Focus 'manual_restore';Check 'manual_entry_restores_and_activates' ([ProductObserverProbe]::IsWindowVisible($observer))
    State $observer $observerClass 0xf030;Wait-Own 'observer_maximized' { [ProductObserverProbe]::IsZoomed($observer) }
    Check 'maximize_reuses_control_hwnds' (Control-Reused 'maximize' (Snapshot))
    State $observer $observerClass 0xf020;Wait-Own 'maximized_observer_minimized' { [ProductObserverProbe]::IsIconic($observer) }
    Focus-Main;Button $main $mainClass 338
    Wait-Own 'manual_restore_maximized_observer' { -not [ProductObserverProbe]::IsIconic($observer) -and [ProductObserverProbe]::IsZoomed($observer) -and [ProductObserverProbe]::Foreground($observer,$app.Id) }
    Check 'manual_restore_keeps_previous_maximized_state' ([ProductObserverProbe]::IsZoomed($observer))
    Check 'max_min_restore_reuses_control_hwnds' (Control-Reused 'max_min_restore' (Snapshot))
    State $observer $observerClass 0xf120;Wait-Own 'observer_restored' { -not [ProductObserverProbe]::IsZoomed($observer) -and -not [ProductObserverProbe]::IsIconic($observer) }
    Check 'maximize_restore_is_independent' (-not [ProductObserverProbe]::IsIconic($main))
    Check 'normal_restore_reuses_control_hwnds' (Control-Reused 'normal_restore' (Snapshot))
    Assert-Own;[ProductObserverProbe]::Close($observer,$app.Id,$observerClass);Wait-Own 'observer_hidden' { -not [ProductObserverProbe]::IsWindowVisible($observer) }
    Check 'close_hides_same_live_observer' ([ProductObserverProbe]::Windows($app.Id,$observerClass,$false).Count -eq 1 -and -not $app.HasExited)
    Focus-Main;$taskbarEvidence.Add((Taskbar 'observer_hidden'))
    Button $main $mainClass 338;Wait-Own 'manual_reopen_observer' { [ProductObserverProbe]::IsWindowVisible($observer) -and [ProductObserverProbe]::Foreground($observer,$app.Id) }
    Check 'manual_reopen_preserves_same_window' ([ProductObserverProbe]::Windows($app.Id,$observerClass,$true)[0] -eq $observer)
    Check 'hide_reopen_reuses_control_hwnds' (Control-Reused 'hide_reopen' (Snapshot))
    Record-Focus 'manual_reopen';$taskbarEvidence.Add((Taskbar 'observer_reshown'))

    # 只改变自启演示中的暂停正文，测试重建状态；不冒充实时业务或大量日志性能验收。
    $state=Snapshot;if((Row $state 204).text -notmatch '继续|Resume'){Button $observer $observerClass 204}
    $state=Snapshot;Check 'fixture_is_paused' ((Row $state 204).text -match '继续|Resume')
    $fixture=(@('[离线合成夹具] 仅用于本次自启窗口的阅读位置验收，不是现场日志。')+@(0..119|ForEach-Object{'{0:D3} | 中文😀 | {1}' -f $_,('长行 ABCDEFGH '*22)})) -join "`r`n"
    Assert-Own;[ProductObserverProbe]::Fixture($observer,$app.Id,$fixture)
    $selection=$fixture.IndexOf('030 | 中文😀',[StringComparison]::Ordinal)+6
    [ProductObserverProbe]::Position($observer,$app.Id,26,18,$selection,$selection+'中文😀'.Length)
    $history=Read-View 'history_before_theme';Check 'fixture_has_history_horizontal_and_unicode_selection' ($history.FirstLine -gt 0 -and $history.Horizontal -gt 0 -and $history.Text.Substring($history.Start,$history.End-$history.Start) -ceq '中文😀' -and -not $history.AtBottom)
    foreach($theme in @(5012,5011,5010)){
        Command $theme;Start-Sleep -Milliseconds 300;$state=Snapshot;$after=Read-View ('history_theme_'+$theme)
        Check ('theme_'+$theme+'_preserves_paused_history_horizontal_selection') (Same-View $history $after)
        Check ('theme_'+$theme+'_keeps_pause') ((Row $state 204).text -match '继续|Resume')
        Check ('theme_'+$theme+'_history_reuses_control_hwnds') (Control-Reused ('history_theme_'+$theme) $state)
    }
    Assert-Own;[ProductObserverProbe]::Bottom($observer,$app.Id);$tail=Read-View 'paused_tail_before_theme';Check 'fixture_is_at_tail' $tail.AtBottom
    foreach($theme in @(5012,5011,5010)){
        Command $theme;Start-Sleep -Milliseconds 300;$state=Snapshot;$after=Read-View ('paused_tail_theme_'+$theme)
        Check ('theme_'+$theme+'_preserves_paused_tail') ($after.AtBottom -and (Same-View $tail $after))
        Check ('theme_'+$theme+'_paused_tail_reuses_control_hwnds') (Control-Reused ('paused_tail_theme_'+$theme) $state)
    }
    Button $observer $observerClass 206;$state=Snapshot;Check 'return_latest_restores_actual_demo_model' ((Row $state 204).text -match '暂停|Pause' -and (Row $state 202).text -notmatch '\[离线合成夹具\]')
    Check 'readonly_property_retained' ((Row $state 202).read_only -eq $true)
    # 非暂停的真实离线演示模型也往返主题；不新增业务记录，不冒充持续实时输出。
    $liveTail=Read-View 'actual_demo_tail_before_theme';Check 'actual_demo_at_tail' $liveTail.AtBottom
    foreach($theme in @(5012,5011,5010)){
        Command $theme;Start-Sleep -Milliseconds 300;$state=Snapshot;$after=Read-View ('actual_demo_tail_theme_'+$theme)
        Check ('theme_'+$theme+'_preserves_unpaused_actual_demo_tail') ($after.AtBottom -and (Same-View $liveTail $after) -and (Row $state 204).text -match '暂停|Pause')
        Check ('theme_'+$theme+'_actual_tail_reuses_control_hwnds') (Control-Reused ('actual_tail_theme_'+$theme) $state)
    }
    $liveSelection=$liveTail.Text.IndexOf('中文',[StringComparison]::Ordinal);Check 'actual_demo_contains_chinese' ($liveSelection -ge 0)
    Assert-Own;[ProductObserverProbe]::Position($observer,$app.Id,$liveTail.FirstLine,0,$liveSelection,$liveSelection+2)
    $liveSelected=Read-View 'actual_demo_selection_before_theme'
    Check 'actual_demo_selected_chinese_at_tail' ($liveSelected.AtBottom -and $liveSelected.Text.Substring($liveSelected.Start,$liveSelected.End-$liveSelected.Start) -ceq '中文')
    foreach($theme in @(5012,5011,5010)){
        Command $theme;Start-Sleep -Milliseconds 300;$state=Snapshot;$after=Read-View ('actual_demo_selection_theme_'+$theme)
        Check ('theme_'+$theme+'_preserves_unpaused_actual_demo_selection') ($after.AtBottom -and (Same-View $liveSelected $after) -and (Row $state 204).text -match '暂停|Pause')
        Check ('theme_'+$theme+'_actual_selection_reuses_control_hwnds') (Control-Reused ('actual_selection_theme_'+$theme) $state)
    }
    Assert-Own;[ProductObserverProbe]::Bottom($observer,$app.Id)

    State $main $mainClass 0xf020;Wait-Own 'main_minimized_before_exit' { [ProductObserverProbe]::IsIconic($main) }
    Assert-Own;[ProductObserverProbe]::Close($main,$app.Id,$mainClass)
    Wait-Own 'minimized_main_exit_confirmation_created' { @([ProductObserverProbe]::Windows($app.Id,'RemoteOpsStylePreviewDialog',$true)).Count -gt 0 }
    $state=Snapshot;$cancel=Row $state 462;$cancelDialog=[IntPtr]([long]@($state.windows|Where-Object {$_.controls.id -contains 462})[0].handle)
    Check 'exit_prompt_restores_minimized_main' (-not [ProductObserverProbe]::IsIconic($main) -and [ProductObserverProbe]::IsWindowVisible($cancelDialog) -and -not [ProductObserverProbe]::IsIconic($cancelDialog))
    Button $cancelDialog 'RemoteOpsStylePreviewDialog' 462
    Wait-Own 'exit_cancelled' { @([ProductObserverProbe]::Windows($app.Id,'RemoteOpsStylePreviewDialog',$true)).Count -eq 0 }
    Check 'cancel_exit_preserves_both_windows_and_process' (-not $app.HasExited -and [ProductObserverProbe]::IsWindowVisible($main) -and [ProductObserverProbe]::IsWindowVisible($observer) -and -not [ProductObserverProbe]::IsIconic($main))
    Assert-Own;[ProductObserverProbe]::Close($main,$app.Id,$mainClass)
    Wait-Own 'exit_confirmation_created' { @([ProductObserverProbe]::Windows($app.Id,'RemoteOpsStylePreviewDialog',$true)).Count -gt 0 }
    $state=Snapshot;$exit=Row $state 463;$dialog=[IntPtr]([long]@($state.windows|Where-Object {$_.controls.id -contains 463})[0].handle)
    Button $dialog 'RemoteOpsStylePreviewDialog' 463
    Check 'process_exits_after_confirm' ($app.WaitForExit(7000))
    Check 'no_native_windows_remain_after_exit' (@([ProductObserverProbe]::Windows($app.Id,$mainClass,$false)).Count -eq 0 -and @([ProductObserverProbe]::Windows($app.Id,$observerClass,$false)).Count -eq 0 -and @([ProductObserverProbe]::Windows($app.Id,'RemoteOpsStylePreviewDialog',$false)).Count -eq 0)
    $applicationReport=Get-Content -Raw -LiteralPath (Join-Path $output 'application-report.json')|ConvertFrom-Json
    Check 'application_is_offline_product_demo' ($applicationReport.environment.product_demo -eq $true -and $applicationReport.environment.real_business_ui -eq $false)
    $critical=@('background_stopped','native_message_loop_completed','background_cleanup_completed')
    foreach($name in $critical){Check ('application_'+$name) (@($applicationReport.checks|Where-Object {$_.name -ceq $name -and $_.passed}).Count -eq 1)}
    $nonPerformanceFailures=@($applicationReport.checks|Where-Object {-not $_.passed -and $_.name -notin @('ui_responsive','product_page_responsive','native_paint_responsive')})
    $performanceFailures=@($applicationReport.checks|Where-Object {-not $_.passed -and $_.name -in @('ui_responsive','product_page_responsive','native_paint_responsive')})
    Check 'background_and_native_cleanup_pass' ($nonPerformanceFailures.Count -eq 0)
    Check 'application_exit_code_matches_report' ($app.ExitCode -eq $(if($applicationReport.passed){0}else{1}) -and ($applicationReport.passed -or $performanceFailures.Count -gt 0))
} catch {
    $caughtError=$_;$foregroundPid=$null;try{$foregroundPid=[ProductObserverProbe]::ForegroundPid()}catch{}
    $failure=[pscustomobject]@{step=$step;category=if($caughtError.Exception.Message -match 'desktop_' -or $step -in @('own_main_foreground','manual_start_pending','external_foreground_interrupted')){'environment_unavailable'}else{'observer_window_or_fixture_failure'};detail=$caughtError.Exception.Message;foreground_pid=$foregroundPid}
} finally {
    if($null -ne $app -and -not $app.HasExited){
        try{Assert-Own;[ProductObserverProbe]::Close($main,$app.Id,$mainClass);Start-Sleep -Milliseconds 150;$s=Snapshot;$dialogs=@($s.windows|Where-Object {$_.controls.id -contains 463});if($dialogs.Count -eq 1){Button ([IntPtr]([long]$dialogs[0].handle)) 'RemoteOpsStylePreviewDialog' 463};$null=$app.WaitForExit(5000)}catch{}
        if(-not $app.HasExited){$app.Kill();$app.WaitForExit();$failure??=[pscustomobject]@{step='cleanup';category='owned_demo_forced_exit';detail='Only the exact self-started demo process was terminated'}}
    }
    if($null -ne $applicationError -and $applicationError.IsCompleted -and -not [string]::IsNullOrWhiteSpace($applicationError.Result)){[IO.File]::WriteAllText((Join-Path $output 'application-error.txt'),$applicationError.Result)}
    if($oldDpi -ne [IntPtr]::Zero){$null=[ProductObserverProbe]::SetThreadDpiAwarenessContext($oldDpi)}
    # 早期环境失败也可能正常完成应用清理；只读原报告，不能把未加载当性能失败。
    if($null -eq $applicationReport -and (Test-Path -LiteralPath (Join-Path $output 'application-report.json') -PathType Leaf)){
        try{$applicationReport=Get-Content -Raw -LiteralPath (Join-Path $output 'application-report.json')|ConvertFrom-Json}catch{$applicationReportReadError='application_report_invalid_or_unreadable'}
    }
    $performancePassed=$null
    if($null -ne $applicationReport){
        $performanceChecks=@($applicationReport.checks|Where-Object {$_.name -in @('ui_responsive','product_page_responsive','native_paint_responsive')})
        if($performanceChecks.Count -gt 0){$performancePassed=@($performanceChecks|Where-Object {-not $_.passed}).Count -eq 0}
    }
    $actualTaskbarVerified=$false
    $taskbarVerification=[Collections.Generic.List[object]]::new()
    $bars=@($taskbarEvidence|ForEach-Object{if($_.ContainsKey('matched')){$_.matched|Select-Object -ExpandProperty taskbar_handle}}|Sort-Object -Unique)
    foreach($bar in $bars){
        $phaseGroups=[Collections.Generic.List[object]]::new()
        $counts=@(foreach($phase in $taskbarEvidence){
            $rows=@(if($phase.ContainsKey('matched')){$phase.matched|Where-Object taskbar_handle -eq $bar})
            $known=@($rows|Where-Object {$null -ne $_.window_count}|Select-Object -ExpandProperty window_count)
            $unique=@($known|Sort-Object -Unique)
            $status=if($rows.Count -eq 0 -or $known.Count -ne $rows.Count){'unknown_or_missing'}elseif($unique.Count -ne 1){'ambiguous_group_counts'}else{'single_reported_group_count'}
            $phaseGroups.Add([pscustomobject]@{phase=$phase.phase;member_reported_counts=@($known);distinct_group_counts=@($unique);status=$status})
            if($status -ceq 'single_reported_group_count'){[int]$unique[0]}else{-1}
        })
        $verified=$counts.Count -eq 3 -and $counts[0] -ge 2 -and $counts[1] -eq ($counts[0]-1) -and $counts[2] -eq $counts[0]
        $taskbarVerification.Add([pscustomobject]@{taskbar_handle=$bar;visible_hidden_reshown_counts=$counts;phase_group_counts=@($phaseGroups);visible_baseline=if($counts.Count -gt 0 -and $counts[0] -ge 0){$counts[0]}else{$null};hidden_delta_from_baseline=if($counts.Count -gt 1 -and $counts[0] -ge 0 -and $counts[1] -ge 0){$counts[1]-$counts[0]}else{$null};reshown_delta_from_baseline=if($counts.Count -gt 2 -and $counts[0] -ge 0 -and $counts[2] -ge 0){$counts[2]-$counts[0]}else{$null};verified=$verified;status=if($verified){'same_taskbar_baseline_minus_one_restored_verified'}elseif(@($phaseGroups|Where-Object status -eq 'ambiguous_group_counts').Count -gt 0){'pending_ambiguous_group_counts'}else{'pending_missing_or_unexpected_same_taskbar_counts'}})
        if($verified){$actualTaskbarVerified=$true}
    }
    $record=[ordered]@{passed=$null -eq $failure -and $performancePassed -eq $true;functional_passed=$null -eq $failure;application_performance_passed=$performancePassed;application_report_loaded=$null -ne $applicationReport;application_report_passed=if($null -ne $applicationReport){$applicationReport.passed}else{$null};application_report_read_error=$applicationReportReadError;initial_focus_wait_seconds=$InitialFocusWaitSeconds;application_exit_code=if($null -ne $app){$app.ExitCode}else{$null};executable_sha256=$ExpectedHash.ToLowerInvariant();product_version=$version;checks=@($checks);view_states=@($views);control_identities=@($controlIdentities);focus=@($focus);taskbar_evidence=@($taskbarEvidence);taskbar_verification=@($taskbarVerification);actual_taskbar_group_count_verified=$actualTaskbarVerified;failure=$failure;demo_only=$true;real_business_verified=$false;rdp_dpi_verified=$false;clipboard_touched=$false;global_mouse_keyboard_injected=$false;fixture_scope='Paused synthetic EDIT plus unpaused actual offline demo model; application page/paint budgets recorded separately; does not prove continuous large-log throughput or real backend operations';limitations=@('Actual taskbar thumbnail switching is not exercised.','Taskbar styles alone are eligibility evidence; absent/unknown Shell UIA counts remain pending.','No real SSH, serial, RDP or display scaling test.')}
    [IO.File]::WriteAllText((Join-Path $output 'report.json'),($record|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($false))
    if($null -ne $app){$app.Dispose()}
}
$record|ConvertTo-Json -Depth 10
if(-not $record.passed){exit 1}
