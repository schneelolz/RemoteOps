[CmdletBinding()]
param(
    [string]$StatusFile = "$env:ProgramData\RemoteOps\Agent\runtime-status.json"
)

$ErrorActionPreference = 'Stop'
$service = Get-Service -Name 'RemoteOpsAgent' -ErrorAction SilentlyContinue
$status = if (Test-Path -LiteralPath $StatusFile) {
    Get-Content -LiteralPath $StatusFile -Raw | ConvertFrom-Json
}
else {
    $null
}
[PSCustomObject]@{
    ServiceName = 'RemoteOpsAgent'
    ServiceStatus = if ($null -eq $service) { 'NotInstalled' } else { $service.Status.ToString() }
    RuntimeStatus = if ($null -eq $status) { $null } else { $status.status }
    AgentInstanceId = if ($null -eq $status) { $null } else { $status.agent_instance_id }
    Relay = if ($null -eq $status) { $null } else { $status.relay }
    PairingCode = if ($null -eq $status) { $null } else { $status.pairing_code }
    PairingCodeExpiresAt = if ($null -eq $status) { $null } else { $status.pairing_code_expires_at }
    ActiveConnections = if ($null -eq $status) { 0 } else { $status.active_connections }
} | ConvertTo-Json -Depth 4

