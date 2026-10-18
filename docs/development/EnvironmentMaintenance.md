# Environment maintenance authority

Maintenance operations have their own typed owner and never create an AgentRun
or reuse an expired AgentRun grant. The internal issuance and release endpoints
require the independent environment maintenance token; the ordinary internal
token alone cannot authorize them.

`POST /internal/environments/{environmentId}/maintenance-authorizations` accepts
schema `workspace.environment_maintenance.issue.v1`, `operationId`,
`controllerPurpose`, `expectedControllerEpoch` and the exact `request` object.
The server derives workspace, Session, instance, container, host and profile
identity from immutable allocation and current resident records. `profileDigest`
binds the allocation's retained topology digest, not a task-specific profile
digest. Runtime must separately verify the physical container before dispatch.

Issuance locks Workspace, Session, Environment, operation and grant in that order.
Allocation reads use `NOWAIT` to avoid reversing Runtime's allocation lock order.
AgentRun and maintenance ownership share the Environment epoch and are mutually
exclusive. A new grant has a server-controlled five-minute deadline. Expiry never
releases ownership or starts the request again. Exact execution retries return
the original signature; Runtime rejects expired or superseded execution grants.

Explicit cleanup recovery after expiry appends a grant with a new epoch for the
same immutable request and target. It cannot become new execution authority.
An existing completion prevents renewal: matching completion returns
`maintenance_completion_available`, and conflicting completion fails closed.

Release uses
`POST /internal/environments/{environmentId}/maintenance-operations/{operationId}/release`
with schema `workspace.environment_maintenance.release.v1`, `controllerEpoch`
and `controllerLeaseDigest`. The API only releases after finding the exact
Runtime-owned `environment_maintenance_completions` record for the current grant,
with `operationEnded` and confirmed task cleanup. The caller cannot provide the
completion record. Lost release acknowledgements replay after a later controller
takes ownership. This business release is distinct from instance destruction or
physical resource release.

Migration `0022_environment_maintenance_authority` preserves existing Run
controller history and introduces separate operation and append-only grant
records. Reverse migration is refused once maintenance history exists. Tests use
an isolated PostgreSQL database; applying the migration to a deployment requires
coordinated matching Runtime consumers and admission control.
