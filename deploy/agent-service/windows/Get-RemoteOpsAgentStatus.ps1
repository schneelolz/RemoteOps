[CmdletBinding()]
param(
    [ValidatePattern('^[A-Za-z0-9_.-]{1,128}$')]
    [string]$ServiceName = 'RemoteOpsAgent',
    [string]$StatusFile = "$env:ProgramData\RemoteOps\Agent\runtime-status.json"
)

$ErrorActionPreference = 'Stop'
if ($ServiceName -ne 'RemoteOpsAgent' -and -not $PSBoundParameters.ContainsKey('StatusFile')) {
    throw '查询定制服务必须显式指定对应 StatusFile'
}
$service = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
$status = if (Test-Path -LiteralPath $StatusFile) {
    Get-Content -LiteralPath $StatusFile -Raw | ConvertFrom-Json
}
else {
    $null
}
[PSCustomObject]@{
    ServiceName = $ServiceName
    ServiceStatus = if ($null -eq $service) { 'NotInstalled' } else { $service.Status.ToString() }
    RuntimeStatus = if ($null -eq $status) { $null } else { $status.status }
    AgentInstanceId = if ($null -eq $status) { $null } else { $status.agent_instance_id }
    Relay = if ($null -eq $status) { $null } else { $status.relay }
    PairingCode = if ($null -eq $status) { $null } else { $status.pairing_code }
    PairingCodeExpiresAt = if ($null -eq $status) { $null } else { $status.pairing_code_expires_at }
    ActiveConnections = if ($null -eq $status) { 0 } else { $status.active_connections }
} | ConvertTo-Json -Depth 4

