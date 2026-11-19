# Windows setup

Windows x64 clients run a native Windows Runtime and execute host commands
through Git for Windows Bash. Local packaging is verified; the remote release
runner packages native Windows artifacts.

## Install a release candidate

The first public release is planned as **v0.1.0 Windows Preview**, for Windows
x64 (AMD64). Candidate artifacts may exist before a GitHub Release is published.
Download only the artifacts attached to the intended release.

Install Git for Windows before running local commands. The packaged clients
include their Runtime and System Skills; building Rust, Node.js or pnpm from
source is not part of end-user installation.

Extract `centaeris-windows-x64.zip` and run:

```powershell
.\cet.exe --workspace C:\path\to\project
```

The first release is a pre-release: `scripts/install-tui.ps1 -Release v0.1.0`
can select it explicitly once published; the default `latest` selects stable
releases only. The CLI defaults to the same user data root,
`%USERPROFILE%\.centaeris`. The TUI runtime lives inside `cet.exe`. Multiple
CLI windows can share the directory with one writer per session. The raw `rpc` mode needs exclusive access to their data directory.

## Build from source

### Requirements

- Rust `1.95.0` (pinned by `rust-toolchain.toml`)
- Node.js `22.21.0`
- pnpm `12.10.1`
- Git for source development
- Git for Windows (provides the verified `bash.exe` used for host execution); see
  [local execution](../architecture/LocalExecution.md)

Clone the repository and install locked dependencies:

```powershell
npm install --global pnpm@12.10.1
pnpm install --frozen-lockfile
cargo fetch --locked
```

### TUI source build

Build the standalone TUI archive:

```powershell
.\scripts\build-tui.ps1
```

The archive is written to
`packages/cli/dist/centaeris-windows-x64.zip`. It contains `cet.exe`,
license material, and a file manifest
verified by the installer.

After an official GitHub Release exists, `scripts/install-tui.ps1` can install a
named version or `latest`. Do not use the release installer as a substitute for
the source build before release assets exist.

## First run

Choose a working directory and configure a model with `/model`. To select the directory,
pass `--workspace C:\path\to\project`. Local data is created in the user's data
root. A missing or unverified Git Bash or corrupt local
state fails explicitly.
