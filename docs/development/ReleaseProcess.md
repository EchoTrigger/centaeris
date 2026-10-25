# Release process

A release is a separate operation from finishing code or creating a local Git
checkpoint.

The first public release is planned as **Centaeris v0.1.0 - Windows Preview**:
Windows x64 Desktop and TUI from one source revision, published together as a
GitHub pre-release under `v0.1.0`. Its assets are the Desktop setup EXE, portable
Desktop ZIP, TUI ZIP and complete corresponding source ZIP. Web and hosted
product acceptance are handled separately. Current product-specific publishing
entries must be reconciled with this unified release before publication.

## 1. Freeze source

- identify the exact candidate revision;
- require a clean, self-contained tree;
- verify package and protocol versions;
- check that no credentials, customer data, private extension content, test
  results, or unsupported artifacts are tracked.

## 2. Verify

Run `scripts/ci.ps1` from a clean clone. Review the structural requirements in
`docs/eval/ReleaseGate.md`. Build only the Windows x64 targets currently in the
release matrix.

## 3. Inspect artifacts

Verify the Desktop directory and TUI archive contain the exact Runtime built
from the candidate source, the project license, third-party notices, dependency
licenses, and consumed package manifests. Smoke the packaged applications.

Desktop's primary Windows x64 download is an assisted, per-user NSIS EXE
installer built with pinned electron-builder from the existing assembled
application directory. The portable Desktop ZIP, TUI ZIP, and exact source ZIP
remain available. The installer creates a Start menu entry, allows choosing an
application directory, and supports replacement installs and system uninstall.
It preserves the existing user data root (normally `%USERPROFILE%\.centaeris`)
and workspace files. Keep `org.centaeris.desktop` stable as the installation ID.

Build the installer after the normal Desktop directory build:

```powershell
$candidateOutput = Join-Path $env:TEMP "centaeris-installers"
node packages/desktop/scripts/build-installer.mjs --output $candidateOutput
node packages/desktop/scripts/smoke-installer.mjs (Join-Path $candidateOutput "Centaeris-Desktop-0.1.0-windows-x64-setup.exe")
```

The output directory can be chosen explicitly; the default is the operating
system temporary directory. Use `--app-dir <directory>` to package another
assembled candidate. To assemble without replacing a running preview, set
`CENTAERIS_DESKTOP_DIST_DIR` to a fresh absolute directory before running
`packages/desktop/scripts/build-dist.mjs`. Existing custom output directories
are rejected rather than overwritten.

Installation and uninstallation wait briefly for Desktop and the bundled
Runtime to leave the target and previous installation directories. Runtime's
existing idle shutdown owns task/client checks. If it remains in use, setup
stops before replacing files; the installer does not stop tasks or kill clients.
Close Desktop using Exit in its tray menu, finish tasks, and close TUI clients, then retry.

Installer acceptance uses an isolated temporary application/data directory and
requires that no Desktop installation is already registered. It checks Unicode
paths, per-user registration, product version, occupied Runtime protection,
replacement, the installed window's session/config persistence, and user data
retention across uninstall/reinstall. It removes its test installation afterward.
Run additional clean-machine acceptance before public release, including a
standard Windows user and the declared external prerequisites (Git for Windows).

Local candidates may be unsigned. Public signing must follow PE icon/version
customization and cover the app, Runtime and installer; do not modify signed
files. Automatic updates are not enabled by this installer.


Checksums or digests are generated only where an installer, manifest validator,
container runtime, or release platform consumes them.

## 4. Publish

Tagging, pushing, changing a remote default branch, creating a GitHub Release,
and uploading assets require explicit authorization. Release notes must state
the verified platform and must not include private repository history,
deployment details, or unsupported performance claims.
