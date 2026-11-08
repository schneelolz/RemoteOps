#requires -Version 7.2
<# .SYNOPSIS 只读核对工具中心测试候选及配套归档，不启动 EXE、不部署、不替换旧候选。 #>
[CmdletBinding()]
param(
    [string]$WorkDirectory = 'C:/Temp/RemoteOps-jlai9-p19-20261105/build-final',
    [string]$Executable = 'C:/Temp/RemoteOps-jlai9-p19-20261105/delivery-final/精灵AI-Agent-Win32-0.4.0-jlai.9.exe',
    [string]$ExpectedVersion = '0.4.0-jlai.9',
    [string]$ReportPath = (Join-Path (Split-Path -Parent $PSScriptRoot) 'artifacts/tool-center-candidate-validation.json')
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$checks = [Collections.Generic.List[object]]::new()
$failures = [Collections.Generic.List[string]]::new()
$workRoot = [IO.Path]::GetFullPath($WorkDirectory)
$candidateExe = [IO.Path]::GetFullPath($Executable)
$reportFile = [IO.Path]::GetFullPath($ReportPath)
$expectedProtocol = 19
$baseRoot = Join-Path $workRoot 'base'
$productRoot = Join-Path $workRoot 'product'
$archiveRoot = Join-Path $workRoot 'archive'

function Add-Check {
    param([string]$Name, [bool]$Passed, [object]$Details)
    $checks.Add([ordered]@{ name = $Name; passed = $Passed; details = $Details })
    if (-not $Passed) { $failures.Add($Name) }
}

function Get-Hash {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "所需文件不存在：$Path" }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '验收文件不允许链接或重解析点。' }
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Resolve-InputFile {
    param([string]$Root, [string]$Relative)
    if ([IO.Path]::IsPathRooted($Relative) -or $Relative.Contains(':')) { throw '来源记录必须使用相对路径。' }
    $fullRoot = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    $path = [IO.Path]::GetFullPath((Join-Path $fullRoot $Relative))
    if (-not $path.StartsWith($fullRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw '来源记录路径越出独立快照。' }
    for ($parent = Split-Path -Parent $path; $parent -and $parent.Length -ge $fullRoot.Length; $parent = Split-Path -Parent $parent) {
        if ((Get-Item -LiteralPath $parent -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '来源目录不允许链接或重解析点。' }
    }
    return $path
}

function Test-Inputs {
    param([string]$Name, [string]$Root, [object]$Inputs, [switch]$FilesOnly)
    $identities = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $lines = [Collections.Generic.List[string]]::new()
    $mismatches = [Collections.Generic.List[string]]::new()
    foreach ($record in @($Inputs.files)) {
        $relative = [string]$record.file
        if (-not $identities.Add($relative) -or $record.sha256 -cnotmatch '^[a-f0-9]{64}$') { throw '来源记录有重复路径或无效摘要。' }
        $path = Resolve-InputFile $Root $relative
        if ((Get-Hash $path) -cne $record.sha256) { $mismatches.Add($relative) }
        $lines.Add("$($record.sha256)  $relative")
    }
    $identityText = ($lines -join "`n") + "`n"
    $digest = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($identityText))).ToLowerInvariant()
    Add-Check "$Name 逐文件摘要" ($lines.Count -gt 0 -and $mismatches.Count -eq 0) ([ordered]@{ count = $lines.Count; mismatches = @($mismatches) })
    if (-not $FilesOnly) { Add-Check "$Name 内容摘要" ($digest -ceq $Inputs.content_sha256) $digest }
}

function Get-PackageVersion {
    param([string]$Manifest)
    $text = [IO.File]::ReadAllText($Manifest)
    $package = [regex]::Match($text, '(?ms)^\[package\]\s*\r?\n(?<body>.*?)(?=^\[|\z)').Groups['body'].Value
    $match = [regex]::Match($package, '(?m)^version\s*=\s*"(?<version>[^"\r\n]+)"\s*$')
    if (-not $match.Success) { throw "无法读取包版本：$Manifest" }
    return $match.Groups['version'].Value
}

function Get-LockPackages {
    param([string]$Path)
    $text = [IO.File]::ReadAllText($Path)
    foreach ($match in [regex]::Matches($text, '(?ms)^\[\[package\]\]\s*\r?\n(?<body>.*?)(?=^\[\[package\]\]|\z)')) {
        $fields = @{}
        foreach ($field in [regex]::Matches($match.Groups['body'].Value, '(?m)^(name|version|source|checksum)\s*=\s*"([^"\r\n]+)"\s*$')) { $fields[$field.Groups[1].Value] = $field.Groups[2].Value }
        if (-not $fields.ContainsKey('name') -or -not $fields.ContainsKey('version')) { throw '锁文件中的包缺少名称或版本。' }
        [pscustomobject]@{ name = $fields['name']; version = $fields['version']; source = $fields['source']; checksum = $fields['checksum'] }
    }
}

function Test-Executable {
    param([string]$Path, [string]$Version, [string]$ExpectedHash, [long]$ExpectedBytes)
    $hash = Get-Hash $Path
    $item = Get-Item -LiteralPath $Path
    $info = [Diagnostics.FileVersionInfo]::GetVersionInfo($Path)
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    $name = [IO.Path]::GetFileName($Path)
    Add-Check "$name 文件身份" ($hash -ceq $ExpectedHash -and $item.Length -eq $ExpectedBytes) ([ordered]@{ bytes = $item.Length; sha256 = $hash })
    Add-Check "$name PE 版本" ($info.FileVersion -ceq $Version -and $info.ProductVersion -ceq $Version) ([ordered]@{ file_version = $info.FileVersion; product_version = $info.ProductVersion; expected = $Version })
    Add-Check "$name 未签名候选" ([string]$signature.Status -ceq 'NotSigned') ([string]$signature.Status)
}

try {
    $sourcePath = Join-Path $workRoot 'source-inputs.json'
    $source = [IO.File]::ReadAllText($sourcePath) | ConvertFrom-Json
    $manifestPath = Join-Path $archiveRoot 'manifest.json'
    $manifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
    Add-Check '来源记录归档副本' ((Get-Hash $sourcePath) -ceq (Get-Hash (Join-Path $archiveRoot 'source-inputs.json'))) (Get-Hash $sourcePath)
    Add-Check '候选与隔离交付标识' ($manifest.version -ceq $expectedVersion -and $manifest.protocol -eq $expectedProtocol -and $manifest.candidate_only -eq $true -and $manifest.formal_fixed_rev_build -eq $false -and $manifest.old_version_replaced -eq $false -and $manifest.relay_mcp_replaced -eq $false) ([ordered]@{ version = $manifest.version; protocol = $manifest.protocol })
    Add-Check '快照来源修订与内容标识' ($manifest.base_commit -ceq $source.base_commit -and $manifest.base_content_sha256 -ceq $source.base_inputs.content_sha256 -and $manifest.product_content_sha256 -ceq $source.product_inputs.content_sha256 -and $source.candidate_only -eq $true -and $source.formal_fixed_rev_build -eq $false) ([ordered]@{ base_commit = $source.base_commit; base_dirty = $source.base_dirty; product_commit = $source.product_commit; product_dirty = $source.product_dirty })
    Test-Inputs '公共底座' $baseRoot $source.base_inputs
    Test-Inputs '产品源码' $productRoot $source.product_inputs
    $webPath = Join-Path $archiveRoot 'companion-web-inputs.json'
    if (Test-Path -LiteralPath $webPath) {
        $web = [IO.File]::ReadAllText($webPath) | ConvertFrom-Json
        Test-Inputs 'Relay 配套额外后台输入（独立清单）' $baseRoot $web -FilesOnly
        $webField = $manifest.PSObject.Properties['companion_web_inputs']
        $webDeclarationValid = $false
        if ($null -ne $webField) {
            $declaredWeb = $webField.Value
            $webDeclarationValid = $declaredWeb.file -ceq 'companion-web-inputs.json' -and $declaredWeb.sha256 -ceq (Get-Hash $webPath)
        }
        Add-Check '额外后台输入单独声明' $webDeclarationValid ([ordered]@{ file = 'companion-web-inputs.json'; sha256 = (Get-Hash $webPath); agent_base_inputs_unchanged = $true })
    
    
    } else {
        $webFiles = @($source.base_inputs.files | Where-Object file -Like 'relay-admin-prototype/*')
        Add-Check '后台输入纳入统一快照' ($webFiles.Count -ge 5 -and @($webFiles | Where-Object file -CEQ 'relay-admin-prototype/tools.js').Count -eq 1) ([ordered]@{ count = $webFiles.Count; same_base_inputs = $true })
    }

    $productManifest = Join-Path $productRoot 'Cargo.toml'
    Add-Check '产品生成清单身份' ((Get-Hash $productManifest) -ceq $manifest.generated_manifest_sha256 -and (Get-PackageVersion $productManifest) -ceq $expectedVersion) (Get-Hash $productManifest)
    $protocolSource = [IO.File]::ReadAllText((Join-Path $baseRoot 'crates/remoteops-protocol/src/message.rs'))
    $protocol = [regex]::Match($protocolSource, 'pub const PROTOCOL_VERSION: u16 = (\d+);')
    Add-Check '公共线协议版本' ($protocol.Success -and [int]$protocol.Groups[1].Value -eq $expectedProtocol) $expectedProtocol
    Add-Check '品牌图标身份' ((Get-Hash (Join-Path $productRoot 'assets/brand/jlai.ico')) -ceq $source.brand_icon_sha256 -and $manifest.brand_icon_sha256 -ceq $source.brand_icon_sha256) $source.brand_icon_sha256
    Add-Check 'Windows 应用清单同源' ((Get-Hash (Join-Path $productRoot 'app.manifest')) -ceq (Get-Hash (Join-Path $baseRoot 'apps/remoteops-agent-native-prototype/app.manifest'))) '产品与公共候选应用清单摘要一致'

    $baseLock = Join-Path $baseRoot 'Cargo.lock'
    $productLock = Join-Path $productRoot 'Cargo.lock'
    Add-Check '底座锁文件归档' ((Get-Hash $baseLock) -ceq (Get-Hash (Join-Path $archiveRoot 'companions-Cargo.lock'))) (Get-Hash $baseLock)
    Add-Check '产品锁文件归档' ((Get-Hash $productLock) -ceq (Get-Hash (Join-Path $archiveRoot 'candidate-Cargo.lock'))) (Get-Hash $productLock)
    $basePackages = @(Get-LockPackages $baseLock)
    $productPackages = @(Get-LockPackages $productLock)
    $locked = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($package in @($basePackages | Where-Object source)) { [void]$locked.Add("$($package.name)|$($package.version)|$($package.source)|$($package.checksum)") }
    $drift = @($productPackages | Where-Object { $_.source -and -not $locked.Contains("$($_.name)|$($_.version)|$($_.source)|$($_.checksum)") } | Select-Object name, version, source)
    Add-Check '产品依赖固定于底座锁文件' ($drift.Count -eq 0) $drift
    Add-Check '产品根包锁定版本' (@($productPackages | Where-Object { $_.name -ceq 'jlai-agent-win32-candidate' -and $_.version -ceq $expectedVersion -and -not $_.source }).Count -eq 1) $expectedVersion

    $deliveryName = [IO.Path]::GetFileName($candidateExe)
    Add-Check '候选文件名' ($manifest.file -ceq $deliveryName -and $deliveryName -ceq "精灵AI-Agent-Win32-$expectedVersion.exe") $deliveryName
    Test-Executable $candidateExe $expectedVersion $manifest.sha256 ([long]$manifest.bytes)
    foreach ($required in @('remoteops-relay.exe', 'remoteops-controller-mcp.exe')) {
        Add-Check "$required 配套归档存在" (@($manifest.companions | Where-Object file -CEQ $required).Count -eq 1) $required
    }
    foreach ($companion in @($manifest.companions)) {
        $relative = [string]$companion.file
        if ([IO.Path]::GetFileName($relative) -cne $relative -or -not $relative.EndsWith('.exe', [StringComparison]::OrdinalIgnoreCase)) { throw '配套记录必须为简单 EXE 文件名。' }
        $packageName = [IO.Path]::GetFileNameWithoutExtension($relative)
        $version = Get-PackageVersion (Join-Path $baseRoot "apps/$packageName/Cargo.toml")
        Add-Check "$packageName Cargo 版本" ($version -ceq $companion.version -and @($basePackages | Where-Object { $_.name -ceq $packageName -and $_.version -ceq $version -and -not $_.source }).Count -eq 1) $version
        Test-Executable (Join-Path $archiveRoot "companions/$relative") $version $companion.sha256 ([long]$companion.bytes)
    }

    foreach ($relative in @('THIRD_PARTY_LICENSES.txt', 'BASE-THIRD_PARTY_LICENSES.txt', 'DEPENDENCIES.json', 'BASE-DEPENDENCIES.json')) {
        $path = Join-Path $archiveRoot $relative
        $hash = Get-Hash $path
        Add-Check "$relative 许可证归档" ((Get-Item -LiteralPath $path).Length -gt 0) $hash
    }
    foreach ($relative in @('LICENSE', 'third-party/putty/LICENCE.txt')) { Add-Check "$relative 来源许可证" ((Get-Item -LiteralPath (Join-Path $baseRoot $relative)).Length -gt 0) (Get-Hash (Join-Path $baseRoot $relative)) }
    $embedded = [IO.File]::ReadAllText((Join-Path $productRoot 'assets/runtime/embedded-hashes.rs'))
    foreach ($entry in @(@('PLINK', 'tools/plink.exe'), @('ASKPASS', 'remoteops-ssh-askpass.exe'), @('LEGAL', 'THIRD_PARTY_LICENSES.txt'))) {
        $digest = [regex]::Match($embedded, ('pub const ' + $entry[0] + '_SHA256: &str = "([a-f0-9]{64})";'))
        $actual = Get-Hash (Join-Path $productRoot "assets/runtime/$($entry[1])")
        Add-Check "$($entry[0]) 内嵌资源摘要" ($digest.Success -and $digest.Groups[1].Value -ceq $actual -and ($manifest.embedded_hashes -join "`n").Contains($actual)) $actual
    }
    Add-Check '许可正文与内嵌副本一致' ((Get-Hash (Join-Path $archiveRoot 'THIRD_PARTY_LICENSES.txt')) -ceq (Get-Hash (Join-Path $productRoot 'assets/runtime/THIRD_PARTY_LICENSES.txt'))) '独立许可归档与内嵌资源相同'
} catch {
    Add-Check '验收执行' $false $_.Exception.Message
} finally {
    $parent = Split-Path -Parent $reportFile
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
    $report = [ordered]@{
        validated_at = [datetime]::UtcNow.ToString('o'); passed = $failures.Count -eq 0
        work_directory = $workRoot; executable = $candidateExe; expected_version = $expectedVersion; expected_protocol = $expectedProtocol
        executable_started = $false; deployed = $false; old_candidate_replaced = $false
        validation_script_sha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
        checks = @($checks); failed_checks = @($failures)
        limitations = @('本脚本只验证指定快照及归档，不验证运行行为、生产组件或现场平台。', '外部验收脚本生成于源码快照之后，单独归档。')
    }
    [IO.File]::WriteAllText($reportFile, ($report | ConvertTo-Json -Depth 12), [Text.UTF8Encoding]::new($false))
}
if ($failures.Count) { throw "候选验收失败：$($failures -join '、')；报告：$reportFile" }
Write-Output "候选只读验收通过：$($checks.Count) 项；报告：$reportFile"
