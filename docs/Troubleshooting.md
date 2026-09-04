# Troubleshooting

## Git Bash is unavailable

Install or repair Git for Windows and confirm its Bash executable is available.
The Windows Local Runtime does not fall back to WSL, PowerShell, or `cmd` for
model shell execution.

## Desktop build says the application is running

Close the packaged `Centaeris Desktop.exe` from the same output directory and
run `scripts/build-desktop.ps1` again.

## Runtime is missing or stale

Run the complete Desktop or TUI build from the repository root. Development may
set `CENTAERIS_RUNTIME_EXE` to an explicit compatible Runtime executable. A
protocol mismatch must be fixed by rebuilding the matching source, not by
disabling validation.

## System Skills are missing

A source build may use an empty System Skills directory. To bundle System
Skills, set `CENTAERIS_SYSTEM_SKILLS_SOURCE` to a validated bundle before the
Desktop or TUI build. Preserve any upstream LICENSE and NOTICE files.

## A Plugin cannot be activated

Inspect the package error for a missing resource, stale digest, duplicate
identity, invalid credential reference, unsupported contribution, or live MCP
contract mismatch. Activation errors are isolated to the affected package and
are not repaired by weakening the public schema.

## Local state is corrupt

Do not delete `~/.centaeris` as a first troubleshooting step. Preserve a copy,
identify the failing file or database contract, and use the explicit recovery
path exposed by the Host. Removing the data root deletes local Sessions,
configuration, credentials, Plugins, and Skills.
