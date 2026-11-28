[CmdletBinding()]
param(
    [string]$OutputRoot,
    [string]$McpPackageDirectory,
    [string]$SkillPackageDirectory,
    [switch]$SkillOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'RemoteOps-Package.ps1')
$workspaceRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
if (-not $OutputRoot) { $OutputRoot = Join-Path $workspaceRoot 'artifacts\delivery\RemoteOps' }
$OutputRoot = [IO.Path]::GetFullPath($OutputRoot).TrimEnd('\', '/')
$parent = Split-Path -Parent $OutputRoot
if (-not $parent -or $OutputRoot -eq [IO.Path]::GetPathRoot($OutputRoot).TrimEnd('\', '/') -or
    $workspaceRoot.Equals($OutputRoot, [StringComparison]::OrdinalIgnoreCase) -or
    $workspaceRoot.StartsWith($OutputRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw '发布目标不能是磁盘根目录、工作区或工作区的父目录。'
}
for ($current=$OutputRoot; $current; $current=Split-Path -Parent $current) {
    if ((Test-Path -LiteralPath $current) -and ((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw '发布目标及父目录不允许链接或重解析点。'
    }
}
New-Item -ItemType Directory -Path $parent -Force | Out-Null
$lockPath = Join-Path $parent ('.' + (Split-Path -Leaf $OutputRoot) + '.publish.lock')
if ((Test-Path -LiteralPath $lockPath) -and ((Get-Item -LiteralPath $lockPath -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw '发布锁不允许链接。' }
$publishLock = [IO.File]::Open($lockPath, [IO.FileMode]::OpenOrCreate, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
try {
$batch = (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [guid]::NewGuid().ToString('N').Substring(0,8)
$workRoot = Join-Path ([IO.Path]::GetTempPath()) ('RemoteOps-delivery-' + $batch)
$stage = Join-Path $workRoot 'stage'
New-Item -ItemType Directory -Path $stage -Force | Out-Null
if ($SkillOnly) {
    if ($McpPackageDirectory) { throw 'SkillOnly 不能同时指定 MCP 包，防止无意切换 MCP。' }
    $old = Get-Content -LiteralPath (Join-Path $OutputRoot 'delivery-manifest.json') -Raw | ConvertFrom-Json
    $relative = [string]$old.components.mcp.package_directory
    $McpPackageDirectory = [IO.Path]::GetFullPath((Join-Path $OutputRoot $relative))
    if (-not $McpPackageDirectory.StartsWith($OutputRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw '已发布 MCP 路径越界。' }
    $manifestHash = (Get-FileHash -LiteralPath (Join-Path $McpPackageDirectory 'manifest.json') -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($manifestHash -cne $old.components.mcp.manifest_sha256) { throw '已发布 MCP 清单发生变化，停止复用。' }
}
if (-not $McpPackageDirectory) {
    $buildRoot = Join-Path $workRoot 'mcp-build'
    & (Join-Path $PSScriptRoot 'Build-WindowsMcpPackage.ps1') -OutputRoot $buildRoot -ArtifactDirectory (Join-Path $workRoot 'artifacts')
    $packages = @(Get-ChildItem -LiteralPath $buildRoot -Directory)
    if ($packages.Count -ne 1) { throw 'MCP 构建未产生唯一包目录。' }
    $McpPackageDirectory = $packages[0].FullName
}
if (-not $SkillPackageDirectory) {
    $skillResult = & (Join-Path $PSScriptRoot 'Build-RemoteOpsSkill.ps1') -OutputRoot (Join-Path $workRoot 'skill-build')
    $SkillPackageDirectory = $skillResult.PackageDirectory
}
$mcp = Test-RemoteOpsComponentPackage $McpPackageDirectory 'remoteops-mcp'
$skill = Test-RemoteOpsComponentPackage $SkillPackageDirectory 'remoteops-skill'
$previousDeliveryPath = Join-Path $OutputRoot 'delivery-manifest.json'
if (Test-Path -LiteralPath $previousDeliveryPath -PathType Leaf) {
    $previousDelivery = Get-Content -LiteralPath $previousDeliveryPath -Raw | ConvertFrom-Json
    foreach ($entry in @(@('mcp',$mcp,$McpPackageDirectory), @('skill',$skill,$SkillPackageDirectory))) {
        $oldComponent = $previousDelivery.components.($entry[0])
        $newManifestHash = (Get-FileHash -LiteralPath (Join-Path $entry[2] 'manifest.json') -Algorithm SHA256).Hash.ToLowerInvariant()
        if (-not (Test-RemoteOpsMinimumVersion $entry[1].version $oldComponent.version)) { throw '默认发布拒绝组件降级；原发布保留，回退需单独评估。' }
        if ($oldComponent.version -eq $entry[1].version -and $oldComponent.manifest_sha256 -cne $newManifestHash) {
            throw '同版本组件内容发生变化；请提升对应组件版本，保留已有发布。'
        }
    }
}
foreach ($version in @($mcp.version, $skill.version)) {
    if ($version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$') { throw '组件版本不能用于发布目录。' }
}
if ($mcp.contains_credentials -ne $false -or $skill.contains_credentials -ne $false) { throw '组件未声明无凭据。' }
if (-not (Test-RemoteOpsMinimumVersion $mcp.version $skill.minimum_mcp_version) -or $mcp.protocol -ne $skill.protocol) {
    throw 'Skill 与 MCP 的最低版本或协议不兼容。'
}
$skillMetadata = Get-Content -LiteralPath (Join-Path $SkillPackageDirectory 'skills\remoteops\skill.json') -Raw | ConvertFrom-Json
if ($skillMetadata.version -ne $skill.version -or $skillMetadata.minimum_mcp_version -ne $skill.minimum_mcp_version -or $skillMetadata.protocol -ne $skill.protocol) {
    throw 'Skill 元数据与发布清单不一致。'
}
if ((Test-Path -LiteralPath (Join-Path $McpPackageDirectory 'skills')) -or
    @(Get-ChildItem -LiteralPath $SkillPackageDirectory -Filter '*.exe' -Recurse -File).Count) {
    throw 'MCP 与 Skill 包尚未独立。'
}
foreach ($name in @('remoteops-controller-mcp.exe','remoteops-credential-prompt.exe','Install-RemoteOpsMcp.ps1','Test-RemoteOpsMcp.ps1','README.md')) {
    if (-not (Test-Path -LiteralPath (Join-Path $McpPackageDirectory $name) -PathType Leaf)) { throw "MCP 包缺少 $name" }
}
$mcpName = 'RemoteOps-MCP-Windows-x64-' + $mcp.version
$skillName = 'RemoteOps-Skill-' + $skill.version
$bundleName = 'RemoteOps-Setup-Windows-x64-mcp-' + $mcp.version + '_skill-' + $skill.version
$mcpRelative = 'MCP/' + $mcp.version + '/' + $mcpName
$skillRelative = 'Skill/' + $skill.version + '/' + $skillName
$bundleRelative = 'Bundle/mcp-' + $mcp.version + '_skill-' + $skill.version + '/' + $bundleName
foreach ($entry in @(@($McpPackageDirectory,$mcpRelative), @($SkillPackageDirectory,$skillRelative))) {
    $target = Join-Path $stage $entry[1]
    New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
    Copy-Item -LiteralPath $entry[0] -Destination $target -Recurse
}
$mcpTarget = Join-Path $stage $mcpRelative
$skillTarget = Join-Path $stage $skillRelative
$null = Test-RemoteOpsComponentPackage $mcpTarget 'remoteops-mcp'
$null = Test-RemoteOpsComponentPackage $skillTarget 'remoteops-skill'
$bundleTarget = Join-Path $stage $bundleRelative
New-Item -ItemType Directory -Path $bundleTarget -Force | Out-Null
Copy-Item -LiteralPath $mcpTarget -Destination (Join-Path $bundleTarget 'MCP') -Recurse
Copy-Item -LiteralPath $skillTarget -Destination (Join-Path $bundleTarget 'Skill') -Recurse
$bundleReadme = @"
# RemoteOps 全套安装包

MCP $($mcp.version)，密码辅助程序 $($mcp.credential_prompt_version)，协议 $($mcp.protocol)；Skill $($skill.version)，最低 MCP $($skill.minimum_mcp_version)。两个组件独立版本、独立安装。

首次安装：先在 MCP 目录按 README.md 完成一次性登记，再在 Skill 目录运行 Install-RemoteOpsSkill.ps1。旧 Skill 无版本时，核对备份说明后显式添加 -ReplaceUnversioned；较新版本不会被覆盖。

已有 MCP 升级：运行 MCP\Install-RemoteOpsMcp.ps1 -UpgradeExisting。保留既有身份和凭据，不重新登记。只更新 Skill 时，仅运行 Skill 安装器，不执行 MCP 安装器。自定义 CODEX_HOME 用 -CodexHome；pi 独立技能目录用 -SkillDirectory，具体参数见两个组件各自 README。

MCP 路径变化后须完全退出并重开 Codex CLI；pi 重开或 /reload。Skill 更新后需让宿主重新加载技能或开启新会话，不能将文件复制成功记为当前会话已加载。

Agent 和 Relay 不因本次组件拆分而要求更新。包内不含本机连接配置或凭据。请先核对各目录 manifest.json、SHA256SUMS.txt 以及可信发布渠道给出的 ZIP 摘要。
"@
[IO.File]::WriteAllText((Join-Path $bundleTarget 'README.md'), $bundleReadme + "`r`n", [Text.UTF8Encoding]::new($false))
Write-RemoteOpsChecksums $bundleTarget
$componentRecords = [ordered]@{}
foreach ($entry in @(@('mcp',$mcp,$mcpRelative,$mcpTarget), @('skill',$skill,$skillRelative,$skillTarget))) {
    $zipRelative = $entry[2] + '.zip'
    $zip = Join-Path $stage $zipRelative
    Compress-Archive -Path (Join-Path $entry[3] '*') -DestinationPath $zip -CompressionLevel Optimal
    Test-RemoteOpsArchive $entry[3] $zip
    $componentRecords[$entry[0]] = [ordered]@{
        version=$entry[1].version; package_directory=$entry[2]; archive=$zipRelative
        manifest_sha256=(Get-FileHash -LiteralPath (Join-Path $entry[3] 'manifest.json') -Algorithm SHA256).Hash.ToLowerInvariant()
        archive_sha256=(Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}
$componentRecords.mcp['credential_prompt_version'] = $mcp.credential_prompt_version
$componentRecords.mcp['protocol'] = $mcp.protocol
$componentRecords.skill['minimum_mcp_version'] = $skill.minimum_mcp_version
$componentRecords.skill['protocol'] = $skill.protocol
$bundleZip = Join-Path $stage ($bundleRelative + '.zip')
Compress-Archive -Path (Join-Path $bundleTarget '*') -DestinationPath $bundleZip -CompressionLevel Optimal
Test-RemoteOpsArchive $bundleTarget $bundleZip
$delivery = [ordered]@{ schema_version=1; batch=$batch; published_at=[DateTime]::UtcNow.ToString('o'); skill_only=$SkillOnly.IsPresent; contains_credentials=$false; components=$componentRecords; bundle=[ordered]@{ directory=$bundleRelative; archive=$bundleRelative+'.zip'; archive_sha256=(Get-FileHash -LiteralPath $bundleZip -Algorithm SHA256).Hash.ToLowerInvariant() } }
Write-RemoteOpsJson (Join-Path $stage 'delivery-manifest.json') $delivery
$readme = @"
# RemoteOps MCP 与 Skill 安装

当前 MCP：$($mcp.version)；密码辅助程序：$($mcp.credential_prompt_version)；协议：$($mcp.protocol)。当前 Skill：$($skill.version)，最低 MCP：$($skill.minimum_mcp_version)。

- MCP 独立包：[$mcpName]($mcpRelative/README.md)，ZIP：[$mcpName.zip]($mcpRelative.zip)。负责连接、认证、工具执行及权限校验。
- Skill 独立包：[$skillName]($skillRelative/README.md)，ZIP：[$skillName.zip]($skillRelative.zip)。负责操作流程、交换机 SSH、主机指纹核对和排障经验。
- 首次安装可下载全套包：[$bundleName.zip]($bundleRelative.zip)，解压后分别按 MCP 与 Skill 目录 README 安装。

已有 MCP 只升级程序：在 MCP 包目录运行 .\Install-RemoteOpsMcp.ps1 -UpgradeExisting。该操作不登记新身份，不修改 Skill。只更新 Skill：在 Skill 包目录运行 .\Install-RemoteOpsSkill.ps1；未版本化旧 Skill 需要显式 -ReplaceUnversioned 并备份。Skill 可先安装，但这只完成文件安装，仍须确认宿主已加载且 MCP 满足兼容要求。

两者版本号不要求相同。MCP 更新不覆盖独立 Skill；Skill 更新不改 MCP 程序、连接配置、凭据或进程。Agent 和 Relay 不因拆分而更新。MCP 配置切换后重开 Codex CLI，pi 重开或 /reload；Skill 按宿主重新加载或新建会话。

检查 delivery-manifest.json 和 SHA256SUMS.txt，分别记录组件版本、兼容要求及 ZIP 摘要。历史发布完整备份在同级 $([IO.Path]::GetFileName($OutputRoot))-backups。本目录不含任何本机凭据。

维护者后续发布统一使用 scripts/Publish-WindowsMcpDelivery.ps1 -OutputRoot '实际发布目录'；只有 Skill 更新时加 -SkillOnly，复用校验通过的现有 MCP，不触发 MCP 编译。
"@
[IO.File]::WriteAllText((Join-Path $stage 'README.md'), $readme + "`r`n", [Text.UTF8Encoding]::new($false))
Write-RemoteOpsChecksums $stage
$expectedFiles = @(Get-RemoteOpsPackageFiles $stage)
$backupRoot = Join-Path $parent ((Split-Path -Leaf $OutputRoot) + '-backups')
$backup = Join-Path $backupRoot $batch
New-Item -ItemType Directory -Path $parent -Force | Out-Null
$ready = Join-Path $parent ((Split-Path -Leaf $OutputRoot) + '-ready-' + $batch)
# 先在目标磁盘完整复制与验证；复制失败时原发布完全不动。
New-Item -ItemType Directory -Path $ready | Out-Null
Get-ChildItem -LiteralPath $stage -Force | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $ready -Recurse }
$readyFiles = @(Get-RemoteOpsPackageFiles $ready)
if (($expectedFiles | ConvertTo-Json -Depth 4 -Compress) -cne ($readyFiles | ConvertTo-Json -Depth 4 -Compress)) { throw '发布准备副本的递归摘要不一致；原发布保留。' }
$hadPrevious = Test-Path -LiteralPath $OutputRoot
$movedPrevious = $false
$activated = $false
try {
    if ($hadPrevious) {
        # 移动前确认最终绝对路径仍是指定目标及同级备份，不移动磁盘根或工作区。
        if ([IO.Path]::GetFullPath($backup).StartsWith($OutputRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw '备份路径不得位于发布目录内。' }
        if ((Test-Path -LiteralPath $backupRoot) -and ((Get-Item -LiteralPath $backupRoot -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw '备份目录不能是链接。' }
        New-Item -ItemType Directory -Path $backupRoot -Force | Out-Null
        Move-Item -LiteralPath $OutputRoot -Destination $backup
        $movedPrevious = $true
    }
    Move-Item -LiteralPath $ready -Destination $OutputRoot
    $activated = $true
}
catch {
    $failure = $_
    if ($activated -and (Test-Path -LiteralPath $OutputRoot)) {
        $failed = Join-Path $parent ((Split-Path -Leaf $OutputRoot) + '-failed-' + $batch)
        try { Move-Item -LiteralPath $OutputRoot -Destination $failed }
        catch { throw "发布失败且无法移走故障目录；旧发布备份保留于 $backup，需人工恢复。" }
    }
    if ($movedPrevious) {
        if (Test-Path -LiteralPath $OutputRoot) { throw "发布失败且目标被并发占用；旧发布备份保留于 $backup，需人工恢复。" }
        try { Move-Item -LiteralPath $backup -Destination $OutputRoot }
        catch { throw "发布失败且旧目录恢复失败；完整备份保留于 $backup，需人工恢复。" }
    }
    throw $failure
}
Write-Host "已发布：$OutputRoot"
if ($hadPrevious) { Write-Host "完整备份：$backup" }
Write-Host "验证与准备目录：$workRoot"
[pscustomobject]@{ OutputRoot=$OutputRoot; Backup=$(if ($hadPrevious) { $backup } else { $null }); Batch=$batch; McpVersion=$mcp.version; SkillVersion=$skill.version; SkillOnly=$SkillOnly.IsPresent; FileCount=$expectedFiles.Count; WorkRoot=$workRoot }

}
finally { $publishLock.Dispose() }
