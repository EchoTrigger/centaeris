# Local execution

Windows Desktop and the native Windows TUI run the Runtime directly on Windows
and execute host commands through verified Git for Windows Bash. Linux and macOS
run the same Runtime natively with the system Bash. The local Host claims no OS
sandbox: it launches the requested program with the current user's authority and
reports `policyEnforced: false`.

## Windows

Git for Windows is required. The Runtime discovers `bash.exe` under
`%ProgramW6432%`, `%ProgramFiles%`, `%ProgramFiles(x86)%`, `%LOCALAPPDATA%`, or
on `PATH`, and verifies the MSYS2 runtime beside it. An explicit path can be
selected through `CENTAERIS_RUNTIME_EXE` for the Runtime binary and the host
configuration for Bash. A missing or unverified Bash fails explicitly.

The default development Runtime is `target/debug/centaeris-runtime.exe`
(Windows) or `target/debug/centaeris-runtime` (Linux/macOS). Set
`CENTAERIS_RUNTIME_EXE` to select an explicit build.

```powershell
node packages/desktop/scripts/ensure-runtime.mjs --profile debug
node packages/desktop/scripts/smoke-runtime.mjs
```

`npm run smoke:runtime --workspace @centaeris/electron-host` builds and tests the
default release Runtime. `scripts/build-desktop.ps1` bundles the native
`centaeris-runtime.exe` with the Windows Electron application; packaged window
acceptance runs the same native Runtime.

## Native Windows TUI

The renderer, keyboard handling, Windows clipboard, and Runtime client stay
native. The Rust client spawns `centaeris-runtime.exe` and connects to the
profile-scoped named-pipe Runtime Server; there is no Node, Python, or WSL
dependency.

Start the packaged TUI:

```powershell
.\packages\tui\dist\centaeris\centa.exe --workspace C:\path\to\project
```

Without `--workspace`, the current directory is used. Client disconnection
releases only that connection; active runs keep the Runtime alive even without
a Desktop or TUI observer. Reconnecting clients read the existing Session and
AgentRun rather than resubmitting work. The TUI
package contains native `centa.exe`, native `centaeris-runtime.exe`, and combined
third-party licenses.

## Linux and macOS

The local Host runs the requested program directly and uses the system Bash. On
Unix, commands run in their own process group so timeout, cancellation, and
owned-process teardown can signal the group; a descendant that creates a new
session is outside that group. `macOS Runtime` CI exercises this path on Apple
Silicon and Intel, including public-network DNS.
