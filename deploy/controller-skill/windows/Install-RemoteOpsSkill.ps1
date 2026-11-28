[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$CodexHome = $(if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }),
    [string]$SkillDirectory,
    [switch]$ReplaceUnversioned
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
function Assert-NoLinkedAncestor([string]$Path) {
    for ($current = $Path; -not [string]::IsNullOrEmpty($current); $current = Split-Path -Parent $current) {
        if ((Test-Path -LiteralPath $current) -and
            ((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Skill 路径不允许链接或重解析点：$current"
        }
    }
}
$packageRoot = [IO.Path]::GetFullPath($PSScriptRoot)
$sourceDirectory = Join-Path $packageRoot 'skills\remoteops'
foreach ($path in @($packageRoot, (Join-Path $packageRoot 'manifest.json'), (Join-Path $sourceDirectory 'skill.json'))) { Assert-NoLinkedAncestor $path }
$manifest = [IO.File]::ReadAllText((Join-Path $packageRoot 'manifest.json')) | ConvertFrom-Json
$metadata = [IO.File]::ReadAllText((Join-Path $sourceDirectory 'skill.json')) | ConvertFrom-Json
if ($manifest.component -ne 'remoteops-skill' -or $manifest.version -ne $metadata.version -or
    $metadata.name -ne 'remoteops' -or $metadata.schema_version -ne 1 -or
    $metadata.version -notmatch '^\d+\.\d+\.\d+$' -or $manifest.schema_version -ne 1 -or
    $manifest.minimum_mcp_version -cne $metadata.minimum_mcp_version -or $manifest.protocol -ne $metadata.protocol -or
    $manifest.contains_credentials -ne $false -or $metadata.minimum_mcp_version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$' -or
    ($metadata.protocol -isnot [int] -and $metadata.protocol -isnot [long])) {
    throw 'Skill 包元数据无效。'
}
$version = [version]$metadata.version
$baseFiles = @('SKILL.md', 'skill.json', 'agents/openai.yaml')
$legacyFiles = @($baseFiles) + @('references/tool-center-contract.md', 'references/examples/put-tool-external.json', 'references/examples/put-tool-hosted.json', 'references/examples/record-test.json')
$managedFiles = @($legacyFiles) + 'references/browser-startup.md'
$listed = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
foreach ($entry in $manifest.files) {
    $relative = [string]$entry.file
    if ($relative -notmatch '^[A-Za-z0-9_./-]+$' -or $relative.Split('/') -contains '..') {
        throw 'Skill 清单包含无效路径。'
    }
    $path = [IO.Path]::GetFullPath((Join-Path $packageRoot $relative))
    Assert-NoLinkedAncestor $path
    if (-not $listed.Add($relative)) { throw 'Skill 清单包含重复文件。' }
    if (-not $path.StartsWith($packageRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or
        -not (Test-Path -LiteralPath $path -PathType Leaf) -or
        (Get-Item -LiteralPath $path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint -or
        (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $entry.sha256 -or
        (Get-Item -LiteralPath $path).Length -ne $entry.bytes) {
        throw "Skill 包文件校验失败：$relative"
    }
}
foreach ($item in Get-ChildItem -LiteralPath $packageRoot -Recurse -Force) {
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Skill 包包含链接。' }
    if (-not $item.PSIsContainer) {
        $relative = $item.FullName.Substring($packageRoot.Length+1).Replace('\', '/')
        if ($relative -cnotin @('manifest.json', 'SHA256SUMS.txt') -and -not $listed.Contains($relative)) { throw "Skill 包包含未列入清单的文件：$relative" }
    }
}
foreach ($name in $managedFiles) {
    if (@($manifest.files | Where-Object { $_.file -ceq "skills/remoteops/$name" }).Count -ne 1) {
        throw "Skill 清单缺少受管文件：$name"
    }
}
$skillText = [IO.File]::ReadAllText((Join-Path $sourceDirectory 'SKILL.md'))
if ($skillText -notmatch ('(?m)^  version: "' + [regex]::Escape($metadata.version) + '"\s*$')) {
    throw 'SKILL.md 与 skill.json 版本不一致。'
}

if (-not [string]::IsNullOrWhiteSpace($SkillDirectory)) {
    if ($PSBoundParameters.ContainsKey('CodexHome')) { throw 'CodexHome 与 SkillDirectory 只能选择一种。' }
    $destinations = @([IO.Path]::GetFullPath($SkillDirectory))
} else {
    $compatDirectory = Join-Path $CodexHome 'skills\remoteops'
    $defaultCodexHome = Join-Path $env:USERPROFILE '.codex'
    $standardDirectory = if ([IO.Path]::GetFullPath($CodexHome).TrimEnd('\') -ieq [IO.Path]::GetFullPath($defaultCodexHome).TrimEnd('\')) {
        Join-Path $env:USERPROFILE '.agents\skills\remoteops'
    } else { $compatDirectory }
    $destinations = @(@($standardDirectory, $compatDirectory) | ForEach-Object { [IO.Path]::GetFullPath($_) } | Select-Object -Unique)
}
$plans = @()
foreach ($destination in $destinations) {
    Assert-NoLinkedAncestor $destination
    if ([IO.Path]::GetFileName($destination.TrimEnd('\')) -ine 'remoteops') { throw 'Skill 目标目录必须以 remoteops 命名。' }
    $same = $false
    if (Test-Path -LiteralPath $destination) {
        if (-not (Test-Path -LiteralPath $destination -PathType Container)) { throw 'Skill 目标不是目录。' }
        $installedMetadataPath = Join-Path $destination 'skill.json'
        $statePath = Join-Path $destination '.remoteops-skill-install.json'
        if (Test-Path -LiteralPath $installedMetadataPath -PathType Leaf) {
            $installed = [IO.File]::ReadAllText($installedMetadataPath) | ConvertFrom-Json
            if ($installed.name -ne 'remoteops' -or $installed.version -notmatch '^\d+\.\d+\.\d+$') { throw '已安装 Skill 版本无效；保留现有目录。' }
            if ([version]$installed.version -gt $version) { throw '拒绝降级已安装的更新 Skill。' }
            if (-not (Test-Path -LiteralPath $statePath -PathType Leaf)) { throw '已版本化 Skill 缺少受管状态；保留现有目录。' }
            $state = [IO.File]::ReadAllText($statePath) | ConvertFrom-Json
            if ($state.version -ne $installed.version -or $state.component -ne 'remoteops-skill') { throw '已安装 Skill 状态无效。' }
            $installedFiles = if (@($state.files).Count -eq $baseFiles.Count) { @($baseFiles) } elseif (@($state.files).Count -eq $legacyFiles.Count) { @($legacyFiles) } else { @($managedFiles) }
            foreach ($file in $state.files) {
                if ($installedFiles -cnotcontains [string]$file.file) { throw '已安装 Skill 状态包含未知文件。' }
                $existing = Join-Path $destination $file.file
                if (-not (Test-Path -LiteralPath $existing -PathType Leaf) -or
                    (Get-FileHash -LiteralPath $existing -Algorithm SHA256).Hash -ine $file.sha256) {
                    throw '已安装 Skill 存在本地修改；保留现有目录。'
                }
            }
            if (@($state.files).Count -ne $installedFiles.Count) { throw '已安装 Skill 状态不完整。' }
            foreach ($name in $installedFiles) {
                if (@($state.files | Where-Object { $_.file -ceq $name }).Count -ne 1) { throw '已安装 Skill 状态包含重复或缺失文件。' }
            }
            $allowedPaths = @($installedFiles) + '.remoteops-skill-install.json'
            foreach ($existing in Get-ChildItem -LiteralPath $destination -Recurse -Force) {
                if ($existing.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '已安装 Skill 包含链接，拒绝更新。' }
                if (-not $existing.PSIsContainer) {
                    $relative = $existing.FullName.Substring($destination.Length + 1).Replace('\', '/')
                    if ($allowedPaths -cnotcontains $relative) { throw '已安装 Skill 含额外文件；保留现有目录。' }
                }
            }
            $same = $installed.version -eq $metadata.version
            if ($same) {
                if ($installedFiles.Count -ne $managedFiles.Count) { throw '同版 Skill 内容不同；请发布新版本。' }
                foreach ($name in $managedFiles) {
                    if ((Get-FileHash -LiteralPath (Join-Path $destination $name) -Algorithm SHA256).Hash -ine
                        (Get-FileHash -LiteralPath (Join-Path $sourceDirectory $name) -Algorithm SHA256).Hash) {
                        throw '同版 Skill 内容不同；请发布新版本。'
                    }
                }
            }
        } elseif (-not $ReplaceUnversioned) {
            throw '已有未版本化 Skill；如确认迁移，请加 ReplaceUnversioned，原目录将完整备份。'
        }
    }
    $storageParent = Split-Path -Parent (Split-Path -Parent $destination)
    if ([string]::IsNullOrWhiteSpace($storageParent)) { throw 'Skill 目标没有可用的外部备份父目录。' }
    $storage = [IO.Path]::GetFullPath((Join-Path $storageParent 'remoteops-skill-backups'))
    Assert-NoLinkedAncestor $storage
    foreach ($other in $destinations) {
        $searchRoot = (Split-Path -Parent $other).TrimEnd('\')
        if ($storage -ieq $searchRoot -or $storage.StartsWith($searchRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Skill 备份与暂存目录必须位于所有目标 Skill 搜索目录之外。'
        }
    }
    $plans += [pscustomobject]@{ destination=$destination; storage=$storage; same=$same; stage=$null; backup=$null; committed=$false }
}

function New-StoragePath([string]$Storage, [string]$Kind) {
    $path = [IO.Path]::GetFullPath((Join-Path $Storage ($Kind + '-' + [guid]::NewGuid().ToString('N'))))
    if (-not $path.StartsWith($Storage + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw 'Skill 证据路径超出固定存储目录。' }
    Assert-NoLinkedAncestor $path
    return $path
}

try {
    foreach ($plan in $plans) {
        if ($plan.same) { continue }
        $parent = Split-Path -Parent $plan.destination
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
        New-Item -ItemType Directory -Force -Path $plan.storage | Out-Null
        $plan.stage = New-StoragePath $plan.storage 'stage'
        New-Item -ItemType Directory -Path (Join-Path $plan.stage 'agents') -Force | Out-Null
        foreach ($name in $managedFiles) {
            $stageFile = Join-Path $plan.stage $name
            New-Item -ItemType Directory -Path (Split-Path -Parent $stageFile) -Force | Out-Null
            Copy-Item -LiteralPath (Join-Path $sourceDirectory $name) -Destination $stageFile
        }
        $state = [ordered]@{ component='remoteops-skill'; version=$metadata.version; files=@() }
        foreach ($name in $managedFiles) {
            $state.files += [ordered]@{ file=$name; sha256=(Get-FileHash -LiteralPath (Join-Path $plan.stage $name) -Algorithm SHA256).Hash.ToLowerInvariant() }
        }
        [IO.File]::WriteAllText((Join-Path $plan.stage '.remoteops-skill-install.json'), ($state | ConvertTo-Json -Depth 5) + "`r`n", [Text.UTF8Encoding]::new($false))
    }
    foreach ($plan in $plans) {
        if ($plan.same) { continue }
        if (Test-Path -LiteralPath $plan.destination) {
            $plan.backup = New-StoragePath $plan.storage 'backup'
            Move-Item -LiteralPath $plan.destination -Destination $plan.backup
        }
        Move-Item -LiteralPath $plan.stage -Destination $plan.destination
        $plan.committed = $true
    }
} catch {
    foreach ($plan in $plans) {
        if ($plan.committed -and (Test-Path -LiteralPath $plan.destination)) {
            Move-Item -LiteralPath $plan.destination -Destination (New-StoragePath $plan.storage 'failed')
        }
        if ($null -ne $plan.backup -and (Test-Path -LiteralPath $plan.backup) -and -not (Test-Path -LiteralPath $plan.destination)) {
            Move-Item -LiteralPath $plan.backup -Destination $plan.destination
        }
    }
    throw
}
foreach ($plan in $plans) {
    Write-Host "RemoteOps Skill $($metadata.version)：$($plan.destination)"
    if ($null -ne $plan.backup) { Write-Host "原目录备份：$($plan.backup)" }
}
Write-Host "兼容要求：MCP >= $($metadata.minimum_mcp_version)，协议 $($metadata.protocol)。本脚本未检查运行中的 MCP，也未修改 MCP 配置或凭据。"
Write-Host '请在支持 Skill 的宿主中新建会话；pi 需重开或 /reload。仅更新 Skill 不需要重启 MCP 进程。'
