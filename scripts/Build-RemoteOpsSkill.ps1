[CmdletBinding()]
param([string]$OutputRoot)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$workspaceRoot = Split-Path -Parent $PSScriptRoot
$source = Join-Path $workspaceRoot 'deploy\skills\remoteops'
$metadata = [IO.File]::ReadAllText((Join-Path $source 'skill.json')) | ConvertFrom-Json
if ($metadata.name -ne 'remoteops' -or $metadata.schema_version -ne 1 -or $metadata.version -notmatch '^\d+\.\d+\.\d+$') {
    throw '公共 Skill 元数据无效。'
}
$text = [IO.File]::ReadAllText((Join-Path $source 'SKILL.md'))
if ($text -notmatch ('(?m)^  version: "' + [regex]::Escape($metadata.version) + '"\s*$')) { throw 'SKILL.md 与 skill.json 版本不一致。' }
if ([string]::IsNullOrWhiteSpace($OutputRoot)) { $OutputRoot = Join-Path $workspaceRoot ('artifacts\skills\' + $metadata.version) }
$resolvedOutputRoot = [IO.Path]::GetFullPath($OutputRoot)
$packageName = 'RemoteOps-Skill-' + $metadata.version
$packageDirectory = Join-Path $resolvedOutputRoot $packageName
$archivePath = Join-Path $resolvedOutputRoot ($packageName + '.zip')
if ((Test-Path -LiteralPath $packageDirectory) -or (Test-Path -LiteralPath $archivePath)) {
    throw 'Skill 输出已存在；请选择新的输出目录，保留旧版本产物。'
}
for ($current=$resolvedOutputRoot; -not [string]::IsNullOrEmpty($current); $current=Split-Path -Parent $current) {
    if ((Test-Path -LiteralPath $current) -and ((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Skill 输出路径不允许链接。' }
}
New-Item -ItemType Directory -Path (Join-Path $packageDirectory 'skills\remoteops\agents') -Force | Out-Null
foreach ($name in @('SKILL.md', 'skill.json', 'agents/openai.yaml')) {
    Copy-Item -LiteralPath (Join-Path $source $name) -Destination (Join-Path (Join-Path $packageDirectory 'skills\remoteops') $name)
}
Copy-Item -LiteralPath (Join-Path $source 'references') -Destination (Join-Path $packageDirectory 'skills\remoteops\references') -Recurse
$windowsTemplate = Join-Path $workspaceRoot 'deploy\controller-skill\windows'
$macosTemplate = Join-Path $workspaceRoot 'deploy\controller-skill\macos'
foreach ($name in @('Install-RemoteOpsSkill.ps1', 'Uninstall-RemoteOpsSkill.ps1', 'README.md')) {
    Copy-Item -LiteralPath (Join-Path $windowsTemplate $name) -Destination $packageDirectory
}
foreach ($name in @('install-remoteops-skill.sh', 'uninstall-remoteops-skill.sh', 'manage-remoteops-skill.py')) {
    Copy-Item -LiteralPath (Join-Path $macosTemplate $name) -Destination $packageDirectory
}
Copy-Item -LiteralPath (Join-Path $workspaceRoot 'LICENSE') -Destination $packageDirectory
$files = @(Get-ChildItem -LiteralPath $packageDirectory -File -Recurse | Sort-Object FullName | ForEach-Object {
    [ordered]@{ file=$_.FullName.Substring($packageDirectory.Length+1).Replace('\','/'); bytes=$_.Length; sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
})
$manifest = [ordered]@{ schema_version=1; component='remoteops-skill'; version=$metadata.version; minimum_mcp_version=$metadata.minimum_mcp_version; protocol=$metadata.protocol; contains_credentials=$false; files=$files }
[IO.File]::WriteAllText((Join-Path $packageDirectory 'manifest.json'), ($manifest | ConvertTo-Json -Depth 8)+"`r`n", [Text.UTF8Encoding]::new($false))
$sums = @(Get-ChildItem -LiteralPath $packageDirectory -File -Recurse | Sort-Object FullName | ForEach-Object {
    (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() + '  ' + $_.FullName.Substring($packageDirectory.Length+1).Replace('\','/')
})
[IO.File]::WriteAllText((Join-Path $packageDirectory 'SHA256SUMS.txt'), ($sums -join "`r`n")+"`r`n", [Text.UTF8Encoding]::new($false))
Compress-Archive -Path (Join-Path $packageDirectory '*') -DestinationPath $archivePath -CompressionLevel Optimal
$archiveHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText(($archivePath + '.sha256'), $archiveHash+'  '+[IO.Path]::GetFileName($archivePath)+"`r`n", [Text.UTF8Encoding]::new($false))
Write-Host "Skill 包：$archivePath"
Write-Host "SHA256：$archiveHash"
[pscustomobject]@{ Version=$metadata.version; PackageDirectory=$packageDirectory; ArchivePath=$archivePath; Sha256=$archiveHash }
