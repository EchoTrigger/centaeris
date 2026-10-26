# Workspace resource administration

Workspace membership and platform administration are separate authorities.
`WorkspaceMembership.role` is `owner`, `admin`, or `member` for one workspace.
The Django user's `is_superuser` flag identifies a platform operator; inviting
an administrator creates an `admin` membership and does not change that flag.
The role is read from the current membership on each authorized request, so a
new invitation does not depend on stale browser or login-session role flags.

## Role origins

This release provides no local self-registration/signup endpoint. Accepting a
workspace invitation creates an ordinary user account when needed and assigns
that workspace's invitation role (`admin` or `member`); it does not create an
owner membership or set `is_superuser`. The explicit `bootstrap_superadmin`
command creates the platform operator and the Default workspace's owner
membership. Ownership transfer remains a current-owner operation with password
reauthentication.

## Management boundary

| Capability | Workspace owner/admin | Workspace member | Platform operator |
| --- | --- | --- | --- |
| Install/update/remove a workspace Plugin ZIP, including Skills and MCP declarations | Own workspace | Denied | Any active workspace through its scoped resource API |
| Enable/disable an available Plugin | Own workspace | Denied | Scoped resource administration |
| Create/edit workspace model providers, models, and replacement credentials | Own workspace | Denied | Any active workspace through its scoped resource API |
| Read existing credential plaintext through management APIs | Denied | Denied | Denied |
| Consume enabled shared/workspace models and Skills | Current membership and existing usage authorization | Same usage authorization | Platform authority does not manufacture a membership |
| Mutate platform Plugin packages, providers, models, or credentials | Denied | Denied | Explicit platform administration APIs |
| Invite members or adjust non-owner roles | Existing owner/admin membership rules | Denied | Existing membership rules remain separate |
| Transfer ownership or remove/demote the current owner | Transfer requires current owner and password reauthentication; ordinary member mutation cannot remove an owner | Denied | Platform resource authority does not bypass ownership transfer |
| Delete a workspace | This change adds no deletion workflow; deletion remains an owner-only boundary | Denied | No new deletion authority |

The backend pins the current workspace and the actor's authority during scoped
mutations. Supplied workspace IDs and resource IDs must identify that same
workspace. UI visibility is a convenience; HTTP and service authorization enforce
the boundary. Generic workspace membership helpers do not gain a superuser
bypass from resource administration.

## Shared resources and local overrides

Existing platform packages, providers, models, and MCP credential metadata remain
shared. Workspace settings show their metadata as read-only. Platform settings
use separate platform operator entries and `/api/admin/...` routes.

Workspace resource settings use `/api/workspaces/{workspaceId}/...` routes.
Workspace model selection reads `/api/models?workspaceId={workspaceId}` after
checking current membership. Calling the catalog without a workspace returns the
platform catalog, without exposing another workspace's local models.

To use a different model provider configuration, create a workspace provider
from a template or copy the shared provider's public configuration and supply a
new key. Create the required local models for a custom provider. Neither action
copies or retrieves the platform's existing secret. Local MCP credentials take
precedence over the platform credential with the same Plugin name and credential
reference, only inside their owning workspace.

Upload a Skill ZIP or complete Plugin ZIP to workspace Plugin settings. A Skill
ZIP contains a root `SKILL.md` or one enclosing Skill directory; the extractor
wraps it in a validated Plugin. Its directory name or ZIP basename must be a
canonical lowercase kebab name. Plugin ZIPs keep the supported manifest layout
and may contain Skills, MCP declarations, or hooks. The Skills library links
administrators to package management. Reupload
updates the workspace installation after package validation and active-run
checks. Removing a local installation disables that workspace's enablement;
it does not remove a shared platform package or another workspace's installation.
Workspace-local package content remains immutable for existing signed references.

For workspace-local installations, an existing retained environment keeps its
original mounted package version, including while paused. Updating the installation does not replace the original
container or discard its memory. A Run requesting a different package version
in that environment must be rejected explicitly; start a new environment or
conversation to use the updated package. Retirement of the old environment is a
separate lifecycle action. This immutable-version guarantee covers workspace-local
installations. Legacy platform catalog replacement retains its existing maintenance
boundary and is not covered by workspace-local version continuity.

## Credentials

Management responses contain identifiers, configuration status, versions, and
scope metadata. They never return an existing secret. Replacement forms start
empty, remain masked, and are discarded when switching workspaces. Saving a
replacement requires a newly supplied key. Local create/rotate/delete endpoints
cannot mutate a platform credential by passing its ID. Secret decryption remains
inside the existing authorized execution/connector path, separate from resource
management. Audit records describe the action without secret values.

## Forward migration

`0032_workspace_resource_scope` follows `0031_merge_lifecycle_and_presentation`.
It adds nullable workspace ownership to model providers and MCP credentials,
adds explicit workspace Plugin installation bindings, and introduces uniqueness
constraints for the platform and workspace scopes.

Existing provider and credential rows retain `workspace = NULL`, meaning
platform-shared. Their IDs, encrypted secret bytes, versions, model relationships,
connector references, users, membership roles, and owners stay intact. Existing
Plugin catalog packages remain platform-shared and existing workspace enablement
is preserved. No ownership is guessed from a creator, most recent user, or page
name. New local resources require an explicit workspace.

The migration does not promote existing administrators, create replacement
owners, delete rows, clear data, or copy shared secrets into tenant records.
Preserving the platform fallback preserves reasonable existing access without
allowing tenant admins to mutate a shared secret. Converting an existing shared
resource into an exclusively tenant-owned resource needs a separately reviewed
ownership decision and access-impact assessment.

Rollout must coordinate the migration and matching API/Runtime readers and
writers; an older process must not continue interpreting workspace-owned rows as
global. Verify migration fixtures preserve existing identities and ciphertext,
then verify owner/admin/member/operator and cross-workspace allow/deny behavior,
new-admin invitation persistence, local precedence, secret redaction, and package
resolution. Deployment, production migration execution, reconstruction, and
rollback are separate operational approvals. This document supplies no data
clearing or production execution command.

## Coordinated deployment boundary

Apply schema migration 0032 and switch the matching API, Worker, and Runtime
consumers together. Drain the old API and Worker consumers before enabling any
workspace-local credential writes. Older scope-unaware lookups such as
`filter(plugin_name=..., credential_ref=...).first()` can select a newly created
tenant credential as if it were global. Old and new consumers must not coexist
after local credentials are introduced.

Runtime must understand immutable digest-addressed Plugin paths before local
packages are used. Verify the matching Runtime adapter is running before making
workspace package installations available to executions. This transition neither
promotes administrator flags nor clears existing data. Production execution and
any separate rollback plan still require explicit operational authorization.

## Platform workspace resource browser

A platform superuser can open `/settings/workspace-resources`, select an active workspace, and manage its local models, packages and credentials through the same scoped APIs. The operator-only metadata endpoint returns workspace IDs, names and status. It does not create memberships or grant access to private Sessions, Agents or Library objects. Ordinary workspace owners and administrators cannot use this cross-workspace browser.


## Retained deployment history

Native deployments retain the released snapshot-publication and maintenance-target
schemas (0027, 0028 and their 0031 merge) before applying 0032. Their rows, signed
payloads, provider targets, Session history and files are archival facts; the
native API contains no producer or resumer for those previous provider operations.
Every snapshot-publication reference retains its bytes, including committed rows,
and ordinary GC cannot remove them after a Session purge.

`verify_native_archive`, API startup and readiness refuse unresolved publication
responsibilities, queued/running Runs on unsupported profiles, or unreleased old
controllers. The matching Runtime independently refuses unsettled physical,
creation, power and cleanup duties. Both barriers must pass; a released API
controller alone does not prove physical retirement. Preflight failures require
an explicit responsibility-resolution plan, not a data reset or invented terminal
state.

Owners retain authenticated read access to their historical Session metadata,
transcript and published files. Starting another Run or maintenance operation on
an unsupported environment returns
`environment_profile_unsupported_create_new_environment`; create a fresh native
environment instead. The old profile and execution identities remain unchanged.
An archived transcript still uses the existing read-projection service; this
bridge adds no provider executor or alternate Agent loop.


A narrow read-only eligibility check also permits a known historical environment
whose unreleased controller refers to a durably recorded failure before Session
admission, when its original signed grant and canonical terminal lifecycle Job
match and all physical-intent, allocation, holding, snapshot and active-lease
checks prove no product-managed effects. This relies on the historical producer's
mandatory durable creation journal before physical dispatch. Stop legacy API,
Worker and Runtime/provider writers before evaluating eligibility and keep them
stopped through native activation. A missing ledger, unknown schema, invalid
signature, conflicting history or any effect record continues to block activation.

Eligibility preserves the old controller flag, epoch, owner, Job, dead letter and
failure record. It creates no cleanup or release receipt and grants no right to
execute or clean up the unsupported old profile. The independent Runtime physical
responsibility checks remain required.
