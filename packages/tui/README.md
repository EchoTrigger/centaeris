# Centaeris TUI

`packages/tui` is the terminal Local Host. The Rust crate `centaeris-tui`
builds the `centa` binary, a Ratatui-based terminal projection and input
handler over the same Local Runtime Host protocol Desktop uses
(`centaeris-runtime`, via `packages/runtime`). The package also carries the
Node distribution layer for the TUI: `centa.mjs` (entry point under the
`@centaeris/tui-local` package name) and the built `dist/` output used by the
Windows TUI archive.

Like Desktop, TUI does not implement a second Session/tool loop; it renders
the same canonical Runtime events Desktop does, differing only in layout.

See [Architecture](../../docs/architecture/Architecture.md) and
[Windows setup](../../docs/getting-started/Windows.md) for the build and
first-run paths.

## Build

```powershell
.\scripts\build-tui.ps1
```

Builds `centa.exe`, the matching Runtime, licenses, built-in System Skills,
and the package manifest before creating the Windows archive.
