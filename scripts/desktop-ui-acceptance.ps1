param(
    [switch]$ValidationOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

$repoRoot = Split-Path -Parent $PSScriptRoot
$env:ComSpec = Join-Path $env:SystemRoot "System32\cmd.exe"

Push-Location $repoRoot
try {
    $stepCount = if ($ValidationOnly) { 4 } else { 6 }

    Write-Host "[1/$stepCount] npm ci" -ForegroundColor Cyan
    npm ci

    Write-Host "[2/$stepCount] ui gate" -ForegroundColor Cyan
    npm run gate --workspace centaeris-ui

    Write-Host "[3/$stepCount] electron check" -ForegroundColor Cyan
    npm run check --workspace @centaeris/electron-host

    Write-Host "[4/$stepCount] third-party license assembly" -ForegroundColor Cyan
    npm run test:third-party-licenses --workspace @centaeris/electron-host

    if ($ValidationOnly) {
        Write-Host "Electron desktop + ui validation done." -ForegroundColor Green
        return
    }

    Write-Host "[5/6] electron build and dist license check" -ForegroundColor Cyan
    npm run build --workspace @centaeris/electron-host
    npm run check:dist-licenses --workspace @centaeris/electron-host

    Write-Host "[6/6] electron smoke" -ForegroundColor Cyan
    npm run smoke:runtime --workspace @centaeris/electron-host
    npm run smoke:window --workspace @centaeris/electron-host
} finally {
    Pop-Location
}

Write-Host "Electron desktop + ui acceptance done." -ForegroundColor Green
