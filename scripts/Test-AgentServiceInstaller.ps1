[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') { throw 'Windows 服务安装器回归仅适用于 Windows。' }
$installer = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../deploy/agent-service/windows/Install-RemoteOpsAgentService.ps1'))
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('Remote Ops 服务安装测试 ' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
$sourceExecutable = Join-Path $fixtureRoot 'source agent.exe'
$sourceConfig = Join-Path $fixtureRoot 'source config.json'
[IO.File]::WriteAllText($sourceExecutable, 'synthetic executable; never run')
[IO.File]::WriteAllText($sourceConfig, '{}')
$global:RemoteOpsInstallerTestState = @{}
$global:RemoteOpsInstallerTestState.Results = [Collections.Generic.List[object]]::new()

function Assert-Condition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

# 只模拟系统边界；实际执行公共脚本的文件复制、路径合并及错误控制。
function Get-Service {
    [CmdletBinding()]
    param([string]$Name)
    if ($global:RemoteOpsInstallerTestState.Existing) { return $global:RemoteOpsInstallerTestState.Service }
}
function Stop-Service {
    [CmdletBinding()]
    param([string]$Name, [switch]$Force)
    $global:RemoteOpsInstallerTestState.Calls.Add('Stop')
    $global:RemoteOpsInstallerTestState.Service.Status = 'Stopped'
}
function Start-Service {
    [CmdletBinding()]
    param([string]$Name)
    $global:RemoteOpsInstallerTestState.Calls.Add('Start')
}
function Get-CimInstance {
    [CmdletBinding()]
    param([string]$ClassName, [string]$Filter)
    Assert-Condition ($ClassName -eq 'Win32_Service' -and $Filter -eq "Name='TestRemoteOpsInstaller'") 'CIM 查询必须精确限定服务。'
    if ($global:RemoteOpsInstallerTestState.Existing) { return $global:RemoteOpsInstallerTestState.CimService }
}
function Invoke-CimMethod {
    [CmdletBinding()]
    param([string]$ClassName, [object]$InputObject, [string]$MethodName, [hashtable]$Arguments)
    $global:RemoteOpsInstallerTestState.Calls.Add($MethodName)
    $global:RemoteOpsInstallerTestState.Method = $MethodName
    $global:RemoteOpsInstallerTestState.MethodArguments = $Arguments
    $global:RemoteOpsInstallerTestState.MethodTarget = $InputObject
    if ($global:RemoteOpsInstallerTestState.Existing) {
        Assert-Condition $global:RemoteOpsInstallerTestState.Service.Waited '更新前必须完成停止等待。'
        Assert-Condition ([object]::ReferenceEquals($InputObject, $global:RemoteOpsInstallerTestState.CimService)) '更新必须针对准确 CIM 实例。'
    } else {
        Assert-Condition ($ClassName -eq 'Win32_Service') '创建必须使用 Win32_Service。'
    }
    if ($global:RemoteOpsInstallerTestState.ThrowCim) { throw 'controlled-cim-failure' }
    if ($global:RemoteOpsInstallerTestState.MissingReturnValue) { return [pscustomobject]@{} }
    return [pscustomobject]@{ ReturnValue = $global:RemoteOpsInstallerTestState.CimCode }
}
function Set-Service {
    [CmdletBinding()]
    param([string]$Name, [string]$Description)
    $global:RemoteOpsInstallerTestState.Calls.Add('Description')
    if ($global:RemoteOpsInstallerTestState.ThrowDescription) { throw 'controlled-description-failure' }
}
function icacls.exe {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$NativeArguments)
    $global:LASTEXITCODE = 0
}
function sc.exe {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$NativeArguments)
    Assert-Condition ($NativeArguments[0] -eq 'failure') '不得通过原生命令创建或更新服务。'
    Assert-Condition (($NativeArguments -join ' ') -eq 'failure TestRemoteOpsInstaller reset= 86400 actions= restart/5000/restart/30000/none/0') '故障恢复参数发生变化。'
    $global:RemoteOpsInstallerTestState.Calls.Add('Recovery')
    $global:LASTEXITCODE = $global:RemoteOpsInstallerTestState.ScCode
    if ($global:RemoteOpsInstallerTestState.ScCode -ne 0) { return 'controlled-sc-failure' }
    return 'controlled-sc-success'
}

function Invoke-Case {
    param(
        [string]$Name,
        [bool]$Existing = $false,
        [bool]$LocalSystem = $false,
        [bool]$Start = $true,
        [int]$CimCode = 0,
        [int]$ScCode = 0,
        [bool]$ThrowCim = $false,
        [bool]$MissingReturnValue = $false,
        [bool]$ThrowDescription = $false,
        [string]$ExpectedError
    )
    $global:RemoteOpsInstallerTestState.Existing = $Existing
    $global:RemoteOpsInstallerTestState.Calls = [Collections.Generic.List[string]]::new()
    $global:RemoteOpsInstallerTestState.CimService = [pscustomobject]@{ Name = 'TestRemoteOpsInstaller' }
    $global:RemoteOpsInstallerTestState.Service = [pscustomobject]@{ Status = 'Running'; Waited = $false }
    $global:RemoteOpsInstallerTestState.Service | Add-Member -MemberType ScriptMethod -Name WaitForStatus -Value { param($status, $timeout) $this.Waited = $true }
    $global:RemoteOpsInstallerTestState.Method = $null
    $global:RemoteOpsInstallerTestState.MethodArguments = $null
    $global:RemoteOpsInstallerTestState.CimCode = $CimCode
    $global:RemoteOpsInstallerTestState.ScCode = $ScCode
    $global:RemoteOpsInstallerTestState.ThrowCim = $ThrowCim
    $global:RemoteOpsInstallerTestState.MissingReturnValue = $MissingReturnValue
    $global:RemoteOpsInstallerTestState.ThrowDescription = $ThrowDescription
    $installRoot = Join-Path $fixtureRoot ($Name + ' 程序')
    $dataRoot = Join-Path $fixtureRoot ($Name + ' 数据')
    $arguments = @{
        ConfigPath = $sourceConfig
        ServiceExecutable = $sourceExecutable
        ServiceName = 'TestRemoteOpsInstaller'
        DisplayName = 'RemoteOps 定制服务 测试'
        InstallRoot = $installRoot
        DataRoot = $dataRoot
        StartService = $Start
        AllowLocalSystem = $LocalSystem
    }
    $caught = $null
    try { & $installer @arguments | Out-Null } catch { $caught = $_.Exception.Message }
    if ($ExpectedError) {
        Assert-Condition ($null -ne $caught -and $caught.Contains($ExpectedError)) ("未报告预期错误：" + $Name + '; actual=' + $caught)
        Assert-Condition (-not $global:RemoteOpsInstallerTestState.Calls.Contains('Start')) '失败不得启动服务。'
    } else {
        Assert-Condition ($null -eq $caught) ("安装器回归失败：" + $caught)
        $expectedMode = if ($Existing) { 'Change' } else { 'Create' }
        Assert-Condition ($global:RemoteOpsInstallerTestState.Method -eq $expectedMode) '服务创建和更新分支错误。'
        $expectedPath = '"{0}" --config "{1}" --status-file "{2}"' -f (Join-Path $installRoot 'remoteops-agent-service.exe'), (Join-Path $dataRoot 'agent-config.json'), (Join-Path $dataRoot 'runtime-status.json')
        Assert-Condition ($global:RemoteOpsInstallerTestState.MethodArguments.PathName -ceq $expectedPath) '路径中的空格、中文或内部双引号被改写。'
        $expectedAccount = if ($LocalSystem) { 'LocalSystem' } else { 'NT AUTHORITY\LocalService' }
        Assert-Condition ($global:RemoteOpsInstallerTestState.MethodArguments.StartName -eq $expectedAccount) '服务账户语义发生变化。'
        Assert-Condition ($global:RemoteOpsInstallerTestState.MethodArguments.StartMode -eq 'Automatic') '未配置开机自动启动。'
        Assert-Condition ($global:RemoteOpsInstallerTestState.MethodArguments.StartPassword -ceq '') '内置账户密码参数错误。'
        if (-not $Existing) {
            Assert-Condition ($global:RemoteOpsInstallerTestState.MethodArguments.ServiceType -eq 16 -and $global:RemoteOpsInstallerTestState.MethodArguments.ErrorControl -eq 1 -and $global:RemoteOpsInstallerTestState.MethodArguments.DesktopInteract -eq $false) '服务类型或交互设置错误。'
        }
        Assert-Condition ($global:RemoteOpsInstallerTestState.Calls.Contains('Start') -eq $Start) 'StartService 开关未被遵守。'
        if ($Start) {
            Assert-Condition (($global:RemoteOpsInstallerTestState.Calls -join ',').EndsWith('Description,Recovery,Start')) '配置全部成功后才能启动。'
        }
        Assert-Condition ([IO.File]::ReadAllText((Join-Path $installRoot 'remoteops-agent-service.exe')) -eq 'synthetic executable; never run') '程序复制失败。'
    }
    $global:RemoteOpsInstallerTestState.Results.Add([pscustomobject]@{ case = $Name; passed = $true })
}

Invoke-Case -Name 'create-quoted-path'
Invoke-Case -Name 'update-local-system' -Existing $true -LocalSystem $true
Invoke-Case -Name 'configure-without-start' -Start $false
Invoke-Case -Name 'create-access-denied' -CimCode 2 -ExpectedError '2'
Invoke-Case -Name 'change-invalid-parameter' -Existing $true -CimCode 21 -ExpectedError '21'
Invoke-Case -Name 'missing-cim-result' -MissingReturnValue $true -ExpectedError 'CIM'
Invoke-Case -Name 'cim-exception' -ThrowCim $true -ExpectedError 'controlled-cim-failure'
Invoke-Case -Name 'description-failure' -ThrowDescription $true -ExpectedError 'controlled-description-failure'
Invoke-Case -Name 'recovery-failure' -ScCode 1072 -ExpectedError '1072'
$global:RemoteOpsInstallerTestState.Results | ConvertTo-Json
Write-Output ('Passed: ' + $global:RemoteOpsInstallerTestState.Results.Count + '; fixture=' + $fixtureRoot)
