#requires -Version 7.2
<# .SYNOPSIS 验证独立样式小样的分类筛选、并发状态和结束，不连接真实业务。 #>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][Alias('Exe')][string]$Executable,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedHash,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [switch]$VisualOnly,
    [switch]$PauseProbe,
    [switch]$LargeOnly
)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$exe=(Resolve-Path -LiteralPath $Executable).Path
$output=[IO.Path]::GetFullPath($OutputDirectory)
$tempRoot=[IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp')).TrimEnd('\')
if(-not $IsWindows -or -not $tempRoot.StartsWith('C:\',[StringComparison]::OrdinalIgnoreCase) -or -not $output.StartsWith($tempRoot+'\',[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $output)){throw '必须使用全新的 C 盘本地 Temp 子目录。'}
for($ancestor=Split-Path -Parent $output;$ancestor;$ancestor=Split-Path -Parent $ancestor){if((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor).Attributes -band [IO.FileAttributes]::ReparsePoint)){throw '测试目录不能经过重解析点。'}}
if((Get-FileHash -LiteralPath $exe).Hash -ine $ExpectedHash -or [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion -cne '0.3.7'){throw '仅接受精确摘要对应的 0.3.7 样式生成物。'}
. (Join-Path $PSScriptRoot 'NativeStyle-DesktopGate.ps1')
Assert-NativeStyleDesktopUnlocked
Add-Type -TypeDefinition @'
using System;using System.Runtime.InteropServices;using System.Text;
/// <summary>仅核验本次小样所属的原生文本控件。</summary>
public static class CategoryEditProbe {
 [DllImport("user32.dll",CharSet=CharSet.Unicode)]static extern int GetClassName(IntPtr window,StringBuilder value,int length);
 [DllImport("user32.dll")]static extern uint GetWindowThreadProcessId(IntPtr window,out uint pid);
 [DllImport("user32.dll")]static extern IntPtr GetAncestor(IntPtr window,uint flags);
 /// <summary>进程匹配后读取窗口类名，拒绝其他进程的句柄。</summary>
 public static string OwnClass(IntPtr window,int expectedPid){uint pid;GetWindowThreadProcessId(window,out pid);if(pid!=expectedPid)return String.Empty;var value=new StringBuilder(128);return GetClassName(window,value,value.Capacity)>0?value.ToString():String.Empty;}
 /// <summary>确认前台句柄是本次主窗口或其仍有效的子控件。</summary>
 public static bool OwnRoot(IntPtr window,int expectedPid,IntPtr root){uint pid;GetWindowThreadProcessId(window,out pid);return pid==expectedPid&&root!=IntPtr.Zero&&GetAncestor(window,2)==root;}
}
'@
$helper=Join-Path $PSScriptRoot 'Test-NativeStylePreview.ps1'
New-Item -ItemType Directory -Path $output|Out-Null
$checks=[Collections.Generic.List[object]]::new();$frames=[Collections.Generic.List[object]]::new();$invocations=[Collections.Generic.List[object]]::new();$failure=$null;$step='startup';$lastSnapshot=$null
$start=[Diagnostics.ProcessStartInfo]::new($exe);$start.UseShellExecute=$false;$start.WorkingDirectory=$output;$start.RedirectStandardError=$true
foreach($key in @('APPDATA','LOCALAPPDATA','TEMP','TMP')){$directory=Join-Path $output $key.ToLowerInvariant();New-Item -ItemType Directory -Path $directory|Out-Null;$start.Environment[$key]=$directory}
foreach($key in @($start.Environment.Keys)){if($key -match '^(REMOTEOPS_|JLAI_)'){$null=$start.Environment.Remove($key)}}
$app=[Diagnostics.Process]::Start($start);$applicationError=$app.StandardError.ReadToEndAsync();$identity=$app.StartTime.ToUniversalTime().ToString('o')
function Client([string]$Action='Snapshot',[int]$Id=0,[string]$Path){
    $script:step=$Action+'_'+$Id
    $client=[Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'));$client.UseShellExecute=$false;$client.CreateNoWindow=$true;$client.RedirectStandardOutput=$true;$client.RedirectStandardError=$true;$client.StandardOutputEncoding=[Text.UTF8Encoding]::new($false)
    foreach($item in @('-NoProfile','-STA','-File',$helper,'-ExpectedVersion',([Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion),'-Action',$Action,'-ProcessId',[string]$app.Id,'-ProcessStartUtc',$identity,'-ExpectedHash',$ExpectedHash,'-ControlId',[string]$Id)){$client.ArgumentList.Add($item)}
    if($Action -eq 'Invoke'){$client.ArgumentList.Add('-FocusBeforeInvoke')}
    if($Path){$client.ArgumentList.Add('-OutputPath');$client.ArgumentList.Add($Path)}
    $worker=[Diagnostics.Process]::Start($client);$stdout=$worker.StandardOutput.ReadToEndAsync();$stderr=$worker.StandardError.ReadToEndAsync()
    try{
        if(-not $worker.WaitForExit(12000)){$worker.Kill();$worker.WaitForExit();throw 'UIA worker timeout'}
        if($worker.ExitCode){$errorText=$stderr.GetAwaiter().GetResult();[IO.File]::WriteAllText((Join-Path $output 'client-last-error.txt'),$errorText);throw ('UIA worker failed: '+$errorText.Trim())}
        $result=$stdout.GetAwaiter().GetResult()|ConvertFrom-Json
        if($Action -eq 'Snapshot'){$script:lastSnapshot=$result}
        if($Action -eq 'Invoke'){
            $knownButton=if($script:lastSnapshot){@($script:lastSnapshot.windows.controls|Where-Object id -eq $Id)}else{@()}
            $invocations.Add([pscustomobject]@{control_id=$Id;utc=[DateTime]::UtcNow.ToString('o');last_snapshot_foreground=if($script:lastSnapshot){$script:lastSnapshot.foreground}else{$null};last_snapshot_button=@($knownButton|Select-Object id,hwnd,text,enabled,bounds);invocation=$result})
            [IO.File]::WriteAllText((Join-Path $output 'invocation-history.json'),(@($invocations)|ConvertTo-Json -Depth 9),[Text.UTF8Encoding]::new($false))
        }
        return $result
    }finally{$worker.Dispose()}
}
function Check([string]$Name,[bool]$Passed){$script:step=$Name;$checks.Add([pscustomobject]@{name=$Name;passed=$Passed});if(-not $Passed){throw ('分类观察窗检查失败：'+$Name)}}
function Row($Snapshot,[int]$Id){$rows=@($Snapshot.windows.controls|Where-Object id -eq $Id);if($rows.Count -ne 1){throw ('分类观察窗控件不唯一：'+$Id)};return $rows[0]}
function Wait-State([string]$ConditionName,[scriptblock]$Condition,[int]$TimeoutMs=8000){
    $watch=[Diagnostics.Stopwatch]::StartNew()
    do{
        $state=Client
        if(& $Condition $state){return $state}
        if($watch.ElapsedMilliseconds -ge $TimeoutMs){$script:step=$ConditionName;throw ('分类观察窗等待超时：'+$ConditionName)}
        Start-Sleep -Milliseconds 100
    }while($true)
}
function Wait-Control([int]$Id){return Wait-State ('control_'+$Id) {param($s) @($s.windows.controls|Where-Object id -eq $Id).Count -eq 1}}
function Is-Running($Snapshot,[int]$Count){return (Row $Snapshot 222).text -match ('执行中\s*[：:]\s*'+$Count+'(?:\D|$)')}
function Selected($Snapshot,[string]$Name){return (Row $Snapshot 222).text -match ('查看\s*[：:]\s*'+[regex]::Escape($Name)+'(?:\s|[·|]|$)')}
function Running-Label($Snapshot,[int]$Id,[int]$Count){return (Row $Snapshot $Id).text -match ('·\s*'+$Count+'(?:\D|$)')}
function Capture([string]$Name){
    $path=Join-Path $output ($Name+'.png');$capture=Client 'Capture' 202 $path
    $frames.Add([pscustomobject]@{file=[IO.Path]::GetFileName($path);sha256=(Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant();dpi=$capture.dpi;client_width=$capture.client_width;client_height=$capture.client_height;window_bounds=$capture.window_bounds})
}
try{
    $null=$app.WaitForInputIdle(5000);$null=Wait-Control 303
    if($VisualOnly){
        $baselineInvoke=Client 'Invoke' 304;$autoBefore=Client
        $main=@($autoBefore.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0]
        Check 'automatic_show_has_valid_own_main_baseline' ($autoBefore.foreground -ne 0 -and $baselineInvoke.after_metadata.process_id -eq $app.Id -and [CategoryEditProbe]::OwnRoot([IntPtr]$autoBefore.foreground,$app.Id,[IntPtr]$main.handle))
        $null=Client 'Command' 5043;$state=Wait-Control 202
        $observer=@($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver')[0]
        [IO.File]::WriteAllText((Join-Path $output 'observer-autoshow-focus.json'),(@{baseline_invocation=$baselineInvoke;before_snapshot=$autoBefore;after_snapshot=$state}|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($false))
        Check 'automatic_show_keeps_foreground' ($autoBefore.foreground -eq $state.foreground -and $state.foreground -ne $observer.handle)
    }else{
        $open=Client 'Invoke' 338;$state=Wait-Control 202
        $observer=@($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver')[0]
    }
    $edit=Row $state 202
    Check 'observer_opens_as_native_readonly_edit' ($edit.read_only -eq $true -and $edit.type -in @('ControlType.Edit','ControlType.Document') -and ($edit.native_style -band 0x800) -ne 0 -and [CategoryEditProbe]::OwnClass([IntPtr]$edit.hwnd,$app.Id) -ieq 'Edit')
    Check 'all_filter_is_initial_selection' (Selected $state '全部')
    Check 'default_history_is_idle' (Is-Running $state 0)
    $initial=(Row $state 202).text
    Check 'default_history_contains_chinese_and_masked_auth' ($initial -match '中文' -and $initial -match '\[已隐藏\]' -and $initial -match '\[CMD\]' -and $initial -match '\[SSH\]')
    if($LargeOnly){
        $null=Client 'Command' 5044
        $state=Wait-State 'large_demo_text_ready' {param($s) (Row $s 202).text_length -eq 579615} 12000
        $large=Row $state 202;$largeText=$large.text;$largeHandle=$large.hwnd
        Check 'large_demo_readonly_native_edit' ($large.read_only -eq $true -and [CategoryEditProbe]::OwnClass([IntPtr]$large.hwnd,$app.Id) -ieq 'Edit')
        Check 'large_demo_character_count_preserved' ($large.text_length -eq 579615 -and $largeText.Length -eq 579615)
        Check 'large_demo_all_10001_records_preserved' (@($largeText -split '\r\n').Count -eq 10001 -and $largeText -match '00001 · 中文日志' -and $largeText -match '10000 · 中文日志' -and (Row $state 203).text -match '10001/10001')
        $null=Client 'Invoke' 213;$state=Wait-State 'large_shell_selected' {param($s) Selected $s 'Shell'}
        Check 'large_shell_filter_preserves_exact_text' ((Row $state 202).text -ceq $largeText)
        Check 'large_shell_filter_reuses_native_edit' ((Row $state 202).hwnd -eq $largeHandle)
        $null=Client 'Invoke' 212;$state=Wait-State 'large_powershell_empty' {param($s) (Selected $s 'PowerShell') -and (Row $s 202).text -ceq ''}
        $empty=Row $state 202
        Check 'large_powershell_filter_is_true_empty_with_hint' ($empty.text -ceq '' -and @($state.windows.controls|Where-Object id -eq 223).Count -eq 1 -and $empty.read_only -eq $true)
        Check 'large_empty_filter_has_no_unneeded_scrollbars' (($empty.native_style -band (0x100000 -bor 0x200000)) -eq 0)
        Check 'large_empty_filter_reuses_native_edit' ($empty.hwnd -eq $largeHandle)
        $null=Client 'Invoke' 210;$state=Wait-State 'large_all_restored' {param($s) (Selected $s '全部') -and (Row $s 202).text_length -eq 579615}
        Check 'large_all_filter_restores_exact_text' ((Row $state 202).text -ceq $largeText -and (Row $state 202).hwnd -eq $largeHandle)
        Capture 'light-large-all-restored'
    }elseif($PauseProbe){
        $null=Client 'Invoke' 220;$state=Wait-State 'probe_started' {param($s) -not (Is-Running $s 0)}
        Check 'probe_started_real_demo' (-not (Is-Running $state 0))
        $null=Client 'Invoke' 204;$state=Wait-State 'probe_first_pause' {param($s) (Row $s 204).text -match '继续显示'}
        Check 'probe_first_pause_applied' ((Row $state 204).text -match '继续显示')
        $null=Client 'Invoke' 206;$state=Wait-State 'probe_first_resume' {param($s) (Row $s 204).text -match '暂停显示'}
        Check 'probe_first_resume_applied' ((Row $state 204).text -match '暂停显示')
        $null=Client 'Command' 5012;$state=Wait-Control 202
        $null=Client 'Invoke' 205;$hidden=Client
        Check 'probe_hides_observer' (@($hidden.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver').Count -eq 0)
        $null=Client 'Invoke' 338;$state=Wait-Control 202
        Check 'probe_reopens_observer' (@($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver').Count -eq 1)
        $null=Client 'Invoke' 220;$state=Wait-State 'probe_second_started' {param($s) -not (Is-Running $s 0)}
        $null=Client 'Invoke' 204;$state=Wait-State 'probe_second_pause' {param($s) (Row $s 204).text -match '继续显示'}
        Check 'probe_second_pause_after_reopen_applied' ((Row $state 204).text -match '继续显示')
        $frozen=(Row $state 202).text
        $null=Client 'Command' 5011;$state=Wait-Control 202
        Check 'probe_paused_theme_keeps_caption_and_body' ((Row $state 204).text -match '继续显示' -and (Row $state 202).text -ceq $frozen)
        $null=Client 'Invoke' 206;$state=Wait-State 'probe_second_resume' {param($s) (Row $s 204).text -match '暂停显示'}
        $null=Client 'Invoke' 221;$state=Wait-State 'probe_finish' {param($s) Is-Running $s 0}
        Check 'probe_finishes_idle' (Is-Running $state 0)
    }elseif($VisualOnly){
        Add-Type -AssemblyName System.Drawing
        foreach($visualTheme in @(@('light',5011,$false),@('dark',5012,$true))){
            $themeName=[string]$visualTheme[0];$dark=[bool]$visualTheme[2]
            $null=Client 'Command' ([int]$visualTheme[1]);$null=Wait-Control 202
            $null=Client 'Invoke' 212;$null=Client 'Invoke' 220
            $state=Wait-State 'visual_ssh_two_while_powershell_selected' {param($s) (Running-Label $s 214 2) -and (Selected $s 'PowerShell')} 18000
            Check ($themeName+'_visual_before_powershell_selected_ssh_two') ((Selected $state 'PowerShell') -and (Running-Label $state 214 2))
            $selected=Row $state 212;$ssh=Row $state 214
            $path=Join-Path $output ($themeName+'-selected-powershell-ssh-two.png')
            $capture=Client 'Capture' 202 $path
            $after=Client
            Check ($themeName+'_visual_after_powershell_selected_ssh_two') ((Selected $after 'PowerShell') -and (Running-Label $after 214 2))
            $dpi=$capture.dpi;$bitmap=[Drawing.Bitmap]::new($path)
            try{
                $dotX=[int][math]::Round($ssh.bounds[0]+10*$dpi/96-$capture.window_bounds[0]);$dotY=[int][math]::Round($ssh.bounds[1]+$ssh.bounds[3]/2-$capture.window_bounds[1])
                $borderX=[int][math]::Round($selected.bounds[0]+$selected.bounds[2]/2-$capture.window_bounds[0]);$borderY=[int][math]::Round($selected.bounds[1]-$capture.window_bounds[1])
                $dotExpected=if($dark){[Drawing.Color]::FromArgb(255,29,211,98).ToArgb()}else{[Drawing.Color]::FromArgb(255,13,165,75).ToArgb()}
                $borderExpected=if($dark){[Drawing.Color]::FromArgb(255,20,105,245).ToArgb()}else{[Drawing.Color]::FromArgb(255,15,103,242).ToArgb()}
                $dotActual=$bitmap.GetPixel($dotX,$dotY).ToArgb();$borderActual=$bitmap.GetPixel($borderX,$borderY).ToArgb()
                Check ($themeName+'_running_ssh_dot_is_green') ($dotActual -eq $dotExpected)
                Check ($themeName+'_selected_powershell_border_is_blue') ($borderActual -eq $borderExpected)
                $frames.Add([pscustomobject]@{file=[IO.Path]::GetFileName($path);sha256=(Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant();dpi=$dpi;window_bounds=$capture.window_bounds;ssh_native_bounds=$ssh.bounds;powershell_native_bounds=$selected.bounds;dot_point=@($dotX,$dotY);dot_argb=$dotActual;dot_expected_argb=$dotExpected;border_point=@($borderX,$borderY);border_argb=$borderActual;border_expected_argb=$borderExpected;before_status=(Row $state 222).text;after_status=(Row $after 222).text})
            }finally{$bitmap.Dispose()}
            $null=Client 'Invoke' 221;$state=Wait-State 'visual_finish_all' {param($s) Is-Running $s 0}
            Check ($themeName+'_visual_finish_all_is_idle') (Is-Running $state 0)
        }
    }else{
    foreach($theme in @(@('light',5011),@('dark',5012))){
        $themeName=$theme[0];$null=Client 'Command' $theme[1];$state=Wait-Control 202
        foreach($entry in @(@(210,'全部'),@(211,'CMD'),@(212,'PowerShell'),@(213,'Shell'),@(214,'SSH'),@(215,'串口'))){
            $id=[int]$entry[0];$name=[string]$entry[1]
            Check ($themeName+'_native_filter_'+$id) ((Row $state $id).invoke -and (Row $state $id).type -eq 'ControlType.Button')
            $editBefore=(Row $state 202).hwnd
            $null=Client 'Invoke' $id;$state=Wait-State ('selected_'+$name) {param($s) Selected $s $name}
            $text=(Row $state 202).text
            $matches=switch($id){
                210 {$text -ceq $initial}
                211 {$text -ceq "[CMD] > echo 中文输出正常`r`n中文输出正常。"}
                212 {$text -ceq '' -and @($state.windows.controls|Where-Object id -eq 223).Count -eq 1}
                213 {$text -ceq '' -and @($state.windows.controls|Where-Object id -eq 223).Count -eq 1}
                214 {$text -match '\[SSH\]' -and $text -notmatch '\[CMD\]|串口'}
                215 {$text -match '串口' -and $text -notmatch '\[CMD\]|\[SSH\]'}
            }
            Check ($themeName+'_filter_'+$id+'_only_own_records') ([bool]$matches)
            Check ($themeName+'_filter_'+$id+'_remains_readonly') ((Row $state 202).read_only -eq $true)
            Check ($themeName+'_filter_'+$id+'_reuses_native_edit') ((Row $state 202).hwnd -eq $editBefore)
            if($id -in @(210,212,214)){Capture ($themeName+'-filter-'+$id)}
        }
        $null=Client 'Invoke' 210;$state=Wait-Control 202
        Check ($themeName+'_all_restores_exact_history') ((Row $state 202).text -ceq $initial)
    }
    # 运行状态独立于筛选；只冻结正文，不冻结合成任务的状态推进。
    $null=Client 'Command' 5011;$null=Client 'Invoke' 212;$null=Client 'Invoke' 220
    $state=Wait-State 'cmd_and_powershell_concurrent' {param($s) (Running-Label $s 211 1) -and (Running-Label $s 212 1)}
    Check 'concurrent_cmd_and_powershell_running' ((Running-Label $state 211 1) -and (Running-Label $state 212 1))
    Check 'start_keeps_selected_powershell' (Selected $state 'PowerShell')
    [IO.File]::WriteAllText((Join-Path $output 'first-pause-before.json'),($state|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($false))
    $pauseInvocation=Client 'Invoke' 204
    [IO.File]::WriteAllText((Join-Path $output 'first-pause-invocation.json'),($pauseInvocation|ConvertTo-Json -Depth 4),[Text.UTF8Encoding]::new($false))
    $state=Wait-State 'first_pause_display_applied' {param($s) (Row $s 204).text -match '继续显示'}
    $pausedText=(Row $state 202).text
    Check 'pause_is_labeled_display_only' ((Row $state 204).text -match '继续显示')
    $state=Wait-State 'two_ssh_tasks_running' {param($s) Running-Label $s 214 2} 18000
    Check 'two_ssh_tasks_visible_while_other_category_selected' ((Running-Label $state 214 2) -and (Selected $state 'PowerShell'))
    Check 'paused_display_has_not_consumed_new_rows' ((Row $state 202).text -ceq $pausedText)
    $null=Client 'Invoke' 214;$state=Wait-State 'selected_ssh_during_pause' {param($s) (Selected $s 'SSH') -and (Row $s 202).text -match '\[SSH(?:\s+#[^\]]+)?\]'}
    Check 'filter_switch_refreshes_snapshot_even_while_paused' ((Row $state 202).text -match '\[SSH(?:\s+#[^\]]+)?\]' -and (Row $state 204).text -match '继续显示')
    Capture 'light-concurrent-selected-ssh'
    $null=Client 'Invoke' 221;$state=Wait-State 'manual_all_finished' {param($s) Is-Running $s 0}
    Check 'finish_all_keeps_selected_ssh' (Selected $state 'SSH')
    Check 'finish_all_clears_all_running_badges' (@(211..215|Where-Object{(Row $state $_).text -match '·\s*\d+'}).Count -eq 0)
    $null=Client 'Invoke' 206;$state=Wait-Control 202;$finishedText=(Row $state 202).text
    Check 'latest_resumes_without_changing_category' ((Selected $state 'SSH') -and (Row $state 204).text -match '暂停显示')
    $null=Client 'Command' 5012;$state=Wait-Control 202;Capture 'dark-manually-finished-ssh'
    Check 'theme_switch_preserves_filter_and_history' ((Selected $state 'SSH') -and (Row $state 202).text -ceq $finishedText)
    $sameHandle=@($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver')[0].handle
    $null=Client 'Invoke' 205;$hidden=Client
    Check 'close_hides_observer' (@($hidden.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver').Count -eq 0)
    $null=Client 'Invoke' 338;$state=Wait-Control 202
    Check 'reopen_reuses_same_observer' (@($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver')[0].handle -eq $sameHandle)
    Check 'reopen_keeps_filter_and_history' ((Selected $state 'SSH') -and (Row $state 202).text -ceq $finishedText)
    # 独立第二次演示等待自然终态，不使用结束按钮冒充自然完成。
    $null=Client 'Invoke' 210;$null=Client 'Invoke' 220
    $state=Wait-State 'natural_demo_started' {param($s) -not (Is-Running $s 0)}
    Check 'second_demo_really_started' (-not (Is-Running $state 0))
    [IO.File]::WriteAllText((Join-Path $output 'second-pause-before.json'),($state|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($false))
    $pauseInvocation=Client 'Invoke' 204
    [IO.File]::WriteAllText((Join-Path $output 'second-pause-invocation.json'),($pauseInvocation|ConvertTo-Json -Depth 4),[Text.UTF8Encoding]::new($false))
    $state=Wait-State 'second_pause_display_applied' {param($s) (Row $s 204).text -match '继续显示'}
    $pausedBeforeTheme=(Row $state 202).text
    Check 'second_demo_pause_is_display_only' ((Row $state 204).text -match '继续显示')
    $null=Client 'Command' 5011;$state=Wait-Control 202
    Check 'theme_rebuild_during_pause_keeps_frozen_body' ((Row $state 202).text -ceq $pausedBeforeTheme -and (Row $state 204).text -match '继续显示')
    Check 'paused_theme_rebuild_keeps_selected_all' (Selected $state '全部')
    Capture 'light-paused-after-theme-switch'
    $null=Client 'Invoke' 206;$state=Wait-Control 202
    Check 'second_demo_latest_resumes_display' ((Row $state 204).text -match '暂停显示')
    $state=Wait-State 'natural_demo_finished' {param($s) Is-Running $s 0} 40000
    Check 'natural_completion_clears_running_badges' (@(211..215|Where-Object{(Row $state $_).text -match '·\s*\d+'}).Count -eq 0)
    Check 'natural_completion_keeps_all_filter' (Selected $state '全部')
    $all=(Row $state 202).text
    Check 'natural_demo_contains_all_five_sources' ($all -match '\[CMD(?:\s+#[^\]]+)?\]' -and $all -match '\[PowerShell(?:\s+#[^\]]+)?\]' -and $all -match '\[Shell(?:\s+#[^\]]+)?\]' -and $all -match '\[SSH(?:\s+#[^\]]+)?\]' -and $all -match '\[串口(?:\s+#[^\]]+)?\]')
    Check 'history_is_retained_after_demonstrations' ($all.Contains($initial))
    $null=Client 'Command' 5012;$state=Wait-Control 202
    Capture 'dark-naturally-finished-all'
    # 隐藏期间结束活动，再显式重开，验证追赶状态与正文且不残留后续计划。
    $hiddenObserver=@($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver')[0].handle
    $null=Client 'Invoke' 220;$state=Wait-State 'hidden_finish_demo_started' {param($s) -not (Is-Running $s 0)}
    Check 'hidden_finish_case_really_starts_operations' (-not (Is-Running $state 0))
    $null=Client 'Invoke' 205;$hidden=Client
    Check 'hidden_finish_case_observer_hidden' (@($hidden.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver').Count -eq 0)
    $null=Client 'Command' 5046;$hidden=Client
    Check 'menu_finish_keeps_observer_hidden' (@($hidden.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver').Count -eq 0)
    $null=Client 'Invoke' 338;$state=Wait-Control 202
    Check 'reopen_after_hidden_finish_reuses_observer' (@($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewObserver')[0].handle -eq $hiddenObserver)
    Check 'reopen_after_hidden_finish_is_idle' (Is-Running $state 0)
    Check 'reopen_after_hidden_finish_keeps_all_filter' (Selected $state '全部')
    Check 'reopen_after_hidden_finish_has_no_running_badges' (@(211..215|Where-Object{(Row $state $_).text -match '·\s*\d+'}).Count -eq 0)
    $hiddenFinishedText=(Row $state 202).text
    Check 'reopen_after_hidden_finish_has_end_records' ($hiddenFinishedText -match '\[完成\]' -and $hiddenFinishedText.Contains($all))
    Capture 'dark-finished-while-hidden'
    $state=Client
    Check 'finish_clears_pending_future_demo_output' ((Row $state 202).text -ceq $hiddenFinishedText -and (Is-Running $state 0))
    }
    $null=Client 'Invoke' 205;$null=Client 'Close';$null=Wait-Control 463;$null=Client 'Invoke' 463
    Check 'normal_exit' ($app.WaitForExit(5000) -and $app.ExitCode -eq 0)
}catch{
    $failure=[pscustomobject]@{step=$step;category=if($_.Exception.Message -match 'desktop_'){'environment_unavailable'}else{'observer_category_or_fixture_failure'};detail=$_.Exception.Message}
    if($lastSnapshot){[IO.File]::WriteAllText((Join-Path $output 'failure-last-snapshot.json'),($lastSnapshot|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($false))}
}finally{
    if(-not $app.HasExited){try{$state=Client;if($state.windows.controls.id -notcontains 463){$null=Client 'Close';$null=Wait-Control 463};$null=Client 'Invoke' 463;$null=$app.WaitForExit(5000)}catch{};if(-not $app.HasExited){$app.Kill();$app.WaitForExit();$failure??=[pscustomobject]@{step='cleanup';category='owned_preview_forced_exit';detail='Only this test process was terminated'}}}
    if($applicationError.IsCompleted -and -not [string]::IsNullOrWhiteSpace($applicationError.Result)){[IO.File]::WriteAllText((Join-Path $output 'application-error.txt'),$applicationError.Result)}
    $record=[ordered]@{passed=$null -eq $failure;executable_sha256=$ExpectedHash.ToLowerInvariant();checks=@($checks);frames=@($frames);failure=$failure;application_exit_code=$app.ExitCode;demo_only=$true;real_business_verified=$false;rdp_dpi_verified=$false;clipboard_touched=$false;invocation_count=$invocations.Count;invocation_log='invocation-history.json';visual_only=[bool]$VisualOnly;pause_probe=[bool]$PauseProbe;large_only=[bool]$LargeOnly}
    [IO.File]::WriteAllText((Join-Path $output 'report.json'),($record|ConvertTo-Json -Depth 9),[Text.UTF8Encoding]::new($false));$app.Dispose()
}
$record|ConvertTo-Json -Depth 5
if(-not $record.passed){exit 1}
