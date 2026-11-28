[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$CodexHome = $(if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }),
    [string]$SkillDirectory
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not [string]::IsNullOrWhiteSpace($SkillDirectory)) {
    if ($PSBoundParameters.ContainsKey('CodexHome')) { throw 'CodexHome 与 SkillDirectory 只能选择一种。' }
    $destinations = @([IO.Path]::GetFullPath($SkillDirectory))
} else {
    $compat = Join-Path $CodexHome 'skills\remoteops'
    $standard = if ([IO.Path]::GetFullPath($CodexHome).TrimEnd('\') -ieq [IO.Path]::GetFullPath((Join-Path $env:USERPROFILE '.codex')).TrimEnd('\')) {
        Join-Path $env:USERPROFILE '.agents\skills\remoteops'
    } else { $compat }
    $destinations = @(@($standard, $compat) | ForEach-Object { [IO.Path]::GetFullPath($_) } | Select-Object -Unique)
}
$plans = @()
foreach ($destination in $destinations) {
    if ([IO.Path]::GetFileName($destination.TrimEnd('\')) -ine 'remoteops') { throw 'Skill 目标目录必须以 remoteops 命名。' }
    if (-not (Test-Path -LiteralPath $destination)) { continue }
    for ($current=$destination; -not [string]::IsNullOrEmpty($current); $current=Split-Path -Parent $current) {
        if ((Test-Path -LiteralPath $current) -and ((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Skill 路径不允许链接。' }
    }
    $statePath = Join-Path $destination '.remoteops-skill-install.json'
    if (-not (Test-Path -LiteralPath $statePath -PathType Leaf)) { throw 'Skill 没有受管状态；保留现有目录。' }
    $state = [IO.File]::ReadAllText($statePath) | ConvertFrom-Json
    $baseFiles = @('SKILL.md', 'skill.json', 'agents/openai.yaml')
    $legacyFiles = @($baseFiles) + @('references/tool-center-contract.md', 'references/examples/put-tool-external.json', 'references/examples/put-tool-hosted.json', 'references/examples/record-test.json')
    $expected = if (@($state.files).Count -eq $baseFiles.Count) { @($baseFiles) } elseif (@($state.files).Count -eq $legacyFiles.Count) { @($legacyFiles) } else { @($legacyFiles) + 'references/browser-startup.md' }
    if ($state.component -ne 'remoteops-skill' -or @($state.files).Count -ne $expected.Count) { throw 'Skill 安装状态无效。' }
    foreach ($name in $expected) {
        if (@($state.files | Where-Object { $_.file -ceq $name }).Count -ne 1) { throw 'Skill 安装状态包含重复或缺失文件。' }
    }
    foreach ($entry in $state.files) {
        $file = Join-Path $destination $entry.file
        for ($parent=Split-Path -Parent $file; $parent.Length -ge $destination.Length; $parent=Split-Path -Parent $parent) {
            if ((Test-Path -LiteralPath $parent) -and ((Get-Item -LiteralPath $parent -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Skill 文件父目录不允许链接。' }
        }
        if ($expected -cnotcontains [string]$entry.file -or -not (Test-Path -LiteralPath $file -PathType Leaf) -or
            (Get-Item -LiteralPath $file -Force).Attributes -band [IO.FileAttributes]::ReparsePoint -or
            (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ine $entry.sha256) { throw 'Skill 已有本地修改或状态异常；拒绝卸载。' }
    }
    $plans += [pscustomobject]@{ destination=$destination; state=$state }
}
foreach ($plan in $plans) {
    foreach ($entry in $plan.state.files) { Remove-Item -LiteralPath (Join-Path $plan.destination $entry.file) -Force }
    Remove-Item -LiteralPath (Join-Path $plan.destination '.remoteops-skill-install.json') -Force
    foreach ($directory in @((Join-Path $plan.destination 'references/examples'), (Join-Path $plan.destination 'references'), (Join-Path $plan.destination 'agents'), $plan.destination)) {
        if ((Test-Path -LiteralPath $directory -PathType Container) -and @(Get-ChildItem -LiteralPath $directory -Force).Count -eq 0) { Remove-Item -LiteralPath $directory -Force }
    }
    Write-Host "RemoteOps Skill 已卸载：$($plan.destination)"
}
Write-Host 'MCP 程序、配置、凭据、审计与备份均未修改。'
