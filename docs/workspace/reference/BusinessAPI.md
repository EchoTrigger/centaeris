# Business application access

The former `/api/v1/chats` Chat/Message/File transport is retired. It depended
on Agent targets, business-user Agent branches and their coordination Sessions.
Those usage routes, user-app grant management endpoints and the browser's
application authorization/integration guide are no longer available. Existing
Agent-target grants do not authorize ordinary Session access.

Ordinary Workspace Sessions retain the browser REST and SSE contracts described
in [Workspace API](API.md). They use the signed-in user's current membership,
Session ownership and resource authorization. Keeping these Session transports
does not establish an external application authentication, external-user mapping
or embedding contract. No replacement Session API or embedded browser entry
point is implemented by this retirement.

BusinessApplication registration is a separate management concern from resource
usage. An application record alone grants no access to Sessions, model execution,
files or a user's entire Library. Signed-in users can list active applications;
superusers can list all applications, register a pending application by name,
and activate or revoke it. The retained routes are
`GET /api/business-apps`, `GET/POST /api/admin/business-apps` and
`PATCH /api/admin/business-apps/{appId}`. Their exact management contract is in
[Workspace API](API.md#businessapplication-registration). They issue no access
token or replacement Session credential.
