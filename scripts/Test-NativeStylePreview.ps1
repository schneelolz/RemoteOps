#requires -Version 7.2
<# .SYNOPSIS 只操作本脚本启动或精确指定的 Win32 样式预览，截图不包含其他程序。 #>
[CmdletBinding()]
param(
    [string]$Executable,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedHash,
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$ExpectedVersion,
    [string]$OutputDirectory,
    [switch]$ClearBackOnly,
    [switch]$FocusBeforeInvoke,
    [ValidateSet('Validate','Snapshot','Invoke','Command','Close','Capture','SetValue','Toggle','Focus','OpenMenu','SelectMenu','DismissMenu')][string]$Action='Validate',
    [int]$ProcessId,
    [string]$ProcessStartUtc,
    [int]$ControlId,
    [string]$Value,
    [string]$OutputPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
if(-not $IsWindows){throw 'Windows required'}
. (Join-Path $PSScriptRoot 'NativeStyle-DesktopGate.ps1')
Assert-NativeStyleDesktopUnlocked
if($Action -ne 'Validate'){
    $process=[Diagnostics.Process]::GetProcessById($ProcessId)
    if($process.SessionId -ne [Diagnostics.Process]::GetCurrentProcess().SessionId){throw 'Preview session mismatch'}
    if($process.StartTime.ToUniversalTime().ToString('o') -cne $ProcessStartUtc -or
        (Get-FileHash -LiteralPath $process.MainModule.FileName).Hash -ine $ExpectedHash -or
        [Diagnostics.FileVersionInfo]::GetVersionInfo($process.MainModule.FileName).ProductVersion -cne $ExpectedVersion){throw 'Preview identity mismatch'}
    & (Join-Path $PSScriptRoot '../crates/remoteops-visual/src/windows_uia_initialize.ps1')|Out-Null
    Add-Type -TypeDefinition @'
using System;using System.Collections.Generic;using System.Runtime.InteropServices;using System.Text;
public static class StyleProbe {
 public delegate bool Callback(IntPtr hwnd,IntPtr param);
 [StructLayout(LayoutKind.Sequential)]public struct ScrollInfo{public uint cbSize,fMask;public int nMin,nMax;public uint nPage;public int nPos,nTrackPos;}
 [DllImport("user32.dll")]static extern bool GetScrollInfo(IntPtr w,int bar,ref ScrollInfo info);
 public static ScrollInfo Scroll(IntPtr w,int bar){var info=new ScrollInfo{cbSize=(uint)Marshal.SizeOf<ScrollInfo>(),fMask=23};if(!GetScrollInfo(w,bar,ref info))info.cbSize=0;return info;}
 public static long FirstLine(IntPtr w){return SendMessage(w,0xce,IntPtr.Zero,IntPtr.Zero).ToInt64();}
 public static long TextLength(IntPtr w){return SendMessage(w,0xe,IntPtr.Zero,IntPtr.Zero).ToInt64();}

 [StructLayout(LayoutKind.Sequential)]public struct Size{public int X,Y;}
 [DllImport("user32.dll")]static extern IntPtr GetDC(IntPtr w);
 [DllImport("user32.dll")]static extern int ReleaseDC(IntPtr w,IntPtr dc);
 [DllImport("user32.dll",EntryPoint="SendMessageW")]static extern IntPtr SendMessage(IntPtr w,uint m,IntPtr a,IntPtr b);
 [DllImport("gdi32.dll")]static extern IntPtr SelectObject(IntPtr dc,IntPtr obj);
 [DllImport("gdi32.dll",CharSet=CharSet.Unicode)]static extern bool GetTextExtentPoint32(IntPtr dc,string text,int count,out Size size);
 [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)]public struct LogFont{public int Height,Width,Escapement,Orientation,Weight;public byte Italic,Underline,StrikeOut,CharsetByte,OutPrecision,ClipPrecision,Quality,PitchAndFamily;[MarshalAs(UnmanagedType.ByValTStr,SizeConst=32)]public string FaceName;}
 [DllImport("gdi32.dll",EntryPoint="GetObjectW")]static extern int GetObject(IntPtr obj,int size,out LogFont font);
 [DllImport("gdi32.dll",CharSet=CharSet.Unicode)]static extern IntPtr CreateFontIndirect(ref LogFont font);
 [DllImport("gdi32.dll")]static extern bool DeleteObject(IntPtr obj);
 public static int TextWidth(IntPtr w,string text){LogFont font;if(GetObject(SendMessage(w,0x31,IntPtr.Zero,IntPtr.Zero),Marshal.SizeOf<LogFont>(),out font)==0)return -1;var copy=CreateFontIndirect(ref font);if(copy==IntPtr.Zero)return -1;var dc=GetDC(w);if(dc==IntPtr.Zero){DeleteObject(copy);return -1;}var old=SelectObject(dc,copy);try{Size size;return GetTextExtentPoint32(dc,text,text.Length,out size)?size.X:-1;}finally{SelectObject(dc,old);ReleaseDC(w,dc);DeleteObject(copy);}}
 [StructLayout(LayoutKind.Sequential)]public struct Rect{public int Left,Top,Right,Bottom;}
 [DllImport("user32.dll")]static extern bool EnumWindows(Callback cb,IntPtr p);
 [DllImport("user32.dll")]public static extern uint GetWindowThreadProcessId(IntPtr w,out uint p);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)]static extern int GetClassName(IntPtr w,StringBuilder s,int n);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)]public static extern int GetWindowText(IntPtr w,StringBuilder s,int n);
 [DllImport("user32.dll")]public static extern bool IsWindowVisible(IntPtr w);
 [DllImport("user32.dll")]public static extern bool IsWindowEnabled(IntPtr w);
 [StructLayout(LayoutKind.Sequential)]public struct Point{public int X,Y;}
 [DllImport("user32.dll")]public static extern bool ClientToScreen(IntPtr w,ref Point p);
 [DllImport("user32.dll",EntryPoint="GetWindowLongPtrW")]public static extern IntPtr GetWindowStyle(IntPtr w,int index);
 [DllImport("user32.dll")]public static extern bool GetClientRect(IntPtr w,out Rect r);
 [DllImport("user32.dll")]public static extern bool GetWindowRect(IntPtr w,out Rect r);
 [DllImport("user32.dll")]public static extern uint GetDpiForWindow(IntPtr w);
 [DllImport("user32.dll")]public static extern bool PostMessage(IntPtr w,uint m,IntPtr a,IntPtr b);
 [DllImport("user32.dll")]public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")]public static extern IntPtr GetAncestor(IntPtr w,uint f);
 [DllImport("user32.dll")]public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr c);
 [DllImport("user32.dll")]public static extern bool PrintWindow(IntPtr w,IntPtr dc,uint flags);
 public static string Class(IntPtr w){var s=new StringBuilder(256);GetClassName(w,s,s.Capacity);return s.ToString();}
 public static IntPtr[] Menus(int pid){var result=new List<IntPtr>();EnumWindows((w,p)=>{uint n;GetWindowThreadProcessId(w,out n);if(n==pid&&IsWindowVisible(w)&&Class(w)=="#32768")result.Add(w);return true;},IntPtr.Zero);return result.ToArray();}
 public static IntPtr[] Windows(int pid){var result=new List<IntPtr>();EnumWindows((w,p)=>{uint n;GetWindowThreadProcessId(w,out n);if(n==pid&&IsWindowVisible(w)&&Class(w).StartsWith("RemoteOpsStylePreview"))result.Add(w);return true;},IntPtr.Zero);return result.ToArray();}
}
'@
    $null=[StyleProbe]::SetThreadDpiAwarenessContext([IntPtr](-4))
    $windows=@([StyleProbe]::Windows($ProcessId))
    $main=@($windows|Where-Object{[StyleProbe]::Class($_) -eq 'RemoteOpsStylePreviewMain'})
    if($main.Count -ne 1){throw 'Preview main unavailable'}
    function Elements([IntPtr]$Window){
        return [System.Windows.Automation.AutomationElement]::FromHandle($Window).FindAll(
            [System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.Condition]::TrueCondition)
    }
    function Control([int]$Id){
        $matches=[Collections.Generic.List[object]]::new()
        foreach($window in $windows){foreach($element in (Elements $window)){
            if($element.Current.ProcessId -eq $ProcessId -and $element.Current.AutomationId -ceq [string]$Id -and
                $element.Current.NativeWindowHandle -ne 0 -and [StyleProbe]::IsWindowVisible([IntPtr]$element.Current.NativeWindowHandle) -and [StyleProbe]::GetAncestor([IntPtr]$element.Current.NativeWindowHandle,2) -eq $window){$matches.Add($element)}
        }}
        if($matches.Count -ne 1){throw ('Preview control not unique: '+$Id)}
        return $matches[0]
    }
    function Focus-OwnWindow($Element){
        $root=[StyleProbe]::GetAncestor([IntPtr]$Element.Current.NativeWindowHandle,2)
        if($windows -notcontains $root){throw 'Own focus root unavailable'}
        # 自定义顶层窗不公开UIA键盘焦点；聚焦已核验的原生按钮，桌面门禁处理重建期间失效句柄。
        $Element.SetFocus()
    }
    function Foreground-Metadata([IntPtr]$Handle){
        $owner=0u;$thread=[StyleProbe]::GetWindowThreadProcessId($Handle,[ref]$owner)
        return @{handle=$Handle.ToInt64();process_id=$owner;thread_id=$thread;class_name=[StyleProbe]::Class($Handle);worker_pid=$PID}
    }
    $result=switch($Action){
        'Snapshot'{
            $rows=@(foreach($window in $windows){
                $rect=[StyleProbe+Rect]::new();$null=[StyleProbe]::GetClientRect($window,[ref]$rect)
                $title=[Text.StringBuilder]::new(1024);$null=[StyleProbe]::GetWindowText($window,$title,$title.Capacity)
                $origin=[StyleProbe+Point]::new();$null=[StyleProbe]::ClientToScreen($window,[ref]$origin)
                $controls=@(foreach($element in (Elements $window)){
                    $id=0;if(-not [int]::TryParse($element.Current.AutomationId,[ref]$id) -or $id -lt 200 -or
                        $element.Current.NativeWindowHandle -eq 0 -or -not [StyleProbe]::IsWindowVisible([IntPtr]$element.Current.NativeWindowHandle) -or [StyleProbe]::GetAncestor([IntPtr]$element.Current.NativeWindowHandle,2) -ne $window){continue}
                    $patterns=@($element.GetSupportedPatterns()|ForEach-Object Id);$text=$element.Current.Name;$readonly=$null
                    if($patterns -contains [System.Windows.Automation.ValuePattern]::Pattern.Id){$v=$element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern);$text=$v.Current.Value;$readonly=$v.Current.IsReadOnly}
                    elseif($patterns -contains [System.Windows.Automation.TextPattern]::Pattern.Id){$v=$element.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern);$text=$v.DocumentRange.GetText(-1);$a=$v.DocumentRange.GetAttributeValue([System.Windows.Automation.TextPattern]::IsReadOnlyAttribute);if($a -is [bool]){$readonly=$a}}
                    $bounds=$element.Current.BoundingRectangle
                    [pscustomobject]@{id=$id;hwnd=[long]$element.Current.NativeWindowHandle;horizontal_scroll=if($id -in @(202,311,524)){[StyleProbe]::Scroll([IntPtr]$element.Current.NativeWindowHandle,0)}else{$null};vertical_scroll=if($id -in @(202,311,524)){[StyleProbe]::Scroll([IntPtr]$element.Current.NativeWindowHandle,1)}else{$null};first_visible_line=if($id -in @(202,311,524)){[StyleProbe]::FirstLine([IntPtr]$element.Current.NativeWindowHandle)}else{$null};text_length=if($id -in @(202,311,524)){[StyleProbe]::TextLength([IntPtr]$element.Current.NativeWindowHandle)}else{$null};scroll_pattern=$patterns -contains [System.Windows.Automation.ScrollPattern]::Pattern.Id;text=$text;type=$element.Current.ControlType.ProgrammaticName;enabled=$element.Current.IsEnabled;invoke=$patterns -contains [System.Windows.Automation.InvokePattern]::Pattern.Id;toggle_state=if($patterns -contains [System.Windows.Automation.TogglePattern]::Pattern.Id){$element.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Current.ToggleState.ToString()}else{$null};read_only=$readonly;bounds=@($bounds.X,$bounds.Y,$bounds.Width,$bounds.Height);native_style=[StyleProbe]::GetWindowStyle([IntPtr]$element.Current.NativeWindowHandle,-16).ToInt64();text_width=if($id -in @(307,308,338,309)){[StyleProbe]::TextWidth([IntPtr]$element.Current.NativeWindowHandle,$text)}else{$null}}
                })
                [pscustomobject]@{handle=$window.ToInt64();class=[StyleProbe]::Class($window);title=$title.ToString();enabled=[StyleProbe]::IsWindowEnabled($window);dpi=[StyleProbe]::GetDpiForWindow($window);client_origin=@($origin.X,$origin.Y);width=$rect.Right-$rect.Left;height=$rect.Bottom-$rect.Top;controls=$controls}
            });@{windows=$rows;foreground=[StyleProbe]::GetForegroundWindow().ToInt64();foreground_metadata=(Foreground-Metadata ([StyleProbe]::GetForegroundWindow()));menu_count=@([StyleProbe]::Menus($ProcessId)).Count}
        }
        'Invoke'{
            $button=Control $ControlId;$before=[StyleProbe]::GetForegroundWindow();$beforeMeta=Foreground-Metadata $before
            if($FocusBeforeInvoke){Focus-OwnWindow $button}
            $focused=[StyleProbe]::GetForegroundWindow();$focusedMeta=Foreground-Metadata $focused
            $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
            $after=[StyleProbe]::GetForegroundWindow();$afterMeta=Foreground-Metadata $after
            @{accepted=$true;focus_requested=[bool]$FocusBeforeInvoke;foreground_before=$before.ToInt64();foreground_before_invoke=$focused.ToInt64();foreground_after=$after.ToInt64();before_metadata=$beforeMeta;focused_metadata=$focusedMeta;after_metadata=$afterMeta}
        }
        'Focus'{
            if($ControlId -ne 301){throw 'Only own settings button may receive test focus'}
            Focus-OwnWindow (Control $ControlId)
            @{accepted=$true}
        }
        'Toggle'{
            if($ControlId -ne 424){throw 'Only own default-control checkbox may toggle'}
            $checkbox=Control $ControlId
            if($FocusBeforeInvoke){Focus-OwnWindow $checkbox}
            $checkbox.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
            @{accepted=$true}
        }
        'Command'{
            if($ControlId -notin @(5001,5002,5010,5011,5012,5020,5021,5022,5030,5031,5032,5033,5034,5040,5041,5042,5043,5044,5045,5046)){throw 'Preview command not allowed'}
            $null=[StyleProbe]::PostMessage($main[0],0x111,[IntPtr]$ControlId,[IntPtr]::Zero);@{accepted=$true}
        }
        'OpenMenu'{$button=Control 301;Focus-OwnWindow $button;$null=[StyleProbe]::PostMessage([IntPtr]$button.Current.NativeWindowHandle,0xf5,[IntPtr]::Zero,[IntPtr]::Zero);@{accepted=$true}}
        'SelectMenu'{
            $menus=@([StyleProbe]::Menus($ProcessId));if($menus.Count -ne 1){throw 'Own menu not unique'}
            $items=@((Elements $menus[0])|Where-Object{$_.Current.ControlType -eq [System.Windows.Automation.ControlType]::MenuItem -and $_.Current.Name -eq '中文'})
            if($items.Count -ne 1){throw 'Own menu Chinese action missing'}
            $items[0].GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke();@{accepted=$true}
        }
        'DismissMenu'{$null=[StyleProbe]::PostMessage($main[0],0x1f,[IntPtr]::Zero,[IntPtr]::Zero);@{accepted=$true}}
        'SetValue'{
            if($ControlId -ne 502){throw 'Only own Relay field is editable'}
            $inputElement=Control $ControlId
            Focus-OwnWindow $inputElement
            $inputElement.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
            $readback=$inputElement.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
            if($readback -cne $Value){throw 'Own Relay input readback mismatch'}
            @{accepted=$true;value_matches=$true}
        }
        'Close'{
            $window=if($ControlId -eq -1){$menus=@([StyleProbe]::Menus($ProcessId));if($menus.Count -ne 1){throw 'Own menu unavailable'};$menus[0]}elseif($ControlId){[StyleProbe]::GetAncestor([IntPtr](Control $ControlId).Current.NativeWindowHandle,2)}else{$main[0]}
            $null=[StyleProbe]::PostMessage($window,0x10,[IntPtr]::Zero,[IntPtr]::Zero);@{accepted=$true}
        }
        'Capture'{
            if(-not $OutputPath){throw 'Capture output required'}
            $window=if($ControlId -eq -1){$menus=@([StyleProbe]::Menus($ProcessId));if($menus.Count -ne 1){throw 'Own menu unavailable'};$menus[0]}elseif($ControlId){[StyleProbe]::GetAncestor([IntPtr](Control $ControlId).Current.NativeWindowHandle,2)}else{$main[0]}
            Add-Type -AssemblyName System.Drawing;$rect=[StyleProbe+Rect]::new();$null=[StyleProbe]::GetWindowRect($window,[ref]$rect)
            $bitmap=[Drawing.Bitmap]::new($rect.Right-$rect.Left,$rect.Bottom-$rect.Top);$g=[Drawing.Graphics]::FromImage($bitmap)
            try{$dc=$g.GetHdc();try{if(-not [StyleProbe]::PrintWindow($window,$dc,2)){throw 'Own-window capture failed'}}finally{$g.ReleaseHdc($dc)};$bitmap.Save([IO.Path]::GetFullPath($OutputPath),[Drawing.Imaging.ImageFormat]::Png)}finally{$g.Dispose();$bitmap.Dispose()};$clientRect=[StyleProbe+Rect]::new();$null=[StyleProbe]::GetClientRect($window,[ref]$clientRect);@{captured=$true;handle=$window.ToInt64();class=[StyleProbe]::Class($window);dpi=[StyleProbe]::GetDpiForWindow($window);client_width=$clientRect.Right-$clientRect.Left;client_height=$clientRect.Bottom-$clientRect.Top;window_bounds=@($rect.Left,$rect.Top,$rect.Right,$rect.Bottom)}
        }
    }
    $result|ConvertTo-Json -Depth 7;exit 0
}

$temporary=[IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp'))
$output=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $temporary.StartsWith('C:\',[StringComparison]::OrdinalIgnoreCase) -or -not $output.StartsWith($temporary.TrimEnd('\')+'\',[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $output)){throw 'Require new C-temp output'}
$exe=(Resolve-Path -LiteralPath $Executable).Path
if((Get-FileHash -LiteralPath $exe).Hash -ine $ExpectedHash -or [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion -cne $ExpectedVersion){throw 'Preview hash or version mismatch'}
New-Item -ItemType Directory -Path $output|Out-Null
$start=[Diagnostics.ProcessStartInfo]::new($exe);$start.UseShellExecute=$false;$start.WorkingDirectory=$output;$start.RedirectStandardError=$true
foreach($key in @('APPDATA','LOCALAPPDATA','TEMP','TMP')){$directory=Join-Path $output $key.ToLowerInvariant();New-Item -ItemType Directory -Path $directory|Out-Null;$start.Environment[$key]=$directory}
foreach($key in @($start.Environment.Keys)){if($key -match '^(REMOTEOPS_|JLAI_)'){$start.Environment.Remove($key)|Out-Null}}
$app=[Diagnostics.Process]::Start($start);$applicationError=$app.StandardError.ReadToEndAsync();$identity=$app.StartTime.ToUniversalTime().ToString('o')
$checks=[Collections.Generic.List[object]]::new();$failure=$null;$step='startup'
function Client([string]$Operation='Snapshot',[int]$Id=0,[string]$Text,[string]$Capture){
    $script:step=$Operation+'_'+$Id
    $client=[Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'));$client.UseShellExecute=$false;$client.CreateNoWindow=$true;$client.RedirectStandardOutput=$true;$client.RedirectStandardError=$true
    $client.StandardOutputEncoding=[Text.UTF8Encoding]::new($false)
    foreach($arg in @('-NoProfile','-STA','-File',$PSCommandPath,'-ExpectedHash',$ExpectedHash,'-ExpectedVersion',$ExpectedVersion,'-Action',$Operation,'-ProcessId',[string]$app.Id,'-ProcessStartUtc',$identity,'-ControlId',[string]$Id)){$client.ArgumentList.Add($arg)}
    if($PSBoundParameters.ContainsKey('Text')){$client.ArgumentList.Add('-Value');$client.ArgumentList.Add($Text)}
    if($Capture){$client.ArgumentList.Add('-OutputPath');$client.ArgumentList.Add($Capture)}
    $child=[Diagnostics.Process]::Start($client);$stdout=$child.StandardOutput.ReadToEndAsync();$stderr=$child.StandardError.ReadToEndAsync()
    try{if(-not $child.WaitForExit(12000)){$child.Kill();$child.WaitForExit();throw 'UIA worker timeout'};if($child.ExitCode){[IO.File]::WriteAllText((Join-Path $output 'client-last-error.txt'),$stderr.GetAwaiter().GetResult());throw ('UIA worker failed: '+$stderr.GetAwaiter().GetResult().Trim())};return $stdout.GetAwaiter().GetResult()|ConvertFrom-Json}finally{$child.Dispose()}
}
function Check([string]$Name,[bool]$Passed){$script:step=$Name;$checks.Add([pscustomobject]@{name=$Name;passed=$Passed});if(-not $Passed){throw 'Preview assertion failed'}}
function Row($Snapshot,[int]$Id){$found=@($Snapshot.windows.controls|Where-Object id -eq $Id);if($found.Count -ne 1){throw ('Preview row missing '+$Id)};return $found[0]}
function Footer-Fits($Snapshot){
    $main=@($Snapshot.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0]
    foreach($id in @(307,308,338,309)){$row=Row $Snapshot $id;if($row.text_width -lt 0 -or $row.text_width+(17+6+16)*$main.dpi/96 -gt $row.bounds[2]+1){return $false}}
    return $true
}
function Wait-Control([int]$Id){
    $watch=[Diagnostics.Stopwatch]::StartNew();$lastSnapshot=$null;$lastError=$null
    do{
        try{$s=Client;$lastSnapshot=$s;if(@($s.windows.controls|Where-Object id -eq $Id).Count -eq 1){return $s}}
        catch{$lastError=$_.Exception.Message;if($app.HasExited -or $lastError -match 'desktop_'){throw}}
        if($watch.ElapsedMilliseconds -gt 15000){
            $script:step='WaitControl_'+$Id
            [IO.File]::WriteAllText((Join-Path $output 'wait-timeout.json'),(@{expected_control=$Id;last_snapshot=$lastSnapshot;last_error=$lastError;responding=$app.Responding}|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($false))
            throw ('Preview control wait timeout: '+$Id)
        }
        Start-Sleep -Milliseconds 80
    }while($true)
}
function Capture([string]$Name,[int]$Id=0){$null=Client 'Capture' $Id -Capture (Join-Path $output ($Name+'.png'))}
try{
    if(-not $ClearBackOnly){
    $s=Wait-Control 303;$main=@($s.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0]
    Check 'main_500x375_at_actual_dpi' ($main.width -eq [math]::Round(500*$main.dpi/96) -and $main.height -eq [math]::Round(375*$main.dpi/96))
    Check 'synthetic_code_visible' ((Row $s 303).text -eq '123-456-789')
    foreach($id in @(301,304,307,308,338,309)){Check ('native_button_uia_'+$id) ((Row $s $id).invoke -and (Row $s $id).type -eq 'ControlType.Button')}
    $heading=Row $s 300;$settings=Row $s 301
    Check 'title_row_vertical_alignment' ([math]::Abs(($heading.bounds[1]+$heading.bounds[3]/2)-($settings.bounds[1]+$settings.bounds[3]/2)) -le 1)
    $footer=@(307,308,338,309|ForEach-Object{Row $s $_})
    for($n=1;$n -lt $footer.Count;$n++){Check ('footer_buttons_do_not_overlap_'+$n) ($footer[$n-1].bounds[0]+$footer[$n-1].bounds[2] -lt $footer[$n].bounds[0])}
    Check 'footer_buttons_equal_width_height' (@($footer|Where-Object{[math]::Abs($_.bounds[2]-$footer[0].bounds[2]) -gt 1 -or [math]::Abs($_.bounds[3]-$footer[0].bounds[3]) -gt 1}).Count -eq 0)
    Check 'footer_buttons_expected_size' ([math]::Abs($footer[0].bounds[2]-107*$main.dpi/96) -le 1 -and [math]::Abs($footer[0].bounds[3]-37*$main.dpi/96) -le 1)
    Check 'footer_buttons_share_baseline' (@($footer|Where-Object{$_.bounds[1] -ne $footer[0].bounds[1]}).Count -eq 0)
    $gaps=@(for($n=1;$n -lt $footer.Count;$n++){$footer[$n].bounds[0]-$footer[$n-1].bounds[0]-$footer[$n-1].bounds[2]})
    Check 'footer_buttons_equal_spacing' (@($gaps|Where-Object{[math]::Abs($_-8*$main.dpi/96) -gt 1}).Count -eq 0)
    Check 'footer_chinese_labels_fit' (Footer-Fits $s)
    $openedInvocation=Client 'Invoke' 338;$opened=Wait-Control 202
    $observerHandle=@($opened.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver')[0].handle
    $retained=(Row $opened 202).text
    [IO.File]::WriteAllText((Join-Path $output 'main-observer-focus-evidence.json'),(@{invocation=$openedInvocation;after_snapshot=$opened}|ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false))
    Check 'main_observer_entry_opens_without_focus' ($openedInvocation.foreground_before -eq $openedInvocation.foreground_after -and $opened.foreground -eq $openedInvocation.foreground_before -and (Row $opened 202).read_only -eq $true)
    Check 'footer_chinese_open_observer_label_fits' (Footer-Fits $opened)
    $null=Client 'Invoke' 338;$hidden=Client
    Check 'main_observer_entry_hides' (@($hidden.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver').Count -eq 0)
    $null=Client 'Invoke' 338;$reopened=Wait-Control 202
    Check 'observer_entry_reuses_window_and_output' (@($reopened.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver')[0].handle -eq $observerHandle -and (Row $reopened 202).text -eq $retained)
    $null=Client 'Close' 202;$hidden=Client
    Check 'observer_close_hides_instead_of_stopping_preview' (@($hidden.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver').Count -eq 0 -and -not $app.HasExited)
    $null=Client 'Invoke' 338;$reopened=Wait-Control 202
    Check 'observer_reopens_after_titlebar_close' (@($reopened.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver')[0].handle -eq $observerHandle)
    $null=Client 'Invoke' 205;$s=Wait-Control 303
    Capture 'main-light-zh'
    $null=Client 'OpenMenu';$menuWait=[Diagnostics.Stopwatch]::StartNew();do{$s=Client;if($s.menu_count -eq 1){break};if($menuWait.ElapsedMilliseconds -gt 5000){throw 'Native menu missing'}}while($true);Capture 'settings-light-zh' -1;$null=Client 'SelectMenu';$null=Client 'DismissMenu';Check 'native_settings_menu_operates' ($true)
    $null=Client 'Invoke' 307;$s=Wait-Control 409;Capture 'details-light-zh' 409
    Check 'details_modal_disables_owner' (-not @($s.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0].enabled)
    $observerInvocation=Client 'Invoke' 337;$s=Wait-Control 202
    [IO.File]::WriteAllText((Join-Path $output 'observer-focus-evidence.json'),(@{invocation=$observerInvocation;after_snapshot=$s}|ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false))
    Check 'observer_open_no_focus' ($observerInvocation.foreground_before -eq $observerInvocation.foreground_after -and $s.foreground -eq $observerInvocation.foreground_before);Capture 'observer-light-zh' 202
    Check 'observer_real_readonly' ((Row $s 202).read_only -eq $true)
    Check 'observer_contains_three_mock_sources' ((Row $s 202).text -match 'SSH' -and (Row $s 202).text -match 'CMD|PowerShell' -and (Row $s 202).text -match 'Console|串口')
    Check 'observer_synthetic_auth_hidden' ((Row $s 202).text -match '已隐藏|REDACTED')
    $null=Client 'Invoke' 205;$null=Client 'Invoke' 409;$s=Wait-Control 303
    Check 'closing_modal_reenables_owner' (@($s.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0].enabled)
    $null=Client 'Command' 5020;$s=Wait-Control 425;Capture 'permissions-light-zh' 425;$null=Client 'Invoke' 425
    $null=Client 'Command' 5033;$s=Wait-Control 502;$null=Client 'SetValue' 502 '演示 Relay 中文 空格'
    $s=Client;Check 'native_chinese_edit_value' ((Row $s 502).text -eq '演示 Relay 中文 空格');Capture 'setup-light-zh' 502
    $setup=@($s.windows|Where-Object{$_.controls.id -contains 502})[0];$input=Row $s 502
    Check 'setup_native_edit_stays_single_line' ($input.type -eq 'ControlType.Edit' -and $input.read_only -eq $false -and ($input.native_style -band 4) -eq 0)
    Check 'setup_edit_is_vertically_centered' ([math]::Abs(($input.bounds[1]+$input.bounds[3]/2)-($setup.client_origin[1]+109*$setup.dpi/96)) -le 1)
    $longAddress='中文 中继地址 空格/'+('relay-segment-'*8)+':7443'
    $null=Client 'SetValue' 502 $longAddress;$s=Client
    Check 'setup_long_unicode_value_is_preserved' ((Row $s 502).text -ceq $longAddress)
    $null=Client 'Invoke' 505
    $null=Client 'Command' 5021;$s=Wait-Control 541;Capture 'about-light-zh' 541
    Check 'about_version_is_separate_from_description' ((Row $s 543).text.Contains("v$ExpectedVersion") -and -not (Row $s 542).text.Contains("v$ExpectedVersion"))
    Check 'about_button_has_native_invoke' ((Row $s 541).invoke)
    $null=Client 'Invoke' 541;$s=Wait-Control 303
    Check 'about_close_restores_main' (@($s.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0].enabled)
    $null=Client 'Command' 5034;$s=Wait-Control 524;Check 'fingerprint_readonly' ((Row $s 524).read_only -eq $true);Capture 'tls-light-zh' 524;$null=Client 'Invoke' 521
    $null=Client 'Invoke' 308;$s=Wait-Control 311;Check 'log_page_readonly' ((Row $s 311).read_only -eq $true)
    $log=(Row $s 311).text
    Check 'log_records_have_no_added_blank_lines' ($log -notmatch '\r?\n[ \t]*\r?\n' -and @($log -split '\r?\n'|Where-Object Length).Count -eq 4)
    Capture 'logs-light-zh' 311
    $null=Client 'Invoke' 327;$s=Wait-Control 311
    Check 'log_error_filter_preserves_one_record' ((Row $s 311).text -match 'demo-04' -and (Row $s 311).text -notmatch 'demo-01|demo-02|demo-03')
    $null=Client 'Invoke' 324;$s=Wait-Control 311
    Check 'log_all_filter_restores_exact_text' ((Row $s 311).text -ceq $log)
    $null=Client 'Invoke' 310
    $null=Client 'Command' 5012;$s=Wait-Control 303;Capture 'main-dark-zh'
    Check 'footer_dark_chinese_labels_fit' (Footer-Fits $s)
    $null=Client 'Invoke' 308;$s=Wait-Control 311;Capture 'logs-dark-zh' 311
    Check 'dark_log_preserves_exact_records' ((Row $s 311).text -ceq $log)
    $null=Client 'Invoke' 310
    $null=Client 'Command' 5002;$s=Wait-Control 303;Capture 'main-dark-en'
    Check 'footer_english_labels_fit' (Footer-Fits $s)
    $null=Client 'Invoke' 338;$s=Wait-Control 202
    Check 'footer_english_open_observer_label_fits' (Footer-Fits $s)
    Capture 'main-dark-en-viewer-open'
    $null=Client 'Invoke' 338;$s=Wait-Control 303
    $null=Client 'Command' 5021;$s=Wait-Control 541;Capture 'about-dark-en' 541
    Check 'about_english_copy_is_available' ((Row $s 543).text -match 'UI preview' -and (Row $s 542).text -match 'No connections')
    $null=Client 'Close' 541;$s=Wait-Control 303
    $null=Client 'Command' 5033;$s=Wait-Control 502;Capture 'setup-dark-en' 502
    $setup=@($s.windows|Where-Object{$_.controls.id -contains 502})[0];$input=Row $s 502
    Check 'setup_dark_english_edit_is_vertically_centered' ([math]::Abs(($input.bounds[1]+$input.bounds[3]/2)-($setup.client_origin[1]+109*$setup.dpi/96)) -le 1)
    $null=Client 'Invoke' 505;$s=Wait-Control 303
    Check 'english_main_actions' ((Row $s 307).text -match 'details|Details')
    $null=Client 'Command' 5011;$s=Wait-Control 303;Capture 'main-light-en'
    }
    $null=Wait-Control 303
    $null=Client 'Command' 5001
    $null=Client 'Invoke' 308;$null=Wait-Control 311;$null=Client 'Invoke' 329;$s=Wait-Control 311
    Check 'log_clear_leaves_empty_readonly_field' ((Row $s 311).text -eq '' -and (Row $s 311).read_only -eq $true)
    $null=Client 'Invoke' 310;$null=Wait-Control 303
    $null=Client 'Invoke' 309;$s=Wait-Control 463;Capture 'exit-light-zh' 463
    $null=Client 'Invoke' 462;$s=Wait-Control 303;Check 'exit_cancel_keeps_preview_running' (-not $app.HasExited)
    $null=Client 'Invoke' 309;$null=Wait-Control 463;$null=Client 'Invoke' 463
    Check 'normal_preview_exit' ($app.WaitForExit(8000) -and $app.ExitCode -eq 0)
}catch{$failure=[pscustomobject]@{step=$step;category=if($_.Exception.Message -match 'desktop_'){'environment_unavailable'}else{'style_preview_or_fixture_failure'};detail=$_.Exception.Message}}
finally{
    if(-not $app.HasExited){try{$null=Client 'Close';$null=Wait-Control 463;$null=Client 'Invoke' 463;$null=$app.WaitForExit(5000)}catch{};if(-not $app.HasExited){$app.Kill();$app.WaitForExit();$failure??=[pscustomobject]@{step='cleanup';category='preview_forced_exit'}}}
    $applicationExit=if($app.HasExited){$app.ExitCode}else{$null};if($applicationError.IsCompleted -and -not [string]::IsNullOrWhiteSpace($applicationError.Result)){[IO.File]::WriteAllText((Join-Path $output 'application-error.txt'),$applicationError.Result)};$app.Dispose();$record=[pscustomobject]@{passed=$null -eq $failure;executable_sha256=$ExpectedHash.ToLowerInvariant();product_version=$ExpectedVersion;checks=@($checks);failure=$failure;application_exit_code=$applicationExit;clear_back_only=[bool]$ClearBackOnly;demo_only=$true;real_business_verified=$false;rdp_dpi_verified=$false}
    [IO.File]::WriteAllText((Join-Path $output 'report.json'),($record|ConvertTo-Json -Depth 6),[Text.UTF8Encoding]::new($false))
}
$record|ConvertTo-Json -Depth 6
if(-not $record.passed){exit 1}
