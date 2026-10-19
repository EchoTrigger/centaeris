# Environment maintenance authority

Provision, setup, verification and file transfer use a typed maintenance operation
in the Session's environment. They acquire the same controller as an AgentRun,
with `maintenanceOperation` ownership instead of a placeholder Run. Issuance and
release require the independent environment maintenance token.

## Profile and provisioning

`POST /internal/environments/{environmentId}/environment-profile` binds an unused
Session to its immutable `runtime.environment_profile.v1`. The six profile fields
are schema, image digest, workspace root, numeric user, network and resources.
`profileDigest` always means the canonical digest of that complete profile.

`POST /internal/environments/{environmentId}/provision-authorizations` issues
`workspace.environment_provision.issue.v1` for `operationId`, `sessionId` and
`expectedControllerEpoch`. The server supplies the profile, Session user, Agent
and plugin activation from authoritative records. A provision grant requires no
physical instance or container to exist yet.

Runtime receives the signed payload and request at
`POST /internal/environments/provision`. It reserves capacity, allocates a typed
maintenance-owned instance, creates and attests the container, prepares missing
workspace directories, and writes `environment_provision_completions`. Release
requires that exact immutable receipt. An uncertain create retains its debit.

`POST /internal/environments/observe` accepts
`runtime.environment.observe.v1`, `sessionId` and `environmentId`. Its result
reports the profile, controller, allocation, physical state and resource holding.
No AgentRun is required to inspect setup or provisioning.

## Task operations

`POST /internal/environments/{environmentId}/maintenance-authorizations` accepts
`workspace.environment_maintenance.issue.v1`, `operationId`, `controllerPurpose`,
`expectedControllerEpoch` and the exact request. The server derives the existing
instance, container, host and profile identity from stored allocation facts.
Runtime rechecks current authority and the physical target before dispatch.

Issuance locks Workspace, Session, Environment, operation and grant in that order.
Allocation reads use `NOWAIT` to preserve lock order. Exact retries return the
original signed grant and deadline. Command `timeoutMs` is a positive integer up
to one hour; its lease lasts the greater of five minutes or that timeout plus
30 seconds. Provision and filesystem grants last five minutes. Expiry retains
ownership until explicit cleanup; it does not dispatch work again.

Commands use `POST /internal/environments/maintenance/task`. File operations use
`POST /internal/environments/maintenance/filesystem`, with a four-byte big-endian
header length, JSON authorization and filesystem metadata, then raw content.
Read results use the corresponding metadata-and-bytes frame. Length and digest
checks bind the complete payload. Maintenance filesystem operations run as the
profile user in the container filesystem; model filesystem operations retain
their Core workspace policy. Host control paths remain inaccessible.

## Completion and release

Runtime records an operation completion only after confirmed scope cleanup.
The filesystem endpoint closes its operation and records the receipt before
returning the framed result. Exact retries recover the same operation's result.

Release uses
`POST /internal/environments/{environmentId}/maintenance-operations/{operationId}/release`
with `workspace.environment_maintenance.release.v1`, `controllerEpoch` and
`controllerLeaseDigest`. The API reads the exact Runtime completion; callers
cannot supply their own receipt. Lost release acknowledgements replay after a
later controller takes ownership. Controller release is separate from instance
retirement and physical resource release.

Cleanup recovery after expiry acquires a new epoch for the same immutable
operation and target. It authorizes cleanup and observation only. An existing
completion returns `maintenance_completion_available` rather than extending
execution authority.
