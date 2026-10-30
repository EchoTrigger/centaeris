# Session environments

Every Session has a stable logical environment. Each AgentRun captures that
environment; child work Sessions receive their own. The environment is the
execution context, and an AgentRun is one use of it. `perRun` and `retained`
select physical lifetime within the same environment implementation.

## Identity and profile

`Session.current_environment` selects an environment owned by that Session.
`AgentRun.environment` captures it at creation. PostgreSQL protects these
associations from model, bulk and raw changes. Session and environment creation
commit in one transaction; a missing association is an error, not a read-time
repair. Workspace membership and delegation remain Session authority.

An immutable `runtime.environment_profile.v1` holds image digest,
`workspaceRoot`, numeric `user`, `network` and resources. Ordinary first use
freezes the authoritative image/defaults. Independently authorized maintenance
may bind an unused environment to explicit values before provisioning it.
Harbor uses this same profile and lifecycle. Profile digests bind the complete
configuration, and existing physical instances must match that digest.

Session responses expose `currentEnvironmentId` and `executionMode`.
`GET /api/sessions/{sessionId}/agent-runs/{agentRunId}/environment` returns the
Run's immutable association with current access checks and `Cache-Control:
no-store`. It performs no writes. Resetting a Session's logical environment is
not part of this API.

## One current authorization contract

`workspace.agent_run_authorization.v1` signs the Run, Session, complete
environment profile and its digest. The independent
`workspace.environment_authorization.v1` additionally binds the selected
environment, original Run grant digest, lifetime policy and controller fence.
Start, step and teardown each implement one current v1 structure for both
lifetime policies. Current fields, signatures and permissions are validated at
their normal boundaries.

API and Worker acquire the environment controller before dispatch. Its typed
owner is either `agentRun` or `maintenanceOperation`; epoch, lease digest and
purpose fence each operation. Runtime checks both signed and current persisted
authority before a host operation. Teardown receives cleanup authority rather
than reconstructing a start request. Cancel and supplement carry the original
signed Run identity; they do not need authority to execute in the environment.

## Controller contention and delivery recovery

Lifecycle authorization shares the Workspace and original membership rows with
Runtime's authorization readers. Session, environment, Run and grant mutations
retain exclusive locks. Lifecycle row-lock acquisition uses NOWAIT; only confirmed
PostgreSQL lock contention becomes `environment_controller_busy`, after the
transaction has rolled back. Runtime retains its authorization locks through the
physical operation, including protection against non-key status and role changes.

An exact controller busy/owned response makes the Worker yield the same Job at
initial resolution, after Core terminal, and during cleanup or release. Yield
preserves the Run binding and does not increment failure retries. A committed
Core terminal remains terminal; a later claim resumes cleanup rather than stepping
the model again. Authorization rejection and unknown transport outcomes do not
become controller contention.

A yield request freezes its identity before delivery. If its acknowledgement is
lost, the Worker makes a bounded recovery attempt with the identical request
within the last confirmed lease window. Only a successful acknowledgement confirms
delivery; a queued-state observation alone does not. The old owner stops on lease
loss or unconfirmed handoff and does not synthesize a Run failure. Any unresolved
lease follows the existing independent lease-reclaim policy. The initial queue
expiry policy remains independent of both failure retries and lease reclamation.

## Allocation and host execution

Core selects execution IDs and owns Run/recovery semantics.
`runtime.execution_instances` preserves immutable allocation identity, including
environment, Session, host, original grant and profile digest. Runtime schema 13
stores a typed allocation owner: provisioning can allocate an instance before
any AgentRun exists. A later Run may use that instance only after current
authority, physical inventory and immutable configuration checks.

`resident_sandboxes` owns physical liveness and capacity accounting. Reservation
precedes creation. Exact retries recover the same allocation, and unknown
creation outcomes retain their debit. Container adoption checks exact identity,
profile, network and security facts. Physical labels describe the original
allocation even when a later controller uses it.

Retained preparation acquires an environment gate before global admission.
Global admission covers inventory reconciliation, reservation, the durable create
dispatch record, resume/create and physical identity confirmation. Runtime commits
the confirmed resident and running charge before releasing that global lock.
Controller takeover and workspace preparation then proceed independently across
environments, with the environment gate retained and current controller, lease
and exact instance association revalidated and pinned through the host operation.
A failure in this later stage keeps the confirmed instance and its resource debit.
Cleanup and retirement acquire the same environment gate before global admission.

Reconciliation does not rewrite an unchanged resident fact. If a resident missing
from inventory is still pinned, reconciliation defers its deletion and retains its
charge. Neither a skipped row nor a released gate proves resources are refundable.
Preparation availability advances the existing durable eligibility clock; it is a
hint to retry admission, never execution authority. An environment contender that
cannot immediately update its pinned Job's wait marker retains the existing timed
retry instead of blocking behind the active operation. The shared preparation
wake pool does not identify which particular gate is available.

Opt-in preparation observations separate environment/global acquisition, global
hold, inventory, reconciliation/reservation, physical confirmation, takeover and
workspace preparation. The acceptance report retains only timings associated with
its accepted Jobs. Missing or failed collection is `notObserved`; nested phase
intervals must not be added together. These timings explain delays independently
of the sustained physical-concurrency acceptance gate.

The native [host supervisor](../../packages/hosted_execution/deploy/README.md)
owns process launch, task journals and output. It attests the Docker/runc target,
runs commands with the profile's identity and security configuration, and
loads fixed helpers from a protected root-owned installation path and seals
their bytes, calculating execution/replay identity internally. Workloads
receive no host control mount or writable cgroup mount. Filesystem operations,
inputs, artifacts, snapshots, hooks and MCP use this common execution path.
Missing workspace directories are prepared for the profile user while image
content and existing permissions are preserved.

Task ownership and scope are separate. Run tasks end with their Run; maintenance
tasks end with their operation; Environment tasks can survive between Runs in a
retained instance. Leader exit, complete output and empty process domain are
independent observations. A controller disconnect or lost acknowledgement
does not authorize replay of accepted work.

## Completion and lifetime

Both lifetime policies use the same controller, task cleanup and durable receipt
pipeline. `perRun` additionally removes the exact physical instance, independently
confirms absence and releases its debit before cleanup confirmation. `retained`
keeps the verified instance for later use. Permanent environment retirement is
a separate operation.

Both physical-removal paths atomically enqueue native Host retirement in the
existing Runtime job store. A Host-local worker retries unavailable, busy or
unconfirmed responses; only an exact `Retired` acknowledgement completes the job.
Every attempt rechecks the immutable physical-removal facts and absence of a
live resource holding. A lost acknowledgement or Runtime restart therefore keeps
the cleanup obligation. Startup performs one bounded keyset traversal to recover
older completed facts; periodic work drains only pending jobs, including expired
leases, without repeatedly sweeping history. Host independently verifies the
exact container is absent before removing its private state. This does not
reconstruct workloads or change the physical cleanup receipt.

API controller release reads the matching Runtime cleanup receipt. Repeated
release requests recover the same receipt, including after a later takeover.
An expired execution lease only permits independently acquired cleanup recovery.
Hot attachment uses live files instead of restoring an older checkpoint.
Host loss does not reconstruct live processes or missing exit status.

Provision and maintenance completion follow the same ownership rules; see
[maintenance authority](EnvironmentMaintenance.md). `/internal/environments/observe`
reports environment allocation and physical facts independently of AgentRun
status.

## Persistence and upgrades

Migration `0016_session_environments` established immutable Session/Run
associations and preserves historical signed grants. Migrations `0017` through
`0022` retain their controller, release, snapshot, profile and network history.
`0023_unified_environment_profile` reconstructs known profile records while
retaining their original evidence. `0024_environment_provision_authority` and
`0025_maintenance_grant_digest` add independent provision ownership and immutable
maintenance grant digests. Runtime schema 13 retains prior allocations and
resource reservations.

Historical reconstruction is explicit migration code. Current execution readers
define only the current structure. Signed historical payloads are never silently
rewritten or inferred from current settings. Drain old live and waiting instances
under their matching stack before switching API, Worker, Runtime and supervisor
together. Preserve identity and receipt history during forward repairs.

## Verification

Focused coverage includes Session creation, child isolation, access revocation,
immutable profile binding, Python/Rust signature parity, typed allocation owners,
controller fencing, lost acknowledgements, receipt replay and PostgreSQL forward
migrations. Disposable native-host acceptance exercises process identity,
background scopes, output completion, helper byte transfer and cleanup. The
[Workspace release gate](../workspace/eval/ReleaseGate.md) records the integrated
requirements for a release.
