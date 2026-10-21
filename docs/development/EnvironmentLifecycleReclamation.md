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

## Capacity release scheduling contract

The capacity-wake candidate adds Runtime schema 16 with an explicit forward migration from 15. Before attempting resident admission, the current execute owner durably registers the resource-release generation it observed. A transaction which actually deletes a resident holding advances a singleton generation and notifies `runtime_job_ready_v1`. The notification carries no permission: readiness and ordinary claim compare durable generations, then retain the existing global/Workspace execution limits, lease fencing, reclaim embargo and control exclusion. The waiting Job remains the same Job.

The release transaction does constant-size signaling work and does not scan or lock waiting Jobs. A release preceding yield remains visible afterward; missed notifications and Runtime restart do not erase eligibility. A new claim consumes the registration and records the release/claim timestamps; a non-capacity yield clears it. A subsequent failed admission registers the latest generation and waits for another release or the existing periodic fallback. An Unknown physical outcome does not remove its holding and supplies no release signal. Protected retained environments ending a Run keep their holding and supply no release signal either.

PostgreSQL [LISTEN](https://www.postgresql.org/docs/current/sql-listen.html) requires committing the subscription before inspecting current state; the existing listener follows this order. [NOTIFY](https://www.postgresql.org/docs/current/sql-notify.html) is delivered after commit and serves as a hint to inspect durable state. The generation and holding update share one transaction, so a rollback exposes neither a capacity change nor a committed signal. Old queued Jobs without a registered attempt retain their existing fallback deadline until a new attempt records one. Deploy matching schema-aware Runtime binaries; this migration does not establish mixed-version rolling compatibility.

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
