# Storage execution and contract boundaries

Core owns storage-independent request rules. SQLite and PostgreSQL adapters own
atomic persistence, conflict detection, lease fencing and transaction outcomes.
Database formats and public network contracts are unchanged by the consolidation.

## SQLite job updates

`sqlite_reliability.rs` owns `complete_runtime_job_conn` and
`fail_runtime_job_conn`. These functions use their caller's connection and neither
open a connection nor start or commit a transaction. Ordinary job operations and
the external-context/dead-letter compound operations call the same updates.
Each outer operation still owns its transaction and terminal outbox publication.

`tests::job_updates` characterizes both entry points with valid and invalid lease
owners, the exact expiry boundary, terminal records, retries and missing retry
timestamps. An injected outbox failure must roll back the job and all associated
objects, links or dead-letter records. These tests pass before and after SQL
consolidation; they assert stored facts and errors rather than source spelling.

## Checkpoint request validation

`SaveWaitCheckpointRequest::validate` and
`ConsumeWaitCheckpointRequest::validate` in Core own the existing session/turn
binding, nonempty event list and unique event-ID rules. Both database adapters
invoke validation before acquiring a connection or starting a transaction,
including direct Store calls. Stored-record comparisons, replay conflicts,
conditional deletion and rollback remain adapter responsibilities.

Core tests preserve error text and rejection precedence. SQLite and the isolated
PostgreSQL gate test direct Store calls and assert rejected requests write no
checkpoint or event. No new field limits or payload parsing are introduced.

## Asynchronous SQLite connection validation

This is an independent, test-only candidate. Production still opens connections
through `SqliteRuntimeStore` and uses the existing Actor where currently assembled.
No Core asynchronous port, database format, license or network protocol changes.

The tested combination is `tokio-rusqlite =0.6.0`, `rusqlite =0.32.1` with
`bundled`, and `libsqlite3-sys 0.30.1`. The candidate's `rusqlite ^0.32` requirement
resolves to the existing workspace pin. No driver upgrade was necessary, so a
second version path was not pursued. The lock adds only the candidate and its
`crossbeam-channel 0.5.17` / `crossbeam-utils 0.8.23` dependencies. Existing
`thiserror 2.0.18`, `tokio-util 0.7.18` and `tempfile 3.27.0` are reused as test
dependencies; `rusqlite/hooks` enables real commit/authorizer failure injection.
The new candidate is not a production dependency.

The path is test caller -> small admission/lifetime policy -> the library's
`Connection::call` -> existing connection-level operations. It has one library
queue and no Actor, additional message queue, database thread or `spawn_blocking`
bridge. Reads reuse `load_runtime_job_by_id`. The ordinary Store and candidate
reuse the complete external-context/object/link/job/outbox transaction and the
complete lease-recovery transaction, rather than copying SQL into tests.

The internal `TransactionEvent` observation seam marks transaction start,
commit attempt, confirmed commit and rollback. Ordinary Store calls pass a no-op
observer. Error strings and outer transaction boundaries remain unchanged.
Observers also allow deterministic blocking and panic injection while a real
transaction is open or after its real commit.

The test adapter holds an owned semaphore permit and TaskTracker token inside
each submitted closure until database execution and cleanup finish. Dropping the
reply waiter does not release them or cancel accepted work. A controlled execution
clock rejects expired work before SQL. Stopping admission and registering work
share a lock; TaskTracker alone does not stop admission.

For FULL durability operations, the same closure reads the original synchronous
setting, raises it, executes the whole transaction, and restores it. The receipt
keeps write outcome separate from connection health and cleanup errors:

| Outcome | Meaning |
| --- | --- |
| ReadOnly | A read completed without attempting a write transaction |
| NotExecuted | Admission or execution-side rejection, or transaction start failed |
| RolledBack | Explicit rollback succeeded, or pre-commit unwind finished rollback |
| Committed | The transaction commit returned successfully |
| Unknown | Commit or transport did not establish a safe write outcome |

Panic, unknown outcome, unfinished transaction or restoration failure retires the
connection for this candidate. Every queued closure checks health before executing;
new and already queued work cannot use a retired connection. No write is replayed
automatically. Cleanup failure cannot replace a known commit or rollback fact.
Configuration-restoration panics are also isolated: the original result is kept,
the cleanup error is recorded separately, and the connection is retired.
Closing first stops admission, then a fixed independent task waits for actual work
and calls library close; cancelling a particular close waiter does not cancel
that task. The host must still wait for close before dropping its Tokio runtime;
production host shutdown has not been migrated in this change.

The 13 driver tests use temporary file databases and the real schema. They cover
pre-submission cancellation, dropped queued waiters, explicit expiry, execution
wait timeout, lost post-commit replies, panic, restoration failure, transaction
start/commit/rollback failures and close/submit/waiter races. Channels mark exact
execution boundaries; timeouts bound test failure and no fixed sleep guesses a
stage. Independent connections or reopened stores inspect job, object, link,
outbox, recovery counter and diagnostic-event facts after work ends.

Unit tests use `panic=unwind`; their process-exit test additionally installs an
aborting panic hook. For an actual `panic=abort` build, run:

```powershell
cargo rustc --locked -p centaeris-runtime-sqlite --example sqlite_abort -- -C panic=abort
.\target\debug\examples\sqlite_abort.exe
```

This standalone executable includes the adapter source to reuse the private SQL
operations and runs them directly on the candidate's database thread. It verifies
two child exits, before commit and after commit, and reopens the database to check
all associated records. A flushed marker proves the child reached the intended
boundary. No unwind or RAII cleanup is claimed for these abort cases. Process
exit/reopen checks are not power-loss durability tests.

## Subsequent migration boundaries

| Current call chain | Future change / ordering requirement |
| --- | --- |
| Local Runtime `agent_runtime_store_actor` -> Core Actor -> blocking operation -> SQLite `with_conn` | Replace only after the selected async capabilities reach the driver directly; then remove local Actor assembly and per-call connection creation for migrated capabilities |
| Core Subagent functions -> concrete Actor methods -> job/external-context operations | Introduce capability-oriented async ports; retain claim/start/work/atomic result commit order and lease-owner binding |
| Core `TurnCheckpointStore` async wrappers -> `run_blocking_runtime_operation` -> synchronous Store | Migrate checkpoint read/write and associated events together; remove their blocking wrapper only when all calls use the same ordering owner |
| Core `SessionManager` -> synchronous snapshot Store and CAS writers | Still synchronous; snapshot read/modify/CAS and recovery/projection ordering must be inventoried before migration |
| Hosted Runtime -> Core Actor -> PostgreSQL, plus direct PostgreSQL control/job operations | Retain required Actor ports, commands and dispatch; SQLite validation does not authorize changing PostgreSQL assembly or pools |
| Local server drain -> worker/task shutdown | Integrate stop-admission, actual database-work drain, connection close and observed completion before removing the old local path |

Do not assume operations split between the old Actor and new connection share
execution order. A candidate driver proves operation-level contracts, not parity
of a mixed production call graph. SQLite writer serialization must also be
preserved across cloned handles and other opens of the same file.

The next smallest units are: first, one caller-owned job sequence with explicit
claim/start/read/result-commit ordering, its minimal async capabilities and actual
callers; second, checkpoint/snapshot capability
migration with ordering tests; then local host close integration and deletion of
unused SQLite execution paths. Keep PostgreSQL pool work separate. Reassess each
unit by code actually removed; do not create a second general execution framework.

## Local verification record

The clean source baseline was `main` at
`e11adb09458e7038acc797edd0a7e51e2d650587`. The work is kept on
`chore/storage-contracts` as independent SQL, Core-validation and driver-validation
changes. This validation record covers source changes and local tests; it does
not establish deployment or release acceptance. Persistent formats and licenses
are unchanged.

| Command / boundary | Executed result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed, including the Rust source gate |
| `cargo test --locked -p centaeris-core query_loop` | 89 passed |
| `cargo test --locked -p centaeris-core session::store` | 10 passed, including 4 new request-contract tests |
| `cargo test --locked -p centaeris-runtime-sqlite` | 112 passed, no ignored tests; includes 13 driver tests and 3 SQL entry-point characterization tests |
| `cargo test --locked -p centaeris-runtime-sqlite --lib async_connection` | 13 passed, no ignored tests; also executed by the unfiltered package and product gates |
| `python scripts/ci.py Rust` | Passed: formatting, checks, Clippy with warnings denied, generated protocol parity, dependency direction and licensing; full local-product test command executed 1,375 tests, with 3 existing opt-in tests ignored |
| `python scripts/workspace/runtime_outbox_gate.py` | 67 discovered tests executed and passed on a fresh dedicated PostgreSQL database, including the new direct-Store request-validation test; none counted as passed merely because of an ignored marker |
| `cargo rustc ... --example sqlite_abort -- -C panic=abort`, then run executable | 2 database-worker process-exit cases passed |

SQL entry-point tests passed before and after consolidation. Direct SQLite and
PostgreSQL request tests passed before moving validation; Core and direct Store
tests passed after migration. The independent PostgreSQL gate creates and drops
its own temporary database in the dedicated local test container.

Windows compilation used `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`,
`CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_PROFILE_TEST_DEBUG=0` after default debug
builds exceeded available memory. These are command-local settings, not repository
profile changes. Local fixture requests used `NO_PROXY=127.0.0.1,localhost,::1`;
the first PostgreSQL gate was stopped when an existing fixture listener waited
behind the inherited proxy, then the complete isolated gate passed with bypass.
No production HTTP or proxy configuration was changed.

The three existing opt-in local-product tests, packaged-product acceptance,
power-loss durability and full production async-port/host-shutdown migration are
not validated by this work. The candidate proves reuse of the selected actual
operations and their exceptional execution boundaries, rather than complete
production-call-graph parity or a performance improvement.
