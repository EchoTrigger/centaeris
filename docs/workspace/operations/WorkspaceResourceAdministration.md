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
| Consume enabled workspace models and Skills | Current membership and existing usage authorization | Same usage authorization | Platform authority does not manufacture a membership |
| Invite members or adjust non-owner roles | Existing owner/admin membership rules | Denied | Existing membership rules remain separate |
| Transfer ownership or remove/demote the current owner | Transfer requires current owner and password reauthentication; ordinary member mutation cannot remove an owner | Denied | Platform resource authority does not bypass ownership transfer |
| Delete a workspace | This change adds no deletion workflow; deletion remains an owner-only boundary | Denied | No new deletion authority |

The backend pins the current workspace and the actor's authority during scoped
mutations. Supplied workspace IDs and resource IDs must identify that same
workspace. UI visibility is a convenience; HTTP and service authorization enforce
the boundary. Generic workspace membership helpers do not gain a superuser
bypass from resource administration.

## Workspace resources

Workspace resource settings use `/api/workspaces/{workspaceId}/...` routes.
Workspace model selection checks current membership before listing the workspace's
models. Providers, installed Plugins, and connector credentials belong to one
workspace.

Upload a Skill ZIP or complete Plugin ZIP to workspace Plugin settings. A Skill
ZIP contains a root `SKILL.md` or one enclosing Skill directory; the extractor
wraps it in a validated Plugin. Its directory name or ZIP basename must be a
canonical lowercase kebab name. Plugin ZIPs keep the supported manifest layout
and may contain Skills, MCP declarations, or hooks. The Skills library links
administrators to package management. Reupload
updates the workspace installation after package validation and active-run
checks. Removing an installation disables the package for that workspace.
Workspace-local package content remains immutable for existing signed references.

For workspace-local installations, an existing retained environment keeps its
original mounted package version, including while paused. Updating the installation does not replace the original
container or discard its memory. A Run requesting a different package version
in that environment must be rejected explicitly; start a new environment or
conversation to use the updated package. Retirement of the old environment is a
separate lifecycle action.

## Credentials

Management responses contain identifiers, configuration status, versions, and
scope metadata. They never return an existing secret. Replacement forms start
empty, remain masked, and are discarded when switching workspaces. Saving a
replacement requires a newly supplied key. Secret decryption remains
inside the existing authorized execution/connector path, separate from resource
management. Audit records describe the action without secret values.

## Upgrade from shared platform configuration

Migration `0046_workspace_owned_resources` preserves provider/model rows for
historical references, disables the shared models and providers, revokes one-shot
credentials bound to those models, and deletes shared provider and MCP secrets.
Workspace-local configuration and credentials remain scoped to their workspace.
Preserving a historical identity does not preserve permission to execute with a
retired secret. Configure replacement workspace credentials explicitly.

Existing workspace plugin enablements without a workspace installation are
converted to installation metadata from `PLUGIN_CATALOG_ROOT`. Existing
installations are kept. This migration does not copy the legacy package bytes
into content-addressed storage: legacy directories must remain mounted and
unchanged while historical snapshots resolve through them. Do not remove the old
plugin volume merely because the installation metadata has moved.

Before upgrading, preserve a database backup and the matching plugin storage,
check that every existing enabled plugin is present in the mounted legacy
catalog, and arrange replacement workspace model and connector credentials.
Missing package data aborts conversion; it must not be repaired by substituting
an unrelated package with the same name.

The migration is explicitly irreversible. Deleted secrets cannot be recovered by
a reverse migration. Rolling deployment code back alone does not restore the
previous platform configuration; recovery requires a coordinated restoration of
the pre-upgrade database and matching storage. Settle in-flight external work
before a planned credential retirement, since the retired key cannot query or
cancel its former one-shot operations. Emergency revocation still takes priority.

The release gate must use a database at migration `0045` with a historical Run,
legacy plugin binding and both shared and workspace-local credentials, migrate
forward, then exercise authorized history reads, current selection and secret
isolation. Directly invoking the migration function on today's model registry
does not establish historical-state migration compatibility.
See [one-shot upgrade acceptance](../eval/OneShotUpgradeAcceptance.md) for the
historical PostgreSQL fixture, executable commands, and the separately recorded
completed and pending checks.
