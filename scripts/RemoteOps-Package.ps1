Set-StrictMode -Version Latest

function Get-RemoteOpsPackageFiles {
    param([Parameter(Mandatory)][string]$Directory)
    $root = [IO.Path]::GetFullPath($Directory).TrimEnd([IO.Path]::DirectorySeparatorChar)
    if ((Get-Item -LiteralPath $root -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw '包目录不允许链接或重解析点。'
    }
    $items = @(Get-ChildItem -LiteralPath $root -Recurse -Force)
    if (@($items | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }).Count) {
        throw '包内容不允许链接或重解析点。'
    }
    @($items | Where-Object { -not $_.PSIsContainer } | Sort-Object FullName | ForEach-Object {
        [pscustomobject][ordered]@{
            file = $_.FullName.Substring($root.Length + 1).Replace('\', '/')
            bytes = $_.Length
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    })
}

function Write-RemoteOpsJson {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)]$Value)
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 20) + "`r`n", [Text.UTF8Encoding]::new($false))
}

function Write-RemoteOpsChecksums {
    param([Parameter(Mandatory)][string]$Directory)
    $lines = @(Get-RemoteOpsPackageFiles $Directory | Where-Object file -NE 'SHA256SUMS.txt' | ForEach-Object {
        "$($_.sha256)  $($_.file)"
    })
    [IO.File]::WriteAllText((Join-Path $Directory 'SHA256SUMS.txt'), ($lines -join "`r`n") + "`r`n", [Text.UTF8Encoding]::new($false))
}

function Test-RemoteOpsComponentPackage {
    param([Parameter(Mandatory)][string]$Directory, [string]$Component)
    $root = [IO.Path]::GetFullPath($Directory)
    $allFiles = @(Get-RemoteOpsPackageFiles $root)
    $manifest = Get-Content -LiteralPath (Join-Path $root 'manifest.json') -Raw | ConvertFrom-Json
    if ($manifest.schema_version -ne 1 -or ($Component -and $manifest.component -ne $Component)) {
        throw '组件清单格式或类型不匹配。'
    }
    $actual = @($allFiles | Where-Object { $_.file -notin @('manifest.json', 'SHA256SUMS.txt') })
    $expected = @($manifest.files)
    if ($expected.Count -ne $actual.Count) { throw '组件清单与实际文件数量不一致。' }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $expected) {
        if (-not $seen.Add([string]$entry.file) -or $entry.file -match '(^/|\\|(^|/)\.\.?(/|$)|:)' -or $entry.sha256 -notmatch '^[0-9a-f]{64}$') {
            throw '组件清单包含重复、越界路径或无效摘要。'
        }
        $match = @($actual | Where-Object file -CEQ $entry.file)
        if ($match.Count -ne 1 -or $match[0].bytes -ne $entry.bytes -or $match[0].sha256 -cne $entry.sha256) {
            throw "组件文件未通过校验：$($entry.file)"
        }
        if ($entry.file -match '(?i)(controller-config|setup-state|relay-ca|agent-state|audit|\.token$|\.secret$|\.key$|\.pfx$|\.p12$)') {
            throw '组件包含不应发布的运行状态或凭据文件。'
        }
    }
    $sumsPath = Join-Path $root 'SHA256SUMS.txt'
    if (-not (Test-Path -LiteralPath $sumsPath -PathType Leaf)) { throw '组件缺少 SHA256SUMS.txt。' }
    $sumEntries = @(Get-Content -LiteralPath $sumsPath | Where-Object { $_ })
    $sumFiles = @($allFiles | Where-Object file -NE 'SHA256SUMS.txt')
    if ($sumEntries.Count -ne $sumFiles.Count) { throw '组件摘要清单文件数量不一致。' }
    $sumSeen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($line in $sumEntries) {
        if ($line -notmatch '^([a-f0-9]{64})  (.+)$') { throw '组件摘要清单格式无效。' }
        $hash = $Matches[1]; $name = $Matches[2]
        $match = @($sumFiles | Where-Object file -CEQ $name)
        if (-not $sumSeen.Add($name) -or $match.Count -ne 1 -or $match[0].sha256 -cne $hash) { throw '组件摘要清单与实际内容不一致。' }
    }
    return $manifest
}

function Test-RemoteOpsMinimumVersion {
    param([Parameter(Mandatory)][string]$Actual, [Parameter(Mandatory)][string]$Minimum)
    $pattern = '^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$'
    if ($Actual -notmatch $pattern) { throw '实际组件版本格式无效。' }
    $leftCore = [version]"$($Matches[1]).$($Matches[2]).$($Matches[3])"
    $leftPre = [string]$Matches[4]
    if ($Minimum -notmatch $pattern) { throw '最低组件版本格式无效。' }
    $rightCore = [version]"$($Matches[1]).$($Matches[2]).$($Matches[3])"
    $rightPre = [string]$Matches[4]
    if ($leftCore -ne $rightCore) { return $leftCore -gt $rightCore }
    if (-not $leftPre) { return $true }
    if (-not $rightPre) { return $false }
    $left = $leftPre.Split('.')
    $right = $rightPre.Split('.')
    for ($i = 0; $i -lt [Math]::Min($left.Length, $right.Length); $i++) {
        if ($left[$i] -ceq $right[$i]) { continue }
        $ln = $left[$i] -match '^\d+$'
        $rn = $right[$i] -match '^\d+$'
        if ($ln -and $rn) { return [decimal]$left[$i] -gt [decimal]$right[$i] }
        if ($ln -ne $rn) { return -not $ln }
        return [string]::CompareOrdinal($left[$i], $right[$i]) -gt 0
    }
    return $left.Length -ge $right.Length
}

function Test-RemoteOpsArchive {
    param([Parameter(Mandatory)][string]$Directory, [Parameter(Mandatory)][string]$Archive)
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $expected = @(Get-RemoteOpsPackageFiles $Directory)
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        $files = @($zip.Entries | Where-Object { $_.Name })
        if ($files.Count -ne $expected.Count) { throw 'ZIP 文件数量不一致。' }
        foreach ($file in $expected) {
            $entries = @($files | Where-Object { $_.FullName.Replace('\', '/') -ceq $file.file })
            if ($entries.Count -ne 1 -or $entries[0].Length -ne $file.bytes) { throw 'ZIP 路径或文件大小不一致。' }
            $stream = $entries[0].Open()
            $hasher = [Security.Cryptography.SHA256]::Create()
            try { $hash = [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
            finally { $hasher.Dispose(); $stream.Dispose() }
            if ($hash -cne $file.sha256) { throw 'ZIP 文件摘要不一致。' }
        }
    }
    finally { $zip.Dispose() }
}
