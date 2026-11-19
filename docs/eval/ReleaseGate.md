# Release gate

A public release requires the full local gate below to pass from a clean clone:

```powershell
.\scripts\ci.ps1
```

Windows CLI build and acceptance run natively on Windows x64 and require
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
cargo test --locked -p centaeris-cli-runtime message_log::observation_cas::tests::observation_manifest_growth_is_linear_with_early_changes_through_4095_observations -- --ignored --exact --nocapture --test-threads=1
```

The checked-in CI workflow runs the Rust and Node source gates. The manual
Windows x64 Release Candidate workflow packages the CLI and exact source ZIPs
after running the local gate and the storage-growth test. Publication uses a
separate manual release entry.

The checked-in `Performance` workflow runs the observation storage-growth gate
for relevant pull requests and `main` changes, and also supports an explicit
manual run.

The portable source gate checks formatting, compilation, Clippy, Core and
SQLite contracts, the local Runtime, and the shared frontend packages. The
Windows release gate additionally builds the TUI archive.

The portable Rust gate also runs the headless benchmark client's standard-library
behavior tests without external model requests. The Linux mock-model acceptance
and Harbor adapter checks are described in [the adapter guide](../../scripts/harbor/README.md).

Runtime process, terminal and scheduler coverage is in the
`centaeris-cli-runtime` test targets. The CLI integration target
`local_execution_sandbox` checks native execution with the shipped entry point.

Persistent-format changes must also pass [persistence acceptance](PersistenceAcceptance.md).
These Rust tests run in the existing workspace test gate. Product metadata and
lockfiles must pass `python -B scripts/test_product_version.py`.

The repository must also pass these structural checks:

- every workspace member is below `packages/`;
- no source `#[path]` includes another crate;
- no private plugin, credential, customer data, private deployment configuration, or unrelated third-party research snapshot is tracked; hosted control-plane source stays in its owned packages, and built-in Skills remain in the separate reviewed `system-skills/` and `skills/system/` bundles;
- the public built-in System Skills are present in the source archive and each CLI distribution, with applicable upstream licenses, attribution, and modification notices;
- root ignore rules exclude test results, browser artifacts, logs, local environment files, and unrelated binary documents before source freeze; the Git index is inspected separately because ignore rules do not remove tracked files;
- Core treats `ExecutionHost` file identities as opaque and does not classify Host-private namespaces;
- an empty package catalog builds and starts;
- public package versions are `0.1.0`, and public schema/version constants remain internally consistent with the code navigation in `API.md`;
- the root license, first-party Rust/npm package metadata, README, and contribution policy consistently identify `AGPL-3.0-only`; third-party and brand-asset exceptions remain explicit;
- every distributed binary or application is accompanied by the AGPL license and a clear path to the complete corresponding source for the exact released revision; generated source archives are verified from the release candidate rather than assumed from a branch tip;
- the README, contribution guide, and issue/PR templates consistently describe the contribution workflow and DCO provenance certification; external PR creation is open, and `main` requires pull requests, the `Required gates` check, resolved review discussions, and linear history, with force pushes and deletion blocked;
- public references describe the current code-owned contracts and verified behavior; documentation-only proposals do not become release requirements;
- the Local Runtime tests cover JSONL framing, initialize descriptor validation, pre-initialize broadcast isolation, Runtime-owned leases across client detach/reconnect, cancellation acceptance versus terminal facts, bounded service shutdown, conservative crash recovery, replay and in-process shutdown;
- the existing Session tests cover current manifest, event, projection, terminal, and live-text behavior without claiming unimplemented wire restrictions;
- the public repository contains a checked-in CI workflow that runs the documented gates on the supported runner and makes no release-platform claim beyond its tested build matrix;
- installers request only artifacts produced by that matrix; the current Unix installer must not advertise absent macOS or Linux assets;
- release artifacts are produced and tested by an explicit build matrix for every advertised platform; the current repository only produces Windows x64 CLI artifacts;
- `Cargo.lock` and `pnpm-lock.yaml` resolve only this repository's workspaces.

The gate's Core/storage boundary checks protect dependency direction rather than preserving a migration blacklist: Core owns contracts and private semantics, while `runtime_sqlite` owns its implementation and the public Core-plus-SQLite integration coverage.

Hosted components additionally follow [their release gate](../workspace/eval/ReleaseGate.md). The shared CI returns the Required gates result for every run; product jobs are selected by scripts/ci_scope.py. A clean checkout must build local and hosted products independently. See [source unification](../development/SourceUnification.md).
