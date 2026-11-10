param(
    [Parameter(Mandatory = $true)][string]$InstallDirectory,
    [string]$PreviousDirectory = "",
    [ValidateRange(0, 30)][int]$WaitSeconds = 15
)
Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

try {
    $prefixes = @()
    foreach ($directory in @($InstallDirectory, $PreviousDirectory)) {
        if ([string]::IsNullOrWhiteSpace($directory)) { continue }
        if (-not [System.IO.Path]::IsPathRooted($directory)) { throw "Installation directory must be absolute" }
        $full = [System.IO.Path]::GetFullPath($directory).TrimEnd('\')
        $root = [System.IO.Path]::GetPathRoot($full).TrimEnd('\')
        if ($full -eq $root -or $full -eq $env:USERPROFILE) { throw "Choose an application directory" }
        $dataRoots = @((Join-Path $env:USERPROFILE ".centaeris"))
        if ($env:CENTAERIS_DESKTOP_DATA_DIR) { $dataRoots += $env:CENTAERIS_DESKTOP_DATA_DIR }
        foreach ($dataDirectory in $dataRoots) {
            $dataRoot = [System.IO.Path]::GetFullPath($dataDirectory).TrimEnd('\')
            if ($full -eq $dataRoot -or $full.StartsWith($dataRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
                throw "Application files must be separate from Centaeris user data"
            }
        }
        if ((Test-Path -LiteralPath $full -PathType Container) -and
            -not (Test-Path -LiteralPath (Join-Path $full "Centaeris Desktop.exe")) -and
            (Get-ChildItem -LiteralPath $full -Force | Select-Object -First 1)) {
            throw "Choose an empty directory or an existing Centaeris Desktop installation"
        }
        $prefixes += $full + '\'
    }
    if ($prefixes.Count -eq 0) { throw "Installation directory is required" }
    $deadline = [DateTime]::UtcNow.AddSeconds($WaitSeconds)
    do {
        $busy = @(Get-CimInstance Win32_Process -Filter "Name='Centaeris Desktop.exe' OR Name='centaeris-runtime.exe'" | Where-Object {
            $executable = if ($_.ExecutablePath) { [System.IO.Path]::GetFullPath($_.ExecutablePath) } else { $null }
            if (-not $executable) { return $false }
            foreach ($prefix in $prefixes) {
                if ($executable.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { return $true }
            }
            return $false
        })
        if ($busy.Count -eq 0) { exit 0 }
        if ([DateTime]::UtcNow -ge $deadline) { break }
        Start-Sleep -Milliseconds 500
    } while ($true)
    Write-Output "Centaeris is still in use. Choose Exit from the Centaeris tray menu, close TUI clients, and finish active tasks before retrying. Background Runtime exits automatically when idle."
    exit 2
} catch {
    Write-Output ("Unable to verify installation safety: " + $_.Exception.Message)
    exit 3
}
