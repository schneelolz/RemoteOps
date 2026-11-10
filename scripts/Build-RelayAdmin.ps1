[CmdletBinding()]
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$repository = Split-Path -Parent $PSScriptRoot
$frontend = Join-Path $repository 'relay-admin'
Push-Location $frontend
try {
    & npm.cmd ci --ignore-scripts
    if ($LASTEXITCODE -ne 0) { throw '前端锁定依赖安装失败。' }
    & npm.cmd run build
    if ($LASTEXITCODE -ne 0) { throw '前端类型检查或构建失败。' }
    $version = (Get-Content -LiteralPath 'package.json' -Raw | ConvertFrom-Json).version
    if (-not $OutputDirectory) { $OutputDirectory = Join-Path $repository "artifacts\relay-admin\$version" }
    if (Test-Path -LiteralPath $OutputDirectory) { throw "输出目录已存在，禁止覆盖：$OutputDirectory" }
    New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
    Copy-Item -LiteralPath 'dist' -Destination (Join-Path $OutputDirectory 'dist') -Recurse
    Copy-Item -LiteralPath 'package.json','package-lock.json' -Destination $OutputDirectory
    Copy-Item -LiteralPath (Join-Path $repository 'deploy\relay-admin') -Destination (Join-Path $OutputDirectory 'deploy') -Recurse
    & node (Join-Path $PSScriptRoot 'collect-relay-admin-licenses.mjs') $OutputDirectory
    if ($LASTEXITCODE -ne 0) { throw '前端许可证归档失败。' }
    $utf8 = [Text.UTF8Encoding]::new($false)
    $release = @{component='remoteops-relay-admin';version=$version;backend_minimum='0.2.0-preview.19';built_at=[DateTime]::UtcNow.ToString('o');lock_sha256=(Get-FileHash 'package-lock.json').Hash.ToLowerInvariant()}
    [IO.File]::WriteAllText((Join-Path $OutputDirectory 'dist\release.json'), ($release | ConvertTo-Json), $utf8)
    $resolved = (Resolve-Path -LiteralPath $OutputDirectory).Path
    $entries = @(Get-ChildItem -LiteralPath $resolved -File -Recurse | Sort-Object FullName | ForEach-Object {
        [ordered]@{path=[IO.Path]::GetRelativePath($resolved,$_.FullName).Replace('\','/');sha256=(Get-FileHash -LiteralPath $_.FullName).Hash.ToLowerInvariant();bytes=$_.Length}
    })
    [IO.File]::WriteAllText((Join-Path $resolved 'manifest.json'), (@{component='remoteops-relay-admin';version=$version;files=$entries} | ConvertTo-Json -Depth 6), $utf8)
    Write-Output "前端独立交付已生成：$resolved"
}
finally { Pop-Location }
