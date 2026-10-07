$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$envPath = Join-Path $repoRoot ".env"

if (-not (Test-Path -LiteralPath $envPath)) {
    throw "Create .env from .env.example and fill its required secrets before starting."
}

Push-Location $repoRoot
try {
    $env:CENTAERIS_SOURCE_REVISION = (node scripts/workspace/source-revision.mjs).Trim()
    if ($LASTEXITCODE -ne 0) { throw "Source revision validation failed." }
    python scripts/workspace/build.py --env-file .env
    if ($LASTEXITCODE -ne 0) { throw "Compose image build failed." }

    docker compose up -d
    if ($LASTEXITCODE -ne 0) { throw "Compose startup failed." }
} finally {
    Pop-Location
}
