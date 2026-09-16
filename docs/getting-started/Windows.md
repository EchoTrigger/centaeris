# Windows setup

Windows x64 clients use a Linux Runtime in WSL2. Local packaging is verified;
the remote release runner still needs WSL2 provisioning.

## Requirements

- Rust `1.95.0` (pinned by `rust-toolchain.toml`)
- Node.js `22.21.0`
- npm `10.9.4`
- Git for source development
- WSL2 Ubuntu 24.04 with systemd, a working user service manager, and the pinned
  Rust toolchain for source builds; see [local execution](../architecture/LocalExecution.md)

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
matching Linux Runtime ELF, license material, and a file manifest verified by the
installer.

After an official GitHub Release exists, `scripts/install-tui.ps1` can install a
named version or `latest`. Do not use the release installer as a substitute for
the source build before release assets exist.

## First run

Choose a Linux working directory and configure a model in the client. For the TUI,
pass `--workspace /home/your-user/project` or its WSL UNC path. Local data is
created in the Linux user's `~/.centaeris`. Windows drive workspaces, missing WSL2,
an incompatible Runtime, or corrupt local state fail explicitly.
