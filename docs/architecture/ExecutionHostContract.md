# ExecutionHost contract decision

Status: contract and Linux/nono implementation verified locally, 2026-09-15;
Desktop/TUI WSL2 acceptance passed; macOS Runtime CI passed on both architectures.
The contract migration changes Host result/error shapes, not supported platforms
or release gates. Current code-owned shapes are described in the
[API index](../reference/API.md).
The reviewed baselines are Core/Runtime `8990e42` and Workspace `457fbe8`.

## Decision

Core owns tool semantics and the host-independent execution contract. A trusted
product entry supplies an execution policy; the Host implements that policy
using its actual operating-system boundary. Core does not select a sandbox
backend or interpret a backend name to determine success, denial, or recovery.

The local target is nono on Linux and macOS. Windows local execution uses WSL2;
the Local Runtime and its execution Host run inside the selected distribution.
Both Windows clients connect through the shared WSL bootstrap and Runtime relay.
Native Windows TUI packaging and coexistence acceptance passed before removal of
the Windows execution backend. Native Windows Runtime startup and execution now
fail explicitly with WSL2 setup guidance.

A hosted Host uses its platform's existing OS isolation. It need not additionally
launch nono. Both kinds implement the same Core execution contract.

```text
Core tool -> ExecutionHost binding -> Host
                                      local: nono -> bash or file helper
                                      hosted: platform exec -> bash or file helper
```

The sandbox launcher establishes restrictions before starting the command.
Wrapping Bash alone does not isolate file helpers, MCP servers, or hooks.
Every process entry that accesses execution resources must use the appropriate
Host boundary before the local migration is considered complete.

## Ownership

| Concern | Authority |
| --- | --- |
| Tool schema, exact edits, snapshots, bounded observations | Core tools |
| Stable operation identity, intent/receipt, recovery, result publication | Existing Core Runtime execution flow |
| Authorized execution policy | Trusted product entry and frozen execution binding |
| Process launch, policy enforcement, pipe draining, process-tree termination | Execution Host |
| Backend selection, configuration, readiness probes, native diagnostics | Host implementation |
| Resource ownership, authorized input revocation, shared storage, persistence authorization | Owning product/platform |

Policy syntax validation is distinct from authorization and enforcement. Core
validates the shared contract. A Host validates whether it can enforce it and
enforces access at the resource boundary. Repeating the same request through
three independently maintained policy interpreters is not a required guarantee.
Rechecking mutable authorization at a later commit boundary may be necessary.

## Policy and requests

Retain the existing filesystem and network policy meanings for this migration:
workspace root, readable/writable roots, denied paths, temporary root, and
disabled/public-internet/domain-allowlist network access. Do not introduce a
policy registry, model parameters, permission profiles, or a second approval
system. Do not remove an existing restriction because a new backend cannot
express it; the Host must reject unsupported requirements before dispatch.

Policy originates outside model-controlled arguments. The existing execution
binding carries it into both command and filesystem operations. Derived scopes
must preserve existing authorized-input behavior and cannot widen the caller's
grant. Do not reduce multiple scopes to a workspace-only grant.

The command contract carries `program`, `args`, `cwd`, `env`, `timeoutMs`, and
`policy`, together with the existing operation identity and cancellation probe.
The file contract carries `modelPath`, `operation`, `cwd`, `policy`, and the
existing operation identity. Keep the existing binding and runner; do not add
another execution router or require all hosts to implement a new RPC service.
Program and arguments remain separate values, not a constructed shell string.

WSL path conversion happens at the product entry, before the binding is frozen.
Core and the local Host use Linux paths consistently. Host file identities stay
opaque; Core must not infer a Windows/WSL/container resource from path prefixes.
Paths that cannot be mapped to the chosen distribution fail explicitly.

## Results and errors

Keep exit status, bounded stdout/stderr, decoding summaries, raw byte counts,
timeout/cancellation outcomes, and input-state changes as generic execution
facts. A nonzero command exit is different from failure to start the Host.
An unknown launch or cancellation outcome is never converted into a safe retry.
Keep the existing operation identity and durable intent/receipt mechanism.

The target error categories explicitly distinguish permission denial, policy
enforcement unavailable, Host unavailable, I/O failure, and indeterminate
execution/cancellation. Core must not infer these categories from the presence
of `sandboxType`, a backend label, stdout, or a diagnostic message.

Host readiness and successful enforcement describe the actual execution scope;
a backend label alone is not evidence that the requested policy was enforced.
Backend names and native diagnostics remain Host-owned diagnostics. Core may
transport bounded diagnostics using the existing diagnostic envelope, but does
not enumerate concrete implementations or use their text for control flow.
Diagnostics must not expose credentials or sensitive host-private identities.

The existing durable `ToolFailureKind::SandboxUnavailable` category remains
canonical. Receipt recovery deserializes this enum, so renaming its wire value
would break old failed receipts. The new `ExecutionError::PolicyUnavailable`
maps to it explicitly. This is a retained semantic category, not an alias for
an obsolete schema or a registry of concrete backends.

## Target code changes

The policy/process contract replacements below are implemented and exported from
`centaeris_core::execution`. File I/O relocation remains pending. There are no
compatibility aliases. Exact serialized fields are defined and tested in code;
this decision does not add a second wire schema.

| Current contract | Target |
| --- | --- |
| `execution::sandbox` mixes policy, backend identity, errors, decoding | Separate policy from generic process contracts under `execution` |
| `SandboxPolicy`, filesystem/network policy | Host-independent execution policy; preserve existing meanings |
| `SandboxTransformRequest` | Generic command request, retaining separate program/arguments |
| `SandboxErr` with concrete `SandboxType` | Explicit generic execution errors |
| `SandboxedProcessOutput`, `SandboxAttempt`, `SandboxPolicySummary` | Generic process facts and explicit enforcement result, without repeated backend identity |
| `ExecutionHostStatus.sandbox_type` | Host-independent readiness/enforcement facts; backend detail stays in Host diagnostics |
| Core's `SandboxType` enum | Host-owned backend selection; no Core enum for nono, Bubblewrap, Seatbelt, OCI, or gVisor |
| Direct/scoped filesystem implementations currently in Core | Separate I/O implementation from tool semantics at a small tested seam; retain shared behavior, do not duplicate it in adapters |

File I/O relocation is not a prerequisite for accepting this contract. Determine
its smallest shared implementation home when changing the file helper; do not
create a package or generic VFS merely to move code. Keep exact editing, snapshot
conflicts, and output/reference semantics in Core throughout.

## Lifecycle

Core decides when a call is cancelled and records its outcome. The Host owns
observing and terminating the execution process tree, draining bounded output,
and reporting whether termination was confirmed. nono owns applying local OS
restrictions. Its supervisor behavior must be evaluated against the existing
Host lifecycle before retaining or replacing any overlapping supervisor.

Existing background-process ownership and recovery behavior must first be
characterized. Do not accidentally kill permitted background work on a normal
command exit, or leave processes unowned after cancellation or Runtime shutdown.
No fallback to an unisolated command is allowed when required isolation fails.

## Migration and acceptance

1. Accept this contract and update the four-tool work order (this change).
2. Characterize current outcomes; replace Core's backend-coupled types and
   update local and external Host consumers together. Keep current backends
   working during this contract migration. Check events, protocol consumers,
   diagnostics, persisted-data expectations, and documentation; no compatibility
   aliases or invented success fields.
3. Integrate nono behind the local Host, including file helpers and the other
   process entrypoints. Establish the WSL2 Runtime connection and filesystem
   mapping, then remove replaced native launchers after acceptance.
4. Consolidate proven duplicate hosted authorization checks at their owning
   boundary. Continue with four-tool deduplication and limited I/O corrections.

| Boundary test | Required observable result |
| --- | --- |
| Policy denied or unsupported | No command/file side effect; explicit category |
| Missing backend | No unsandboxed fallback |
| Command success/nonzero exit | Exact exit and output facts; no message-based classification |
| Timeout, cancellation, uncertain launch | Confirmed outcomes distinguished from unknown; no replay of unknown effects |
| Output under pressure | Bounded retained output, both streams drained, correct raw byte counts |
| bash and file helper | Equivalent authorized scope, including denied paths and network requirements |
| Authorized inputs and shared resources | Existing read-only, revocation, scope, and commit rules preserved |
| Background work and shutdown | Existing ownership characterized; no orphaned work on shutdown |
| Windows/WSL2 | Correct distribution, spaces/Unicode paths, unmappable-path errors, reconnect and cancellation |
| Contract consumers | Strict camelCase, unknown fields/variants rejected, no old-name aliases |

Use narrow fake Hosts for Core behavior tests and actual OS tests for enforcement.
Core tests cannot certify Landlock, Seatbelt, WSL2, or a hosted container.
Implementation changes run the existing focused `query_loop` and applicable
[release gates](../eval/ReleaseGate.md). No new browser CI suite is required.

The Core contract and both local/hosted consumers have been migrated. Focused
tests cover strict result shapes, denial classification, and non-retryable
unknown outcomes. Linux isolation and Desktop/WSL2 have local acceptance evidence
below. macOS CI passed on Apple Silicon and Intel at `5be7d90` (run
`34966224952`); the Windows Rust/source and performance gates also passed.
Remote release-runner WSL2 provisioning, macOS detached-descendant/crash cleanup,
hosted authorization consolidation and the four-tool I/O work remain pending.

## Local verification (2026-09-15)

### Current implementation and acceptance

- Linux calls pinned `nono = 0.75.0` with default features disabled, prepares
  capabilities in the Host and applies them before exec. Bubblewrap is removed.
  Bash, file helpers, lifecycle hooks and sidecars use this boundary.
- Fixed Host seccomp rules close the observed private Unix-socket gap. Delegated
  cgroup v2 scopes own descendants, including children that call `setsid`.
  A systemd service with `Delegate=yes` and `KillMode=control-group` owns the
  Runtime and all scopes if the Runtime crashes. This does not claim PID
  namespace isolation or newer Landlock signal scoping on an ABI 3 kernel.
- Hook stdin is fed while output is drained; each captured hook stream is
  bounded to 64 KiB while preserving its original byte count. Large input and
  output regression tests reproduce and protect the former pipe deadlock.
- Desktop installs the bundled Linux ELF into Linux user storage and maintains
  a `wsl.exe` stdio relay to the existing private Runtime socket. JSON-RPC,
  initialization/build identity, ownership and reconnect semantics stay in the
  existing transport. Individual tools do not cross back into Windows.
- Real WSL tests passed for initialize, reconnect, private-state denial, sidecar
  stop and Runtime crash cleanup. Packaged Desktop window acceptance passed for
  session creation/reload, settings persistence, reopening and idle shutdown.
  Linux full-workspace tests passed, including 561 Core unit tests.
  Windows `scripts/ci.ps1 -Stage Rust` also passed after fixing the protocol
  manifest generator's platform-dependent JSON key order. Desktop host checks,
  license assembly and UI tests passed; the UI worker retry used two workers
  after the default worker pool exited unexpectedly.
- macOS uses a single-threaded private launcher to call nono before exec; its
  allocation-using API is never called in `pre_exec`. Bash, helpers, hooks and
  sidecars are wired. Apple Silicon and Intel CI passed at `5be7d90`, including
  actual execution, DNS and Runtime regressions. Its current process-group
  lifecycle must not be described as equivalent
  to Linux cgroup cleanup of detached descendants or Runtime crashes.

See [Local execution setup](LocalExecution.md) for WSL prerequisites and paths.
The feasibility notes below are historical evidence, superseded by the current
implementation where they describe an unimplemented launcher or socket gap.

### Linux nono feasibility follow-up

The Ubuntu 24.04 WSL2 environment reports kernel
`6.6.87.2-microsoft-standard-WSL2` and Landlock ABI 3. An isolated temporary-file
probe verified that a writable parent grant plus a read-only child grant still
allows writing the child, while an ungranted outside write is denied. The
read-only child grant cannot subtract the parent grant. This was a direct
Landlock capability probe, not a completed nono integration test.

The reviewed nono CLI also rejects deny-within-allow overlaps on Linux in
`policy.rs::validate_deny_overlaps`. Its capability primitive cannot directly
replace the current Linux bind-mount handling of `deniedReadPaths` and
`deniedWritePaths` beneath an allowed parent. Adding ordinary read-only grants
for those children would silently weaken policy and is not an acceptable mapping.

The maintainer accepted separating protected resources from writable roots
before adopting nono. Local execution will use disjoint workspace, writable
scratch, read-only input/result, and Host-private control/state roots. A writable
ancestor must never cover a protected descendant; unsupported overlaps fail
before launching a process. No production
launcher has been switched, and no deny rule has been removed. A separate
broker, path-enumeration permission engine, or nested mount sandbox has not
been added as a workaround.

### Accepted local resource layout and nono probe

The accepted layout separates four authority levels:

| Resource | Placement and command access |
| --- | --- |
| Project files | Writable workspace root |
| Command scratch | A dedicated writable directory, never the whole OS temporary root |
| Published inputs/results | Separate read-only roots; result capture writer receives its own exact write grant |
| Runtime state, secrets and control files | Host-private roots with no command grant |

Existing local image inputs and plugins are below the user data root, normally
`~/.centaeris`, outside a normal project workspace. Published tool results are
already below the OS temporary root, in `agent-tool-results/<session hash>`.
There is no reserved workspace `.centaeris` directory. The user data root is
Host-owned application storage, not project metadata. Layout validation uses
actual configured paths; it neither creates nor assigns special permissions to
a directory based on its name.
This inspection does not imply that arbitrary user-selected workspaces or custom
data roots are disjoint. A workspace covering managed state, or a broad writable
temporary grant covering published results, must be rejected before execution.
Do not automatically move arbitrary user files or silently drop a deny rule.

WSL2 execution roots must use its Linux filesystem. The reviewed nono source
rejects 9P-backed paths such as Windows drive mounts; accepting WSL2 does not
make `/mnt/c` or `/mnt/d` valid nono sandbox roots. Windows UI/path transport and
Linux workspace provisioning remain implementation work.

A user-local Rust 1.95.0 toolchain was installed in WSL2 without changing the
shell PATH or replacing `/usr/bin/cargo`. A standalone probe pinned the published
`nono = 0.75.0` crate with default features disabled and compiled successfully.
The probe used `Sandbox::prepare_seccomp_with_abi`,
`SeccompOpts::network_baseline`, and a pre-exec application in a child process.
It did not invoke the nono CLI, load profiles, or sandbox the Runtime parent.

Actual results on the same ABI 3 kernel, using owned temporary fixtures:

| Operation | Result |
| --- | --- |
| Write workspace / independent scratch | Allowed |
| Read published result | Allowed |
| Overwrite published result | Denied |
| Read ungranted private file | Denied |
| Create TCP / UDP socket under blocked-network policy | Denied |
| Connect to a Unix socket in the ungranted private directory | **Allowed; confirmed by the Host-side listener** |

Only a temporary mock Unix service was contacted. This is an observed gap in
using the pure library primitive as a replacement for the current Host boundary,
not a claim that nono's full CLI mediation has the same behavior. In particular,
`SeccompOpts::network_baseline` documents its block as non-Unix sockets. The
library's optional AF_UNIX notification machinery is separate from this call.
Filesystem layout separation alone does not solve socket access.

The existing Linux supervisor also depends on Bubblewrap's PID namespace for
reaping and process-tree lifetime. A plain library restriction plus exec must
not be substituted without preserving timeout/cancellation and background-work
ownership. A user-delegated systemd scope with `Delegate=yes` can be created in
the test WSL distro; this only establishes a possible cgroup lifecycle route,
not completed process-tree acceptance.

Local probe evidence is retained under ignored `test-results/nono/`, including
source, pinned manifest/lockfile and output. Probe source SHA-256:
`26e104a9ea537d659fce2c636122ab6af14802fc3969956d5e928ee1ad1ef5c9`.
Production dependencies and launchers have not been switched. The follow-up
scope decision is whether the Host should add fixed Unix-socket restriction
and cgroup lifecycle support alongside nono, without adding dynamic grants,
profiles or an approval engine.

### Completed contract migration verification

- Core/Runtime: formatting, workspace check, Clippy with warnings denied,
  generated protocol verification, focused `query_loop`, SQLite integration,
  and full Rust workspace tests passed. Core includes 560 passing unit tests.
- Workspace: full `scripts/ci.ps1` passed, including 17 disposable-database
  PostgreSQL regressions, 456 API tests and 69 web tests. After the final Core
  error-mapping adjustment, Workspace formatting/check and full Rust tests were
  rerun successfully.
- Additional Workspace Clippy found the existing eight-argument
  `terminalize_agent_run_failure` warning. Its complete function is unchanged
  from the reviewed baseline; it was not refactored as part of this migration.
- Verification ran on Windows. This does not certify Linux/macOS OS isolation
  or WSL2/nono support. No remote CI, deployment, or publication was performed.
