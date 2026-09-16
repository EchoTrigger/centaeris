# Local execution

Windows Desktop and the native Windows TUI use WSL2. The local acceptance environment is Ubuntu 24.04,
Landlock ABI 3 and cgroup v2 with `cgroup.kill`. The distribution must run systemd
and provide a working `systemctl --user` session. Runtime startup creates a
profile-specific transient user service with delegation; no model tool can
change the service or grant policy.

`CENTAERIS_WSL_DISTRIBUTION` selects the distribution (default `Ubuntu-24.04`).
`CENTAERIS_WSL_DATA_DIR` optionally selects an absolute Linux data directory.
The default data directory is the Linux user's `~/.centaeris`, outside the
workspace. There is no reserved workspace `.centaeris` directory. Existing
Windows profiles and arbitrary project files are not automatically moved.

The Desktop default workspace is `~/.local/share/centaeris/workspaces/default` inside
WSL. Windows folder dialogs can select Linux folders through
`\\wsl.localhost\<distribution>\...`. Workspace and executable resource grants
must be on the Linux filesystem: nono rejects Windows drive mounts. Windows
image and plugin sources can be imported through `/mnt/<drive>/...`; these
import paths do not become writable execution roots. Skill source directories
must live on the Linux filesystem.

The packaged Linux binary is installed under
`~/.local/share/centaeris/bin/<sha256>/centaeris-runtime`. The relay is one
persistent connection to the existing Runtime Unix socket. No Python bridge,
nono CLI profile, permission broker or additional Core protocol is introduced.

For development, install the repository's pinned Rust toolchain in the selected
distribution, plus its native C build tools. On Windows:

```powershell
node packages/desktop/scripts/ensure-runtime.mjs --profile debug
$env:CENTAERIS_RUNTIME_EXE = "$PWD/target/wsl/debug/centaeris-runtime"
node packages/desktop/scripts/smoke-wsl-runtime.mjs
```

The smoke test uses a fresh temporary Linux profile and workspace. It exercises
connection/build identity, reconnect, sidecar isolation and stop, then kills
only its own Runtime service to check descendant cleanup.

`npm run smoke:runtime --workspace @centaeris/electron-host` builds and tests
the default release WSL artifact. `npm run smoke:tui-wsl --workspace
@centaeris/electron-host` verifies both clients against one Linux profile.

`scripts/build-desktop.ps1` builds the release ELF and bundles it with the
Windows Electron application and Linux dependency licenses. Packaged window
acceptance also uses WSL2. The remote Release Candidate runner has not yet been
provisioned or certified for these new prerequisites; local acceptance does
not establish remote release readiness.

The macOS Runtime regression workflow passed on Apple Silicon and Intel at
`5be7d90` (run `34966224952`), including nono execution and public-network DNS.
macOS detached-process lifetime containment remains separate follow-up work.

## Native Windows TUI

The renderer, keyboard handling and Windows clipboard stay native. The Rust
client invokes `wsl.exe` directly, with no Node or Python runtime dependency.
Desktop and TUI consume `packages/runtime/host/wsl-bootstrap.sh` for installation,
service startup and the existing Runtime stdio relay. They share request-path
metadata and path fixtures; Core execution semantics remain unchanged.

Start the packaged TUI with a Linux workspace:

```powershell
.\packages\tui\dist\centaeris\centa.exe --workspace /home/your-user/project
```

The selected distribution's `\\wsl.localhost\<distribution>\...` paths also
work. Without `--workspace`, the current directory must resolve to that Linux
filesystem. Windows drive workspaces fail explicitly. Client disconnection
releases only that connection; a Runtime used by Desktop keeps running.

The package contains native `centa.exe`, Linux `centaeris-runtime`, the shared
bootstrap embedded in the TUI, and combined Windows-TUI/Linux-Runtime licenses.
The old Windows Git Bash launcher, Job Object backend and unsandboxed sidecar
fallback have been removed. Direct native Windows Runtime startup fails with a
WSL2-required error. An inert Windows execution facade keeps host-independent
Rust tests buildable; every execution and filesystem operation returns unavailable.

Local acceptance covered both clients sharing one profile, TUI disconnect and
session persistence, Unicode/space paths, sidecar private-file denial and detached
child cleanup, packaged TUI startup/exit, and packaged Desktop window/idle shutdown.
The retained protocol and mapping tests cover build identity, relay framing and
invalid paths. This is not a claim that the remote Windows release runner has WSL2.
