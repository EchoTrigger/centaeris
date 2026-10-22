# E2B execution integration

The hosted product reuses the official E2B SDK for environment lifecycle.
Centaeris owns task admission, queueing, authorization, cancellation intent,
cleanup obligations and delivery. Core remains the authority for the native
Agent's tools, events, checkpoints and continuation. E2B owns its sandboxes,
physical admission and snapshot implementation.

This work is limited to hosted lifecycle with the existing Core harness.
External harness validation follows hosted acceptance. Subscription interfaces,
account sharing, billing redesign, Desktop isolation and a workflow engine
are separate work.

## Current implementation

The [host adapter](../../packages/e2b_adapter/README.md) pins Python SDK 2.58.0.
Runtime has bounded persistent-process transport. Ordinary, short control and
slow cleanup require independent process instances because the SDK shares its
sandbox connection pool within a process. Transport deadlines cover blocked
stdin and response reads. Losing a response closes that transport without
replaying the request; it does not prove the provider operation stopped.

API and Runtime validate a distinct signed E2B profile. It freezes provider
configuration identity, template/build identity, workspace/user/network,
lifetime, timeout and input-transfer budget. Later Runtime advertisements
cannot replace a bound Environment profile. Legacy payloads, digests and
signatures retain their exact meaning.

The bridge keeps inline files at 256 KiB and command output at 64 KiB.
Larger binary files use a separate bounded spool protocol with a 128 MiB
per-transfer ceiling, stable file identity, byte counts and SHA-256 verification.
The host filesystem port retains Core's existing 64 MiB binary contract.
Transport limits are independent of the signed input-transfer budget.
Original command streams preserve stdout/stderr bytes, including invalid UTF-8;
a restarted adapter cannot reconstruct a lost stream and must return Unknown.

These components do not yet replace production Core execution. Runtime startup,
physical allocation, command recovery and teardown still use the legacy host.
An E2B profile fails explicitly at that host boundary. The production
advertisement remains legacy. No retained default or provider cutover is enabled.

## Target flow

The same existing AgentRun and lifecycle Job advance through the new host.
There is no second Agent loop.

~~~mermaid
flowchart TD
    A["API: authorize Workspace and freeze policy"] --> B["Existing queue and lifecycle Job"]
    B --> C["Worker: one execute or control lease"]
    C --> D["Runtime: verify owner and persist provider attempt"]
    D --> E["Official E2B SDK adapter"]
    E --> F["E2B sandbox"]
    F --> G["Existing Core: files, tools and delivery"]
    G --> H["Protect final workspace and delivery"]
    H --> T["Authoritative Run terminal"]
    T --> I["Scoped cleanup and environment retention"]
    I --> J["Verify cleanup, release and Job completion"]
    K["Cancellation intent"] --> C
    E --> L["Unknown: retain identity and investigation duty"]
    L --> D
~~~

Before a provider effect, the business store commits the attempt, stable
idempotency key, provider configuration digest and responsible owner. A known
sandbox ID is immutable within that attempt. Provider observations supplement
these records; they do not become another node-placement or VM state machine.

A retry first resolves the same attempt. A timeout, empty lookup or expired
idempotency window never authorizes a replacement create or refund.
SDK action and connection retries are disabled. A fresh successful durable
dispatch CAS grants a single send. Provider idempotency is an optional additional
capability; a client configuration flag neither verifies it nor authorizes retries.

## Signed policy

The new schema is runtime.e2b_environment_profile.v1. Its shared fixture is
[the profile contract](../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json).
The profile binds templateRef.templateId and a canonical build UUID, passed
to the SDK as templateId:buildId. Mutable tags cannot substitute for a build.

The providerConfigDigest identifies configured provider/account scope without
including credentials. The host must match this digest before dispatch.
The inputBudgetBytes field limits Centaeris input/restore traffic; it is not a
filesystem quota. The profile makes no byte, inode, PID or tmpfs hard-limit claim.

The profile's lifetime controls authorization. Worker configuration cannot
reinterpret an existing Environment. Network mismatch fails before freezing.
Unknown fields and unsupported schemas fail explicitly. Adapter and Runtime wire decoders also reject duplicate fields.
Existing immutable profile columns preserve the binding; signed legacy
records are not rewritten.

## Control and retention

Keep global 8 / Workspace 4 / ordinary Worker 8 as the acceptance baseline.
These are business execution limits. Physical admission belongs to E2B;
Centaeris does not reproduce its host-capacity ledger.

Short control must remain available when ordinary work or slow cleanup is
saturated. A control lease does not authorize ordinary execution, create or resume.

The current adapter supports exact-sandbox deletion in the cleanup lane.
Ordinary PID signaling is not process-tree cleanup proof. A fresh control
process verifies exact provider metadata, template, running state and explicit
kill/no-auto-resume policy through GET, then uses the pinned generated process
protocol for a fixed helper. It does not connect, resume or extend timeout.
The fixed helper can quiesce the workload UID; unresolved root-helper effects
remain Unknown. Retained Run-scoped cancellation is not accepted yet.

Begin product cutover with explicitly supported perRun policy. Retained requires
verified Run-versus-Environment task cleanup, file/session continuity and the
promised retention capability. Files, native harness sessions, memory snapshots
and online services remain distinct promises. Accepted pause requests do not
verify full-memory preservation. Existing retained environments keep their
original obligations.

## Behavioral acceptance

| Boundary | Required evidence |
| --- | --- |
| SDK requests | Real pinned SDK with isolated HTTP fixtures: exact template/build/key, no retry after lost response, 429 or 503, known identity retained, secret redaction |
| Host transport | Process reuse, bounded frames, response correlation, blocked-input deadline and control progress while ordinary remains blocked |
| Authorization | Shared Python/Rust fixtures, immutable provider/build/policy, transfer budget, unsupported quota rejection, unchanged legacy signatures |
| Persistence | Real PostgreSQL first-use freeze, later defaults ignored, mismatch rollback and old Environment/grant preservation |
| Core execution | Existing API, Worker, Runtime, Core and SDK write/read a unique marker, execute a deterministic command and publish the actual artifact |
| Recovery | Adapter/Runtime restart and lost create/command replies preserve identity and duty without repeating accepted effects |
| Cancellation | Ordinary execution, Worker, HTTP, DB and SDK lanes remain saturated while correct cleanup, release and complete finish |
| Retention | Only supported policy is accepted; continuity or restored state is verified rather than inferred from pause acceptance |
| Retirement | Exact provider identity is removed and remaining duties settled; unknown creates cannot be hidden by successful deletion |
| Transition | Old duties drain safely; replacement production path works without the legacy host socket, runtime or supervisor |

SDK fixtures do not establish Firecracker or full product acceptance. Unit,
PostgreSQL, real-provider and performance results are recorded separately.
Use the existing loopback model and actual Core tool events for deterministic
product acceptance without external model requests.

After the host reboot, the WSL2 development host exposes `/dev/kvm`, `svm` and
`kvm_amd`. Opening KVM, reading API version 12 and creating then closing an empty
VM all pass. The host has kernel 6.18.54.1, 4096-byte pages, cgroup v2, TUN and
an available NBD module. This KVM check preceded Embed deployment. The authorized
host setup now
provides NBD, 1 GiB of hugepages and the required system and network configuration.

Self-hosted SDK configuration selects both the control-plane API URL and the
sandbox data URL. The pair is passed explicitly to the SDK client, without
inferring one from the other or inheriting unselected ambient endpoints. Tests
use separate loopback control, data and interference servers to verify actual
binary-file routing. Provider creation idempotency remains unverified for this
deployment.
Setting a client capability flag does not enable it on the server; the Runtime
fresh-dispatch boundary and zero retries provide the required single-send rule.

Local Embed smoke uses the pinned official SDK and one 512 MiB, 2-vCPU
sandbox at a time. The fixed template includes the existing execution agent and
workload UID/GID 10001. The latest physical probe verifies prepare-workspace,
binary filesystem frames, create-only conflict, concurrent ordinary wait and
fresh control quiescence, unchanged sandbox expiry and exact-ID deletion followed
by provider NotFound. The test sandbox was removed; this is not yet the
API/Worker/Core product lifecycle acceptance.

A separate command-contract probe preserves NUL and invalid UTF-8 bytes. Killing
a parent PID did not stop its detached descendant, so PID acknowledgement is not
tree-cleanup evidence. A bulk probe also transfers more than 1 MiB with an exact
hash. Earlier single-sandbox create responses were 426–539 ms and delete
acknowledgements 10–22 ms. These smoke samples establish neither percentiles nor
physical memory reclamation time.

Deployment uses the authorized download proxy, separate Redis port and scoped
WSL DNS forwarding. No provider source was changed. Each physical smoke used a
single recorded create attempt with retries disabled. Provider creation
idempotency remains unverified for this deployment.

Measure release builds after the product route works: queue wait, prepare,
first useful tool, Run terminal, Job complete and environment settlement.
Compare direct SDK with the product route under equal persistence semantics.
Earlier Docker/runsc dev-profile timings are historical diagnostics.

## Retiring the legacy implementation

Preserve the carried correctness fixes: proven non-dispatch settlement,
execute/control isolation, server-checked completion, durable release wakeups
and forward migrations. Retained reuse and Unknown outcomes stay conservative.

Backend-neutral hook and artifact ports enable reuse of existing product
behavior. SessionWorkspace resolution and uncertain-commit reconciliation now
share their existing authorization and HTTP contract outside the Docker host.
Remaining Docker dependencies include startup, allocation/observation,
SessionWorkspace protection, command recovery, MCP stdio, background tasks and
teardown. Reuse host-agnostic guest file/input/artifact codecs where appropriate;
the runsc launcher and host supervisor are not an E2B lifecycle implementation.

The provider creation ledger persists exact Environment, instance, profile,
configuration, attempt and lifecycle Job identities before dispatch. A durable
CAS grants one send or closes a proven unsent operation; dispatched Unknown
cannot be refunded or replaced with another attempt. Late reported sandbox IDs
remain candidate evidence and do not grant execution authority. Migration 16 to
17 preserves signed grants and Job facts; migration 17 to 18 adds verified
provider identity and settlement facts to the same operation record.

Cleanup verifies the full binding before deletion and persists it before the
physical call. Only proven non-dispatch or exact verified removal can settle an
operation. Settlement, the versioned provider cleanup receipt, controller release
and Job completion remain separate facts. NoEffects and complete inspect provider
obligations as well as legacy records. Unknown cannot disappear behind a legacy
receipt, a timeout or an empty observation.

The remaining product boundary is the Core host facade and production factory:
route the same AgentRun through E2B, protect the final SessionWorkspace, then
verify cleanup before release and complete. A perRun policy still requires final
workspace export and commit; it cannot bypass protection by pretending to retain
an instance. The shared snapshot codec and uncertain-commit HTTP protocol are
reused. Unknown helper effects must survive recovery before a reconstructed
runner can declare the workspace safe. The current bounded command-stream cache
supports only the same adapter process.

Delete legacy execution after replacement behavior is tested and no live binding
or pending cleanup duty needs it. Keep historical authorization and settlement
records. Do not switch active Runs, reinterpret old grants or claim that runsc
process state converts to Firecracker snapshots. File migration requires hashes.

Removal acceptance is behavioral: a hosted stack without the legacy socket,
runsc and supervisor completes the same supported Core workflow. Do not enforce
source-name or file-name blacklists. Local Desktop/TUI dependency graphs remain
independent.

## Local verification

The earlier foundation at `b35389da` passed 129 Core query-loop tests,
488 ordinary hosted Runtime tests (233 fixture-dependent tests ignored), and a
separate owned PostgreSQL gate of 200 selected tests. Those results apply to
that source boundary, not to unfinished production routing.

The later SDK transfer/attachment commit passes 93 tests. The affected API
suite passes 36 tests. On the integrated Linux test executable, 84 selected
ordinary tests and 54 distinct owned-PostgreSQL tests pass, covering host
execution, exact binary SDK transport, SessionWorkspace, receipts, authority,
old cleanup and forward migrations. The required 129 Core query-loop tests
also pass. An archive fixture generator remains explicitly ignored; it is not
an acceptance result. These are affected-boundary results, not a completed full
release gate. Production factory wiring and the full product chain remain open.

Fixtures clear inherited proxy variables only in their own processes. Real E2B
helper smoke passes. The Core/provider product route, complete restart and
cancellation chain, and legacy removal remain pending. No retained policy or
production cutover is activated by these component results.

## References

- [Pinned official Python SDK](https://pypi.org/project/e2b/2.58.0/)
- [E2B template builds](https://docs.e2b.dev/template/tags)
- [E2B Embed requirements](https://github.com/e2b-dev/runtime/blob/main/embed/compose/README.md)
- [E2B persistence](https://docs.e2b.dev/sandbox/persistence)
- [Provider idempotency](https://github.com/e2b-dev/runtime/blob/main/docs/ARCHITECTURE.md)
