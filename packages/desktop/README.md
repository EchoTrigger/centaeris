# Electron Host

Electron is the local Desktop Host. The React renderer receives only the
controlled `window.centaerisHost` bridge; it does not receive `ipcRenderer`,
Node.js, or unrestricted Electron primitives.

## Boundary

Electron main/preload owns windows, tray behavior, native directory and reveal
actions, and the renderer bridge. `packages/runtime` owns the shared Local
Runtime process and exposes the exact Host protocol over JSON-RPC 2.0/JSONL.
Desktop and TUI use the same Runtime for one user data root.

Session, prompt, tool, Plugin, Skill, continuation, and runtime-job semantics
belong to Core. Electron does not implement a second loop, parse extension
contracts, process Office documents, or provide a hosted-execution fallback.

The Windows Local ExecutionHost uses verified Git for Windows Bash with the
current user's authority. It does not claim an OS sandbox, request UAC, create
accounts, or modify ACL, WFP, or host-network policy. The Runtime keeps its
small destructive-root circuit breakers; Electron does not add a second
approval state machine.

`Use default directory` creates or reuses a dated directory below the user home
and activates it through the same Runtime command as a custom path. Electron
does not create a workspace snapshot or managed dependency environment.

## Development

Desktop requires Node/npm and Rust/Cargo. It does not call Python, read a root
virtual environment, or bundle hosted-product services.

```powershell
pnpm install --frozen-lockfile
node packages/desktop/scripts/ensure-runtime.mjs --profile debug
pnpm --filter centaeris-ui run dev
pnpm --filter @centaeris/electron-host run dev
```

The default development Runtime is `target/debug/centaeris-runtime.exe` on Windows.
`CENTAERIS_RUNTIME_EXE` may select an explicit build. Missing or incompatible
Runtime binaries fail.

## Build and gates

Build the complete Desktop directory from the repository root:

```powershell
.\scripts\build-desktop.ps1
```

Focused checks:

```powershell
pnpm --filter @centaeris/electron-host run check
pnpm --filter @centaeris/electron-host run check:host-parity
pnpm --filter @centaeris/electron-host run smoke:runtime
pnpm --filter @centaeris/electron-host run smoke:window
cargo test --locked -p centaeris-runtime
```

Host parity compares renderer `invoke/listen`, the Electron contract, and
Runtime Host commands. Unknown commands, missing handlers, missing credentials,
and unavailable Bash fail explicitly.
