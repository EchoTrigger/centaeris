# Environment lifecycle reclamation

Callers submit Agent invocations; the platform owns queuing, admission, environment reuse and reclamation. Reliable resource facts are the prerequisite for automated placement and elastic capacity supply.

Queued cancellation and physical resource settlement must continue independently of ordinary execution capacity. Complete that lifecycle before making retained environments the default for ordinary Sessions. Execution limits remain eight globally, four per Workspace and eight slots per Worker process.

## Observed gaps and expected behavior

| Boundary | Current evidence | Required behavior |
| --- | --- | --- |
| Workspace execution capacity exhausted | Four running lifecycle owners remain unchanged; cancellation intent persists, but the cancelled queued job is neither ready nor claimable. | An independent bounded control path commits the cancellation without waiting for an unrelated execution slot or granting execution authority. |
| Global execution capacity exhausted | Eight running owners across two Workspaces remain unchanged; a cancelled job in an otherwise empty third Workspace is neither ready nor claimable. | The same control progress must hold independently of the global execution limit. |
| Failure before physical creation | The initial regression left a reservation charged after a fresh pre-create callback failed. The undispatched settlement below now closes this case. | Preserve allocation history and atomically settle the proven unmaterialized attempt, its holding and subsequent obligations. |
| Cleanup proof lost at the host boundary | A controlled host callback removes its own synchronous attempt but returns only a string failure. The ledger retains the reservation. | Carry typed physical evidence to durable settlement. A string error or an empty inventory cannot authorize release. |
| Historical reserved allocations | Earlier records may have no attempt evidence or container ID. | Recover a durable investigation obligation; never infer non-creation from age, lease expiry or an empty observation. |
| Workspace storage budget | The current container request carries the data budget label but no explicit Docker tmpfs or storage option. | Establish the actual writable backing and enforce the declared byte/inode contract at the physical boundary. |

The database regressions are in
[`resource_lifecycle_tests.rs`](../../packages/runtime_server/src/postgres_store/integration_tests/resource_lifecycle_tests.rs).
The two capacity tests isolate the storage claim/ready boundary. They do not establish a complete Worker cancellation, teardown and release result. The pre-create settlement regression now passes and is included in the required PostgreSQL gate.

Existing unknown-create regressions continue to require charged resources and adoption of the exact late instance after reopening. Any settlement fix must preserve those protections. A known failure and an unknown physical outcome must never share an unconditional release path.

## Undispatched allocation settlement

A fresh AgentRun allocation commits its holding and one creation attempt before the pre-create callback. The attempt's immutable allocation reference binds instance, host, origin grant and profile. A callback failure may close only this fresh, still-prepared attempt. Under current controller and lease authority, recording `notDispatched` and removing the exact reserved holding commit together. Allocation history remains intact. The same failure while reusing a resident changes neither the original holding nor its physical instance.

Successful preparation commits `dispatched` before entering the host callback, then reacquires authority pins across the physical operation. Failures after this conservative boundary retain their charge, including a crash before the actual send or an error string from a callback that privately removed its object. A possible dispatch cannot be rewritten as non-dispatch.

Cleanup recognizes an unmaterialized allocation by durable evidence rather than treating it as `NeverAllocated` or environment loss. Its receipt retains the planned instance identity without claiming physical deletion. Both lifetime policies replay that receipt after reopening. Observation reports `notMaterialized`, and a later Run can obtain a fresh allocation without recreating the old execution.

Runtime schema migration 14 adds the dispatch journal without inventing evidence for historical reservations. Historical unknowns remain charged and cannot dispatch again through ordinary AgentRun preparation. No-Run provision dispatch, typed post-create removal evidence, interrupted-attempt investigation and the independent cancellation control path remain follow-up boundaries.

## Workspace creation is a separate boundary

A bare Docker create/start probe does not perform the host's workspace preparation. The native `prepare-workspace` helper creates a missing workspace for the profile's numeric user and preserves existing image content and permissions. Absence of `/mnt/data` immediately after bare container creation is therefore not, by itself, an established product defect.

Acceptance must exercise the real preparation and task path before judging workspace availability. It must then identify the workspace mount/backing, enforce its configured limits, verify file visibility across supported lifecycle operations, and distinguish workspace data from temporary directories. A write to `/tmp` cannot certify the `/mnt/data` contract.

The opt-in `export_owned_runsc_storage_probe_request` test exports a request from the production container request builder for an explicitly owned physical probe. Probe drivers, private configuration and artifacts remain outside the source checkout. A separate Docker daemon must also run in a separate Linux network namespace; a different socket/data root alone does not isolate Docker's host network management.

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

The first demonstrated obstruction is execution-capacity eligibility at ready/claim: cancellation is acknowledged promptly but cannot obtain processing while ordinary execution is full. Releasing one unrelated execution owner removes that obstruction immediately. The next implementation boundary is a persisted, capability-restricted control lease and corresponding ready/claim eligibility, followed by a real Worker control path. This experiment does not measure physical cleanup or Job completion latency.

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

The two saturated-capacity cancellation regressions were executed separately and still fail at `ready=false, claimed=0` while the original four/eight execution owners remain running. They are unfinished control-path acceptance, not part of the passing settlement result. No-Run attempt unification, complete enforcement, full service-chain cancellation, real storage enforcement, target Linux physical acceptance and the default lifetime transition remain open.
