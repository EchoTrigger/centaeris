param(
    [switch]$ValidationOnly,
    [switch]$SkipFrontendTests
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true

$repoRoot = Split-Path -Parent $PSScriptRoot
if ($IsWindows) { $env:ComSpec = Join-Path $env:SystemRoot "System32\cmd.exe" }
if (-not $ValidationOnly -and -not $IsWindows) { throw "Packaged Desktop acceptance currently supports Windows only." }

Push-Location $repoRoot
try {
    $stepCount = if ($ValidationOnly) { 4 } else { 6 }

    Write-Host "[1/$stepCount] pnpm install --frozen-lockfile" -ForegroundColor Cyan
    pnpm install --frozen-lockfile

    if ($SkipFrontendTests) {
        Write-Host "[2/$stepCount] ui source validation" -ForegroundColor Cyan
        pnpm --filter centaeris-ui run build
        pnpm --filter centaeris-ui run lint:source
    } else {
        Write-Host "[2/$stepCount] ui gate" -ForegroundColor Cyan
        pnpm --filter centaeris-ui run gate
    }

    if ($SkipFrontendTests) {
        Write-Host "[3/$stepCount] electron host source validation" -ForegroundColor Cyan
        pnpm --filter @centaeris/electron-host run check:syntax
        pnpm --filter @centaeris/electron-host run check:host-parity
    } else {
        Write-Host "[3/$stepCount] electron check" -ForegroundColor Cyan
        pnpm --filter @centaeris/electron-host run check
    }

    Write-Host "[4/$stepCount] third-party license assembly" -ForegroundColor Cyan
    pnpm --filter @centaeris/electron-host run test:third-party-licenses

    if ($ValidationOnly) {
        Write-Host "Electron desktop + ui validation done." -ForegroundColor Green
        return
    }

    Write-Host "[5/6] electron build and dist license check" -ForegroundColor Cyan
    pnpm --filter @centaeris/electron-host run build
    pnpm --filter @centaeris/electron-host run check:dist-licenses

    Write-Host "[6/6] electron smoke" -ForegroundColor Cyan
    pnpm --filter @centaeris/electron-host run smoke:runtime
    pnpm --filter @centaeris/electron-host run smoke:window
} finally {
    Pop-Location
}

Write-Host "Electron desktop + ui acceptance done." -ForegroundColor Green
