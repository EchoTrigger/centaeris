# ExecutionHost contract

Status: the host-independent contract is implemented and exported from
`centaeris_core::execution`. The local Host runs native child processes on the
host platform: verified Git for Windows Bash on Windows and the system Bash on
Linux and macOS. It does not claim an OS sandbox. The Core-owned shapes are
described in the [API index](../reference/API.md).

## Decision

Core owns tool semantics and the host-independent execution contract. A trusted
product entry supplies an execution policy; the Host implements what it can and
reports honestly whether the policy was enforced. Core does not select a backend
or interpret a backend name to determine success, denial, or recovery.

Local execution launches the requested program directly with the current user's
authority. All three local platforms report `ExecutionHostKind::LocalProcess`
with `policyEnforced: false`, because no OS isolation boundary is applied.
Windows host commands run through verified Git for Windows Bash. Hosted Hosts
use their platform's existing OS isolation and implement the same Core contract.

```text
Core tool -> ExecutionHost binding -> Host
                                      local: host process -> bash or program
                                      hosted: platform exec -> bash or program
```

## Ownership

Ordinary `edit` and `write` use a shared file-mutation guard, but do not require a
previous model `read` or maintain a read-snapshot ledger. `edit` matches against
current text; `write` creates or overwrites the target. Ordinary filesystem writes
use direct file I/O, without a publication-time version check or temporary-file
replacement guarantee. An interrupted write can leave partial content.

The internal `WriteFile.observedFileHash` is the version observed during the
current tool invocation. Resource-specific Hosts may use it for coordination;
ordinary filesystem execution does not enforce it. It replaces `expectedFileHash`
for write requests, with no alias; delete requests retain their separate contract.
An ordinary write receipt has no `previousFileHash`, since the Host does not read
the destination again. Tool diffs and previous-file facts describe the tool's own
observation, not a globally locked version. Content hashes remain for result and
existing pre-apply facts; they do not establish disk verification.

The existing pre-apply event schema retains its nullable `readSnapshotHash` field;
new tools emit null. Revising the durable event protocol is separate from removing
the live snapshot ledger. Hosted private memory-directory coordination remains a
Workspace decision; Core does not interpret its URI or assume a separate memory model.

| Concern | Authority |
| --- | --- |
| Tool schema, exact edits, snapshots, bounded observations | Core tools |
| Stable operation identity, intent/receipt, recovery, result publication | Existing Core Runtime execution flow |
| Authorized execution policy | Trusted product entry and frozen execution binding |
| Process launch, policy validation, pipe draining, process-tree termination | Execution Host |
| Backend selection, configuration, readiness probes, native diagnostics | Host implementation |
| Resource ownership, authorized input revocation, shared storage, persistence authorization | Owning product/platform |

Policy syntax validation is distinct from authorization and enforcement. Core
validates the shared contract. A Host validates the requests it accepts. The
local Host accepts only the `publicInternet` network policy; it rejects other
network policies before dispatch because it cannot enforce them, rather than
silently allowing traffic the caller asked to block.

## Policy and requests

The filesystem and network policy meanings are unchanged: workspace root,
readable/writable roots, denied paths, temporary root, and
disabled/public-internet/domain-allowlist network access. The local Host does not
introduce a policy registry, model parameters, permission profiles, or a second
approval system.

Policy originates outside model-controlled arguments. The execution binding
carries it into both command and filesystem operations. Derived scopes preserve
authorized-input behavior and cannot widen the caller's grant.

The command contract carries `program`, `args`, `cwd`, `env`, `timeoutMs`, and
`policy`, together with the operation identity and cancellation probe. The file
contract carries `modelPath`, `operation`, `cwd`, `policy`, and the operation
identity. Program and arguments remain separate values, not a constructed shell
string.

File operations are policy-scoped in Core and run in-process in the Runtime;
they are not delegated to a sandbox helper subprocess. Host file identities stay
opaque. Windows uses the host's own paths; there is no cross-platform path
rewriting at the binding boundary.

## Results and errors

Exit status, bounded stdout/stderr, decoding summaries, raw byte counts,
timeout/cancellation outcomes, and input-state changes remain generic execution
facts. A nonzero command exit is different from failure to start the Host. An
unknown launch or cancellation outcome is never converted into a safe retry. The
existing operation identity and durable intent/receipt mechanism are preserved.

Error categories explicitly distinguish permission denial, policy enforcement
unavailable, Host unavailable, I/O failure, and indeterminate
execution/cancellation. Core does not infer these categories from a backend
label, stdout, or a diagnostic message.

Host readiness and successful enforcement describe the actual execution scope; a
label alone is not evidence that a requested policy was enforced. Diagnostics
must not expose credentials or sensitive host-private identities.

The durable `ToolFailureKind::SandboxUnavailable` category remains canonical.
Receipt recovery deserializes this enum, so renaming its wire value would break
old failed receipts. The `ExecutionError::PolicyUnavailable` path maps to it
explicitly. This is a retained semantic category, not an alias for an obsolete
schema or a registry of concrete backends.

## Lifecycle

Core decides when a call is cancelled and records its outcome. The Host owns
observing and terminating the execution process tree, draining bounded output,
and reporting whether termination was confirmed.

- Unix local commands run in their own process group; cancellation, timeout, and
  owned-process teardown signal the whole group. A descendant that calls `setsid`
  is outside that group and is not claimed to be terminated.
- Windows local commands run under a kill-on-close Job Object; termination
  applies to the whole job, and a successful non-timed-out command releases the
  job so permitted background work can outlive the shell.
- Permitted background work is not killed on a normal command exit. Unknown or
  indeterminate outcomes are not replayed.

No fallback to a differently scoped command is performed: the local Host runs the
program with the authority it has, and reports `policyEnforced: false`.

## Boundary expectations

| Boundary test | Required observable result |
| --- | --- |
| Unsupported network policy | No command side effect; explicit `PolicyUnavailable` |
| Command success/nonzero exit | Exact exit and output facts; no message-based classification |
| Timeout, cancellation, uncertain launch | Confirmed outcomes distinguished from unknown; no replay of unknown effects |
| Output under pressure | Bounded retained output, both streams drained, correct raw byte counts |
| File operations | Policy-scoped in Core; denied paths honored for tool file access |
| Authorized inputs and shared resources | Existing read-only, revocation, scope, and commit rules preserved |
| Contract consumers | Strict camelCase, unknown fields/variants rejected, no old-name aliases |

Implementation changes run the focused `query_loop` gate and the applicable
[release gates](../eval/ReleaseGate.md). The `macOS Runtime` workflow exercises
native local execution on Apple Silicon and Intel.
