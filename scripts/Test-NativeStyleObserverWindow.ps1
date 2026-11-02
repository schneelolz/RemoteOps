#requires -Version 7.2
<# .SYNOPSIS 仅验证自启0.3.8观察窗的原生最大化、还原与尺寸布局。 #>
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedHash,[Parameter(Mandatory)][string]$OutputDirectory)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
if(-not $IsWindows -or [Threading.Thread]::CurrentThread.GetApartmentState() -ne 'STA'){throw '需要STA Windows PowerShell。'}
$exe=(Resolve-Path -LiteralPath $Executable).Path;$output=[IO.Path]::GetFullPath($OutputDirectory)
$tempRoot=[IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp')).TrimEnd('\')
if(-not $tempRoot.StartsWith('C:\',[StringComparison]::OrdinalIgnoreCase) -or -not $output.StartsWith($tempRoot+'\',[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $output)){throw '测试输出必须是全新C盘Temp子目录。'}
for($ancestor=Split-Path -Parent $output;$ancestor;$ancestor=Split-Path -Parent $ancestor){if((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor).Attributes -band [IO.FileAttributes]::ReparsePoint)){throw '测试目录不能经过重解析点。'}}
if((Get-FileHash -LiteralPath $exe).Hash -ine $ExpectedHash -or [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion -cne '0.3.8'){throw '仅测试指定摘要的0.3.8。'}
. (Join-Path $PSScriptRoot 'NativeStyle-DesktopGate.ps1')
Assert-NativeStyleDesktopUnlocked
& (Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1')|Out-Null
Add-Type -TypeDefinition @"
using System;using System.Runtime.InteropServices;using System.Text;
/// <summary>每次变更前验证窗口属于本次观察窗，不注入全局鼠标或键盘。</summary>
public static class ObserverWindowProbe {
 [StructLayout(LayoutKind.Sequential)]public struct Rect{public int Left,Top,Right,Bottom;}
 [StructLayout(LayoutKind.Sequential)]public struct Point{public int X,Y;}
 [StructLayout(LayoutKind.Sequential)]public struct Placement{public uint Length,Flags,Show;public Point Min,Max;public Rect Normal;}
 [StructLayout(LayoutKind.Sequential)]public struct Monitor{public uint Size;public Rect Bounds,Work;public uint Flags;}
 [DllImport("user32.dll")]static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)]static extern int GetClassName(IntPtr window,StringBuilder value,int length);
 [DllImport("user32.dll")]static extern bool IsWindow(IntPtr window);
 [DllImport("user32.dll")]public static extern bool IsZoomed(IntPtr window);
 [DllImport("user32.dll")]public static extern bool IsIconic(IntPtr window);
 [DllImport("user32.dll")]public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")]static extern IntPtr GetAncestor(IntPtr window,uint flags);
 [DllImport("user32.dll")]public static extern bool GetWindowRect(IntPtr window,out Rect rect);
 [DllImport("user32.dll")]public static extern IntPtr GetWindowLongPtrW(IntPtr window,int index);
 [DllImport("user32.dll")]static extern uint GetDpiForWindow(IntPtr window);
 [DllImport("user32.dll")]static extern IntPtr MonitorFromWindow(IntPtr window,uint flags);
 [DllImport("user32.dll")]static extern bool GetMonitorInfoW(IntPtr monitor,ref Monitor info);
 [DllImport("user32.dll")]static extern bool GetWindowPlacement(IntPtr window,ref Placement placement);
 [DllImport("user32.dll")]static extern bool AdjustWindowRectExForDpi(ref Rect rect,uint style,bool menu,uint extended,uint dpi);
 [DllImport("user32.dll")]static extern bool SetWindowPos(IntPtr window,IntPtr order,int x,int y,int width,int height,uint flags);
 [DllImport("user32.dll")]static extern bool PostMessageW(IntPtr window,uint message,IntPtr w,IntPtr l);
 [DllImport("user32.dll",EntryPoint="SendMessageTimeoutW",SetLastError=true)]static extern IntPtr SendMessageTimeout(IntPtr window,uint message,IntPtr w,IntPtr l,uint flags,uint timeout,out IntPtr result);
 [DllImport("user32.dll")]public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
 [DllImport("dwmapi.dll")]static extern int DwmGetWindowAttribute(IntPtr window,int attribute,out Rect rect,int size);
 /// <summary>进程及原生根窗口类别均须匹配。</summary>
 public static void Own(IntPtr window,int expected){uint pid;GetWindowThreadProcessId(window,out pid);var name=new StringBuilder(128);GetClassName(window,name,name.Capacity);if(!IsWindow(window)||pid!=expected||name.ToString()!="RemoteOpsStylePreviewObserver")throw new InvalidOperationException("Own Observer identity mismatch");}
 /// <summary>只读确认有效前台属于本次主窗，允许其仍有效的原生子控件。</summary>
 public static bool MainForeground(IntPtr main,int expected){var foreground=GetForegroundWindow();uint pid;GetWindowThreadProcessId(foreground,out pid);return IsWindow(foreground)&&pid==expected&&GetAncestor(foreground,2)==main;}
 public static void Minimize(IntPtr window,int expected){Own(window,expected);if(!PostMessageW(window,0x112,(IntPtr)0xf020,IntPtr.Zero))throw new InvalidOperationException("Own minimize command failed");}
 public static Placement GetPlacement(IntPtr window){var p=new Placement{Length=(uint)Marshal.SizeOf<Placement>()};if(!GetWindowPlacement(window,ref p))throw new InvalidOperationException("Window placement unavailable");return p;}
 public static Rect Work(IntPtr window){var m=new Monitor{Size=(uint)Marshal.SizeOf<Monitor>()};if(!GetMonitorInfoW(MonitorFromWindow(window,2),ref m))throw new InvalidOperationException("Monitor unavailable");return m.Work;}
 public static Rect Frame(IntPtr window){Rect rect;if(DwmGetWindowAttribute(window,9,out rect,Marshal.SizeOf<Rect>())!=0&&!GetWindowRect(window,out rect))throw new InvalidOperationException("Frame unavailable");return rect;}
 /// <summary>按实际DPI换算内容区尺寸，不更改屏幕缩放或窗口层级。</summary>
 public static void Resize(IntPtr window,int expected,int width,int height){Own(window,expected);uint dpi=GetDpiForWindow(window);var rect=new Rect{Right=(int)((long)width*dpi/96),Bottom=(int)((long)height*dpi/96)};if(!AdjustWindowRectExForDpi(ref rect,(uint)GetWindowLongPtrW(window,-16).ToInt64(),false,(uint)GetWindowLongPtrW(window,-20).ToInt64(),dpi)||!SetWindowPos(window,IntPtr.Zero,0,0,rect.Right-rect.Left,rect.Bottom-rect.Top,0x16))throw new InvalidOperationException("Own resize failed");}
 public static void SystemState(IntPtr window,int expected,bool maximize){Own(window,expected);if(!PostMessageW(window,0x112,(IntPtr)(maximize?0xf030:0xf120),IntPtr.Zero))throw new InvalidOperationException("Own system command failed");}
 /// <summary>仅查询已验证窗口右下边框命中，不产生鼠标操作。</summary>
 public static long ResizeHit(IntPtr window,int expected){Own(window,expected);Rect rect;if(!GetWindowRect(window,out rect))throw new InvalidOperationException("Own bounds unavailable");int x=rect.Right-2,y=rect.Bottom-2;long coordinates=(ushort)x|((long)(ushort)y<<16);IntPtr result;if(SendMessageTimeout(window,0x84,IntPtr.Zero,(IntPtr)coordinates,2,2000,out result)==IntPtr.Zero)throw new InvalidOperationException("Own hit query failed");return result.ToInt64();}
}
"@
$oldDpi=[ObserverWindowProbe]::SetThreadDpiAwarenessContext([IntPtr](-4))
$helper=Join-Path $PSScriptRoot 'Test-NativeStylePreview.ps1'
New-Item -ItemType Directory -Path $output|Out-Null
$checks=[Collections.Generic.List[object]]::new();$frames=[Collections.Generic.List[object]]::new();$transitions=[Collections.Generic.List[object]]::new();$failure=$null;$step='startup';$lastSnapshot=$null;$app=$null;$observer=[IntPtr]::Zero;$applicationError=$null
$start=[Diagnostics.ProcessStartInfo]::new($exe);$start.UseShellExecute=$false;$start.WorkingDirectory=$output;$start.RedirectStandardError=$true
foreach($key in @('APPDATA','LOCALAPPDATA','TEMP','TMP')){$dir=Join-Path $output $key.ToLowerInvariant();New-Item -ItemType Directory -Path $dir|Out-Null;$start.Environment[$key]=$dir}
foreach($key in @($start.Environment.Keys)){if($key -match '^(REMOTEOPS_|JLAI_)'){$null=$start.Environment.Remove($key)}}
function Assert-Own{
    Assert-NativeStyleDesktopUnlocked;$app.Refresh()
    if($app.HasExited -or $app.StartTime.ToUniversalTime().ToString('o') -cne $identity -or $app.SessionId -ne [Diagnostics.Process]::GetCurrentProcess().SessionId -or (Get-FileHash -LiteralPath $app.MainModule.FileName).Hash -ine $ExpectedHash){throw 'Own preview identity changed'}
    if($observer -ne [IntPtr]::Zero){[ObserverWindowProbe]::Own($observer,$app.Id)}
}
function Client([string]$Action='Snapshot',[int]$Id=0,[string]$Path){
    Assert-Own;$script:step=$Action+'_'+$Id
    $workerInfo=[Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'));$workerInfo.UseShellExecute=$false;$workerInfo.CreateNoWindow=$true;$workerInfo.RedirectStandardOutput=$true;$workerInfo.RedirectStandardError=$true;$workerInfo.StandardOutputEncoding=[Text.UTF8Encoding]::new($false)
    foreach($arg in @('-NoProfile','-STA','-File',$helper,'-Action',$Action,'-ControlId',[string]$Id,'-ProcessId',[string]$app.Id,'-ProcessStartUtc',$identity,'-ExpectedHash',$ExpectedHash)){$workerInfo.ArgumentList.Add($arg)}
    if($Action -eq 'Invoke'){$workerInfo.ArgumentList.Add('-FocusBeforeInvoke')};if($Path){$workerInfo.ArgumentList.Add('-OutputPath');$workerInfo.ArgumentList.Add($Path)}
    $worker=[Diagnostics.Process]::Start($workerInfo);$stdout=$worker.StandardOutput.ReadToEndAsync();$stderr=$worker.StandardError.ReadToEndAsync()
    try{if(-not $worker.WaitForExit(12000)){$worker.Kill();$worker.WaitForExit();throw 'UIA worker timeout'};if($worker.ExitCode){$message=$stderr.GetAwaiter().GetResult();[IO.File]::WriteAllText((Join-Path $output 'client-last-error.txt'),$message);throw ('UIA worker failed: '+$message.Trim())};$result=$stdout.GetAwaiter().GetResult()|ConvertFrom-Json;if($Action -eq 'Snapshot'){$script:lastSnapshot=$result};return $result}finally{$worker.Dispose()}
}
function Check([string]$Name,[bool]$Pass){$script:step=$Name;$checks.Add([pscustomobject]@{name=$Name;passed=$Pass});if(-not $Pass){throw ('观察窗检查失败：'+$Name)}}
function Row($State,[int]$Id){$rows=@($State.windows.controls|Where-Object id -eq $Id);if($rows.Count -ne 1){throw ('控件不唯一：'+$Id)};return $rows[0]}
function Wait-State([string]$Name,[scriptblock]$Condition){$watch=[Diagnostics.Stopwatch]::StartNew();do{$state=Client;if(& $Condition $state){return $state};if($watch.ElapsedMilliseconds -ge 10000){$script:step=$Name;throw ('等待超时：'+$Name)};Start-Sleep -Milliseconds 100}while($true)}
function Observer($State){$rows=@($State.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver');if($rows.Count -ne 1){throw 'Observer not unique'};return $rows[0]}
function Capture([string]$Name){$path=Join-Path $output ($Name+'.png');$result=Client 'Capture' 202 $path;$frames.Add([pscustomobject]@{file=[IO.Path]::GetFileName($path);sha256=(Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant();dpi=$result.dpi;client_width=$result.client_width;client_height=$result.client_height;window_bounds=$result.window_bounds})}
function Same-Rect($A,$B,[int]$Tolerance=1){return [Math]::Abs($A.Left-$B.Left) -le $Tolerance -and [Math]::Abs($A.Top-$B.Top) -le $Tolerance -and [Math]::Abs($A.Right-$B.Right) -le $Tolerance -and [Math]::Abs($A.Bottom-$B.Bottom) -le $Tolerance}
function Layout-Fits($State){
    $window=Observer $State
    foreach($control in $window.controls){$x=$control.bounds[0]-$window.client_origin[0];$y=$control.bounds[1]-$window.client_origin[1];if($x -lt -1 -or $y -lt -1 -or $x+$control.bounds[2] -gt $window.width+1 -or $y+$control.bounds[3] -gt $window.height+1){return $false}}
    $text=Row $State 202;$status=Row $State 222;$pause=Row $State 204
    if($status.bounds[1]+$status.bounds[3] -gt $text.bounds[1] -or $text.bounds[1]+$text.bounds[3] -gt $pause.bounds[1]){return $false}
    $buttons=@(210..215|ForEach-Object{Row $State $_});for($i=1;$i -lt $buttons.Count;$i++){if($buttons[$i-1].bounds[0]+$buttons[$i-1].bounds[2] -gt $buttons[$i].bounds[0]){return $false}}
    return $true
}
function Set-Visual([bool]$Maximize){
    Assert-Own;$method='native_system_command';$element=[System.Windows.Automation.AutomationElement]::FromHandle($observer);$pattern=$null
    if($element.TryGetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern,[ref]$pattern)){$pattern.SetWindowVisualState($(if($Maximize){[System.Windows.Automation.WindowVisualState]::Maximized}else{[System.Windows.Automation.WindowVisualState]::Normal}));$method='uia_window_pattern'}else{[ObserverWindowProbe]::SystemState($observer,$app.Id,$Maximize)}
    $state=Wait-State ('window_visual_'+$Maximize) {param($s) [ObserverWindowProbe]::IsZoomed($observer) -eq $Maximize}
    $transitions.Add([pscustomobject]@{maximize=$Maximize;method=$method;placement=[ObserverWindowProbe]::GetPlacement($observer);frame=[ObserverWindowProbe]::Frame($observer)});return $state
}
try{
    $app=[Diagnostics.Process]::Start($start);$applicationError=$app.StandardError.ReadToEndAsync();$identity=$app.StartTime.ToUniversalTime().ToString('o');$null=$app.WaitForInputIdle(5000)
    $mainState=Wait-State 'main_ready' {param($s) $s.windows.controls.id -contains 303}
    $main=[IntPtr]([long]@($mainState.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0].handle)
    $null=Client 'Invoke' 304
    $autoBefore=Wait-State 'valid_main_foreground' {param($s) [ObserverWindowProbe]::MainForeground($main,$app.Id)}
    $foregroundBefore=[ObserverWindowProbe]::GetForegroundWindow().ToInt64()
    $null=Client 'Command' 5043
    $state=Wait-State 'observer_ready' {param($s) $s.windows.controls.id -contains 202};$observer=[IntPtr]([long](Observer $state).handle)
    [IO.File]::WriteAllText((Join-Path $output 'automatic-show-focus.json'),(@{before_snapshot=$autoBefore;after_snapshot=$state;valid_main_before=$true;foreground_before=$foregroundBefore;foreground_after=[ObserverWindowProbe]::GetForegroundWindow().ToInt64()}|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($false))
    Check 'automatic_show_preserves_valid_main_foreground' ([ObserverWindowProbe]::MainForeground($main,$app.Id) -and [ObserverWindowProbe]::GetForegroundWindow().ToInt64() -eq $foregroundBefore)
    $normal=[ObserverWindowProbe+Rect]::new();$null=[ObserverWindowProbe]::GetWindowRect($observer,[ref]$normal);$normalPlacement=[ObserverWindowProbe]::GetPlacement($observer)
    $style=[ObserverWindowProbe]::GetWindowLongPtrW($observer,-16).ToInt64();$extended=[ObserverWindowProbe]::GetWindowLongPtrW($observer,-20).ToInt64()
    Check 'caption_and_system_menu_present' (($style -band 0xc80000) -eq 0xc80000)
    Check 'maximize_box_present' (($style -band 0x10000) -ne 0)
    Check 'resizable_thick_frame_present' (($style -band 0x40000) -ne 0)
    Check 'tool_window_style_removed' (($extended -band 0x80) -eq 0)
    Check 'right_bottom_border_reports_resize_hit' ([ObserverWindowProbe]::ResizeHit($observer,$app.Id) -eq 17)
    $initial=(Row $state 202).text
    foreach($theme in @(@('light',5011),@('dark',5012))){
        $name=$theme[0];$null=Client 'Command' $theme[1];$state=Wait-State 'theme_ready' {param($s) $s.windows.controls.id -contains 202}
        Check ($name+'_normal_layout_fits') (Layout-Fits $state);Capture ($name+'-normal')
        $state=Set-Visual $true;$frame=[ObserverWindowProbe]::Frame($observer);$work=[ObserverWindowProbe]::Work($observer)
        Check ($name+'_maximized_uses_monitor_work_area') (Same-Rect $frame $work 2)
        Check ($name+'_maximized_layout_fits') (Layout-Fits $state)
        Check ($name+'_maximized_preserves_readonly_history') ((Row $state 202).read_only -eq $true -and (Row $state 202).text -ceq $initial);Capture ($name+'-maximized')
        $state=Set-Visual $false;$restored=[ObserverWindowProbe+Rect]::new();$null=[ObserverWindowProbe]::GetWindowRect($observer,[ref]$restored)
        Check ($name+'_restore_returns_original_rectangle') (Same-Rect $normal $restored)
        Check ($name+'_restore_returns_normal_placement') (Same-Rect $normalPlacement.Normal ([ObserverWindowProbe]::GetPlacement($observer).Normal))
        Check ($name+'_restored_layout_fits') (Layout-Fits $state);Capture ($name+'-restored')
    }
    foreach($size in @(@(1000,640),@(640,360),@(800,470))){
        Assert-Own;[ObserverWindowProbe]::Resize($observer,$app.Id,$size[0],$size[1]);$width=$size[0];$height=$size[1]
        $state=Wait-State 'resize_applied' {param($s) $o=Observer $s;[Math]::Abs($o.width-$width*$o.dpi/96) -le 1 -and [Math]::Abs($o.height-$height*$o.dpi/96) -le 1}
        Check ('client_resize_'+$width+'x'+$height) (Layout-Fits $state)
        Check ('history_preserved_'+$width+'x'+$height) ((Row $state 202).text -ceq $initial -and (Row $state 202).read_only -eq $true)
    }
    $null=Client 'Invoke' 212;$state=Wait-State 'powershell_selected' {param($s) (Row $s 222).text -match '查看：PowerShell'}
    $state=Set-Visual $true;Check 'maximize_preserves_selected_empty_category' ((Row $state 222).text -match '查看：PowerShell' -and (Row $state 202).text -eq '')
    $state=Set-Visual $false;Check 'restore_preserves_selected_empty_category' ((Row $state 222).text -match '查看：PowerShell' -and (Row $state 202).text -eq '')
    $state=Set-Visual $true;$maximizedObserver=$observer.ToInt64()
    $null=Client 'Close' 202;$hidden=Client
    Check 'maximized_close_hides_observer' (@($hidden.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver').Count -eq 0)
    $null=Client 'Command' 5043;$state=Wait-State 'maximized_observer_reopened' {param($s) $s.windows.controls.id -contains 202}
    Check 'maximized_reopen_preserves_same_observer' ((Observer $state).handle -eq $maximizedObserver)
    Check 'maximized_reopen_preserves_maximized_state' ([ObserverWindowProbe]::IsZoomed($observer))
    $state=Set-Visual $false
    Assert-Own;[ObserverWindowProbe]::Minimize($observer,$app.Id)
    $null=Wait-State 'observer_minimized' {param($s) [ObserverWindowProbe]::IsIconic($observer)}
    Check 'minimize_really_applied' ([ObserverWindowProbe]::IsIconic($observer))
    $null=Client 'Command' 5043
    $state=Wait-State 'minimized_observer_reopened' {param($s) -not [ObserverWindowProbe]::IsIconic($observer) -and $s.windows.controls.id -contains 202}
    Check 'minimized_reopen_returns_visible_normal_window' (-not [ObserverWindowProbe]::IsIconic($observer) -and -not [ObserverWindowProbe]::IsZoomed($observer) -and (Observer $state).handle -eq $maximizedObserver)
    Check 'minimized_reopen_preserves_category_and_layout' ((Row $state 222).text -match '查看：PowerShell' -and (Layout-Fits $state))
    $sameObserver=$observer.ToInt64();$null=Client 'Close' 202;$hidden=Client
    Check 'caption_close_hides_observer' (@($hidden.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver').Count -eq 0)
    $null=Client 'Command' 5043;$state=Wait-State 'observer_reopened' {param($s) $s.windows.controls.id -contains 202}
    Check 'reopen_preserves_same_observer_and_category' ((Observer $state).handle -eq $sameObserver -and (Row $state 222).text -match '查看：PowerShell')
    $null=Client 'Close' 202;$null=Client 'Close';$null=Wait-State 'exit_prompt' {param($s) $s.windows.controls.id -contains 463};$null=Client 'Invoke' 463
    Check 'normal_exit' ($app.WaitForExit(5000) -and $app.ExitCode -eq 0)
}catch{
    $failure=[pscustomobject]@{step=$step;category=if($_.Exception.Message -match 'desktop_'){'environment_unavailable'}else{'observer_window_or_fixture_failure'};detail=$_.Exception.Message}
    if($lastSnapshot){[IO.File]::WriteAllText((Join-Path $output 'failure-last-snapshot.json'),($lastSnapshot|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($false))}
}finally{
    if($null -ne $app -and -not $app.HasExited){try{$null=Client 'Close';$null=Wait-State 'cleanup_exit' {param($s) $s.windows.controls.id -contains 463};$null=Client 'Invoke' 463;$null=$app.WaitForExit(5000)}catch{};if(-not $app.HasExited){$app.Kill();$app.WaitForExit();$failure??=[pscustomobject]@{step='cleanup';category='owned_preview_forced_exit';detail='Only the self-started preview was terminated'}}}
    if($null -ne $applicationError -and $applicationError.IsCompleted -and -not [string]::IsNullOrWhiteSpace($applicationError.Result)){[IO.File]::WriteAllText((Join-Path $output 'application-error.txt'),$applicationError.Result)}
    if($oldDpi -ne [IntPtr]::Zero){$null=[ObserverWindowProbe]::SetThreadDpiAwarenessContext($oldDpi)}
    $record=[ordered]@{passed=$null -eq $failure;executable_sha256=$ExpectedHash.ToLowerInvariant();checks=@($checks);frames=@($frames);transitions=@($transitions);failure=$failure;application_exit_code=if($null -ne $app){$app.ExitCode}else{$null};demo_only=$true;real_business_verified=$false;rdp_dpi_verified=$false;clipboard_touched=$false;global_mouse_keyboard_injected=$false;resize_evidence='native client resize and border hit test; no physical drag claim'}
    [IO.File]::WriteAllText((Join-Path $output 'report.json'),($record|ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false));if($null -ne $app){$app.Dispose()}
}
$record|ConvertTo-Json -Depth 7
if(-not $record.passed){exit 1}
