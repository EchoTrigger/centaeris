# Windows setup

Windows x64 is the only currently verified release platform.

## Requirements

- Rust `1.94.1`
- Node.js `22.21.0`
- npm `10.9.4`
- Git for Windows, including Git Bash

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
matching Local Runtime, license material, and a file manifest verified by the
installer.

After an official GitHub Release exists, `scripts/install-tui.ps1` can install a
named version or `latest`. Do not use the release installer as a substitute for
the source build before release assets exist.

## First run

Choose a real working directory and configure a model in the client. Local data
is created below `~/.centaeris`. Missing Git Bash, an incompatible Runtime, an
invalid model credential, or corrupt local state fails explicitly.
