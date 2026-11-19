# Local execution

The native Windows CLI runs the Runtime directly on Windows
and executes host commands through verified Git for Windows Bash. Linux and macOS
run the same Runtime natively with the system Bash. The local Host claims no OS
sandbox: it launches the requested program with the current user's authority and
reports `policyEnforced: false`.

Local file tools use the same current-user authority. `cwd` resolves relative
paths; absolute paths and parent paths can access files outside the working
directory when the OS permits it. The local Host does not apply a workspace
allowlist or a private-path denylist to ordinary file access. Resource-specific
Hosts and authorized-input resolution keep their own access contracts.

## Windows

Git for Windows is required. The Runtime discovers `bash.exe` under
`%ProgramW6432%`, `%ProgramFiles%`, `%ProgramFiles(x86)%`, `%LOCALAPPDATA%`, or
on `PATH`, and verifies the MSYS2 runtime beside it. A missing or unverified Bash fails explicitly.

The Windows CLI manages the Runtime inside `cet.exe` and uses the same executable
as its native Windows execution helper. Build the development binary with
`cargo build --locked -p centaeris-tui --bin cet`. Run the packaged CLI with
`cet.exe --workspace C:\path\to\project`. Without `--workspace`, it uses
the current directory. The Runtime drains owned work when the CLI exits.

## Linux and macOS

The local Host runs the requested program directly and uses the system Bash. On
Unix, commands run in their own process group so timeout, cancellation, and
owned-process teardown can signal the group; a descendant that creates a new
session is outside that group. `macOS Runtime` CI exercises this path on Apple
Silicon and Intel, including public-network DNS.
