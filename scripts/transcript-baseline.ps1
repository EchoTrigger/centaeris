$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $repositoryRoot
try {
    cargo test --locked -p centaeris-tui app::tests::transcript_rendering_p0_baseline -- --ignored --exact --nocapture
    if ($LASTEXITCODE -ne 0) { throw "TUI transcript baseline failed with exit code $LASTEXITCODE" }

    node packages/ui/scripts/run-transcript-baseline.mjs
    if ($LASTEXITCODE -ne 0) { throw "Desktop transcript baseline failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}
