param(
    [string]$RegistryRoot = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall'
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
try {
    $entries = @(Get-ChildItem -LiteralPath $RegistryRoot)
} catch [System.Management.Automation.ItemNotFoundException] {
    # A fresh user profile may not have an uninstall registry root yet.
    return
}
$entries |
    Get-ItemProperty |
    Where-Object { $_.DisplayName -like 'Centaeris Desktop*' } |
    Select-Object DisplayName, DisplayVersion, UninstallString |
    ConvertTo-Json -Compress
