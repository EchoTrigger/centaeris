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
        python -B scripts/test_system_skills.py
        & (Join-Path $PSScriptRoot "desktop-ui-acceptance.ps1")
        & (Join-Path $PSScriptRoot "build-tui.ps1")
        npm --prefix packages/desktop run smoke:runtime
    } else {
        python scripts/ci.py $Stage
    }
} finally {
    Pop-Location
}
