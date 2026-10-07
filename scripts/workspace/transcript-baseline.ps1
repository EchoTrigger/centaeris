$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $repositoryRoot
try {
    node --test packages/web/tests/unit/transcript-baseline.test.mjs
    if ($LASTEXITCODE -ne 0) { throw "Workspace Web transcript baseline failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}
