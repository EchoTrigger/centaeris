# Release gate

A public release requires the full local gate below to pass from a clean clone:

```powershell
.\scripts\ci.ps1
```

Windows Desktop/TUI build and acceptance now require a configured WSL2
Ubuntu 24.04 environment, a working systemd user session and the pinned Rust
toolchain inside WSL. See [local execution setup](../architecture/LocalExecution.md).
The checked-in remote Release Candidate runner has not yet been provisioned
or revalidated for that change; its previous Windows-only release evidence
does not certify the new Desktop artifact.

The `macOS Runtime` workflow tests nono execution on Apple Silicon and Intel.
The first run found a private Unix-socket access gap on both architectures.
The fix passed execution and DNS tests on both architectures; corrections to
the subsequent capability and SSE regression tests passed in run `34966224952`
on both architectures. Adding the workflow does not establish a
supported macOS release platform. It adds no browser CI suite.

The 4,095-observation storage-growth stress test is intentionally excluded from
the normal test suite. Run it once for a release candidate, or when the Runtime
message-log implementation changes:

```powershell
cargo test --locked -p centaeris-runtime message_log::observation_cas::tests::observation_manifest_growth_is_linear_with_early_changes_through_4095_observations -- --ignored --exact --nocapture --test-threads=1
```

The checked-in `CI` workflow runs the Rust gate and the Node source-validation
portion in parallel for pull requests and `main` changes. GitHub Actions does
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

The full local script runs formatting, workspace checks, Clippy with warnings
denied, the focused Core query-loop and SQLite integration gates, the full Rust
workspace tests, the Windows x64 TUI package build, and the desktop/UI acceptance
script. Local Desktop acceptance performs `npm ci`, the UI gate (typecheck, Vite
build, and Vitest), retained Electron host/security tests and build,
third-party-license assembly and distribution validation, plus runtime and window
smoke tests, plus Desktop/TUI coexistence, persistence and sandboxed sidecar
acceptance in WSL2. Playwright/E2E and visual-snapshot tests are not part of the gate;
visual and interaction acceptance is manual.
Use [FrontendManualAcceptance.md](FrontendManualAcceptance.md) for the retained
Desktop interaction and appearance checks.

The repository must also pass these structural checks:

- every workspace member is below `packages/`;
- no source `#[path]` includes another crate;
- no hosted control-plane source, commercial package, concrete Skill, credential, customer data, private deployment configuration, or third-party research snapshot is tracked;
- root ignore rules exclude test results, browser artifacts, logs, local environment files, and unrelated binary documents before source freeze; the Git index is inspected separately because ignore rules do not remove tracked files;
- Core treats `ExecutionHost` file identities as opaque and does not classify Host-private namespaces;
- an empty package catalog builds and starts;
- public package versions are `1.0.0`, and public schema/version constants remain internally consistent with the code navigation in `API.md`;
- the root license, first-party Rust/npm package metadata, README, and contribution policy consistently identify `AGPL-3.0-only`; third-party and brand-asset exceptions remain explicit;
- every distributed binary or application is accompanied by the AGPL license and a clear path to the complete corresponding source for the exact released revision; generated source archives are verified from the release candidate rather than assumed from a branch tip;
- the README, contribution guide, and issue template consistently describe the temporary restriction on external works; pull request creation is limited to collaborators, and any future reopening of external contributions requires the published contributor agreement and explicit contributor acceptance described in the contribution guide;
- public references describe the current code-owned contracts and verified behavior; documentation-only proposals do not become release requirements;
- the existing Local Runtime tests cover JSONL framing, initialize descriptor validation, Windows pipe DACL ownership, pre-initialize broadcast isolation, connection ownership cleanup, lease transfer or interruption, replay, and idle shutdown behavior used by the bundled Desktop and TUI clients;
- the existing Session tests cover current manifest, event, projection, terminal, and live-text behavior without claiming unimplemented wire restrictions;
- the public repository contains a checked-in CI workflow that runs the documented gates on the supported runner and makes no release-platform claim beyond its tested build matrix;
- installers request only artifacts produced by that matrix; the current Unix installer must not advertise absent macOS or Linux assets;
- release artifacts are produced and tested by an explicit build matrix for every advertised platform; the current repository only produces Windows x64 TUI and desktop artifacts;
- `Cargo.lock` and `package-lock.json` resolve only this repository's workspaces.

The gate's Core/storage boundary checks protect dependency direction rather than preserving a migration blacklist: Core owns contracts and private semantics, while `runtime_sqlite` owns its implementation and the public Core-plus-SQLite integration coverage.
