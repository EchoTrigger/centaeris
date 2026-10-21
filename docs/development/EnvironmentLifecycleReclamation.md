# Environment lifecycle reclamation

Callers submit Agent invocations; the platform owns queuing, admission, environment reuse and reclamation. Reliable resource facts are the prerequisite for automated placement and elastic capacity supply.

Queued cancellation and physical resource settlement must continue independently of ordinary execution capacity. Complete that lifecycle before making retained environments the default for ordinary Sessions. Execution limits remain eight globally, four per Workspace and eight slots per Worker process.

## Observed gaps and expected behavior

| Boundary | Current evidence | Required behavior |
| --- | --- | --- |
| Workspace execution capacity exhausted | The deployed baseline could not claim a cancelled queued Job while four owners remained active. The control-lease implementation below now claims it independently. | An independent bounded control path commits the cancellation without waiting for an unrelated execution slot or granting execution authority. |
| Global execution capacity exhausted | The deployed baseline could not claim a cancelled Job while eight owners across two Workspaces remained active. The same independent control path now handles it. | The same control progress must hold independently of the global execution limit. |
| Failure before physical creation | The initial regression left a reservation charged after a fresh pre-create callback failed. The undispatched settlement below now closes this case. | Preserve allocation history and atomically settle the proven unmaterialized attempt, its holding and subsequent obligations. |
| Cleanup proof lost at the host boundary | A controlled host callback removes its own synchronous attempt but returns only a string failure. The ledger retains the reservation. | Carry typed physical evidence to durable settlement. A string error or an empty inventory cannot authorize release. |
| Historical reserved allocations | Earlier records may have no attempt evidence or container ID. | Recover a durable investigation obligation; never infer non-creation from age, lease expiry or an empty observation. |
| Workspace storage budget | The current container request carries the data budget label but no explicit Docker tmpfs or storage option. | Establish the actual writable backing and enforce the declared byte/inode contract at the physical boundary. |

The database regressions are in
[`resource_lifecycle_tests.rs`](../../packages/runtime_server/src/postgres_store/integration_tests/resource_lifecycle_tests.rs).
The two capacity regressions check ordinary exclusion, control readiness/claim, persisted purpose and unchanged execution owners. Separate Runtime HTTP tests cover terminal history, receipt replay and completion gates; the service-chain measurement below covers a never-admitted queued Run. Physical cleanup remains a separate acceptance boundary.

Existing unknown-create regressions continue to require charged resources and adoption of the exact late instance after reopening. Any settlement fix must preserve those protections. A known failure and an unknown physical outcome must never share an unconditional release path.

## Undispatched allocation settlement

A fresh AgentRun allocation commits its holding and one creation attempt before the pre-create callback. The attempt's immutable allocation reference binds instance, host, origin grant and profile. A callback failure may close only this fresh, still-prepared attempt. Under current controller and lease authority, recording `notDispatched` and removing the exact reserved holding commit together. Allocation history remains intact. The same failure while reusing a resident changes neither the original holding nor its physical instance.

Successful preparation commits `dispatched` before entering the host callback, then reacquires authority pins across the physical operation. Failures after this conservative boundary retain their charge, including a crash before the actual send or an error string from a callback that privately removed its object. A possible dispatch cannot be rewritten as non-dispatch.

Cleanup recognizes an unmaterialized allocation by durable evidence rather than treating it as `NeverAllocated` or environment loss. Its receipt retains the planned instance identity without claiming physical deletion. Both lifetime policies replay that receipt after reopening. Observation reports `notMaterialized`, and a later Run can obtain a fresh allocation without recreating the old execution.

Runtime schema migration 14 adds the dispatch journal without inventing evidence for historical reservations. Historical unknowns remain charged and cannot dispatch again through ordinary AgentRun preparation. No-Run provision dispatch, typed post-create removal evidence and interrupted-attempt investigation remain follow-up boundaries. The bounded cancellation path is described below.

## Workspace creation is a separate boundary

A bare Docker create/start probe does not perform the host's workspace preparation. The native `prepare-workspace` helper creates a missing workspace for the profile's numeric user and preserves existing image content and permissions. Absence of `/mnt/data` immediately after bare container creation is therefore not, by itself, an established product defect.

Acceptance must exercise the real preparation and task path before judging workspace availability. It must then identify the workspace mount/backing, enforce its configured limits, verify file visibility across supported lifecycle operations, and distinguish workspace data from temporary directories. A write to `/tmp` cannot certify the `/mnt/data` contract.

The opt-in `export_owned_runsc_storage_probe_request` test exports a request from the production container request builder for an explicitly owned physical probe. Probe drivers, private configuration and artifacts remain outside the source checkout. A separate Docker daemon must also run in a separate Linux network namespace; a different socket/data root alone does not isolate Docker's host network management.

## Bounded lifecycle control leases

Runtime schema 15 adds a persistent `execute/control` purpose and, for control leases, an `advance/cleanup` stage to the existing lifecycle Job. Missing historical purposes become `execute`; existing signed grants, owners and job history are preserved. The purpose and stage cannot change while the same lease owner remains current. Upgrade API, Worker and Runtime together after draining incompatible live consumers, following [Session environment upgrades](SessionEnvironments.md#persistence-and-upgrades).

Ordinary ready/claim excludes Jobs with a digest-bound durable cancellation or terminal-control intent. Control ready and claim share the same server predicate: queued, due, no current owner, matching durable intent and the appropriate authoritative terminal state. Requesting `control` alone grants nothing. Execute capacity stays global 8 and Workspace 4, with eight ordinary Worker threads. Independent control threads and global control-lease budgets provide two advance and two cleanup slots. Reclaim exhaustion retains a pending control Job for investigation/retry rather than deleting its intent.

The control API reads the original signed Run authorization without acquiring an Environment controller. Runtime restores Core Session state and uses Core's termination operations. A never-admitted Run with no allocation or owned controller receives a durable `noEffectsForRun` fact containing its no-operation cleanup and release receipt identities. It does not acquire another Run's controller. A Run with prior Core history or an allocation cannot use that shortcut: it yields the same Job to the cleanup stage and follows existing cleanup/release authority.

Server boundaries reject control authority at ordinary start, API execute-controller/start resolution, Runtime step, model requests, ordinary record append and host execution-controller validation. An old signed execute grant does not override a current control lease. `complete` verifies an authoritative terminal plus matching cleanup/release facts, including on successful reply replay. Missing or altered no-effects evidence and unresolved physical cleanup are rejected.

Short Runtime control HTTP/DB budgets remain four/two. Slow teardown uses a separate HTTP limit and DB pool, both defaulting to two (`RUNTIME_HTTP_CLEANUP_LIMIT`, `RUNTIME_POSTGRES_CLEANUP_POOL_SIZE`). It retains the existing authority pins across physical cleanup. This separation prevents slow teardown from consuming the short-control pools; it does not establish fixed completion times under database, host or shared-executor failure.

This boundary repairs queued cancellation under ordinary execution saturation. Active-owner failure handoff, historical terminal/dead-letter reconciliation, HTTP/DB combined saturation, physical cleanup under pressure, no-Run provision recovery, storage quotas and target-host acceptance still require their own tests. Retained is not yet the default.

## Runtime HTTP cancellation measurements

A small WSL experiment used the deployed Runtime image at `b389f6d29411423d494d78d0b94fe4d68fdc54e0` (`sha256:993a7e156a545ac8317342ff92ea2469246db545e51c7e7da248d2881b4e636c`) and its schema 13 against a fresh PostgreSQL 18 container. The measured cancellation handler, ready query and claim capacity logic are unchanged in candidate `b67e04396e9d9613ba864fb2902fb8685f9d76ae`; this remains a deployed-build baseline, not final-candidate acceptance.

The driver issued real authenticated HTTP schedule/claim/start/cancel/wait/heartbeat/yield requests. Synthetic Session/Run rows supplied the database boundary; no API admission, Worker thread, model or sandbox execution was exercised. Execution limits remained global 8 and Workspace 4. The private Runtime had 512 MiB/1 CPU and PostgreSQL 1 GiB/1 CPU; these are local diagnostic timings, not production throughput estimates.

Each condition ran three times. Full-capacity cases retained their original four/eight running lease owners and heartbeats for a 1.5-second observation window, polling every 500 ms. Global saturation used two Workspaces with four owners each and a target in an empty third Workspace.

| Condition | Cancel HTTP acknowledgement, ms (three samples) | Target before releasing a blocker | Yield one blocker through HTTP, then claim target, ms |
| --- | --- | --- | --- |
| Idle | 12.74, 6.25, 6.21 | Ready and claimed 26.17, 11.87, 11.94 ms after cancellation request began | Not applicable |
| Workspace full | 6.19, 6.24, 6.50 | All four observations per trial timed out for ready and claimed zero; target remained queued through at least 1.5 seconds | 6.26, 6.21, 6.59 |
| Global full | 6.12, 6.55, 6.65 | All four observations per trial timed out for ready and claimed zero; target remained queued through at least 1.5 seconds | 6.46, 6.21, 6.25 |

Every target had exactly one durable cancellation-intent event. Targets had no authoritative terminal event during this experiment; even the idle result proves claimability only. Physical allocations and resident holdings remained zero. A read-only Docker proxy prevented host mutations, owned containers/network were removed, and the existing application containers retained their exact IDs, image IDs and start times.

The first demonstrated obstruction is execution-capacity eligibility at ready/claim: cancellation is acknowledged promptly but cannot obtain processing while ordinary execution is full. Releasing one unrelated execution owner removes that obstruction immediately. The control-lease implementation above and service-chain follow-up below address that obstruction. This experiment does not measure physical cleanup or Job completion latency.

## Service-chain queued-cancellation measurements

The repaired build was tested from committed source `8e7b36ed58a44a2811ac4cc139b17a8050edd115`, using Runtime image `sha256:5873344755da0a3922241e10ba9aa2cb2ae9162a23c08bbd011beaede25cf280` and Runtime binary SHA-256 `496b63be3f1194e2a7ba2aba25fa9a1b1bb41b930f60f676a6f196c1ce5780c4`. API and Worker used that same committed source over the existing dependency images in a separate owned stack. A documentation-only follow-up does not change those executable sources.

This experiment exercised public authenticated Session/Run admission and cancellation, real Worker ordinary and control threads, original signed authorization, API terminal projection, Runtime receipts, PostgreSQL and Job completion. Ordinary workers were held immediately before Runtime step dispatch, after real claim/start/controller resolution; their normal heartbeat loops remained active. These were execution-entry barriers, not eight physical 8 GiB sandboxes. There were no model calls, physical task executions or sandbox allocations. A GET-only Docker socket proxy rejected physical mutations.

WSL had eight virtual CPUs and 16 GiB RAM. The private stack bounded Runtime and Worker to 512 MiB/1 CPU each and API and PostgreSQL to 1 GiB/1 CPU each. Per-sandbox declarations remained 8 GiB, four CPUs, 4 GiB data budget and 512 PIDs; this test did not establish their physical enforcement. Both execution limits and ordinary Worker count remained 8/4/8. Each saturated condition ran three times with a distinct target Session.

| Condition | Cancel HTTP acknowledgement, ms | Request start to observed Job completion, ms | Ordinary execution held through completion |
| --- | --- | --- | --- |
| Workspace full | 25.78, 25.44, 28.84 | 399.10, 397.61, 431.18 | Four owners in the target Workspace |
| Global and Worker full | 26.48, 25.20, 22.86 | 395.52, 411.24, 416.02 | Eight owners in two Workspaces; target in an empty third Workspace |

All six targets had one no-effects receipt, a cancelled Run with committed pre-admission cancellation and a succeeded lifecycle Job whose final lease purpose was `control`. Their Environment epoch stayed zero with no controller owner; no Core execution records, physical allocations or resident holdings appeared. The original four/eight owners stayed unchanged, the same ordinary Worker slots remained held, and no extra ordinary execution was admitted. Completion timestamps include database polling and client overhead; they are diagnostic observations, not latency guarantees or percentile estimates. This experiment includes more services than the earlier Runtime-only baseline, so acknowledgement timings are not a direct speed comparison.

The useful comparison is dependency: the deployed baseline remained unclaimable throughout the 1.5-second observation until an unrelated owner yielded. The repaired service chain completed while every blocker stayed held. Control workers currently poll every 250 ms; their idle request overhead and contention at larger Worker counts have not been benchmarked. HTTP/DB saturation, slow physical teardown and actual sandbox cleanup remain separate measurements.

The probe issued zero Docker mutation requests. All owned probe containers and network were removed, and the existing application's container IDs, image IDs and start times were unchanged. Private fixture drivers and synthetic secrets remained outside the source checkout.

## Verified control boundary

The repaired source passed 115 individually selected tests on fresh owned PostgreSQL databases: all 113 required identities plus both retained Unknown-create reservation/adoption protections. Each identity executed exactly once. These tests cover ordinary/control readiness parity and exclusion, independent bounded stages, unchanged owners, current-purpose host and execution rejection, no-effects reply replay, missing/foreign receipt rejection, prior-history cancellation, cleanup-required completion, purpose immutability, reclaim exhaustion and schema-14-to-15 preservation/drift rejection.

The final Runtime unit suite passed 468 tests; 204 opt-in tests were ignored by that ordinary run and are not counted as passed there. Core `query_loop` passed 129 tests. Worker discovery executed all 101 tests with no skips, including full ordinary-slot occupancy, terminal handoff and cleanup failure retry. The affected API PostgreSQL suite passed 45 tests. All 37 rendered Compose contract tests, 11 PostgreSQL gate-driver tests, warning-denied Runtime Clippy, formatting and whitespace checks passed. This is focused repair validation plus the service-chain experiment above, not completion of the full hosted release or physical-host acceptance gate.

## Reclamation completion conditions

- Preserve one authoritative lifecycle and one current lease owner. A control lease cannot run the model, allocate a sandbox or invoke ordinary execution.
- Keep cancellation intent, authoritative Run terminal state, physical cleanup and resource release as distinct durable facts. Measure their delays separately.
- Persist creation attempts and settlement responsibility before physical side effects. Settle each allocation at most once with explicit physical evidence and fencing.
- Recover pending work after notification loss, worker/runtime restart, lost replies and dependency outages. Slow physical cleanup must not exhaust short control handling capacity.
- Apply Run-scoped cleanup without deleting a retained Environment, another owner's tasks or files that its policy preserves.
- Exercise both AgentRun-owned creation and provision without a Run, plus stale owner/epoch/instance responses and PostgreSQL forward migrations.
- Verify environment retirement and host-private retirement independently, with precise identity, confirmed physical absence and durable receipts.

## Default lifetime change

The maintainer-approved target is `retained` for ordinary Sessions after reclamation acceptance passes. Keep execution limits at eight/four/eight. Do not change previously signed grants or silently reinterpret existing persistent data. Document and test the forward upgrade policy before switching the Worker default and API-visible lifetime consistently.

The switch is gated on the following evidence:

- Full API/Worker/Runtime/PostgreSQL/host cancellation, cleanup, release and completion while execution capacity remains occupied.
- Definitive creation failure settles; unknown and historical uncertain outcomes keep their budget and responsibility until evidence permits release.
- Retained reuse preserves the expected files and Environment tasks; explicit retirement reclaims the exact instance and its resource vector.
- Real workspace preparation and physical storage limits match the public contract.
- The same repaired build passes the target Linux acceptance. WSL adapter probes and database fixtures are reported separately from that result.

## Initial validation

The first isolated PostgreSQL run after restoring the test environment executed eight named tests: five passed and three failed at the intended assertions. The four pre-existing cancellation/capacity/unknown-create baselines passed. The three failures are the two saturated-capacity cancellation cases and the pre-create reservation settlement case above. The additional string-failure characterization passed and preserves conservative accounting.

The focused Core query-loop gate passed all 129 selected tests. Rust test compilation, request export, formatting and whitespace checks passed. That initial candidate changed neither production behavior nor lifetime defaults; the settlement implementation and its later validation are recorded below. Full terminal/control-lane behavior, historical migration, actual workspace quota enforcement and target-host acceptance remain open.

## Verified settlement boundary

The implemented undispatched-allocation boundary passed 108 named tests against owned disposable PostgreSQL databases: all 106 required identities plus the two existing unknown-create reservation/adoption protections. Each selected identity executed exactly once in its database. Coverage includes settlement/release rollback, stale-controller rejection, resident reuse, both lifetime policies, cleanup replay after reopening, later-Run creation and the v13-to-v14 historical-unknown migration.

The final Runtime unit suite passed 467 tests with 199 opt-in tests ignored. Core `query_loop` passed 129 tests. Warning-denied Runtime Clippy, formatting, whitespace, all 11 gate-driver unit tests and discovery of the 106 required identities in the final binary passed. Local HTTP fixtures required a process-local loopback proxy exclusion; no application network configuration changed.

At that settlement-only revision, the two saturated-capacity cancellation regressions still failed at `ready=false, claimed=0` while the original four/eight execution owners remained running. The later control implementation supersedes those results; its evidence is recorded separately. No-Run attempt unification, physical storage enforcement, target Linux physical acceptance and the default lifetime transition remain open.
