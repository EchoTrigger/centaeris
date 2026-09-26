# Building

## Locked toolchains

Use the Rust version in `rust-toolchain.toml`, Python 3.12, Node.js `22.21.0`, and npm `10.9.4`. Commit and use
`Cargo.lock` and `package-lock.json`; do not replace locked installs with
floating dependency resolution in release builds.

```sh
cargo fetch --locked
npm ci
```

## Development targets

```sh
cargo check --workspace --locked
npm run typecheck
npm run dev --workspace centaeris-ui
npm run dev --workspace @centaeris/electron-host
```

## Portable source checks

Run `python scripts/ci.py Source --frontend-tests` on Windows, or
`python3 scripts/ci.py Source --frontend-tests` on macOS/Linux. `Rust` and `Node`
select individual stages. The entry point resolves paths from the repository,
uses the current Python interpreter, and stops at the first failed command.
Windows Rust checks use Git Bash; Unix checks use native tools from PATH.

CI runs the source gates on Windows, Linux and macOS. The additional macOS
Runtime workflow retains native execution and DNS checks, including Intel.
This source matrix does not produce installers or claim desktop packaging support.

## Release targets

```powershell
.\scripts\build-desktop.ps1
.\scripts\build-tui.ps1
```

The Desktop command builds the UI, release Runtime, Electron directory, and
license payload, then checks Runtime freshness and identity. The TUI command
builds `centa.exe`, the matching Runtime, licenses, built-in System Skills, and
the package manifest before creating the Windows archive.

Do not advertise an artifact for a platform that is absent from this build
matrix and its acceptance tests.
