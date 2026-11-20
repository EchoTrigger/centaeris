# Building

## Locked toolchains

Use the Rust version in `rust-toolchain.toml`, Python 3.12, Node.js `22.21.0`, and pnpm `12.10.1`. Commit and use
`Cargo.lock` and `pnpm-lock.yaml`; do not replace locked installs with
floating dependency resolution in release builds.

```sh
cargo fetch --locked
npm install --global pnpm@12.10.1
pnpm install --frozen-lockfile
```

## Development targets

```sh
cargo check --locked
pnpm run typecheck
cargo run --locked -p centaeris-tui --bin cet -- --workspace .
```

## Portable source checks

Run `python scripts/ci.py Source` on Windows, or
`python3 scripts/ci.py Source` on macOS/Linux. `Rust` and `Node`
select individual stages. The entry point resolves paths from the repository,
uses the current Python interpreter, and stops at the first failed command.
Windows Rust checks use Git Bash; Unix checks use native tools from PATH.

CI runs the source gates on Windows, Linux and macOS. The additional macOS
Runtime workflow retains native execution and DNS checks, including Intel.
This source matrix does not produce installers or claim desktop packaging support.

## Release targets

```powershell
.\scripts\build-tui.ps1
```

The TUI command packages `cet.exe` with its embedded Runtime, licenses, built-in
System Skills and the package manifest.

Do not advertise an artifact for a platform that is absent from this build
matrix and its acceptance tests.

## Hosted components

Hosted gates are selected by `python scripts/workspace/ci.py --stage Rust`, `--stage Web`, `--stage Contracts`, `--stage RuntimePostgres`, `--stage ApiPostgres` and `--stage Support`. See [shared source boundaries](SourceUnification.md). Local builds use the root Cargo default members; full hosted validation additionally uses a disposable PostgreSQL endpoint.
