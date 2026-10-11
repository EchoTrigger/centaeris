# Asynchronous Job storage and recovery

Share business rules and let existing libraries own database execution. Core owns
lease, WorkPacket, result and lifecycle semantics. The SQLite adapter owns SQL and
transaction confirmation; tokio-rusqlite owns its connection thread and submission
queue. Tokio supplies admission and tracking. No second task protocol or database
worker is introduced.

## Implemented boundary

Core Subagent execution uses AsyncJobStorePort rather than a concrete Actor.
The port carries JobStoreError and JobWriteOutcome, and exposes
settle_job_operations as an actual-work completion barrier. async-trait supports
injecting this narrow capability independently of synchronous snapshots.

SqliteJobStore implements reads and local Subagent Job writes using existing
connection-level SQL. Ordinary and native SQLite entries share claim/create/update
and terminal transactions. Start/renew retain existing conditional UPDATEs; native
execution wraps them in transactions for explicit confirmation.
commit_job_write observes rusqlite completion; it owns no connection, thread,
queue, retry policy or state machine.

File-backed integration runs creation, claim, start, Worker, compound success or
retry/cancellation, authoritative rereads and lifecycle notification through the
native port. **Host defaults still use the Actor.** This does not complete host
shutdown, caller migration or snapshot migration.

## Per-operation confirmation and reconciliation

Successful writes mean the transaction commit returned. Errors retain operation
and outcome. Pure Core input rejection stays separate. No transaction observation
is interpreted as proof of a read. Only explicitly declared reads use ReadOnly;
an unconfirmed successful write returns unknown and retires the connection.

| Operation | Atomic write boundary | Reconciliation after actual work settles |
| --- | --- | --- |
| Create | WorkPacket and Job; existing kind/idempotency rules retained | Compare Job identity, kind/idempotency, packet reference and persisted binding; absence alone does not authorize resubmission |
| Claim | Due selection, unique lease token, expiry and heartbeat | Start only with the exact acknowledged token; never adopt a worker prefix; a lost result waits for fenced expiry recovery |
| Start | Leased-to-running with exact token and valid expiry | Compare current status and exact owner; never substitute the old claimed record |
| Renew | Owner, active status, unexpired lease; heartbeat/expiry together | Recheck exact owner and persisted heartbeat/expiry; missing confirmation does not establish renewal |
| Reclaim | Eligible leases, reclaim budget/backoff, diagnostics and terminal outbox in one FULL transaction | Inspect persisted counters/state; the next maintenance scan does not replay the original batch/count |
| Complete | Outputs and terminal outbox; compound success also commits result object/link | Validate terminal and output identity; delivery recovery never reruns the Worker |
| Fail | Retry count and queued retry, or failure terminal and outbox | Compare disposition, retry count, next-run time/terminal and outbox; never blindly repeat failure |
| Cancel | Conditional status/owner transition and terminal outbox | Check terminal and exact scope/owner where required; preserve another winning terminal |

NotExecuted means rejection before SQL or failed transaction start. RolledBack
requires confirmed rollback. Committed survives lost replies, subsequent missing
authority and cleanup failure. Commit/transport ambiguity stays Unknown, which
does not authorize retry. String-based host diagnostics retain operation/outcome;
Core provides no implicit conversion erasing uncertainty.

## Connection recovery owner

Clones share one SqliteJobStore owner. Health is independent of write outcome.
Panic, unfinished transaction, unknown result or cleanup failure retires the
epoch. Already queued closures check retirement before SQL and are explicitly
rejected.

The owner stops old admission, waits for tokens held by actual closures, and
awaits library close before opening/configuring the replacement at the same
file/schema. A subsequent read or settlement barrier obtains that healthy source.
Cancelling a reply waiter cannot bypass settlement. Replacement never resubmits
the previous write.

FULL reclaim raises, executes and restores policy inside one closure. Cleanup
failure cannot replace confirmed commit/rollback. Close stops admission under the
work-registration lock; a fixed Tokio closing task drains accepted work and calls
library close independently of any close waiter. The host must keep Tokio alive
until completion; timeout is not a closed acknowledgement.

The Actor adapter remains for PostgreSQL/current local assembly. Its String
transport cannot identify every phase: writes conservatively remain unknown.
Its sequential read barrier establishes previous command completion. PostgreSQL
pool migration remains separate.

## Caller inventory before production switch

| Caller | Current path | Migration / retained responsibility |
| --- | --- | --- |
| Agent tool creation | Synchronous compound Store | Async persistence at the existing async tool boundary; retain preparation/authorization and receipt handoff |
| Core claim/start/renew/work/complete/fail/cancel | Narrow async port | Native integration exercises the sequence; preserve exact lease and result contracts |
| Local queue/resource scan | Actor and Core helpers | Native reads/reclaim; absent/corrupt packets must not starve later valid Jobs |
| Parent projection | Synchronous Job authority plus snapshot CAS | Async Job authority, then unchanged CAS with explicit acknowledgement |
| Child Run terminal/result delivery | Completion path and volatile shutdown set | Durable outbox recovery across partial delivery/restart; persist trusted cancellation cause |
| task_output validation / wait creation | Synchronous Job and object reads | Async authority with existing identity checks and wait handoff |
| Wait recovery / Worker cancellation | Core loop synchronous Job reads | Async Job authority and completion ordering |
| Job query/control APIs | Actor reads/cancellation | Native capability; preserve uncertainty at host boundary |
| Session deletion | Native manifest then Actor delete | Settle related work; recovery must not resurrect removed Sessions |
| Provider polling | Synchronous Job capabilities / Actor | Retain provider.poll behavior; account for shared-file resources and ordering |
| Checkpoint/transcript/snapshot/CAS | Synchronous capabilities | Retain interfaces and SQL; no complete snapshot migration |
| PostgreSQL hosting / worker protocol | Actor and direct PostgreSQL | Retain agent_run.lifecycle, worker.noop, agent_work.return, provider.poll and required dispatch/pools |

The two SQLite paths have different owners. One-path tests do not prove shared
FIFO or a combined bound. Before default switching, report combined in-flight
work, admission wait and actual SQLite lock conflicts, and protect every causal
handoff with an acknowledged operation.

## Remaining default-switch acceptance

- Outbox recovery repairs parent projection and child Run terminal after exits
  at each boundary and partial batch delivery. Completed Workers never rerun;
  sibling failure does not lose successful delivery.
- The real scheduler progresses past absent/corrupt WorkPackets. Existing defects
  require focused failing tests and independent fixes.
- Business producers/workers stop before database admission; final writes and
  delivery settle before close. Cover host startup failure, normal close, idle
  exit, failed connection and close timeout.
- Finish the caller inventory before deleting SQLite Actor paths.
  PostgreSQL-required dispatch remains.

These are outstanding acceptance requirements, not completed work. Persistent
schema/format and licenses are unchanged.

## Verified capability boundary

The capability branch is based on the previous storage consolidation, with host
defaults retained. Verification uses temporary file databases and a fresh isolated
PostgreSQL database, without external model requests.

| Check | Executed result |
| --- | --- |
| Rust source gate | Passed formatting, compilation, Clippy with warnings denied, protocol parity, license/package checks and Core dependency direction |
| Focused query_loop | 89 passed |
| Core session::store | 10 passed |
| SQLite suite inside the full gate | 122 passed, none ignored: 14 connection-policy tests, 4 native owner tests, 25 public Core/Subagent integration tests and existing persistence coverage |
| Full local-product Rust tests | 1,385 passed; 3 existing opt-in tests ignored |
| Isolated PostgreSQL outbox/reliability gate | 67 discovered tests executed and passed |
| Actual panic=abort database worker build | 2 process-exit/reopen cases passed |

Missing-authority and lost-read-classification tests failed before their fixes.
Post-commit reply loss independently verifies the committed Job, result object
and unique outbox record. Channels control retirement, queued rejection and
shutdown; capacity returns to zero. Core/SQLite interfaces expose no driver types
to Core. The versions remain tokio-rusqlite 0.6.0, rusqlite 0.32.1 and
libsqlite3-sys 0.30.1; no shared driver upgrade was performed.

Production default switching, aggregate legacy/native resource evidence, host
lifecycle acceptance and the remaining caller/delivery migration above are not
verified by this capability change. Packaged distribution acceptance and power
loss durability were not run. Source validation is not deployment.
