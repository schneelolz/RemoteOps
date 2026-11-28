#requires -Version 7.2
<# .SYNOPSIS 为本脚本隔离启动的纯样式预览生成逐窗体亮／暗截图相册。 #>
[CmdletBinding(DefaultParameterSetName='Capture')]
param(
    [Parameter(Mandatory,ParameterSetName='Capture')][string]$Executable,
    [Parameter(Mandatory,ParameterSetName='Capture')][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedHash,
    [Parameter(Mandatory,ParameterSetName='Capture')][string]$OutputDirectory,
    [Parameter(ParameterSetName='Capture')][ValidateNotNullOrEmpty()][string]$ExpectedVersion='0.5.0',
    [Parameter(Mandatory,ParameterSetName='SelfTest')][switch]$SelfTest
)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)

function Get-GalleryDefinitions {
    return @(
        @{key='main-waiting';name='主窗口 · 等待连接'},
        @{key='settings-menu';name='设置菜单'},
        @{key='details';name='连接详情'},
        @{key='main-connected';name='主窗口 · 已连接演示'},
        @{key='permissions';name='控制权限'},
        @{key='grant-confirmation';name='完全控制二次确认'},
        @{key='exit-confirmation';name='退出确认'},
        @{key='setup';name='首次设置'},
        @{key='tls';name='TLS 证书确认'},
        @{key='logs';name='日志页'},
        @{key='observer';name='只读操作观察窗'},
        @{key='about';name='版本信息'}
    )
}
function Get-GalleryHtml {
    param([object[]]$Items)
    $cards=foreach($item in $Items){
        $title=[Net.WebUtility]::HtmlEncode([string]$item.name)
        $columns=foreach($theme in @('light','dark')){
            $label=if($theme -eq 'light'){'亮色'}else{'暗色'}
            $frame=$item[$theme]
            if($null -eq $frame){'<div class="theme"><h3>'+ $label +'</h3><p>本项未完成，详见 index.json。</p></div>';continue}
            $path=[Net.WebUtility]::HtmlEncode([string]$frame.path)
            $meta=[Net.WebUtility]::HtmlEncode(('DPI {0} · 客户区 {1} × {2}' -f $frame.dpi,$frame.client_bounds.width,$frame.client_bounds.height))
            '<div class="theme"><h3>'+ $label +'</h3><a href="'+$path+'"><img loading="lazy" src="'+$path+'" alt="'+$title+' · '+$label+'"></a><p>'+ $meta +'</p></div>'
        }
        '<section><h2>'+ $title +'</h2><div class="pair">'+($columns -join '')+'</div></section>'
    }
    return @"
<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>RemoteOps · 逐窗体亮暗对照</title><style>
*{box-sizing:border-box}body{margin:0;background:#edf0f4;color:#182434;font:15px/1.6 'Segoe UI','Microsoft YaHei UI',sans-serif}main{max-width:1500px;margin:32px auto;padding:0 24px}h1{font-size:27px;margin:0 0 8px}header p{max-width:980px;color:#526072;margin:4px 0}section{margin:24px 0;background:#fff;border:1px solid #d7dee7;border-radius:12px;padding:20px}h2{font-size:19px;margin:0 0 12px}h3{font-size:14px;margin:0 0 10px;color:#526072}.pair{display:grid;grid-template-columns:minmax(0,1fr) minmax(0,1fr);gap:24px}.theme{min-width:0}img{display:block;max-width:100%;height:auto;border:1px solid #d7dee7;background:#e7ebf0}.theme p{color:#627084;font-size:12px;margin:8px 0 0}a{color:#234f82}@media(max-width:850px){main{padding:0 12px}.pair{grid-template-columns:1fr}section{padding:14px}}
</style></head><body><main><header><h1>RemoteOps · 逐窗体亮暗对照</h1><p>每一张均为本脚本独立启动的 $([Net.WebUtility]::HtmlEncode($ExpectedVersion)) 纯样式窗口实拍，点击图片查看原图；演示数据均为合成内容，未连接现场业务。</p><p>原生设置菜单可能跟随 Windows 系统颜色。暗色列保留真实菜单截图，不对截图改色。完整尺寸、DPI、摘要和失败记录见 <a href="index.json">index.json</a>。</p></header>
$($cards -join "`n")
</main></body></html>
"@
}
if($SelfTest){
    $definitions=@(Get-GalleryDefinitions)
    if($definitions.Count -ne 12 -or @($definitions.key|Sort-Object -Unique).Count -ne 12){throw '窗体清单自检失败'}
    $fixture=@{key='fixture';name='中文 <窗体> &';light=@{path='light/a.png';dpi=96;client_bounds=@{width=500;height=375}};dark=@{path='dark/a.png';dpi=144;client_bounds=@{width=750;height=563}}}
    $html=Get-GalleryHtml @($fixture)
    if($html -notmatch '中文 &lt;窗体&gt; &amp;' -or ([regex]::Matches($html,'<img ')).Count -ne 2 -or $html -match '<script|https?://'){throw '静态相册转义或依赖自检失败'}
    [pscustomobject]@{passed=$true;definitions=$definitions.Count;expected_images=2*$definitions.Count;html_escaping=$true;external_dependencies=$false;gui_started=$false}|ConvertTo-Json
    exit 0
}
if(-not $IsWindows){throw '仅支持 Windows 样式预览截图。'}
$temporary=[IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp')).TrimEnd('\')
$output=[IO.Path]::GetFullPath($OutputDirectory)
if(-not $temporary.StartsWith('C:\',[StringComparison]::OrdinalIgnoreCase) -or -not $output.StartsWith($temporary+'\',[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $output)){throw '必须提供全新的 C 盘本地 Temp 子目录。'}
for($ancestor=Split-Path -Parent $output;$ancestor;$ancestor=Split-Path -Parent $ancestor){if((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)){throw '截图目录不能经过重解析点。'}}
$exe=(Resolve-Path -LiteralPath $Executable).Path
if((Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash -ine $ExpectedHash -or [Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion -cne $ExpectedVersion){throw "截图只接受精确 SHA256 对应的 $ExpectedVersion 样式预览。"}
$helper=Join-Path $PSScriptRoot 'Test-NativeStylePreview.ps1'
if(-not(Test-Path -LiteralPath $helper -PathType Leaf)){throw '缺少原生样式测试客户端。'}
New-Item -ItemType Directory -Path $output|Out-Null
$items=@(Get-GalleryDefinitions|ForEach-Object{@{key=$_.key;name=$_.name;light=$null;dark=$null}})
$checks=[Collections.Generic.List[object]]::new()
$app=$null;$identity=$null;$appError=$null;$theme=$null;$step='startup';$failure=$null
function Assert-Gallery {
    param([string]$Name,[bool]$Passed)
    $checks.Add([pscustomobject]@{theme=$script:theme;name=$Name;passed=$Passed})
    if(-not $Passed){throw ('截图状态校验失败：'+$Name)}
}
function Client {
    param([string]$Action='Snapshot',[int]$Id=0,[string]$CapturePath)
    $script:step=$Action+'_'+$Id
    if($null -eq $script:app -or $script:app.HasExited){throw '本次隔离预览已退出。'}
    $start=[Diagnostics.ProcessStartInfo]::new((Join-Path $PSHOME 'pwsh.exe'))
    $start.UseShellExecute=$false;$start.CreateNoWindow=$true;$start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
    $start.StandardOutputEncoding=[Text.UTF8Encoding]::new($false);$start.StandardErrorEncoding=[Text.UTF8Encoding]::new($false)
    foreach($argument in @('-NoProfile','-STA','-File',$helper,'-ExpectedVersion',([Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion),'-ExpectedHash',$ExpectedHash,'-Action',$Action,'-ProcessId',[string]$script:app.Id,'-ProcessStartUtc',$script:identity,'-ControlId',[string]$Id)){$start.ArgumentList.Add($argument)}
    if($CapturePath){$start.ArgumentList.Add('-OutputPath');$start.ArgumentList.Add($CapturePath)}
    $worker=[Diagnostics.Process]::Start($start)
    $stdout=$worker.StandardOutput.ReadToEndAsync();$stderr=$worker.StandardError.ReadToEndAsync()
    try{
        if(-not $worker.WaitForExit(12000)){$worker.Kill($true);$null=$worker.WaitForExit(2000);throw '样式截图 UIA 客户端超过 12 秒。'}
        if($worker.ExitCode -ne 0){[IO.File]::WriteAllText((Join-Path $output 'worker-last-error.txt'),$stderr.GetAwaiter().GetResult());throw '样式截图客户端失败，保留原始错误。'}
        return $stdout.GetAwaiter().GetResult()|ConvertFrom-Json
    }finally{$worker.Dispose()}
}
function Find-Window {
    param($Snapshot,[int]$Id)
    $found=@($Snapshot.windows|Where-Object{@($_.controls|Where-Object id -eq $Id).Count -eq 1})
    if($found.Count -ne 1){throw ('找不到唯一的本进程窗体控件：'+$Id)}
    return $found[0]
}
function Wait-State {
    param([int]$Id=0,[int]$MenuCount=-1,[Nullable[bool]]$Enabled=$null)
    $watch=[Diagnostics.Stopwatch]::StartNew()
    do{
        $snapshot=Client
        if($MenuCount -ge 0){if($snapshot.menu_count -eq $MenuCount){return $snapshot}}
        else{
            $matches=if($Id -eq 0){@($snapshot.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')}else{@($snapshot.windows|Where-Object{@($_.controls|Where-Object id -eq $Id).Count -eq 1})}
            if($matches.Count -eq 1 -and ($null -eq $Enabled -or $matches[0].enabled -eq $Enabled)){return $snapshot}
        }
        if($watch.ElapsedMilliseconds -ge 15000){[IO.File]::WriteAllText((Join-Path $output 'wait-last-state.json'),(@{expected_id=$Id;expected_menu_count=$MenuCount;snapshot=$snapshot}|ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false));throw '等待本次隔离预览窗体超时。'}
        Start-Sleep -Milliseconds 60
    }while($true)
}
function Save-Frame {
    param([string]$Key,[int]$Id=0)
    $entry=@($items|Where-Object key -eq $Key)
    if($entry.Count -ne 1){throw '截图索引名称无效。'}
    $relative=$script:theme+'/'+$Key+'.png';$path=Join-Path $output $relative
    $capture=Client 'Capture' $Id -CapturePath $path
    foreach($field in @('handle','class','dpi','client_width','client_height','window_bounds')){if(-not $capture.PSObject.Properties[$field]){throw ('截图客户端未报告真实目标元数据：'+$field)}}
    Assert-Gallery ('capture_'+$Key) ($capture.captured -eq $true -and $capture.dpi -ge 96 -and $capture.client_width -gt 0 -and $capture.client_height -gt 0)
    $entry[0][$script:theme]=@{path=$relative;sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant();handle=$capture.handle;class=$capture.class;dpi=$capture.dpi;client_bounds=@{x=0;y=0;width=$capture.client_width;height=$capture.client_height};window_bounds=$capture.window_bounds;requested_theme=$script:theme;native_menu_system_colors=($Key -eq 'settings-menu')}
}
function Assert-MainEnabled {
    param([bool]$Enabled,[string]$Name)
    $snapshot=Wait-State -Id 303 -Enabled $Enabled
    Assert-Gallery $Name ((Find-Window $snapshot 303).enabled -eq $Enabled)
}
function Stop-OwnedPreview {
    if($null -eq $script:app){return}
    try{
        if(-not $script:app.HasExited){
            try{
                for($attempt=0;$attempt -lt 7;$attempt++){
                    $snapshot=Client
                    $visibleIds=@($snapshot.windows.controls.id)
                    $closeId=@(442,462,505,521,541,425,409,205|Where-Object{$_ -in $visibleIds}|Select-Object -First 1)
                    if($closeId.Count -eq 0){break}
                    $null=Client 'Invoke' $closeId[0]
                }
                $null=Client 'Close';$null=Wait-State -Id 463;$null=Client 'Invoke' 463
                $null=$script:app.WaitForExit(5000)
            }catch{}
            if(-not $script:app.HasExited){$script:app.Kill($true);$null=$script:app.WaitForExit(2000);$script:failure??=@{theme=$script:theme;step='cleanup';category='owned_preview_forced_exit'}}
        }
    }finally{$script:app.Dispose();$script:app=$null}
}
try{
    foreach($theme in @('light','dark')){
        $script:theme=$theme
        $directory=Join-Path $output $theme;New-Item -ItemType Directory -Path $directory|Out-Null
        $start=[Diagnostics.ProcessStartInfo]::new($exe);$start.UseShellExecute=$false;$start.WorkingDirectory=$directory;$start.RedirectStandardError=$true
        $start.ArgumentList.Add('--style-demo')
        foreach($name in @('APPDATA','LOCALAPPDATA','TEMP','TMP')){$isolated=Join-Path $directory ('.isolated/'+$name.ToLowerInvariant());New-Item -ItemType Directory -Path $isolated -Force|Out-Null;$start.Environment[$name]=$isolated}
        foreach($name in @($start.Environment.Keys)){if($name -match '^(REMOTEOPS|JLAI)'){$null=$start.Environment.Remove($name)}}
        $script:app=[Diagnostics.Process]::Start($start);$appError=$script:app.StandardError.ReadToEndAsync();$script:identity=$script:app.StartTime.ToUniversalTime().ToString('o')
        $null=Wait-State -Id 303
        $null=Client 'Command' 5001;$null=Client 'Command' $(if($theme -eq 'light'){5011}else{5012});$null=Client 'Command' 5030
        $null=Wait-State -Id 303;Save-Frame 'main-waiting' 303
        $null=Client 'OpenMenu';$null=Wait-State -MenuCount 1;Save-Frame 'settings-menu' -1;$null=Client 'SelectMenu';$null=Client 'DismissMenu';$null=Wait-State -MenuCount 0
        $null=Client 'Invoke' 307;$snapshot=Wait-State -Id 409
        Assert-Gallery 'details_disables_main' (-not (Find-Window $snapshot 303).enabled);Save-Frame 'details' 409
        $null=Client 'Invoke' 409;Assert-MainEnabled $true 'details_close_restores_main'
        $null=Client 'Command' 5031;$null=Wait-State -Id 303;Save-Frame 'main-connected' 303
        $null=Client 'Command' 5020;$snapshot=Wait-State -Id 425;$permissions=Find-Window $snapshot 425
        Assert-Gallery 'permissions_disables_main' (-not (Find-Window $snapshot 303).enabled)
        $full=@($permissions.controls|Where-Object id -eq 423);Assert-Gallery 'connected_full_control_enabled' ($full.Count -eq 1 -and $full[0].enabled);Save-Frame 'permissions' 425
        $null=Client 'Invoke' 423;$snapshot=Wait-State -Id 442
        Assert-Gallery 'grant_disables_permissions_owner' (-not (Find-Window $snapshot 425).enabled -and (Find-Window $snapshot 442).enabled);Save-Frame 'grant-confirmation' 442
        $null=Client 'Invoke' 442;$snapshot=Wait-State -Id 425 -Enabled $true
        Assert-Gallery 'grant_cancel_restores_permissions' (-not (Find-Window $snapshot 303).enabled -and (Find-Window $snapshot 425).enabled)
        $null=Client 'Invoke' 425;Assert-MainEnabled $true 'permissions_close_restores_main'
        $null=Client 'Invoke' 309;$snapshot=Wait-State -Id 462
        Assert-Gallery 'exit_disables_main' (-not (Find-Window $snapshot 303).enabled);Save-Frame 'exit-confirmation' 462
        $null=Client 'Invoke' 462;Assert-MainEnabled $true 'exit_cancel_restores_main'
        $null=Client 'Command' 5033;$snapshot=Wait-State -Id 502
        Assert-Gallery 'setup_disables_main' (-not (Find-Window $snapshot 303).enabled);Save-Frame 'setup' 502
        $null=Client 'Invoke' 505;Assert-MainEnabled $true 'setup_cancel_restores_main'
        $null=Client 'Command' 5034;$snapshot=Wait-State -Id 524
        Assert-Gallery 'tls_disables_main' (-not (Find-Window $snapshot 303).enabled)
        Assert-Gallery 'tls_text_readonly' (@((Find-Window $snapshot 524).controls|Where-Object id -eq 524)[0].read_only -eq $true);Save-Frame 'tls' 524
        $null=Client 'Invoke' 521;Assert-MainEnabled $true 'tls_cancel_restores_main'
        $null=Client 'Invoke' 308;$snapshot=Wait-State -Id 311
        Assert-Gallery 'logs_text_readonly' (@((Find-Window $snapshot 311).controls|Where-Object id -eq 311)[0].read_only -eq $true);Save-Frame 'logs' 311
        $null=Client 'Invoke' 310;$null=Wait-State -Id 303
        $null=Client 'Invoke' 338;$snapshot=Wait-State -Id 202
        Assert-Gallery 'observer_text_readonly' (@((Find-Window $snapshot 202).controls|Where-Object id -eq 202)[0].read_only -eq $true);Save-Frame 'observer' 202
        $null=Client 'Invoke' 205;Assert-MainEnabled $true 'observer_hide_preserves_main'
        $null=Client 'Command' 5021;$snapshot=Wait-State -Id 541
        Assert-Gallery 'about_disables_main' (-not (Find-Window $snapshot 303).enabled);Save-Frame 'about' 541
        $null=Client 'Invoke' 541;Assert-MainEnabled $true 'about_close_restores_main'
        $null=Client 'Invoke' 309;$null=Wait-State -Id 463;$null=Client 'Invoke' 463
        Assert-Gallery 'normal_preview_exit' ($script:app.WaitForExit(8000) -and $script:app.ExitCode -eq 0)
        $script:app.Dispose();$script:app=$null
    }
}catch{$failure=@{theme=$script:theme;step=$script:step;category='style_gallery_capture_failed';detail=$_.Exception.Message}}
finally{
    Stop-OwnedPreview
    $count=@($items|ForEach-Object{if($null -ne $_.light){$_.light};if($null -ne $_.dark){$_.dark}}).Count
    if($null -eq $failure -and $count -ne 24){$failure=@{category='incomplete_gallery';captured=$count;expected=24}}
    $record=[ordered]@{schema_version=1;version=$ExpectedVersion;executable_sha256=$ExpectedHash.ToLowerInvariant();passed=$null -eq $failure;captured_images=$count;expected_images=24;synthetic_data_only=$true;real_business_verified=$false;menu_note='原生菜单颜色由 Windows 决定，未对截图改色。';items=$items;checks=@($checks);failure=$failure}
    [IO.File]::WriteAllText((Join-Path $output 'index.json'),($record|ConvertTo-Json -Depth 10),[Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText((Join-Path $output 'index.html'),(Get-GalleryHtml $items),[Text.UTF8Encoding]::new($false))
}
[pscustomobject]@{passed=$record.passed;captured_images=$record.captured_images;index_html=(Join-Path $output 'index.html');index_json=(Join-Path $output 'index.json');failure=$failure}|ConvertTo-Json -Depth 4
if(-not $record.passed){exit 1}
