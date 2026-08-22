[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$RelayAddress,
    [Parameter(Mandatory)]
    [string]$RelayServerName,
    [string]$CertificatePath,
    [string[]]$CredentialTargets = @('RemoteOps/HumanControllerToken'),
    [switch]$ValidateOnly
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$guiPath = Join-Path $root 'remoteops-controller-gui.exe'
$auditPath = Join-Path $env:LOCALAPPDATA 'RemoteOps\live-test-audit.jsonl'

if (-not ('RemoteOpsGuiCredentialReader' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class RemoteOpsGuiCredentialReader
{
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct CREDENTIAL
    {
        public uint Flags;
        public uint Type;
        public string TargetName;
        public string Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public IntPtr CredentialBlob;
        public uint Persist;
        public uint AttributeCount;
        public IntPtr Attributes;
        public string TargetAlias;
        public string UserName;
    }

    [DllImport("advapi32.dll", EntryPoint = "CredReadW", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool CredRead(string target, uint type, int reservedFlag, out IntPtr credentialPtr);

    [DllImport("advapi32.dll", SetLastError = true)]
    public static extern void CredFree(IntPtr buffer);
}
'@
}

function Read-RemoteOpsSecret {
    param(
        [Parameter(Mandatory)]
        [string[]]$Targets
    )

    foreach ($target in $Targets) {
        $pointer = [IntPtr]::Zero
        if (-not [RemoteOpsGuiCredentialReader]::CredRead($target, 1, 0, [ref]$pointer)) {
            continue
        }

        try {
            $native = [Runtime.InteropServices.Marshal]::PtrToStructure(
                $pointer,
                [type][RemoteOpsGuiCredentialReader+CREDENTIAL]
            )
            return [Runtime.InteropServices.Marshal]::PtrToStringUni(
                $native.CredentialBlob,
                [int]($native.CredentialBlobSize / 2)
            )
        }
        finally {
            [RemoteOpsGuiCredentialReader]::CredFree($pointer)
        }
    }

    throw "Credential Manager target is missing: $($Targets -join ', ')"
}

try {
    if (-not (Test-Path -LiteralPath $guiPath)) {
        throw "GUI executable is missing: $guiPath"
    }
    if ($CertificatePath -and -not (Test-Path -LiteralPath $CertificatePath)) {
        throw "Relay certificate is missing: $certificatePath"
    }

    $humanToken = Read-RemoteOpsSecret -Targets $CredentialTargets

    if ($humanToken.Length -lt 64) {
        throw 'The human Controller token is invalid.'
    }

    if ($ValidateOnly) {
        $hostName, $portText = $relayAddress.Split(':', 2)
        $client = New-Object System.Net.Sockets.TcpClient
        try {
            $client.Connect($hostName, [int]$portText)
        }
        finally {
            $client.Dispose()
        }
        [PSCustomObject]@{
            GuiExists = $true
            CertificateConfigured = [bool]$CertificatePath
            CredentialsValid = $true
            RelayReachable = $true
        }
        return
    }

    $previousToken = $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN
    try {
        $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = $humanToken
        $arguments = @(
            '--relay', $relayAddress,
            '--server-name', $relayServerName,
            '--audit-log', $auditPath
        )
        if ($CertificatePath) {
            $arguments += @('--ca-cert', $CertificatePath)
        }
        & $guiPath @arguments
    }
    finally {
        if ($null -eq $previousToken) {
            Remove-Item Env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN -ErrorAction SilentlyContinue
        }
        else {
            $env:REMOTEOPS_HUMAN_CONTROLLER_TOKEN = $previousToken
        }
    }
}
catch {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Windows.Forms.MessageBox]::Show(
        $_.Exception.Message,
        'RemoteOps live test failed',
        [System.Windows.Forms.MessageBoxButtons]::OK,
        [System.Windows.Forms.MessageBoxIcon]::Error
    ) | Out-Null
    exit 1
}
finally {
    $humanToken = $null
}
