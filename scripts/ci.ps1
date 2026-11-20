param(
    [ValidateSet("Release", "Rust", "Node", "Source")]
    [string]$Stage = "Release"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true
$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    if ($Stage -eq "Release") {
        if (-not $IsWindows) { throw "Release packaging currently supports Windows only; use python3 scripts/ci.py Source for portable source gates." }
        python scripts/ci.py Rust
        python scripts/ci.py Node
        & (Join-Path $PSScriptRoot "build-tui.ps1")
    } else {
        python scripts/ci.py $Stage
    }
} finally {
    Pop-Location
}
