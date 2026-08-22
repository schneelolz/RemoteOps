[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workspaceRoot = Split-Path -Parent $PSScriptRoot
$markdownFiles = @(
    Get-ChildItem -LiteralPath $workspaceRoot -Recurse -Filter '*.md' -File |
        Where-Object {
            $_.FullName -notmatch '[\\/](?:target|artifacts)[\\/]'
        }
)
$brokenLinks = [System.Collections.Generic.List[string]]::new()

foreach ($file in $markdownFiles) {
    $content = [IO.File]::ReadAllText($file.FullName)
    foreach ($match in [regex]::Matches($content, '\[[^\]]*\]\((?<link>[^)]+)\)')) {
        $link = $match.Groups['link'].Value.Trim()
        if ($link -match '^(?:https?://|mailto:|#)') {
            continue
        }
        $pathPart = $link.Split('#')[0]
        if ([string]::IsNullOrWhiteSpace($pathPart)) {
            continue
        }
        try {
            $decodedPath = [Uri]::UnescapeDataString($pathPart)
            $target = [IO.Path]::GetFullPath((Join-Path $file.DirectoryName $decodedPath))
        }
        catch {
            $brokenLinks.Add("$($file.FullName)：无效链接 $link")
            continue
        }
        if (-not (Test-Path -LiteralPath $target)) {
            $brokenLinks.Add("$($file.FullName)：缺少 $link")
        }
    }
}

if ($brokenLinks.Count -gt 0) {
    $brokenLinks | ForEach-Object { Write-Error $_ }
    exit 1
}

Write-Host "RemoteOps 文档链接检查通过，共检查 $($markdownFiles.Count) 个 Markdown 文件。"
