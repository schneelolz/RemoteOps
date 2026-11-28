[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$OutputDirectory,
    [string]$Xmake = "xmake",
    [string]$BuildRoot = (Join-Path ([IO.Path]::GetTempPath()) "remoteops-minirtc-build")
)
$ErrorActionPreference = "Stop"
$commit = "c3b26b8f0f03fc43666e985052635c2573ded8f8"
$digest = "73f4cd90afe577aebce60df401f4e9c6ad898392719601768516b858a2b4de52"
$root = New-Item -Type Directory -Force -Path $BuildRoot
$archive = Join-Path $root.FullName "minirtc-$commit.tar.gz"
if (!(Test-Path -LiteralPath $archive)) {
    Invoke-WebRequest "https://codeload.github.com/kunkundi/minirtc/tar.gz/$commit" -OutFile $archive
}
if ((Get-FileHash $archive).Hash -ne $digest) { throw "MiniRTC 源码摘要不匹配" }
$build = Join-Path $root.FullName ([guid]::NewGuid().ToString("N"))
$null = New-Item -Type Directory $build
tar -xf $archive -C $build
if ($LASTEXITCODE) { throw "MiniRTC 源码解包失败" }
$src = Join-Path $build "minirtc-$commit"
$native = Join-Path $PSScriptRoot "../crates/remoteops-desktop-media/minirtc"
Copy-Item (Join-Path $native "worker.cpp") (Join-Path $src "remoteops_worker.cpp")
$recipe = Join-Path $src "thirdparty/dav1d/xmake.lua"
# dav1d 1.4.3 的 MSVC 兼容头引用了仅 Clang 提供的宏。
$needle = '        import("package.tools.meson")'
$fix = @'
        if package:is_plat("windows") then
            io.replace("include/compat/msvc/stdatomic.h", "#if defined(__clang__) && __has_include_next(<stdatomic.h>)",
                "#ifndef __has_include_next\n#define __has_include_next(x) 0\n#endif\n#if defined(__clang__) && __has_include_next(<stdatomic.h>)", {plain = true})
        end
'@
$recipeText = [IO.File]::ReadAllText($recipe)
if (!$recipeText.Contains($needle)) { throw "dav1d 构建入口不匹配" }
[IO.File]::WriteAllText($recipe,$recipeText.Replace($needle,$fix + [Environment]::NewLine + $needle))
[IO.File]::AppendAllText((Join-Path $src "xmake.lua"),[Environment]::NewLine + [IO.File]::ReadAllText((Join-Path $native "worker.lua")))
$out = (New-Item -Type Directory -Force -Path $OutputDirectory).FullName
Push-Location $src
try {
    & $Xmake f -p windows -a x64 -m release --USE_CUDA=false -y
    if ($LASTEXITCODE) { throw "MiniRTC 依赖配置失败" }
    & $Xmake b remoteops-minirtc
    if ($LASTEXITCODE) { throw "MiniRTC 媒体组件构建失败" }
    Copy-Item -LiteralPath "build/windows/x64/release/remoteops-minirtc.exe" -Destination $out
    $sources = (New-Item -Type Directory -Force -Path (Join-Path $out 'MiniRTC-sources')).FullName
    Copy-Item -LiteralPath $archive -Destination $sources
    Copy-Item -LiteralPath (Join-Path $src 'LICENSE') -Destination (Join-Path $sources 'MiniRTC-LICENSE.txt')
    Copy-Item -LiteralPath (Join-Path $native 'worker.cpp'),(Join-Path $native 'worker.lua'),$PSCommandPath -Destination $sources
    Get-FileHash (Join-Path $out "remoteops-minirtc.exe")
} finally { Pop-Location }
