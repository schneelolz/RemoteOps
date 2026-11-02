#requires -Version 7.2
<# .SYNOPSIS 仅验证自启样式进程的文本区、滚动、选择与拖拽中的真实屏幕外观。 #>
[CmdletBinding()]
param([Parameter(Mandatory)][Alias('Exe')][string]$Executable,[Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedHash,[Parameter(Mandatory)][string]$OutputDirectory,[switch]$InteractionOnly)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$exe=(Resolve-Path -LiteralPath $Executable).Path
$output=[IO.Path]::GetFullPath($OutputDirectory)
$tempRoot=[IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp')).TrimEnd('\')
if(-not $IsWindows -or -not $tempRoot.StartsWith('C:\',[StringComparison]::OrdinalIgnoreCase) -or -not $output.StartsWith($tempRoot+'\',[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $output)){throw '必须使用全新的 C 盘本地 Temp 子目录。'}
for($ancestor=Split-Path -Parent $output;$ancestor;$ancestor=Split-Path -Parent $ancestor){if((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor).Attributes -band [IO.FileAttributes]::ReparsePoint)){throw '测试目录不能经过重解析点。'}}
if((Get-FileHash -LiteralPath $exe).Hash -ine $ExpectedHash -or [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion -notin @('0.3.6','0.3.7')){throw '测试仅接受精确摘要对应的 0.3.6／0.3.7。'}
$helper=Join-Path $PSScriptRoot 'Test-NativeStylePreview.ps1'
& (Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1')|Out-Null
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;using System.Runtime.InteropServices;using System.Text;
public static class StyleScrollNative {
 public delegate bool Callback(IntPtr w,IntPtr p);
 [StructLayout(LayoutKind.Sequential)]public struct Point{public int X,Y;public Point(int x,int y){X=x;Y=y;}}
 [StructLayout(LayoutKind.Sequential)]public struct Rect{public int Left,Top,Right,Bottom;}
 [StructLayout(LayoutKind.Sequential)]public struct Scroll{public uint cbSize,fMask;public int nMin,nMax;public uint nPage;public int nPos,nTrackPos;}
 [StructLayout(LayoutKind.Sequential)]public struct Bar{public uint cbSize;public Rect rcScrollBar;public int dxyLineButton,xyThumbTop,xyThumbBottom,reserved;[MarshalAs(UnmanagedType.ByValArray,SizeConst=6)]public uint[] rgstate;}
 [StructLayout(LayoutKind.Sequential)]public struct MouseInput{public int X,Y;public uint Data,Flags,Time;public UIntPtr Extra;}
 [StructLayout(LayoutKind.Sequential)]public struct KeyInput{public ushort VirtualKey,Scan;public uint Flags,Time;public UIntPtr Extra;}
 [StructLayout(LayoutKind.Explicit)]public struct InputData{[FieldOffset(0)]public MouseInput Mouse;[FieldOffset(0)]public KeyInput Key;}
 [StructLayout(LayoutKind.Sequential)]public struct Input{public uint Type;public InputData Data;}
 [DllImport("user32.dll")]public static extern uint GetWindowThreadProcessId(IntPtr w,out uint pid);
 [DllImport("user32.dll")]public static extern IntPtr GetAncestor(IntPtr w,uint f);
 [DllImport("user32.dll")]public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")]public static extern bool SetForegroundWindow(IntPtr w);
 [DllImport("user32.dll")]public static extern bool BringWindowToTop(IntPtr w);
 [StructLayout(LayoutKind.Sequential)]public struct MonitorInfo{public uint size;public Rect monitor,work;public uint flags;}
 [DllImport("user32.dll")]static extern IntPtr MonitorFromWindow(IntPtr w,uint flags);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)]static extern bool GetMonitorInfo(IntPtr monitor,ref MonitorInfo info);
 public static Rect WorkArea(IntPtr w){var info=new MonitorInfo{size=(uint)Marshal.SizeOf<MonitorInfo>()};if(!GetMonitorInfo(MonitorFromWindow(w,2),ref info))throw new InvalidOperationException("GetMonitorInfo failed");return info.work;}
 public static Point MoveOwned(IntPtr w,uint pid,int x,int y){AssertWindow(w,pid,false);if(!SetCursorPos(x,y))throw new InvalidOperationException("SetCursorPos failed");Point actual;if(!GetCursorPos(out actual)||actual.X!=x||actual.Y!=y)throw new InvalidOperationException("Requested pointer was clamped; input stopped");if(WindowFromPoint(actual)!=w)throw new InvalidOperationException("Actual pointer target is not own expected window");return actual;}
 [DllImport("user32.dll")]public static extern bool SetWindowPos(IntPtr w,IntPtr after,int x,int y,int cx,int cy,uint flags);
 [DllImport("user32.dll",EntryPoint="GetWindowLongPtrW")]public static extern IntPtr GetStyle(IntPtr w,int index);
 [StructLayout(LayoutKind.Sequential)]public struct Placement{public uint length,flags,showCmd;public Point minimum,maximum;public Rect normal;}
 [DllImport("user32.dll")]static extern bool GetWindowPlacement(IntPtr w,ref Placement p);
 public static Placement Position(IntPtr w){var p=new Placement{length=(uint)Marshal.SizeOf<Placement>()};if(!GetWindowPlacement(w,ref p))throw new InvalidOperationException("GetWindowPlacement failed");return p;}
 [DllImport("user32.dll",CharSet=CharSet.Unicode)]static extern int GetClassName(IntPtr w,StringBuilder s,int count);
 public static string Class(IntPtr w){var s=new StringBuilder(256);GetClassName(w,s,s.Capacity);return s.ToString();}
 [StructLayout(LayoutKind.Sequential)]public struct GuiThread{public uint size,flags;public IntPtr active,focus,capture,menuOwner,moveSize,caret;public Rect caretRect;}
 [DllImport("user32.dll")]static extern bool GetGUIThreadInfo(uint thread,ref GuiThread info);
 public static GuiThread ThreadInfo(IntPtr w){uint pid;var id=GetWindowThreadProcessId(w,out pid);var info=new GuiThread{size=(uint)Marshal.SizeOf<GuiThread>()};if(!GetGUIThreadInfo(id,ref info))throw new InvalidOperationException("GetGUIThreadInfo failed");return info;}
 public static int Menus(uint pid){int count=0;EnumWindows((w,p)=>{uint actual;GetWindowThreadProcessId(w,out actual);if(actual==pid&&IsWindowVisible(w)&&Class(w)=="#32768")count++;return true;},IntPtr.Zero);return count;}
 [DllImport("user32.dll")]public static extern IntPtr WindowFromPoint(Point p);
 [DllImport("user32.dll")]public static extern bool GetClientRect(IntPtr w,out Rect r);
 [DllImport("user32.dll")]public static extern bool ClientToScreen(IntPtr w,ref Point p);
 [DllImport("user32.dll")]public static extern bool GetWindowRect(IntPtr w,out Rect r);
 [DllImport("user32.dll")]public static extern bool GetCursorPos(out Point p);
 [DllImport("user32.dll")]public static extern bool SetCursorPos(int x,int y);
 [DllImport("user32.dll")]public static extern bool IsWindowVisible(IntPtr w);
 [DllImport("user32.dll")]public static extern bool IsIconic(IntPtr w);
 [DllImport("user32.dll")]static extern bool EnumWindows(Callback cb,IntPtr p);
 [DllImport("user32.dll")]public static extern bool GetScrollInfo(IntPtr w,int bar,ref Scroll s);
 [DllImport("user32.dll")]public static extern bool GetScrollBarInfo(IntPtr w,int bar,ref Bar s);
 [DllImport("user32.dll",EntryPoint="SendMessageW")]public static extern IntPtr Send(IntPtr w,uint m,IntPtr a,IntPtr b);
 [DllImport("user32.dll")]static extern uint SendInput(uint count,Input[] data,int size);
 [DllImport("user32.dll")]static extern short GetAsyncKeyState(int key);
 [DllImport("user32.dll")]public static extern uint GetGuiResources(IntPtr process,uint flags);
 [DllImport("user32.dll")]public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr c);
 public static bool MouseHeld;
 public static Scroll Info(IntPtr w,int axis){var s=new Scroll{cbSize=(uint)Marshal.SizeOf<Scroll>(),fMask=0x17};if(!GetScrollInfo(w,axis,ref s))throw new InvalidOperationException("GetScrollInfo failed");return s;}
 public static Bar Geometry(IntPtr w,int axis){var s=new Bar{cbSize=(uint)Marshal.SizeOf<Bar>(),rgstate=new uint[6]};if(!GetScrollBarInfo(w,axis==1?-5:-6,ref s))throw new InvalidOperationException("GetScrollBarInfo failed");return s;}
 public static void AssertWindow(IntPtr w,uint pid,bool foreground){uint actual;GetWindowThreadProcessId(w,out actual);if(actual!=pid||!IsWindowVisible(w))throw new InvalidOperationException("Target is not own visible window");if(foreground&&GetForegroundWindow()!=GetAncestor(w,2))throw new InvalidOperationException("Own root is not foreground");}
 public static void AssertIdleInput(){foreach(var key in new[]{1,2,4,16,17,18,91,92})if((GetAsyncKeyState(key)&0x8000)!=0)throw new InvalidOperationException("User input is active; input test stopped");}
 static void Submit(Input input){var foreground=GetForegroundWindow();uint pid;GetWindowThreadProcessId(foreground,out pid);if(foreground==IntPtr.Zero||pid==0)throw new InvalidOperationException("Interactive desktop unavailable");using(var process=System.Diagnostics.Process.GetProcessById((int)pid)){if(process.ProcessName.Equals("LockApp",StringComparison.OrdinalIgnoreCase)||process.ProcessName.Equals("LogonUI",StringComparison.OrdinalIgnoreCase))throw new InvalidOperationException("Desktop locked; input stopped");}if(SendInput(1,new[]{input},Marshal.SizeOf<Input>())!=1)throw new InvalidOperationException("SendInput failed");}
 public static void MouseDown(){Submit(new Input{Type=0,Data=new InputData{Mouse=new MouseInput{Flags=2}}});MouseHeld=true;}
 public static void MouseUp(){if(MouseHeld){Submit(new Input{Type=0,Data=new InputData{Mouse=new MouseInput{Flags=4}}});MouseHeld=false;}}
 public static void Wheel(int delta){Submit(new Input{Type=0,Data=new InputData{Mouse=new MouseInput{Data=unchecked((uint)delta),Flags=0x800}}});}
 static void Key(ushort key,bool up){Submit(new Input{Type=1,Data=new InputData{Key=new KeyInput{VirtualKey=key,Flags=up?2u:0u}}});}
 public static void Press(ushort key,bool control){AssertIdleInput();if(control)Key(17,false);try{Key(key,false);Key(key,true);}finally{if(control)Key(17,true);}}
 public static bool Occluded(IntPtr root,Rect capture,uint pid){bool overlap=false;EnumWindows((w,p)=>{if(w==root)return false;if(!IsWindowVisible(w)||IsIconic(w))return true;uint actual;GetWindowThreadProcessId(w,out actual);if(actual==pid)return true;Rect r;GetWindowRect(w,out r);if(r.Left<capture.Right&&r.Right>capture.Left&&r.Top<capture.Bottom&&r.Bottom>capture.Top){overlap=true;return false;}return true;},IntPtr.Zero);return overlap;}
}
"@
function Assert-DesktopUnlocked {
    # 隐藏自身窗口后的前台交接可能短暂返回零；只读等待，不在空桌面或锁屏时输入。
    $watch=[Diagnostics.Stopwatch]::StartNew()
    do{$foreground=[StyleScrollNative]::GetForegroundWindow();$foregroundPid=[uint32]0;$null=[StyleScrollNative]::GetWindowThreadProcessId($foreground,[ref]$foregroundPid);if($foreground -ne [IntPtr]::Zero -and $foregroundPid -ne 0){break};if($watch.ElapsedMilliseconds -ge 500){throw '交互桌面不可用，停止GUI测试。'};Start-Sleep -Milliseconds 20}while($true)
    $foregroundProcess=[Diagnostics.Process]::GetProcessById([int]$foregroundPid)
    try{if($foregroundProcess.ProcessName -in @('LockApp','LogonUI')){throw 'Windows桌面已锁定，停止GUI测试；请用户自行解锁。'}}finally{$foregroundProcess.Dispose()}
}
Assert-DesktopUnlocked
$null=[StyleScrollNative]::SetThreadDpiAwarenessContext([IntPtr](-4))
New-Item -ItemType Directory -Path $output|Out-Null
$checks=[Collections.Generic.List[object]]::new();$frames=[Collections.Generic.List[object]]::new();$capabilities=[Collections.Generic.List[object]]::new();$interactions=[Collections.Generic.List[object]]::new();$failure=$null;$step='startup';$mainHwnd=[IntPtr]::Zero;$zorderRestores=[Collections.Generic.Dictionary[long,long]]::new();$positionRestores=[Collections.Generic.Dictionary[long,object]]::new()
$originalCursor=[StyleScrollNative+Point]::new(0,0);$null=[StyleScrollNative]::GetCursorPos([ref]$originalCursor)
$start=[Diagnostics.ProcessStartInfo]::new($exe);$start.UseShellExecute=$false;$start.WorkingDirectory=$output;$start.RedirectStandardError=$true
foreach($name in @('APPDATA','LOCALAPPDATA','TEMP','TMP')){$directory=Join-Path $output $name.ToLowerInvariant();New-Item -ItemType Directory -Path $directory|Out-Null;$start.Environment[$name]=$directory}
foreach($key in @($start.Environment.Keys)){if($key -match '^(REMOTEOPS_|JLAI_)'){$null=$start.Environment.Remove($key)}}
$app=[Diagnostics.Process]::Start($start);$identity=$app.StartTime.ToUniversalTime().ToString('o');$applicationError=$app.StandardError.ReadToEndAsync()
function Check([string]$Name,[bool]$Passed){$script:step=$Name;$checks.Add([pscustomobject]@{name=$Name;passed=$Passed});if(-not $Passed){throw ('滚动条检查失败：'+$Name)}}
function Assert-Process{$app.Refresh();if($app.HasExited -or $app.StartTime.ToUniversalTime().ToString('o') -cne $identity -or (Get-FileHash -LiteralPath $app.MainModule.FileName).Hash -ine $ExpectedHash){throw '测试进程身份已改变。'}}
function Client([string]$Action='Snapshot',[int]$Id=0,[string]$Path){Assert-DesktopUnlocked;Assert-Process;$script:step=$Action+'_'+$Id;$parameters=@{Action=$Action;ProcessId=$app.Id;ProcessStartUtc=$identity;ExpectedHash=$ExpectedHash;ControlId=$Id};if($Path){$parameters.OutputPath=$Path};$json=& $helper @parameters;if(-not $? -or $LASTEXITCODE){throw '滚动条客户端失败。'};return $json|ConvertFrom-Json}
function Wait-Control([int]$Id){$watch=[Diagnostics.Stopwatch]::StartNew();do{$state=Client;if(@($state.windows.controls|Where-Object id -eq $Id).Count -eq 1){return $state};if($watch.ElapsedMilliseconds -ge 10000){[IO.File]::WriteAllText((Join-Path $output 'wait-last-state.json'),(@{expected_control=$Id;snapshot=$state}|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($false));throw ('控件等待超时：'+$Id)}}while($true)}
function Row($State,[int]$Id){$row=@($State.windows.controls|Where-Object id -eq $Id);if($row.Count -ne 1){throw ('控件不唯一：'+$Id)};return $row[0]}
function Handle($State,[int]$Id){$row=Row $State $Id;if(-not $row.PSObject.Properties['hwnd']){throw '客户端缺少实际 HWND。'};$handle=[IntPtr]([long]$row.hwnd);[StyleScrollNative]::AssertWindow($handle,[uint32]$app.Id,$false);return $handle}
function Record-Focus([string]$Phase,[IntPtr]$Root,$Extra){
    $rect=[StyleScrollNative+Rect]::new();$null=[StyleScrollNative]::GetWindowRect($Root,[ref]$rect)
    $mainRect=[StyleScrollNative+Rect]::new();$null=[StyleScrollNative]::GetWindowRect($mainHwnd,[ref]$mainRect)
    $foreground=[StyleScrollNative]::GetForegroundWindow();$foregroundPid=[uint32]0;$null=[StyleScrollNative]::GetWindowThreadProcessId($foreground,[ref]$foregroundPid)
    $interactions.Add(@{phase=$Phase;observer_root=$Root.ToInt64();observer_rect=$rect;main_root=$mainHwnd.ToInt64();main_rect=$mainRect;menu_count=[StyleScrollNative]::Menus([uint32]$app.Id);gui_thread=[StyleScrollNative]::ThreadInfo($Root);foreground=@{handle=$foreground.ToInt64();pid=$foregroundPid;class=[StyleScrollNative]::Class($foreground);root=[StyleScrollNative]::GetAncestor($foreground,2).ToInt64()};extra=$Extra})
    [IO.File]::WriteAllText((Join-Path $output 'focus-events.json'),(@($interactions)|ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
}
function Focus-Edit([IntPtr]$Handle){
    Assert-Process;[StyleScrollNative]::AssertWindow($Handle,[uint32]$app.Id,$false)
    $root=[StyleScrollNative]::GetAncestor($Handle,2);Record-Focus 'before_activation' $root $null
    $initialRect=[StyleScrollNative+Rect]::new();$null=[StyleScrollNative]::GetWindowRect($root,[ref]$initialRect);$work=[StyleScrollNative]::WorkArea($root)
    $width=$initialRect.Right-$initialRect.Left;$height=$initialRect.Bottom-$initialRect.Top
    if($width -gt $work.Right-$work.Left -or $height -gt $work.Bottom-$work.Top){throw '本次Observer大于其显示器工作区，停止物理输入。'}
    $x=[math]::Clamp($initialRect.Left,$work.Left,$work.Right-$width);$y=[math]::Clamp($initialRect.Top,$work.Top,$work.Bottom-$height)
    if($x -ne $initialRect.Left -or $y -ne $initialRect.Top){
        if(-not $positionRestores.ContainsKey($root.ToInt64())){$positionRestores[$root.ToInt64()]=$initialRect}
        $null=[StyleScrollNative]::SetWindowPos($root,[IntPtr]::Zero,$x,$y,0,0,0x15);Record-Focus 'moved_own_window_into_same_workarea' $root @{original_rect=$initialRect;work_area=$work}
    }
    if([StyleScrollNative]::GetForegroundWindow() -ne $root){
        $null=Client 'OpenMenu';$null=Client 'DismissMenu'
        $menuWait=[Diagnostics.Stopwatch]::StartNew();while([StyleScrollNative]::Menus([uint32]$app.Id) -gt 0 -and $menuWait.ElapsedMilliseconds -lt 400){Start-Sleep -Milliseconds 40}
        if([StyleScrollNative]::Menus([uint32]$app.Id) -gt 0){$null=[StyleScrollNative]::Send($mainHwnd,0x1f,[IntPtr]::Zero,[IntPtr]::Zero)}
        Record-Focus 'own_menu_dismissed' $root $null
        if([StyleScrollNative]::Menus([uint32]$app.Id) -ne 0){throw '本次进程菜单尚未结束，停止物理输入。'}
    }
    Record-Focus 'after_own_main_menu' $root $null
    $element=[System.Windows.Automation.AutomationElement]::FromHandle($Handle);$element.SetFocus()
    $activated=[StyleScrollNative]::SetForegroundWindow($root)
    $wait=[Diagnostics.Stopwatch]::StartNew();while([StyleScrollNative]::GetForegroundWindow() -ne $root -and $wait.ElapsedMilliseconds -lt 800){Start-Sleep -Milliseconds 40}
    Record-Focus 'after_uia_and_foreground' $root @{set_foreground_result=$activated}
    if([StyleScrollNative]::GetForegroundWindow() -ne $root){
        [StyleScrollNative]::AssertWindow($root,[uint32]$app.Id,$false);[StyleScrollNative]::AssertIdleInput()
        $originalStyle=[StyleScrollNative]::GetStyle($root,-20).ToInt64();$placement=[StyleScrollNative]::Position($root);$zorderRestores[$root.ToInt64()]=$originalStyle
        $brought=[StyleScrollNative]::BringWindowToTop($root);$raised=[StyleScrollNative]::SetWindowPos($root,[IntPtr]::Zero,0,0,0,0,0x53)
        $rect=[StyleScrollNative+Rect]::new();$null=[StyleScrollNative]::GetWindowRect($root,[ref]$rect)
        $origin=[StyleScrollNative+Point]::new(0,0);$null=[StyleScrollNative]::ClientToScreen($root,[ref]$origin)
        $point=[StyleScrollNative+Point]::new($rect.Left+80,$rect.Top+[int](($origin.Y-$rect.Top)/2))
        $hit=[StyleScrollNative]::WindowFromPoint($point);$temporaryTopmost=$false
        Record-Focus 'after_normal_raise' $root @{bring_result=$brought;raise_result=$raised;point=$point;hit=$hit.ToInt64();original_exstyle=$originalStyle;original_placement=$placement}
        try{
            if([StyleScrollNative]::GetForegroundWindow() -ne $root){
            if($hit -ne $root){
                $temporaryTopmost=($originalStyle -band 8) -eq 0
                if($temporaryTopmost){$null=[StyleScrollNative]::SetWindowPos($root,[IntPtr](-1),0,0,0,0,0x53);Start-Sleep -Milliseconds 50}
                $hit=[StyleScrollNative]::WindowFromPoint($point)
            }
            $hitPid=[uint32]0;$null=[StyleScrollNative]::GetWindowThreadProcessId($hit,[ref]$hitPid)
            Record-Focus 'before_title_click' $root @{point=$point;hit=$hit.ToInt64();hit_pid=$hitPid;hit_class=[StyleScrollNative]::Class($hit);temporary_topmost=$temporaryTopmost}
            if($hit -ne $root -or $hitPid -ne $app.Id){throw '本次Observer标题栏不可见，停止正常激活点击。'}
            $actualPoint=[StyleScrollNative]::MoveOwned($root,[uint32]$app.Id,$point.X,$point.Y);Record-Focus 'actual_title_pointer_verified' $root @{requested=$point;actual=$actualPoint};[StyleScrollNative]::MouseDown();[StyleScrollNative]::MouseUp();Start-Sleep -Milliseconds 80
            }
        }finally{
            if($temporaryTopmost){$null=[StyleScrollNative]::SetWindowPos($root,[IntPtr](-2),0,0,0,0,0x13)}
            Record-Focus 'after_zorder_restore' $root @{actual_exstyle=[StyleScrollNative]::GetStyle($root,-20).ToInt64();original_exstyle=$originalStyle;actual_placement=[StyleScrollNative]::Position($root);original_placement=$placement}
        }
        Check 'activation_original_exstyle_restored' ([StyleScrollNative]::GetStyle($root,-20).ToInt64() -eq $originalStyle)
    }
    Record-Focus 'activation_complete' $root $null
    [StyleScrollNative]::AssertWindow($Handle,[uint32]$app.Id,$true);return $element
}
function Native-State([IntPtr]$Handle){Assert-Process;[StyleScrollNative]::AssertWindow($Handle,[uint32]$app.Id,$false);return @{horizontal=[StyleScrollNative]::Info($Handle,0);vertical=[StyleScrollNative]::Info($Handle,1);first_line=[StyleScrollNative]::Send($Handle,0xce,[IntPtr]::Zero,[IntPtr]::Zero).ToInt64()}}
function Capture-Client([string]$Name,[IntPtr]$Handle){
    Assert-Process;[StyleScrollNative]::AssertWindow($Handle,[uint32]$app.Id,$true)
    $root=[StyleScrollNative]::GetAncestor($Handle,2);$client=[StyleScrollNative+Rect]::new();$null=[StyleScrollNative]::GetClientRect($root,[ref]$client);$origin=[StyleScrollNative+Point]::new(0,0);$null=[StyleScrollNative]::ClientToScreen($root,[ref]$origin)
    $screen=[StyleScrollNative+Rect]::new();$screen.Left=$origin.X;$screen.Top=$origin.Y;$screen.Right=$origin.X+$client.Right;$screen.Bottom=$origin.Y+$client.Bottom
    if([StyleScrollNative]::Occluded($root,$screen,[uint32]$app.Id)){throw '其他窗口遮挡本次预览，停止真实屏幕取图。'}
    $bitmap=[Drawing.Bitmap]::new($client.Right,$client.Bottom,[Drawing.Imaging.PixelFormat]::Format24bppRgb);$graphics=[Drawing.Graphics]::FromImage($bitmap);$path=Join-Path $output ($Name+'.png')
    try{$graphics.CopyFromScreen($origin.X,$origin.Y,0,0,[Drawing.Size]::new($client.Right,$client.Bottom),[Drawing.CopyPixelOperation]::SourceCopy);$bitmap.Save($path,[Drawing.Imaging.ImageFormat]::Png)}finally{$graphics.Dispose();$bitmap.Dispose()}
    $frames.Add([pscustomobject]@{file=[IO.Path]::GetFileName($path);capture='real_screen_client';origin=@($origin.X,$origin.Y);width=$client.Right;height=$client.Bottom;sha256=(Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant()})
    return @{path=$path;origin=$origin}
}
function Check-Rail([string]$Name,[IntPtr]$Handle,[int]$Axis,[bool]$Dark,$Capture){
    $geometry=[StyleScrollNative]::Geometry($Handle,$Axis);$rect=$geometry.rcScrollBar;$bitmap=[Drawing.Bitmap]::new($Capture.path)
    try{$expected=if($Dark){[Drawing.Color]::FromArgb(17,21,28).ToArgb()}else{[Drawing.Color]::FromArgb(255,255,255).ToArgb()};$samples=@(foreach($fraction in @(0.3,0.5,0.7)){if($Axis -eq 1){$x=$rect.Left+1;$y=$rect.Top+[int](($rect.Bottom-$rect.Top)*$fraction)}else{$x=$rect.Left+[int](($rect.Right-$rect.Left)*$fraction);$y=$rect.Top+1};$bitmap.GetPixel($x-$Capture.origin.X,$y-$Capture.origin.Y).ToArgb()});$interactions.Add(@{name=$Name;phase='rail_pixels';axis=$Axis;expected_argb=$expected;actual_argb=$samples;file=[IO.Path]::GetFileName($Capture.path)});Check ($Name+'_real_rail_color') (@($samples|Where-Object{$_ -ne $expected}).Count -eq 0)
        if($Axis -eq 1){$thumbX=[int](($rect.Left+$rect.Right)/2);$thumbY=$rect.Top+[int](($geometry.xyThumbTop+$geometry.xyThumbBottom)/2)}else{$thumbX=$rect.Left+[int](($geometry.xyThumbTop+$geometry.xyThumbBottom)/2);$thumbY=[int](($rect.Top+$rect.Bottom)/2)}
        $thumbPixel=$bitmap.GetPixel($thumbX-$Capture.origin.X,$thumbY-$Capture.origin.Y).ToArgb()
        $allowedThumb=if($Dark){@([Drawing.Color]::FromArgb(101,117,138).ToArgb(),[Drawing.Color]::FromArgb(145,163,188).ToArgb())}else{@([Drawing.Color]::FromArgb(125,139,157).ToArgb(),[Drawing.Color]::FromArgb(84,102,125).ToArgb())}
        $interactions.Add(@{name=$Name;phase='thumb_pixel';position=@($thumbX,$thumbY);actual_argb=$thumbPixel;expected_argb=$allowedThumb})
        Check ($Name+'_visible_thumb_tracks_native_geometry') ($allowedThumb -contains $thumbPixel)}finally{$bitmap.Dispose()}
}
function Probe-RailUIA([string]$Name,[IntPtr]$Handle,[int]$Axis,[int]$X,[int]$Y){
    $point=[Windows.Point]::new($X,$Y);$hit=[System.Windows.Automation.AutomationElement]::FromPoint($point);$current=$hit.Current
    $patterns=@($hit.GetSupportedPatterns()|ForEach-Object ProgrammaticName)
    $capabilities.Add(@{scope=$Name+'_rail_point';point=@($X,$Y);process_id=$current.ProcessId;native_handle=$current.NativeWindowHandle;role=$current.ControlType.ProgrammaticName;patterns=$patterns})
    Check ($Name+'_rail_uia_owned') ($current.ProcessId -eq $app.Id)
    $edit=[System.Windows.Automation.AutomationElement]::FromHandle($Handle)
    $bars=@($edit.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,[System.Windows.Automation.ControlType]::ScrollBar)))
    foreach($barElement in $bars){
        $orientation=$barElement.GetCurrentPropertyValue([System.Windows.Automation.AutomationElement]::OrientationProperty).ToString();$barPatterns=@($barElement.GetSupportedPatterns()|ForEach-Object ProgrammaticName)
        $entry=@{scope=$Name+'_native_bar';orientation=$orientation;process_id=$barElement.Current.ProcessId;native_handle=$barElement.Current.NativeWindowHandle;role=$barElement.Current.ControlType.ProgrammaticName;patterns=$barPatterns;range_tested=$false};$capabilities.Add($entry)
        if($orientation -ne $(if($Axis -eq 1){'Vertical'}else{'Horizontal'})){continue}
        Check ($Name+'_native_bar_uia_owned') ($barElement.Current.ProcessId -eq $app.Id)
        if($barPatterns -contains 'RangeValuePatternIdentifiers.Pattern'){
            $range=$barElement.GetCurrentPattern([System.Windows.Automation.RangeValuePattern]::Pattern);$entry.read_only=$range.Current.IsReadOnly
            if(-not $range.Current.IsReadOnly){$before=[StyleScrollNative]::Info($Handle,$Axis);$target=$range.Current.Minimum+($range.Current.Maximum-$range.Current.Minimum)*0.35;$range.SetValue($target);Start-Sleep -Milliseconds 40;$after=[StyleScrollNative]::Info($Handle,$Axis);$entry.range_tested=$true;$entry.before=$before;$entry.after=$after;Check ($Name+'_native_bar_rangevalue_moves_edit') ($after.nPos -gt $before.nPos)}
        }
    }
}
function Drag-Thumb([string]$Name,[IntPtr]$Handle,[int]$Axis,[bool]$Dark){
    $null=Focus-Edit $Handle;[StyleScrollNative]::AssertIdleInput();$null=[StyleScrollNative]::Send($Handle,$(if($Axis -eq 1){0x115}else{0x114}),[IntPtr]6,[IntPtr]::Zero)
    $bar=[StyleScrollNative]::Geometry($Handle,$Axis);$rect=$bar.rcScrollBar
    if($Axis -eq 1){$x=($rect.Left+$rect.Right)/2;$y=$rect.Top+($bar.xyThumbTop+$bar.xyThumbBottom)/2;$targetX=$x;$targetY=$rect.Top+[int](($rect.Bottom-$rect.Top)*0.65)}else{$x=$rect.Left+($bar.xyThumbTop+$bar.xyThumbBottom)/2;$y=($rect.Top+$rect.Bottom)/2;$targetX=$rect.Left+[int](($rect.Right-$rect.Left)*0.65);$targetY=$y}
    $x=[int]$x;$y=[int]$y;$targetX=[int]$targetX;$targetY=[int]$targetY
    Probe-RailUIA $Name $Handle $Axis $x $y
    $null=[StyleScrollNative]::Send($Handle,$(if($Axis -eq 1){0x115}else{0x114}),[IntPtr]6,[IntPtr]::Zero)
    $hitPoint=[StyleScrollNative+Point]::new($x,$y);$hit=[StyleScrollNative]::WindowFromPoint($hitPoint);$hitPid=[uint32]0;$null=[StyleScrollNative]::GetWindowThreadProcessId($hit,[ref]$hitPid)
    $interactions.Add(@{name=$Name;phase='thumb_hit_test';point=$hitPoint;expected=$Handle.ToInt64();actual=$hit.ToInt64();actual_pid=$hitPid;actual_class=[StyleScrollNative]::Class($hit);geometry=$bar})
    Check ($Name+'_thumb_target_owned') ($hit -eq $Handle)
    $null=[StyleScrollNative]::MoveOwned($Handle,[uint32]$app.Id,$x,$y);[StyleScrollNative]::MouseDown()
    try{
        for($index=1;$index -le 5;$index++){[StyleScrollNative]::AssertWindow($Handle,[uint32]$app.Id,$true);$null=[StyleScrollNative]::MoveOwned($Handle,[uint32]$app.Id,[int]($x+($targetX-$x)*$index/5),[int]($y+($targetY-$y)*$index/5));Start-Sleep -Milliseconds 35}
        $tracked=[StyleScrollNative]::Info($Handle,$Axis);$interactions.Add(@{name=$Name;phase='held';axis=$Axis;info=$tracked;geometry=[StyleScrollNative]::Geometry($Handle,$Axis)});Check ($Name+'_drag_tracks_nonzero_position') ($tracked.nTrackPos -gt $tracked.nMin)
        $capture=Capture-Client ($Name+'-held') $Handle;Check-Rail ($Name+'_held') $Handle $Axis $Dark $capture
    }finally{try{Assert-DesktopUnlocked;[StyleScrollNative]::MouseUp()}catch{}}
    Start-Sleep -Milliseconds 50;$released=[StyleScrollNative]::Info($Handle,$Axis);$interactions.Add(@{name=$Name;phase='released';axis=$Axis;info=$released})
    Check ($Name+'_release_does_not_jump') ([math]::Abs($released.nPos-$tracked.nTrackPos) -le 1)
    $capture=Capture-Client ($Name+'-released') $Handle;Check-Rail ($Name+'_released') $Handle $Axis $Dark $capture
}
function Short-View([string]$Name,[int]$Id,[bool]$Dark){
    $state=Wait-Control $Id;$row=Row $state $Id;$handle=Handle $state $Id
    Check ($Name+'_readonly') ($row.read_only -eq $true);Check ($Name+'_no_unneeded_bars') (($row.native_style -band 0x300000) -eq 0)
    $path=Join-Path $output ($Name+'.png');$capture=Client 'Capture' $Id $path;$bitmap=[Drawing.Bitmap]::new($path)
    try{$point=[StyleScrollNative+Point]::new(0,0);$null=[StyleScrollNative]::ClientToScreen($handle,[ref]$point);$rect=[StyleScrollNative+Rect]::new();$null=[StyleScrollNative]::GetClientRect($handle,[ref]$rect);$pixel=$bitmap.GetPixel($point.X-$capture.window_bounds[0]+$rect.Right-4,$point.Y-$capture.window_bounds[1]+$rect.Bottom-4);$expected=if($Dark){[Drawing.Color]::FromArgb(17,21,28).ToArgb()}else{[Drawing.Color]::FromArgb(255,255,255).ToArgb()};Check ($Name+'_content_surface') ($pixel.ToArgb() -eq $expected)}finally{$bitmap.Dispose()}
    $frames.Add([pscustomobject]@{file=[IO.Path]::GetFileName($path);capture='PrintWindow';sha256=(Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant()})
}
try{
    $null=$app.WaitForInputIdle(5000);$initialMain=Wait-Control 303;$mainHwnd=[IntPtr](@($initialMain.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0].handle)
    if(-not $InteractionOnly){foreach($theme in @('light','dark')){$dark=$theme -eq 'dark';$null=Client 'Command' $(if($dark){5012}else{5011});$null=Client 'Invoke' 308;Short-View ($theme+'-logs-short') 311 $dark;$null=Client 'Invoke' 310;$null=Client 'Command' 5034;Short-View ($theme+'-tls-short') 524 $dark;$null=Client 'Invoke' 521;$null=Client 'Command' 5043;Short-View ($theme+'-observer-short') 202 $dark}}
    $null=Client 'Command' 5011;$null=Client 'Command' 5044;$state=Wait-Control 202;$row=Row $state 202;$handle=Handle $state 202
    Check 'pressure_source_first_and_last_records' ($row.text -match '大日志演示' -and $row.text -match '10000 · 中文日志')
    Check 'pressure_has_10001_original_records' (@($row.text -split '\r\n|\r|\n'|Where-Object Length).Count -eq 10001)
    Check 'pressure_both_native_bars_visible' (($row.native_style -band 0x300000) -eq 0x300000)
    $initial=Native-State $handle;Check 'pressure_both_ranges_exceed_pages' ($initial.horizontal.nMax -gt $initial.horizontal.nPage -and $initial.vertical.nMax -gt $initial.vertical.nPage)
    $element=Focus-Edit $handle;$patterns=@($element.GetSupportedPatterns()|ForEach-Object ProgrammaticName);$capabilities.Add(@{scope='pressure_edit';patterns=$patterns})
    Check 'uia_scroll_pattern_preserved' ($patterns -contains 'ScrollPatternIdentifiers.Pattern')
    $scroll=$element.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern);$scroll.SetScrollPercent(45,55);$uiaPosition=Native-State $handle
    Check 'uia_scroll_moves_native_both_axes' ($uiaPosition.horizontal.nPos -gt 0 -and $uiaPosition.vertical.nPos -gt 0)
    $textPattern=$element.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern);$range=$textPattern.DocumentRange.Clone();$range.MoveEndpointByRange([System.Windows.Automation.Text.TextPatternRangeEndpoint]::End,$range,[System.Windows.Automation.Text.TextPatternRangeEndpoint]::Start);$null=$range.MoveEndpointByUnit([System.Windows.Automation.Text.TextPatternRangeEndpoint]::End,[System.Windows.Automation.Text.TextUnit]::Character,32);$range.Select();$selected=$textPattern.GetSelection()
    Check 'uia_selection_preserves_chinese' ($selected.Count -eq 1 -and $selected[0].GetText(-1) -match '大日志演示')
    $null=[StyleScrollNative]::Send($handle,0xb1,[IntPtr]::Zero,[IntPtr]::Zero);$null=Focus-Edit $handle
    [StyleScrollNative]::Press(0x24,$true);Start-Sleep -Milliseconds 60;$top=Native-State $handle;Check 'ctrl_home_moves_to_top' ($top.vertical.nPos -eq $top.vertical.nMin)
    [StyleScrollNative]::Press(0x22,$false);Start-Sleep -Milliseconds 60;$paged=Native-State $handle;Check 'page_down_moves_document' ($paged.vertical.nPos -gt $top.vertical.nPos)
    [StyleScrollNative]::AssertIdleInput();$rect=[StyleScrollNative+Rect]::new();$null=[StyleScrollNative]::GetClientRect($handle,[ref]$rect);$point=[StyleScrollNative+Point]::new([int]($rect.Right/2),[int]($rect.Bottom/2));$null=[StyleScrollNative]::ClientToScreen($handle,[ref]$point);$null=[StyleScrollNative]::MoveOwned($handle,[uint32]$app.Id,$point.X,$point.Y)
    [StyleScrollNative]::AssertWindow($handle,[uint32]$app.Id,$true);Check 'wheel_target_owned' ([StyleScrollNative]::WindowFromPoint($point) -eq $handle);[StyleScrollNative]::Wheel(-120);Start-Sleep -Milliseconds 60;$wheeled=Native-State $handle;Check 'physical_wheel_moves_document' ($wheeled.vertical.nPos -gt $paged.vertical.nPos)
    [StyleScrollNative]::Press(0x23,$true);Start-Sleep -Milliseconds 60;$bottom=Native-State $handle;Check 'ctrl_end_reaches_document_tail' ($bottom.vertical.nPos -ge $bottom.vertical.nMax-[long]$bottom.vertical.nPage)
    $null=[StyleScrollNative]::Send($handle,0x115,[IntPtr]6,[IntPtr]::Zero);$null=[StyleScrollNative]::Send($handle,0x114,[IntPtr]6,[IntPtr]::Zero)
    $beforeLatest=Native-State $handle;$null=Client 'Invoke' 206;$latest=Native-State $handle
    $interactions.Add(@{phase='latest_unchanged_text';before=$beforeLatest;after=$latest})
    Check 'latest_moves_unchanged_text_to_tail' ($beforeLatest.vertical.nPos -eq $beforeLatest.vertical.nMin -and $latest.vertical.nPos -ge $latest.vertical.nMax-[long]$latest.vertical.nPage)
    $visibleText=@($textPattern.GetVisibleRanges()|ForEach-Object{$_.GetText(-1)}) -join ''
    Check 'latest_last_source_record_is_visible' ($visibleText -match '10000 · 中文日志')
    $latestCapture=Capture-Client 'latest-unchanged-tail' $handle;Check-Rail 'latest_vertical' $handle 1 $false $latestCapture;Check-Rail 'latest_horizontal' $handle 0 $false $latestCapture
    $null=Client 'Invoke' 204
    $null=[StyleScrollNative]::Send($handle,0x115,[IntPtr]6,[IntPtr]::Zero);$null=[StyleScrollNative]::Send($handle,0x114,[IntPtr]6,[IntPtr]::Zero);$beforeGdi=[StyleScrollNative]::GetGuiResources($app.Handle,0);$beforeUser=[StyleScrollNative]::GetGuiResources($app.Handle,1);Check 'gdi_baseline_available' ($beforeGdi -gt 0);Check 'user_baseline_available' ($beforeUser -gt 0)
    foreach($theme in @('dark','light')){$null=Client 'Command' $(if($theme -eq 'dark'){5012}else{5011});$state=Wait-Control 202;$handle=Handle $state 202;Drag-Thumb ($theme+'-vertical') $handle 1 ($theme -eq 'dark');Drag-Thumb ($theme+'-horizontal') $handle 0 ($theme -eq 'dark')}
    $afterGdi=[StyleScrollNative]::GetGuiResources($app.Handle,0);$afterUser=[StyleScrollNative]::GetGuiResources($app.Handle,1);$interactions.Add(@{phase='resource_counts';gdi_before=$beforeGdi;gdi_after=$afterGdi;user_before=$beforeUser;user_after=$afterUser});Check 'gdi_resources_do_not_accumulate' ($afterGdi -gt 0 -and $afterGdi -le $beforeGdi+8);Check 'user_windows_do_not_accumulate' ($afterUser -gt 0 -and $afterUser -le $beforeUser+1)
    $state=Client;$row=Row $state 202;Check 'pressure_text_intact_after_interactions' ($row.text -match '10000 · 中文日志' -and @($row.text -split '\r\n|\r|\n'|Where-Object Length).Count -eq 10001 -and $row.read_only -eq $true)
    $null=[StyleScrollNative]::SetCursorPos($originalCursor.X,$originalCursor.Y)
    $null=Client 'Close';$null=Wait-Control 463;$null=Client 'Invoke' 463;Check 'normal_exit' ($app.WaitForExit(5000) -and $app.ExitCode -eq 0)
}catch{$failure=@{step=$step;category='scrollbar_or_fixture_failure';detail=$_.Exception.Message}}
finally{
    $desktopReady=$true;try{Assert-DesktopUnlocked}catch{$desktopReady=$false}
    if($desktopReady){
    try{[StyleScrollNative]::MouseUp()}catch{}
    foreach($saved in $zorderRestores.GetEnumerator()){try{$window=[IntPtr]$saved.Key;$owner=[uint32]0;$null=[StyleScrollNative]::GetWindowThreadProcessId($window,[ref]$owner);if($owner -eq $app.Id){$null=[StyleScrollNative]::SetWindowPos($window,$(if(($saved.Value -band 8) -ne 0){[IntPtr](-1)}else{[IntPtr](-2)}),0,0,0,0,0x13)}}catch{}}
    foreach($saved in $positionRestores.GetEnumerator()){try{$window=[IntPtr]$saved.Key;$owner=[uint32]0;$null=[StyleScrollNative]::GetWindowThreadProcessId($window,[ref]$owner);if($owner -eq $app.Id){$null=[StyleScrollNative]::SetWindowPos($window,[IntPtr]::Zero,$saved.Value.Left,$saved.Value.Top,0,0,0x15)}}catch{}}
    $foregroundPid=[uint32]0;$null=[StyleScrollNative]::GetWindowThreadProcessId([StyleScrollNative]::GetForegroundWindow(),[ref]$foregroundPid);if($foregroundPid -eq $app.Id){$null=[StyleScrollNative]::SetCursorPos($originalCursor.X,$originalCursor.Y)}
    }else{[StyleScrollNative]::MouseHeld=$false}
    if(-not $app.HasExited){try{if(-not $desktopReady){throw 'Desktop locked during cleanup'};$state=Client;if($state.windows.controls.id -contains 205){$null=Client 'Invoke' 205};$state=Client;if($state.windows.controls.id -notcontains 463){$null=Client 'Close';$null=Wait-Control 463};$null=Client 'Invoke' 463;$null=$app.WaitForExit(5000)}catch{};if(-not $app.HasExited){$app.Kill();$app.WaitForExit();$failure??=@{step='cleanup';category='owned_preview_forced_exit'}}}
    $record=[ordered]@{passed=$null -eq $failure;interaction_only=[bool]$InteractionOnly;short_views_verified=(-not $InteractionOnly);executable_sha256=$ExpectedHash.ToLowerInvariant();checks=@($checks);frames=@($frames);uia_capabilities=@($capabilities);interactions=@($interactions);clipboard_verified=$false;clipboard_note='仅验证UIA选择和中文内容，未读写个人剪贴板。';failure=$failure;application_exit_code=$app.ExitCode;real_business_verified=$false;rdp_dpi_verified=$false}
    [IO.File]::WriteAllText((Join-Path $output 'report.json'),($record|ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false));if($applicationError.IsCompleted -and -not [string]::IsNullOrWhiteSpace($applicationError.Result)){[IO.File]::WriteAllText((Join-Path $output 'application-error.txt'),$applicationError.Result,[Text.UTF8Encoding]::new($false))};$app.Dispose()
}
$record|ConvertTo-Json -Depth 5
if(-not $record.passed){exit 1}
