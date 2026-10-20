# Environment lifecycle reclamation

Queued cancellation and physical resource settlement must continue independently of ordinary execution capacity. Complete that lifecycle before making retained environments the default for ordinary Sessions. Execution limits remain eight globally, four per Workspace and eight slots per Worker process.

## Observed gaps and expected behavior

| Boundary | Current evidence | Required behavior |
| --- | --- | --- |
| Workspace execution capacity exhausted | Four running lifecycle owners remain unchanged; cancellation intent persists, but the cancelled queued job is neither ready nor claimable. | An independent bounded control path commits the cancellation without waiting for an unrelated execution slot or granting execution authority. |
| Global execution capacity exhausted | Eight running owners across two Workspaces remain unchanged; a cancelled job in an otherwise empty third Workspace is neither ready nor claimable. | The same control progress must hold independently of the global execution limit. |
| Failure before physical creation | Allocation and reservation commit, the pre-create callback fails, and physical creation is never called; one reservation remains. | Preserve allocation history and atomically settle the proven unmaterialized attempt, its holding and subsequent obligations. |
| Cleanup proof lost at the host boundary | A controlled host callback removes its own synchronous attempt but returns only a string failure. The ledger retains the reservation. | Carry typed physical evidence to durable settlement. A string error or an empty inventory cannot authorize release. |
| Historical reserved allocations | Earlier records may have no attempt evidence or container ID. | Recover a durable investigation obligation; never infer non-creation from age, lease expiry or an empty observation. |
| Workspace storage budget | The current container request carries the data budget label but no explicit Docker tmpfs or storage option. | Establish the actual writable backing and enforce the declared byte/inode contract at the physical boundary. |

The database regressions are in
[`resource_lifecycle_tests.rs`](../../packages/runtime_server/src/postgres_store/integration_tests/resource_lifecycle_tests.rs).
The two capacity tests isolate the storage claim/ready boundary. They do not establish a complete Worker cancellation, teardown and release result. The pre-create settlement test currently fails; it is an opt-in regression, not a passing release gate.

Existing unknown-create regressions continue to require charged resources and adoption of the exact late instance after reopening. Any settlement fix must preserve those protections. A known failure and an unknown physical outcome must never share an unconditional release path.

## Workspace creation is a separate boundary

A bare Docker create/start probe does not perform the host's workspace preparation. The native `prepare-workspace` helper creates a missing workspace for the profile's numeric user and preserves existing image content and permissions. Absence of `/mnt/data` immediately after bare container creation is therefore not, by itself, an established product defect.

Acceptance must exercise the real preparation and task path before judging workspace availability. It must then identify the workspace mount/backing, enforce its configured limits, verify file visibility across supported lifecycle operations, and distinguish workspace data from temporary directories. A write to `/tmp` cannot certify the `/mnt/data` contract.

The opt-in `export_owned_runsc_storage_probe_request` test exports a request from the production container request builder for an explicitly owned physical probe. Probe drivers, private configuration and artifacts remain outside the source checkout. A separate Docker daemon must also run in a separate Linux network namespace; a different socket/data root alone does not isolate Docker's host network management.

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

The focused Core query-loop gate passed all 129 selected tests. Rust test compilation, request export, formatting and whitespace checks passed. Production implementations and lifetime defaults have not been changed in this initial regression-only candidate. Full terminal/control-lane behavior, historical migration, actual workspace quota enforcement and target-host acceptance remain open.