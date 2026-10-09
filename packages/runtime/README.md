# Centaeris Runtime

`packages/runtime` (`centaeris-runtime`) is the shared Local Runtime process.
It composes Core, the SQLite storage adapter, model providers, local process
and terminal management, and exposes the exact Local Runtime Host protocol
(JSON-RPC 2.0 over JSONL) consumed by Desktop and TUI.

Desktop and TUI are both thin hosts over this one Runtime process and one user
data root (`~/.centaeris` by default); they do not implement a second Session
or tool loop. Runtime owns `ExecutionHost` binding, model admission wiring,
Plugin/Skill/MCP/Hook activation freezing, local process and schedule
lifecycle, the session catalog, and the Windows Local `ExecutionHost` (Git for
Windows Bash) and native Linux/macOS execution paths.

Binaries:

- `centaeris-runtime` — the Local Runtime Host process (`default-run`).
- `centaeris-package-catalog` — a package/Skill catalog utility binary.

See [Architecture](../../docs/architecture/Architecture.md),
[Runtime protocol](../../docs/reference/RuntimeProtocol.md), and the
[ExecutionHost contract](../../docs/architecture/ExecutionHostContract.md) for
the complete Host boundary.

## Focused gate

```powershell
cargo test --locked -p centaeris-runtime
```
