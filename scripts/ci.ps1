param(
    [ValidateSet("Release", "Rust", "Node")]
    [string]$Stage = "Release"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $true
$env:CARGO_BUILD_JOBS = "1"

$repoRoot = Split-Path -Parent $PSScriptRoot
$env:ComSpec = Join-Path $env:SystemRoot "System32\cmd.exe"

Push-Location $repoRoot
try {
    if ($Stage -in @("Release", "Rust")) {
        cargo fmt --all -- --check
        cargo check --workspace --locked
        cargo clippy --workspace --all-targets --locked -- -D warnings
        cargo run --locked -p centaeris-runtime --bin centaeris-runtime-protocol-docs -- --check

        $coreTree = cargo tree --locked -p centaeris-core --edges normal,build,dev
        if ($LASTEXITCODE -ne 0) { throw "Core dependency tree failed" }
        if ($coreTree -match "centaeris-runtime-sqlite|rusqlite") {
            throw "Core depends on the SQLite adapter"
        }
        $coreSqliteMatches = Get-ChildItem -LiteralPath "packages/core/src" -Recurse -File |
            Select-String -Pattern "centaeris_runtime_sqlite|rusqlite::|use\s+rusqlite|#\[path\s*=.*sqlite"
        if ($coreSqliteMatches) {
            $coreSqliteMatches | ForEach-Object { Write-Host $_ }
            throw "Core contains a SQLite adapter or source include"
        }

        cargo test --locked -p centaeris-core query_loop
        cargo test --locked -p centaeris-runtime-sqlite --test core_runtime
        cargo test --workspace --locked
    }

    if ($Stage -eq "Node") {
        & (Join-Path $PSScriptRoot "desktop-ui-acceptance.ps1") -ValidationOnly
    }

    if ($Stage -eq "Release") {
        & (Join-Path $PSScriptRoot "desktop-ui-acceptance.ps1")
        & (Join-Path $PSScriptRoot "build-tui.ps1")
    }
} finally {
    Pop-Location
}
