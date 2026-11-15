# Centaeris CLI

`packages/cli` is the terminal Local Host. The Rust crate `centaeris-tui`
builds the `cet` binary, a Ratatui-based terminal projection and input
handler over the same Local Runtime Host protocol Desktop uses
(`centaeris-runtime`, via `packages/desktop/src/services`). The package also carries the
Node distribution layer for the TUI: `cet.mjs` (entry point under the
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

Builds `cet.exe`, the matching Runtime, licenses, built-in System Skills,
and the package manifest before creating the Windows archive.

## Interactive and headless modes

`cet` and `cet --mode tui` open the existing terminal interface.
Use `--workspace <directory>` to select an existing TUI working directory;
otherwise the current directory is used. `cet --help` and `cet --version`
return locally without opening a terminal interface or connecting to the Runtime.
The other modes use the same Agent protocol, Sessions and Run lifecycle.

```sh
cet --mode print --cwd . "Explain this project"
cet --mode json --session session-id "Continue the analysis"
cet --mode rpc
```

Print waits for a committed Run terminal event, reads the durable Run and
assistant projections, and writes the complete final assistant text.
JSON emits an `accepted` record, canonical `session/update` notifications,
and a `completed` record containing the durable Run and `outputText`.
Transport and request errors emit an `error` record.

`--session` continues a Session using its existing working directory.
`--cwd` selects the working directory for a new Session in print or JSON mode.
These options are mutually exclusive; `--workspace` belongs to TUI mode.
Use `--` before prompt text that begins with a dash.

Ctrl-C requests cancellation and waits for the durable outcome. Exit codes
are 0 for success, 130 for cancellation, and 1 for failure, a stopped Run,
or a CLI error.

RPC forwards JSON-RPC requests and asynchronous notifications over standard
input/output through the Agent's shared profile service. Clients initialize
the connection, correlate request IDs and wait for committed Run outcomes.
The existing `schedule` and `ssh` commands use their established entry points.
