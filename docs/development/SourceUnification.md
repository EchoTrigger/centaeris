# Shared source and product boundaries

The monorepo retains flat top-level packages. ui and web remain separate;
system-skills and skills/system retain their existing content and responsibilities.
packages/core is the source of truth for runtime semantics. api owns hosted
control-plane facts; runtime_server and hosted_execution retain their adapters.

## Provenance and history

Core baseline: 17797776852fcaffb2ef3b444ff4b7bad85f990f.
Workspace snapshot: 81e27cffe1fa29ea1e923923139fb6445f52d808.
[The manifest](../workspace/source-snapshot/manifest.json) records source repositories,
complete SHAs, original Git blob identities, modes and path mappings. Original
Workspace history remains in its repository; no archival or visibility change
is part of source unification. Only the root configuration is effective.

## Dependencies

Rust uses one Cargo workspace and local Core paths. Local default members retain
the original local product crates; hosted packages are selected independently.
The merged resolution retains every registry version within one of the two frozen
source baselines. Existing Core versions stay selected where constraints allow;
incompatible major versions required by hosted packages remain side by side.
The added hosted crates and their dependency graph account for the lock growth.

pnpm retains ui, desktop and web workspaces, existing direct versions and root
Node/TypeScript/Vite/Biome versions. pnpm 12.10.1 replaces npm after the source migration. Five explicit transitive overrides retain
Workspace baseline versions for lang-liquid, legacy-modes, lezer/markdown,
napi-rs/canvas and vscode-languageserver-types instead of refreshing their caret
ranges. lucide-react retains the distinct versions requested by ui and web.
The Python workspace and uv.lock remain unchanged. Local builds do not run uv.

## Product gates

```sh
python scripts/ci.py Source --frontend-tests
python scripts/workspace/ci.py --stage Rust
python scripts/workspace/ci.py --stage Web
python scripts/workspace/ci.py --stage Python
python scripts/workspace/ci.py
python scripts/products.py build tui
python scripts/products.py build desktop
python scripts/products.py build runtime-server
python scripts/products.py build hosted-execution
python scripts/workspace/build.py
```

The Python gate owns disposable test databases selected by TEST_POSTGRES_*.
Heavy product builds are serial. The shared CI always runs scope and Required
gates; selected failed, cancelled, missing or skipped jobs fail the aggregate.
Shared locks, build inputs and unclassified changes expand validation. UI changes
include the hosted parity consumers; hosted changes select their actual components.
Browser interaction E2E remains outside CI.

## Images and data identity

Compose entries stay at the root and retain centaeris-workspace, services, volumes,
mounts and container data paths. Dockerfiles copy product source plus required
workspace manifests. .dockerignore protects the actual context independently of
Git ignore rules, including local overlays, credentials, caches and test output.
Image source/revision labels use the actual clean monorepo SHA. No database schema
or stored deployment data is migrated by this source change.

## Independent publication

Desktop, TUI and Workspace manual release entries build independently. Their
publish input defaults to false. Opt-in publication requires main and passing CI
for the same source SHA; local implementation does not invoke those workflows.
The supported local distribution matrix remains Windows x64. A product release
does not make every platform or other product supported.

## Deferred retry investigations

The following observations come from static review and have not been reproduced
at runtime. They remain follow-up investigations after source migration; existing
logic and tests are retained, and these are not claimed as migration regressions.

- Hosted session send page: a network failure occurs before server acceptance.
  After editing text, model or attachments, the old pending operation blocks the
  changed input; receipt lookup returns 404 but does not release that state.
  Verify non-acceptance before permitting a fresh changed input, while retaining
  duplicate-execution protection.
- Hosted material library: chat creation is accepted, but later material read or
  association fails. After deleting the Session/Agent, retry returns 410 while the
  old operation stays pending and blocks a new material/Agent selection. Verify
  the terminal missing-resource state and allow a new creation operation.

Neither investigation is repaired, suppressed or certified by this migration.
