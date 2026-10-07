# Release gate

A public release requires the full local gate below to pass from a clean clone:

```powershell
.\scripts\ci.ps1
```

Windows Desktop/TUI build and acceptance run natively on Windows x64 and require
Git for Windows on `PATH` or at a standard install location. See
[local execution setup](../architecture/LocalExecution.md). The checked-in remote
Release Candidate runner packages native Windows artifacts.

The `macOS Runtime` workflow tests native local execution on Apple Silicon and
Intel. Adding the workflow does not establish a supported macOS release
platform. It adds no browser CI suite.

The 4,095-observation storage-growth stress test is intentionally excluded from
the normal test suite. Run it once for a release candidate, or when the Runtime
message-log implementation changes:

```powershell
cargo test --locked -p centaeris-runtime message_log::observation_cas::tests::observation_manifest_growth_is_linear_with_early_changes_through_4095_observations -- --ignored --exact --nocapture --test-threads=1
```

The checked-in `CI` workflow runs `scripts/ci.py` Rust and Node source gates
on Windows, Linux and macOS for pull requests and `main` changes. GitHub Actions does
not execute frontend tests: the Node job runs source lint, production
typechecking/build, Electron syntax/host-parity checks, and license assembly.
It also omits release packaging and packaged-application smoke tests so routine
changes do not rebuild the same release artifacts.

The manual `Release Candidate` workflow runs the full gate and the observation
storage-growth gate from a clean Windows x64 checkout, packages the Desktop and
TUI distributions, creates a source archive from the exact checked-out commit,
and uploads those three ZIP files as a temporary workflow artifact. It does not
create a tag or a GitHub Release. GitHub records the uploaded artifact digest;
the workflow does not create an additional checksum file with no consumer.

The checked-in `Performance` workflow runs the observation storage-growth gate
for relevant pull requests and `main` changes, and also supports an explicit
manual run.

The portable source gate runs formatting, workspace checks, Clippy with warnings
denied, the focused Core query-loop and SQLite integration gates, the full local-product Rust
package tests and UI/Electron source checks. The Windows `ci.ps1 Release`
entry additionally runs TUI packaging and desktop/UI acceptance. Local Desktop acceptance performs `npm ci`, the UI gate (typecheck, Vite
build, and Vitest), retained Electron host/security tests and build,
third-party-license assembly and distribution validation, plus runtime and window
smoke tests, plus Desktop/TUI coexistence, persistence and sidecar
acceptance on Windows x64. Playwright/E2E and visual-snapshot tests are not part of the gate;
visual and interaction acceptance is manual.
The Node and release stages also run the bundled System Skill Python helper
behavior checks.
Use [FrontendManualAcceptance.md](FrontendManualAcceptance.md) for the retained
Desktop interaction and appearance checks.

The portable Rust gate also runs the headless benchmark client's standard-library
behavior tests without external model requests. The Linux mock-model acceptance
and Harbor adapter checks are described in [the adapter guide](../../scripts/harbor/README.md).

Changes to Runtime-owned process sessions also run the native, isolated-profile
transport acceptance after `cargo build --locked -p centaeris-runtime --bin
centaeris-runtime`: `node scripts/test-process-sessions.mjs`. It covers two-client
output cursors, duplicate start, Session deletion/start races, disconnect survival,
scoped stop and explicit service-shutdown tree cleanup. It requires the existing
local Bash setup and makes no model requests.

Agent process adapter/completion changes also run `node scripts/test-process-agent.mjs`
against the debug Runtime. It uses an isolated profile and a loopback mock model,
covering automatic follow-up after client detach, busy-session deferral, output
readback, lost-acknowledgement deduplication and cancellation suppression. It sends
no model requests to external services.

Persistent-format changes must also pass [persistence acceptance](PersistenceAcceptance.md).
These Rust tests run in the existing workspace test gate. Product metadata and
lockfiles must pass `python -B scripts/test_product_version.py`.

The repository must also pass these structural checks:

- every workspace member is below `packages/`;
- no source `#[path]` includes another crate;
- no private plugin, credential, customer data, private deployment configuration, or unrelated third-party research snapshot is tracked; hosted control-plane source stays in its owned packages, and built-in Skills remain in the separate reviewed `system-skills/` and `skills/system/` bundles;
- the public built-in System Skills are present in the source archive and each Desktop/TUI distribution, with applicable upstream licenses, attribution, and modification notices;
- root ignore rules exclude test results, browser artifacts, logs, local environment files, and unrelated binary documents before source freeze; the Git index is inspected separately because ignore rules do not remove tracked files;
- Core treats `ExecutionHost` file identities as opaque and does not classify Host-private namespaces;
- an empty package catalog builds and starts;
- public package versions are `0.1.0`, and public schema/version constants remain internally consistent with the code navigation in `API.md`;
- the root license, first-party Rust/npm package metadata, README, and contribution policy consistently identify `AGPL-3.0-only`; third-party and brand-asset exceptions remain explicit;
- every distributed binary or application is accompanied by the AGPL license and a clear path to the complete corresponding source for the exact released revision; generated source archives are verified from the release candidate rather than assumed from a branch tip;
- the README, contribution guide, and issue template consistently describe the temporary restriction on external works; pull request creation is limited to collaborators, and any future reopening of external contributions requires the published contributor agreement and explicit contributor acceptance described in the contribution guide;
- public references describe the current code-owned contracts and verified behavior; documentation-only proposals do not become release requirements;
- the Local Runtime tests cover JSONL framing, initialize descriptor validation, Windows pipe DACL ownership, pre-initialize broadcast isolation, Runtime-owned leases across client detach/reconnect, cancellation acceptance versus terminal facts, bounded service shutdown, conservative crash recovery, replay, and idle shutdown behavior used by the bundled Desktop and TUI clients;
- the existing Session tests cover current manifest, event, projection, terminal, and live-text behavior without claiming unimplemented wire restrictions;
- the public repository contains a checked-in CI workflow that runs the documented gates on the supported runner and makes no release-platform claim beyond its tested build matrix;
- installers request only artifacts produced by that matrix; the current Unix installer must not advertise absent macOS or Linux assets;
- release artifacts are produced and tested by an explicit build matrix for every advertised platform; the current repository only produces Windows x64 TUI and desktop artifacts;
- `Cargo.lock` and `package-lock.json` resolve only this repository's workspaces.

The gate's Core/storage boundary checks protect dependency direction rather than preserving a migration blacklist: Core owns contracts and private semantics, while `runtime_sqlite` owns its implementation and the public Core-plus-SQLite integration coverage.

Local SSH/scheduler changes additionally run `node scripts/test-host-automation.mjs`
after building the debug Runtime. Its isolated profile, loopback model and SSH
argv fixture cover concurrent clients, explicit model selection, one-shot/manual
execution, overlap, client detach, lost-acknowledgement restart, missed intervals
and process-start replay. It does not connect to a real remote SSH host.

Agent scheduling changes also run `node scripts/test-agent-schedules.mjs`. A
loopback model exercises context lookup, plan creation, service enablement and
confirmation through actual Agent tool calls, without user CLI/config-file work.

Local PTY changes additionally run `node scripts/test-terminal-sessions.mjs`
after building the debug Runtime. This isolated-profile acceptance makes no model
requests; see [LocalTerminals.md](../reference/LocalTerminals.md).

Hosted components additionally follow [their release gate](../workspace/eval/ReleaseGate.md). The shared CI returns the Required gates result for every run; product jobs are selected by scripts/ci_scope.py. A clean checkout must build local and hosted products independently. See [source unification](../development/SourceUnification.md).
