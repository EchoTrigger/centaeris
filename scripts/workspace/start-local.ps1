$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$envPath = Join-Path $repoRoot ".env"

if (-not (Test-Path -LiteralPath $envPath)) {
    throw "Create .env from .env.example and fill its required secrets before starting."
}

Push-Location $repoRoot
try {
    $dockerStorageDriver = (docker info --format '{{.Driver}}')
    if ($LASTEXITCODE -ne 0) { throw "Docker storage backend could not be verified." }
    if ($dockerStorageDriver.Trim() -notin @("overlayfs", "overlay2")) {
        throw "Local deployment requires OverlayFS-backed Docker storage."
    }
    $env:CENTAERIS_SOURCE_REVISION = (node scripts/workspace/source-revision.mjs).Trim()
    if ($LASTEXITCODE -ne 0) { throw "Source revision validation failed." }
    python scripts/workspace/build.py --env-file .env
    if ($LASTEXITCODE -ne 0) { throw "Compose image build failed." }

    docker compose up -d
    if ($LASTEXITCODE -ne 0) { throw "Compose startup failed." }
} finally {
    Pop-Location
}
