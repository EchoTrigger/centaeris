# Windows setup

Windows x64 clients run a native Windows Runtime and execute host commands
through Git for Windows Bash. Local packaging is verified; the remote release
runner packages native Windows artifacts.

## Requirements

- Rust `1.95.0` (pinned by `rust-toolchain.toml`)
- Node.js `22.21.0`
- npm `10.9.4`
- Git for source development
- Git for Windows (provides the verified `bash.exe` used for host execution); see
  [local execution](../architecture/LocalExecution.md)

Clone the repository and install locked dependencies:

```powershell
npm ci
cargo fetch --locked
```

## Desktop

Build the complete Desktop directory from the repository root:

```powershell
.\scripts\build-desktop.ps1
```

The executable is written to:

```text
packages/desktop/dist/Centaeris Desktop/Centaeris Desktop.exe
```

This is a directory build, not an installer. Close a running packaged Desktop
before rebuilding it.

For development:

```powershell
npm run dev --workspace centaeris-ui
npm run dev --workspace @centaeris/electron-host
```

## TUI

Build the standalone TUI archive:

```powershell
.\scripts\build-tui.ps1
```

The archive is written to
`packages/tui/dist/centaeris-windows-x64.zip`. It contains `centa.exe`, the
matching native `centaeris-runtime.exe`, license material, and a file manifest
verified by the installer.

After an official GitHub Release exists, `scripts/install-tui.ps1` can install a
named version or `latest`. Do not use the release installer as a substitute for
the source build before release assets exist.

## First run

Choose a working directory and configure a model in the client. For the TUI,
pass `--workspace C:\path\to\project`. Local data is created in the user's data
root. A missing or unverified Git Bash, an incompatible Runtime, or corrupt local
state fails explicitly.
