# Building

## Locked toolchains

Use Rust `1.94.1`, Node.js `22.21.0`, and npm `10.9.4`. Commit and use
`Cargo.lock` and `package-lock.json`; do not replace locked installs with
floating dependency resolution in release builds.

```powershell
cargo fetch --locked
npm ci
```

## Development targets

```powershell
cargo check --workspace --locked
npm run typecheck
npm run dev --workspace centaeris-ui
npm run dev --workspace @centaeris/electron-host
```

## Release targets

```powershell
.\scripts\build-desktop.ps1
.\scripts\build-tui.ps1
```

The Desktop command builds the UI, release Runtime, Electron directory, and
license payload, then checks Runtime freshness and identity. The TUI command
builds `centa.exe`, the matching Runtime, licenses, optional System Skills, and
the package manifest before creating the Windows archive.

Do not advertise an artifact for a platform that is absent from this build
matrix and its acceptance tests.
