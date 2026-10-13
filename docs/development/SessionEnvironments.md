# Session environments

Every Session has a stable logical environment. Creating another Run in that
Session captures the same environment; creating a child work Session allocates a
separate one. `SessionEnvironment` is a business identity, not a container or a
lease. Execution remains `perRun`: this change does not keep processes alive,
reuse physical instances, or change snapshot storage and reclamation.

## Persistence and admission

`Session.current_environment` selects an environment whose `session_id` must match
the Session. `AgentRun.environment` captures that selection at creation. The Run's
Session and environment binding, and an environment's Session binding, cannot be
changed. Environment ownership and workspace membership come from the existing
Session; they are not duplicated in another authority record.

The Session model creates its first environment in the same transaction as the
Session. This covers ordinary HTTP creation, first-message creation, coordination
Sessions, business branches, child work, and direct application model creation.
The Run model locks its Session and captures the committed current environment;
all message, input-dispatch, child-work, and work-return admissions share that
boundary. A missing environment fails closed. Neither reads nor authorization
repair missing records.

The nullable current-environment column exists only to permit the circular insert:
Session, environment, then pointer. PostgreSQL checks the final stored Session at
transaction commit, not each intermediate value. It rejects a missing or foreign
pointer. Additional PostgreSQL triggers protect Run and environment identity from
QuerySet updates, bulk updates, and raw SQL; normal title, unread, and Run status
updates do not invoke those binding triggers. SQLite supports structural migration
checks only and is not an execution-authorization backend.

No reset endpoint is provided. Future reset semantics must allocate a new logical
environment and explicitly define its file baseline before moving the pointer.
Existing Run bindings remain fixed; changing physical instances alone must not
change the logical identity.

## Authorization and API

The signed `workspace.agent_run_authorization.v1` payload and signature domain are
unchanged. The signature already binds `sessionId` and `agentRunId`. Their database
environment association is immutable, and the additional check only rejects a
missing or inconsistent association; it grants no new Runtime authority.

The authorization factory checks the association before issuing a grant.
`agent_run_binding_matches` checks it for existing shared consumers: Runtime
startup/scheduling, deferred input reads, workspace resolve/commit/stage/download,
model execution, platform MCP authorization, agent messages, and work validation
and queries. Other existing hand-written authorization checks (including artifact
publication, connector credentials, and `AgentRunAuthorization.save`) have not
been refactored into this helper. PostgreSQL still protects the stored association
for those paths. Current membership, delegation, lease, and input identity checks
remain independently required; an environment identifier is not a capability.

Session responses include `currentEnvironmentId` and `executionMode: "perRun"`.
`GET /api/sessions/{sessionId}/agent-runs/{agentRunId}/environment` returns the Run's
fixed `environmentId`, Session and Run IDs, and the same execution mode. The route
requires current Session access and delegated scope, rejects mismatched Run/Session
identities, and uses `Cache-Control: no-store`. It performs no writes. Hosted
operation receipts and transcript stream protocols are unchanged.

The internal `workspace.agent_run.start.v3` envelope requires an independent
`environmentAuthorization` containing `payload` and `signature`. Its strict
`workspace.environment_authorization.v1` payload contains `environmentId`,
`sessionId`, `agentRunId`, `agentRunAuthorizationDigest`, and `executionMode:
"perRun"`. The API derives it from the immutable Run association after checking
the existing authorization. Signing canonical JSON uses the independent
`workspace:environment-authorization:v1\0` domain followed by the `sha256:`
prefixed payload digest. Runtime verifies both signatures, Run/Session/digest
agreement, exact fields, and supported mode before admission. Old start versions
fail closed; the original v1 grant is never extended or re-signed.

## Physical allocation

Core continues to select execution IDs and own Run/recovery semantics. The hosted
adapter binds an execution to a distinct `instanceId`, its logical environment,
Session, Run, host, immutable image digest, original authorization digest and
environment binding digest in `runtime.execution_instances`. The instance ID is
the domain-separated SHA-256 of the execution ID. This is an allocation record;
it does not establish that a container was created, is running, or was released.
Successive executions, including Core recovery replacements and different Runs
in one environment, receive different instance IDs.

The existing resident budget reservation and live admission callback run first.
Before any container create/adoption, the adapter checks authoritative stored Run
environment/Session and original grant digest, inserts the allocation, compares
every stored binding field on retry, and commits with synchronous durability.
Concurrent retries converge on the same allocation; a different environment,
Run, Session, host, grant or image fails without a second create. Mapping records
survive resident release so an old execution cannot be reinterpreted. They have
no lease, heartbeat, resource totals or lifecycle state.

`resident_sandboxes` remains the sole physical resource/liveness authority and
retains reservations on unknown creation outcomes. Docker receives environment,
instance and binding-digest labels alongside existing Run/execution/grant labels.
Create/adoption verifies all requested labels and security configuration; missing
or foreign labels fail closed. Backend container IDs remain in resident rows.
Host and physical-instance IDs are internal and never become browser/model
capabilities. Teardown remains per Run; no cross-Run reuse or supervisor is added.

## Upgrade and rollback

Migration `0016_session_environments` creates the identity model, expands the
nullable bindings, allocates one environment per existing Session, fills every
Run binding, makes the Run binding non-null, and installs PostgreSQL protection.
Deleted, purged, and never-executed Sessions are included. Authorization payloads,
digests, signatures, operation receipts, and workspace file heads are untouched.

Stop all old API admission writers, including worker-driven input and work-return
admission, before applying this migration and starting the new application. An old
application cannot insert a valid Run after the non-null binding constraint is
installed. This is a coordinated upgrade, not a rolling mixed-writer deployment.
Back up the database and verify migration duration on representative data first;
the current metadata backfill is atomic and performs per-Session updates.
Ship the matching Web build with this API: the work-session reader validates
the complete Session field set, so an older open client must reload after the
coordinated upgrade.

The migration refuses reversal when any environment exists, before removing its
protection. A code rollback must keep these fields and the creation/authorization
binding behavior or use a tested identity-preserving forward repair. Dropping the
schema would destroy already exposed identities and is not a safe rollback.
Empty-schema reversal remains supported for structural validation.

For the coordinated upgrade rehearsal, retain queued and running Runs, including
one with `waiting_for_durable_input`, in an isolated database at migration 0015.
Apply 0016 and compare their status, start time, transition reason, signed payload,
digest and signature byte-for-byte. Every Run must receive its Session's binding;
an insert through the old model must fail instead of creating an unbound Run.
`test_session_environment_migration` exercises these conditions and repeat upgrade
and refused reversal. It does not measure production migration duration.

Inventory live and waiting executions before switching Runtime/tool images.
Waiting is not drained: leave such executions on their matching Runtime/image
until existing resume/terminal policy completes them, or postpone the switch.
There is no automatic conversion or destruction of waiting sandboxes in this
upgrade. An operator must establish that no old-helper instance will be adopted
by the replacement Runtime. This operational gate is separate from the database
migration test and requires the deployment's actual instance inventory.

API and Runtime must switch together to start v3. Queued starts are reconstructed
from authoritative API records using their unchanged original grants. Runtime
schema v8 adds the allocation table through a forward migration; it preserves all
v7 resident reservations and migration history and does not fabricate allocations
for old containers. Old live/waiting containers lack the required labels and
cannot be adopted by this Runtime. Complete them under their matching old stack
or postpone the switch; this code does not automatically terminate them.
Rollback to a v7 Runtime fails schema validation. Preserve allocation identities
and upgrade history with a compatible forward repair or restore an operator-owned
pre-upgrade backup after draining; do not drop mapping rows to permit adoption.

## Verification

Focused tests cover HTTP creation and replay, successive Runs, distinct child
Sessions, owner/delegation revocation, read-only access, model creation failure and
retry, shared-consumer rejection, PostgreSQL bulk/raw immutability, deferred final
state, and preservation of signed historical records during migration. Existing
signed v1 fixtures remain the wire-compatibility check. Query-count coverage checks
Session list growth and the platform credential path; environment joins avoid
adding an authorization round trip on those paths.

Creation has an explicit cost: each new Session inserts an environment and updates
its pointer in the same transaction; each new Run locks and reads its Session with
the selected environment. PostgreSQL also executes the binding checks. The query
budget tests cover existing title/status updates and platform credential reads;
they do not establish zero added SQL for all operations.

Environment authorization projection writes no database records. Allocation uses
one indexed Run/grant lookup, one insert-on-conflict and one primary-key lookup
per physical prepare, with bounded immutable comparison; it scans no tool calls.
The table grows with executions and has no automatic history deletion policy.
Shared Python/Rust signature vectors, strict start rejection, real PostgreSQL
independent-connection/concurrent replay, unknown-create budget retention and
forward migration/drift tests protect this boundary. Docker request/adoption
fixtures verify label behavior; they do not certify real isolation or deployment.
