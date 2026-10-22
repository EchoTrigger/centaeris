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

The current bridge bounds each file transfer at 256 KiB and command text output
at 64 KiB. These limits do not satisfy Core's full binary I/O contract. The
production runner must add lossless bulk transport or reject unsupported
requirements before side effects. SDK-decoded text must not be presented as
original process-byte evidence.

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
    G --> H["Authoritative Run terminal"]
    H --> I["Scoped cleanup and environment retention"]
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
SDK action and connection retries are disabled. Provider idempotency must be
verified on the selected server before create is enabled.

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
Ordinary PID signaling is not process-tree cleanup proof. The pinned public SDK
does not provide a verified no-resume attachment for a fresh control process.
Retained Run-scoped cancellation is not accepted yet.

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

The development host was probed without running Embed setup: WSL2 kernel
6.18.54.1, 4096-byte pages, cgroup v2 and TUN are present, but /dev/kvm is absent.
Opening it returns ENOENT, so this host cannot currently run Embed. Real-provider
acceptance awaits an owned KVM host or E2B endpoint. Global kernel, hugepage and
network configuration must not change as incidental test setup.

Measure release builds after the product route works: queue wait, prepare,
first useful tool, Run terminal, Job complete and environment settlement.
Compare direct SDK with the product route under equal persistence semantics.
Earlier Docker/runsc dev-profile timings are historical diagnostics.

## Retiring the legacy implementation

Preserve the carried correctness fixes: proven non-dispatch settlement,
execute/control isolation, server-checked completion, durable release wakeups
and forward migrations. Retained reuse and Unknown outcomes stay conservative.

Backend-neutral hook and artifact ports enable reuse of existing product
behavior. Remaining Docker dependencies include startup, allocation/observation,
SessionWorkspace protection, command recovery, MCP stdio, background tasks and
teardown. Reuse host-agnostic guest file/input/artifact codecs where appropriate;
the runsc launcher and host supervisor are not an E2B lifecycle implementation.

The next local boundary is a provider effect ledger and the existing Core host
facade: persist create/command/remove attempts, route the same AgentRun through
E2B, protect the final SessionWorkspace, then verify a provider cleanup receipt
before release and complete. NoEffects and complete must inspect the appropriate
provider facts as well as legacy records. A perRun policy still requires final
workspace export and commit; it cannot bypass protection by pretending to retain
an instance.

Delete legacy execution after replacement behavior is tested and no live binding
or pending cleanup duty needs it. Keep historical authorization and settlement
records. Do not switch active Runs, reinterpret old grants or claim that runsc
process state converts to Firecracker snapshots. File migration requires hashes.

Removal acceptance is behavioral: a hosted stack without the legacy socket,
runsc and supervisor completes the same supported Core workflow. Do not enforce
source-name or file-name blacklists. Local Desktop/TUI dependency graphs remain
independent.

## Local verification

The SDK/profile foundation passed the local Rust source gate (including 129
Core query-loop tests), warning-denied hosted Runtime Clippy, hosted Rust
checks/tests and Compose validation. Hosted Runtime ran 485 tests; 211 tests
requiring separate fixtures were ignored by that ordinary command. The
separate owned PostgreSQL gate executed all 178 selected lifecycle/migration
tests without skips. The API affected suite passed 112 tests, Worker passed
101 tests, and the SDK suite passed 36 tests, including nine using the actual
pinned SDK against loopback HTTP.

The test processes cleared inherited SOCKS proxy variables so loopback fixtures
received their requests. No product proxy configuration changed. Real E2B
creation, the new Core/provider product route and legacy removal remain pending;
these foundation results do not activate either E2B or retained policy.
## References

- [Pinned official Python SDK](https://pypi.org/project/e2b/2.58.0/)
- [E2B template builds](https://docs.e2b.dev/template/tags)
- [E2B Embed requirements](https://github.com/e2b-dev/runtime/blob/main/embed/compose/README.md)
- [E2B persistence](https://docs.e2b.dev/sandbox/persistence)
- [Provider idempotency](https://github.com/e2b-dev/runtime/blob/main/docs/ARCHITECTURE.md)
