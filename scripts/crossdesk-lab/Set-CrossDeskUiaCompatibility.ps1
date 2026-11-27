[CmdletBinding()]
param([Parameter(Mandatory)][string]$Source)
$ErrorActionPreference = 'Stop'
# UIA 提供者可以成功返回空接口。无对应 Pattern 的控件仍可供观察。
$file = Join-Path $Source 'apps/desktop/src/platform/windows/desktop_automation.cpp'
$code = [IO.File]::ReadAllText($file)
foreach ($entry in @{ Invoke='invoke'; SelectionItem='select'; Toggle='toggle'; Value='pattern' }.GetEnumerator()) {
    $name = $entry.Value
    $before = "if (SUCCEEDED(element->GetCurrentPatternAs(UIA_$($entry.Key)PatternId, IID_PPV_ARGS(&$name))))"
    $after = "if (SUCCEEDED(element->GetCurrentPatternAs(UIA_$($entry.Key)PatternId, IID_PPV_ARGS(&$name))) && $name)"
    if ($code.Contains($before)) { $code = $code.Replace($before, $after) }
    elseif (!$code.Contains($after)) { throw '上游 UIA 接口变化，需要重新核对兼容处理' }
}
[IO.File]::WriteAllText($file, $code)
