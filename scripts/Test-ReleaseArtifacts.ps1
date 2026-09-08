[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$ArtifactRoot,

    [switch]$RequireLegalFiles,

    [string]$ReferenceMcpExecutable,

    [string]$McpArchive
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspaceRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$resolvedArtifactRoot = [IO.Path]::GetFullPath($ArtifactRoot)

function Get-CompatibleRelativePath {
    param(
        [Parameter(Mandatory)][string]$BasePath,
        [Parameter(Mandatory)][string]$Path
    )

    $base = [IO.Path]::GetFullPath($BasePath).TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar
    ) + [IO.Path]::DirectorySeparatorChar
    $baseUri = [Uri]::new($base)
    $pathUri = [Uri]::new([IO.Path]::GetFullPath($Path))
    return [Uri]::UnescapeDataString($baseUri.MakeRelativeUri($pathUri).ToString()).Replace(
        '/',
        [IO.Path]::DirectorySeparatorChar
    )
}

function Get-ZipEntrySha256 {
    param(
        [Parameter(Mandatory)][string]$ArchivePath,
        [Parameter(Mandatory)][string]$EntryName
    )

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($ArchivePath)
    try {
        $entries = @($archive.Entries | Where-Object Name -EQ $EntryName)
        if ($entries.Count -ne 1) {
            throw "MCP ZIP 必须且只能包含一个 ${EntryName}：$ArchivePath"
        }
        $stream = $entries[0].Open()
        $sha256 = [Security.Cryptography.SHA256]::Create()
        try {
            return ([BitConverter]::ToString($sha256.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
        }
        finally {
            $sha256.Dispose()
            $stream.Dispose()
        }
    }
    finally {
        $archive.Dispose()
    }
}

if (-not (Test-Path -LiteralPath $resolvedArtifactRoot -PathType Container)) {
    throw "Release artifact directory does not exist: $resolvedArtifactRoot"
}

if ($RequireLegalFiles) {
    foreach ($legalFile in @('LICENSE', 'THIRD_PARTY_LICENSES.txt', 'DEPENDENCIES.json')) {
        if (-not (Test-Path -LiteralPath (Join-Path $resolvedArtifactRoot $legalFile) -PathType Leaf)) {
            throw "Release artifact is missing a legal or dependency file: $legalFile"
        }
    }
}

$forbiddenFiles = @(
    Get-ChildItem -LiteralPath $resolvedArtifactRoot -Recurse -File |
        Where-Object {
            (Get-CompatibleRelativePath $resolvedArtifactRoot $_.FullName) -match `
                '(?i)(?:^|[\\/])(?:logs?|dumps?|crashdumps?)(?:[\\/]|$)' -or
            $_.Name -match '(?i)(?:\.log|\.dmp|\.pdb|\.key|\.pem|\.pfx|\.p12|\.token|\.secret)$' -or
            $_.Name -match '(?i)(?:agent|relay)-state.*\.json$' -or
            $_.Name -match '(?i)audit.*\.(?:json|jsonl)$'
        }
)
if ($forbiddenFiles.Count -gt 0) {
    $relativeNames = $forbiddenFiles |
        ForEach-Object { Get-CompatibleRelativePath $resolvedArtifactRoot $_.FullName }
    throw "Release artifact contains forbidden files: $($relativeNames -join ', ')"
}

$agentGui = Join-Path $resolvedArtifactRoot 'remoteops-agent-gui.exe'
if (Test-Path -LiteralPath $agentGui -PathType Leaf) {
    $askpass = Join-Path $resolvedArtifactRoot 'remoteops-ssh-askpass.exe'
    $manifestPath = Join-Path $resolvedArtifactRoot 'manifest.json'
    if (-not (Test-Path -LiteralPath $askpass -PathType Leaf)) {
        throw 'Windows Agent GUI 发布目录缺少 remoteops-ssh-askpass.exe。'
    }
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
        throw 'Windows Agent GUI 发布目录缺少 manifest.json。'
    }
    $manifest = @(Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json)
    foreach ($requiredExecutable in @('remoteops-agent-gui.exe', 'remoteops-ssh-askpass.exe')) {
        $entries = @($manifest | Where-Object File -CEQ $requiredExecutable)
        if ($entries.Count -ne 1) {
            throw "manifest.json 必须且只能包含一个 ${requiredExecutable} 条目。"
        }
        $executablePath = Join-Path $resolvedArtifactRoot $requiredExecutable
        $item = Get-Item -LiteralPath $executablePath
        $hash = (Get-FileHash -LiteralPath $executablePath -Algorithm SHA256).Hash.ToLowerInvariant()
        $entry = $entries[0]
        if ([long]$entry.Length -ne $item.Length) {
            throw "manifest.json 中的文件大小不匹配：$requiredExecutable"
        }
        if ([string]$entry.Sha256 -cne $hash) {
            throw "manifest.json 中的 SHA-256 不匹配：$requiredExecutable"
        }
        if (
            [string]$entry.FileVersion -cne $item.VersionInfo.FileVersion -or
            [string]$entry.ProductVersion -cne $item.VersionInfo.ProductVersion
        ) {
            throw "manifest.json 中的 Windows 文件版本不匹配：$requiredExecutable"
        }
        # GUI 和 askpass 独立发布；分别匹配自己的 Cargo 版本。
        $cargoVersion = [string]$entry.CargoVersion
        if ([string]::IsNullOrWhiteSpace($cargoVersion)) {
            throw "manifest.json 中缺少 Cargo 版本：$requiredExecutable"
        }
        $baseVersion = ($cargoVersion -split '-', 2)[0]
        $versionPattern = '^(?:{0}|{1}(?:\.0)?)$' -f `
            [regex]::Escape($cargoVersion),
            [regex]::Escape($baseVersion)
        if (
            $item.VersionInfo.FileVersion -cnotmatch $versionPattern -or
            $item.VersionInfo.ProductVersion -cnotmatch $versionPattern
        ) {
            throw "Windows 文件版本与自身 Cargo 版本不匹配：$requiredExecutable"
        }
    }
}

$userProfile = [Environment]::GetFolderPath('UserProfile')
$cargoHome = if ([string]::IsNullOrWhiteSpace($env:CARGO_HOME)) {
    Join-Path $userProfile '.cargo'
}
else {
    [IO.Path]::GetFullPath($env:CARGO_HOME)
}
$privatePaths = @($workspaceRoot, $cargoHome, $userProfile) |
    Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
    ForEach-Object {
        $_
        $_.Replace('\', '/')
    } |
    Sort-Object -Unique

$leaks = [System.Collections.Generic.List[string]]::new()
foreach ($file in Get-ChildItem -LiteralPath $resolvedArtifactRoot -Recurse -File) {
    $content = [Text.Encoding]::GetEncoding(28591).GetString(
        [IO.File]::ReadAllBytes($file.FullName)
    )
    foreach ($privatePath in $privatePaths) {
        if ($content.IndexOf($privatePath, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            $relativePath = Get-CompatibleRelativePath $resolvedArtifactRoot $file.FullName
            $leaks.Add("$relativePath contains a local absolute build path")
            break
        }
    }
}

if ($leaks.Count -gt 0) {
    $leaks | ForEach-Object { Write-Error $_ -ErrorAction Continue }
    exit 1
}

$packagedWindowsMcp = Join-Path $resolvedArtifactRoot 'remoteops-controller-mcp.exe'
$packagedMacMcp = Join-Path $resolvedArtifactRoot 'remoteops-controller-mcp'
if (Test-Path -LiteralPath $packagedWindowsMcp -PathType Leaf) {
    $credentialPrompt = Join-Path $resolvedArtifactRoot 'remoteops-credential-prompt.exe'
    if (-not (Test-Path -LiteralPath $credentialPrompt -PathType Leaf)) {
        throw 'Windows MCP package is missing remoteops-credential-prompt.exe.'
    }
}
elseif (Test-Path -LiteralPath $packagedMacMcp -PathType Leaf) {
    $credentialPrompt = Join-Path $resolvedArtifactRoot 'remoteops-credential-prompt'
    if (-not (Test-Path -LiteralPath $credentialPrompt -PathType Leaf)) {
        throw 'macOS MCP package is missing remoteops-credential-prompt.'
    }
}

if (-not [string]::IsNullOrWhiteSpace($ReferenceMcpExecutable)) {
    $resolvedReferenceMcp = [IO.Path]::GetFullPath($ReferenceMcpExecutable)
    $packagedMcp = Join-Path $resolvedArtifactRoot 'remoteops-controller-mcp.exe'
    if (-not (Test-Path -LiteralPath $resolvedReferenceMcp -PathType Leaf)) {
        throw "Reference MCP executable does not exist: $resolvedReferenceMcp"
    }
    if (-not (Test-Path -LiteralPath $packagedMcp -PathType Leaf)) {
        throw "MCP package executable does not exist: $packagedMcp"
    }
    $credentialPromptName = if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        'remoteops-credential-prompt.exe'
    }
    else {
        'remoteops-credential-prompt'
    }
    if (-not (Test-Path -LiteralPath (Join-Path $resolvedArtifactRoot $credentialPromptName) -PathType Leaf)) {
        throw "MCP package is missing the credential prompt helper: $credentialPromptName"
    }
    $referenceHash = (Get-FileHash -LiteralPath $resolvedReferenceMcp -Algorithm SHA256).Hash.ToLowerInvariant()
    $packageHash = (Get-FileHash -LiteralPath $packagedMcp -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($referenceHash -ne $packageHash) {
        throw 'Windows 发布目录与 MCP 安装包目录中的 MCP 可执行文件不一致。'
    }
    if (-not [string]::IsNullOrWhiteSpace($McpArchive)) {
        $resolvedMcpArchive = [IO.Path]::GetFullPath($McpArchive)
        if (-not (Test-Path -LiteralPath $resolvedMcpArchive -PathType Leaf)) {
            throw "MCP archive does not exist: $resolvedMcpArchive"
        }
        $archiveHash = Get-ZipEntrySha256 `
            -ArchivePath $resolvedMcpArchive `
            -EntryName 'remoteops-controller-mcp.exe'
        if ($referenceHash -ne $archiveHash) {
            throw 'Windows 发布目录与 MCP ZIP 中的 MCP 可执行文件不一致。'
        }
    }
}

Write-Host "Release path sanitization passed for $((Get-ChildItem -LiteralPath $resolvedArtifactRoot -Recurse -File).Count) files."
