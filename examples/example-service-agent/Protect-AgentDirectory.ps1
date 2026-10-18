param(
    [Parameter(Mandatory = $true)][string]$Directory,
    [string]$ServiceAccount = 'SYSTEM'
)
$ErrorActionPreference = 'Stop'
$resolvedDirectory = (Resolve-Path -LiteralPath $Directory).Path
if (-not (Test-Path -LiteralPath $resolvedDirectory -PathType Container)) { throw '产品目录必须已存在' }
# 仅收紧显式产品目录及直属文件；不读取或打印凭据内容。
$principals = @(
    [System.Security.Principal.SecurityIdentifier]::new('S-1-5-18'),
    [System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'),
    [System.Security.Principal.NTAccount]::new($ServiceAccount)
)
$directoryAcl = [System.Security.AccessControl.DirectorySecurity]::new()
$directoryAcl.SetAccessRuleProtection($true, $false)
$directoryAcl.SetOwner([System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))
for ($index = 0; $index -lt $principals.Count; $index++) {
    $rights = if ($index -lt 2) { 'FullControl' } else { 'Modify' }
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new($principals[$index], $rights, 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $directoryAcl.AddAccessRule($rule)
}
Set-Acl -LiteralPath $resolvedDirectory -AclObject $directoryAcl
Get-ChildItem -LiteralPath $resolvedDirectory -File | ForEach-Object {
    $fileAcl = [System.Security.AccessControl.FileSecurity]::new()
    $fileAcl.SetAccessRuleProtection($true, $false)
    $fileAcl.SetOwner([System.Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))
    for ($index = 0; $index -lt $principals.Count; $index++) {
        $rights = if ($index -lt 2) { 'FullControl' } else { 'Modify' }
        $fileAcl.AddAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new($principals[$index], $rights, 'Allow'))
    }
    Set-Acl -LiteralPath $_.FullName -AclObject $fileAcl
}
