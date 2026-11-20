# Release process

The Windows x64 CLI candidate contains the standalone `centaeris-windows-x64.zip`
and its complete corresponding source archive. Hosted Workspace publishes
through its separate release workflow.

Run the local gate from a clean checkout on Windows x64:

```powershell
.\scripts\ci.ps1
```

The CLI archive is written to `packages/cli/dist/centaeris-windows-x64.zip`.
It includes `cet.exe`, System Skills, third-party notices and a manifest.
The `Release Candidate` workflow runs the gate and uploads Windows and source
ZIPs for the exact checkout. The `TUI Release` workflow builds the CLI candidate
independently. Publishing requires an explicit selection, `main` and passing
CI for the same revision. See [versioning](../reference/Versioning.md) and
[the release gate](../eval/ReleaseGate.md).
