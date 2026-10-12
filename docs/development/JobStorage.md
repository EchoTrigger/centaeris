# Local Subagent Job storage

Core owns Job, lease, WorkPacket, cancellation and result rules. SQLite owns
transactions and confirmation. tokio-rusqlite 0.6.0 owns the connection thread and
queue, using existing rusqlite 0.32.1 / bundled libsqlite3-sys 0.30.1. No driver
upgrade, license change or persistent schema migration is required.

## Execution and retained capabilities

Local Runtime enables native-job-store by default. The same native call chain is
covered by the ordinary package/CI test entry. The feature-disabled path remains
a comparison baseline; it is not selected by normal Desktop/TUI builds.

AgentRuntime receives AsyncJobStorePort independently of its snapshot store:

Core async Job operation → SqliteJobStore admission → Connection::call
→ shared connection-level SQL and complete transaction.

This path contains no Actor, spawn_blocking bridge, extra queue or custom executor.
The Actor remains for synchronous snapshots, checkpoint, transcript, receipt,
external-context management, provider operations and PostgreSQL. Its close stops
receiver admission and drains actual accepted work.

| Caller | Current Job path / retained responsibility |
| --- | --- |
| Agent creation | Native packet + Job transaction; unknown reply settles and compares complete immutable facts without write replay |
| Creation handoff | Confirm creation, then persist receipt and child binding; queued recovery shares host materialization with the confirmed Spawned path |
| Scheduler | Native list/claim/start/renew/reclaim/complete/fail/cancel; invalid packets fail one fenced Job without starving later valid work |
| Worker cancellation | Async authority at original safe points and after model reply, before tools; no polling cache |
| task_output / wait creation | Native Job/object reads; existing identity, packet metadata and digest rules retained |
| Core wait recovery | Native Job reads; synchronous checkpoint/snapshot interfaces retained |
| Parent projection | Native authority, shared Core terminal validation, existing snapshot CAS |
| Job list/get and cancellation | Native capability; operation/outcome retained in host diagnostics |
| Session deletion | Close Session admission, cancel parent-owned Job for a direct child, drain complete host sequences/native work, then existing Actor deletion |
| Provider / dead-letter maintenance | Existing Actor/direct Store retained; not every Job kind is migrated |
| PostgreSQL | Actor/direct PostgreSQL remain for agent_run.lifecycle, worker.noop, agent_work.return and provider.poll; pool migration separate |
| Snapshot/checkpoint/transcript/event/receipt | Existing synchronous ports and SQL retained |

The two SQLite owners do not share FIFO. Confirm creation before its host handoff;
confirm terminal commit before parent CAS; confirm parent CAS before child
terminal; acknowledge outbox generation only after both. Deletion drains these
complete sequences before removing authoritative rows.

## Operation results and reconciliation

| Operation | Atomic facts / reconciliation after actual work ends |
| --- | --- |
| Create | Packet + Job; compare Job identity/kind/idempotency/packet binding and complete object; absence does not authorize resubmission |
| Claim | Selection + unique token + expiry/heartbeat; lost reply never authorizes a Worker prefix or starting work; expiry/reclaim supplies a fenced attempt |
| Start | Exact owner and valid expiry, leased → running; reread authority, never substitute the old claimed record |
| Renew | Owner/status/expiry predicate + heartbeat/expiry; an old lease record cannot establish renewal |
| Reclaim | Eligible leases + retry budget/backoff/diagnostics/outbox in one FULL transaction; a new scan recalculates eligibility, not the original batch |
| Complete | Terminal + outputs + outbox; compound success includes result object/link; delivery never reruns a terminal Worker |
| Fail | Retry counter/queued retry or terminal/outbox; no uncertain failure replay |
| Cancel | Conditional terminal/outbox; preserve a different winner; service cancellation also commits trusted provenance |
| Acknowledge | Existing generation CAS; a stale ack cannot consume newer delivery |

NotExecuted requires pre-SQL rejection or failed transaction start. RolledBack
requires confirmed rollback. Cleanup/later-read failure does not erase Committed.
Unknown does not authorize retry. Only declared reads use ReadOnly, including a
lost read reply; missing transaction observations never identify a write as read.
Worker storage failures preserve the original typed result.

Clones share one SqliteJobStore owner. Panic, unknown write, unfinished transaction
or restoration failure retires its epoch. Queued closures check health before SQL.
The owner drains accepted work and closes the old library connection before
opening a healthy source at the same file/schema. Settlement and authoritative
reads use that source; dropped waiters cannot bypass actual-work completion or
cause automatic replay.

FULL reclaim raises, executes and restores the original synchronous setting in
one closure. Cleanup cannot replace confirmed commit/rollback. thiserror derives
error boilerplate rather than deciding outcome.

## Durable handoffs and lifecycle

Outbox recovery independently validates Job/child manifest/child Run, persists
parent projection and child terminal, then acknowledges generation. Interrupted
delivery remains pending; one corrupt result does not lose successful siblings.

Creation recovery uses native authority and Core WorkPacket binding. Missing
parents are not recreated. Unavailable working directories retain the Job with a
diagnostic; paginated scan reaches later valid Jobs. Database errors are not bad
packets. A missing child after committed creation is reconstructed without repeating
the creation transaction.
Cancelled/stopped authoritative parent Runs suppress recovered delegation;
successful parent Runs retain normal asynchronous child execution.

Trusted service stop provenance commits with fenced cancellation. An arbitrary
reason cannot impersonate shutdown. Recovery preserves stopped status/shutdown
reason versus user cancelled.

Business admission closes first. Scheduler, delivery and cancellation sequences
hold TaskTracker tokens with separate admission checks. Deletion blocks the child
sequence through Job commit and delivery after Worker return. A direct child
finishes pending parent delivery before its binding disappears.

Startup failure closes initialized owners. Normal, idle and listener-error exits
stop business admission and await accepted sequences before storage closure.
Native/Actor actual close survives a lost close waiter. Native stop intent precedes
the epoch-owner lock. Deadlines report unconfirmed shutdown, preserving recovery
facts, rather than claiming SQL cancellation or completed closure.

## Resource and behavioral evidence

Actor admits 128 queued + one executing; native admits 129 submitted operations
including execution. Shared-file tests observed both: 258 managed operations.
This is neither a global bound on direct SQLite connections nor a memory limit.
Permit/tracking follows real accepted work after dropped/timed-out waiters.
Snapshots expose executing/in-flight and cumulative monotonic admission/queue wait.
Typed native BEGIN/COMMIT lock counts include reclaim but exclude String-converted
internal SQL and synchronous Actor SQL. Native counters reset per epoch.
These observations establish lifecycle behavior, not throughput.

File-backed coverage includes:

- Ordinary/compound SQL lease, retry and object/link/Job/outbox rollback parity;
  direct SQLite/PostgreSQL Checkpoint validation.
- Real model Worker, five process-exit boundaries, ten fresh recoveries without
  repeated Worker requests, mixed success/failure and actual task_output readback.
- Running-child deletion with delayed model response, successful sibling and
  independent SQL checks showing no late result or recreated child rows.
- Cancel acknowledgement before a late read response: zero ToolCall/ToolResult;
  paused-clock Core characterization protects the same boundary.
- Creation reply loss, missing child on restart, missing parent and multiple pages
  of unavailable working directories.
- Queued retirement, dropped/expired waiters, unwind panic, restoration failure,
  startup failure, shared-file contention and cancelled close before/after its lock.
- Actual panic=abort process exit/reopen through the existing sqlite_abort example.
  catch_unwind applies only to unwind builds.

Run cargo fmt --all -- --check, Core query_loop/session::store, the full SQLite
suite, scripts/ci.py Rust, isolated PostgreSQL outbox acceptance and the documented
scheduling/process transport scripts. Native cases must be discovered and
executed; filtered zero-test harnesses are not coverage. PostgreSQL tests use
fresh disposable databases. Packaged distribution and power-loss durability remain
separate checks. Source validation does not establish deployment.
