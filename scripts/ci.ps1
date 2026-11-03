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
        python scripts/ci.py Node --frontend-tests
        pnpm --filter @centaeris/electron-host run build
        pnpm --filter @centaeris/electron-host run check:dist-licenses
        & (Join-Path $PSScriptRoot "build-tui.ps1")
        pnpm --filter @centaeris/electron-host run smoke:runtime
    } else {
        python scripts/ci.py $Stage
    }
} finally {
    Pop-Location
}
