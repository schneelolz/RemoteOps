#requires -Version 7.2
<# .SYNOPSIS 用实际原生 PNG 像素验证退出提示正文与卡片底色一致，支持旧版负对照。 #>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Executable,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{64}$')][string]$ExpectedHash,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [switch]$ExpectMismatch,
    [ValidateSet('Neutral','Tinted')][string]$Surface='Neutral'
)
Set-StrictMode -Version Latest
$ErrorActionPreference='Stop'
$exe=(Resolve-Path -LiteralPath $Executable).Path
$output=[IO.Path]::GetFullPath($OutputDirectory)
$tempRoot=[IO.Path]::GetFullPath((Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Temp')).TrimEnd('\')
if(-not $IsWindows -or -not $tempRoot.StartsWith('C:\',[StringComparison]::OrdinalIgnoreCase) -or -not $output.StartsWith($tempRoot+'\',[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $output)){throw '必须使用全新的 C 盘本地 Temp 子目录。'}
for($ancestor=Split-Path -Parent $output;$ancestor;$ancestor=Split-Path -Parent $ancestor){if((Test-Path -LiteralPath $ancestor) -and ((Get-Item -LiteralPath $ancestor).Attributes -band [IO.FileAttributes]::ReparsePoint)){throw '测试目录不能经过重解析点。'}}
if((Get-FileHash -LiteralPath $exe).Hash -ine $ExpectedHash){throw 'EXE 摘要不符。'}
$helper=Join-Path $PSScriptRoot 'Test-NativeStylePreview.ps1'
New-Item -ItemType Directory -Path $output|Out-Null
$checks=[Collections.Generic.List[object]]::new();$frames=[Collections.Generic.List[object]]::new();$failure=$null
$start=[Diagnostics.ProcessStartInfo]::new($exe);$start.UseShellExecute=$false;$start.WorkingDirectory=$output
foreach($name in @('APPDATA','LOCALAPPDATA','TEMP','TMP')){$directory=Join-Path $output $name.ToLowerInvariant();New-Item -ItemType Directory -Path $directory|Out-Null;$start.Environment[$name]=$directory}
foreach($key in @($start.Environment.Keys)){if($key -match '^(REMOTEOPS_|JLAI_)'){$null=$start.Environment.Remove($key)}}
$app=[Diagnostics.Process]::Start($start);$identity=$app.StartTime.ToUniversalTime().ToString('o')
function Client([string]$Action='Snapshot',[int]$Id=0,[string]$Path){
    $parameters=@{Action=$Action;ProcessId=$app.Id;ProcessStartUtc=$identity;ExpectedHash=$ExpectedHash;ExpectedVersion=[Diagnostics.FileVersionInfo]::GetVersionInfo($exe).ProductVersion;ControlId=$Id}
    if($Path){$parameters.OutputPath=$Path}
    $json=& $helper @parameters
    if(-not $? -or $LASTEXITCODE){throw '退出背景测试客户端失败。'}
    return $json|ConvertFrom-Json
}
function Check([string]$Name,[bool]$Passed){$checks.Add([pscustomobject]@{name=$Name;passed=$Passed});if(-not $Passed){throw ('退出背景检查失败：'+$Name)}}
function Wait-Control([int]$Id){$watch=[Diagnostics.Stopwatch]::StartNew();do{$state=Client;if(@($state.windows.controls|Where-Object id -eq $Id).Count -eq 1){return $state};if($watch.ElapsedMilliseconds -ge 5000){throw ('退出窗控件等待超时：'+$Id)}}while($true)}
function Capture-Exit([string]$Name,[bool]$Dark){
    $state=Wait-Control 461
    $window=@($state.windows|Where-Object{$_.controls.id -contains 461})[0]
    Check ($Name+'_owner_disabled') (-not @($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0].enabled)
    $path=Join-Path $output ($Name+'.png');$capture=Client 'Capture' 463 $path
    Add-Type -AssemblyName System.Drawing
    $bitmap=[Drawing.Bitmap]::new($path)
    try{
        $offsetX=$window.client_origin[0]-$capture.window_bounds[0];$offsetY=$window.client_origin[1]-$capture.window_bounds[1]
        function Pixel([int]$X,[int]$Y){$pixel=$bitmap.GetPixel([int]($offsetX+[math]::Round($X*$window.dpi/96)),[int]($offsetY+[math]::Round($Y*$window.dpi/96)));return $pixel.ToArgb()}
        $card=if($Dark){[Drawing.Color]::FromArgb(255,46,30,37).ToArgb()}else{[Drawing.Color]::FromArgb(255,255,237,241).ToArgb()}
        $outside=if($Dark){[Drawing.Color]::FromArgb(255,24,28,35).ToArgb()}else{[Drawing.Color]::FromArgb(255,247,248,250).ToArgb()}
        if($Surface -eq 'Neutral'){$card=$outside}
        $samples=@((Pixel 32 198),(Pixel 240 192),(Pixel 400 198))
        Check ($Name+'_card_palette') ((Pixel 20 180) -eq $card -and (Pixel 420 180) -eq $card)
        Check ($Name+'_outside_unchanged') ((Pixel 8 180) -eq $outside)
        $matches=@($samples|Where-Object{$_ -ne $card}).Count -eq 0
        Check ($Name+'_text_area_matches_expectation') ($matches -eq (-not $ExpectMismatch))
        $frames.Add([pscustomobject]@{file=[IO.Path]::GetFileName($path);dpi=$window.dpi;card_argb=$card;text_area_argb=$samples;matches_card=$matches;sha256=(Get-FileHash -LiteralPath $path).Hash.ToLowerInvariant()})
    }finally{$bitmap.Dispose()}
}
try{
    $null=$app.WaitForInputIdle(5000);$null=Wait-Control 303
    $null=Client 'Command' 5011;$null=Client 'Invoke' 309;Capture-Exit 'exit-light' $false
    $null=Client 'Command' 5012;Capture-Exit 'exit-open-switch-dark' $true
    $null=Client 'Command' 5011;Capture-Exit 'exit-open-switch-light' $false
    $null=Client 'Invoke' 462;$state=Wait-Control 303
    Check 'cancel_restores_main' (@($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0].enabled)
    $null=Client 'Command' 5012;$null=Client 'Invoke' 309;Capture-Exit 'exit-dark-reopened' $true
    $null=Client 'Invoke' 464;$state=Wait-Control 303
    Check 'close_button_restores_main' (@($state.windows|Where-Object class -eq 'RemoteOpsStylePreviewMain')[0].enabled)
    $null=Client 'Close';$null=Wait-Control 463;$null=Client 'Invoke' 463
    Check 'normal_exit' ($app.WaitForExit(5000) -and $app.ExitCode -eq 0)
}catch{$failure=@{category='exit_background_or_fixture_failure';detail=$_.Exception.Message}}
finally{
    if(-not $app.HasExited){try{$state=Client;if($state.windows.controls.id -notcontains 463){$null=Client 'Close';$null=Wait-Control 463};$null=Client 'Invoke' 463;$null=$app.WaitForExit(5000)}catch{};if(-not $app.HasExited){$app.Kill();$app.WaitForExit();$failure??=@{category='owned_preview_forced_exit'}}}
    $record=[ordered]@{passed=$null -eq $failure;executable_sha256=$ExpectedHash.ToLowerInvariant();expected_match=-not $ExpectMismatch;surface=$Surface;checks=@($checks);frames=@($frames);failure=$failure;application_exit_code=$app.ExitCode;real_business_verified=$false;rdp_dpi_verified=$false}
    [IO.File]::WriteAllText((Join-Path $output 'report.json'),($record|ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false));$app.Dispose()
}
$record|ConvertTo-Json -Depth 5
if(-not $record.passed){exit 1}
