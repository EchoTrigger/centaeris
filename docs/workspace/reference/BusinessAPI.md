# Business Chat API

The business backend uses `/api/v1/chats` for both native Agents and
published assistants. The resources are Chat, Message and File; Agent is
the configured target selected when issuing a grant. Execution, model selection,
coordination and work dispatch remain platform responsibilities.

## Authority and identity

Send `Authorization: Bearer <token>` from a trusted backend. Do not mix browser
cookies with bearer credentials or distribute a shared grant to end users.
The backend authenticates the external user and supplies their stable
`businessUserId`; it must not accept another user's identity from untrusted input.
A chat ID selects a resource, not an end-user credential. The application
backend must check which of its users may access that chat.

One application, configured target, platform owner and exact business user map
to one persistent, private chat. Repeated creation and token rotation
retain it. Other applications and targets have separate namespaces. Existing
root chats, files and memory are not copied. Deleted chats are not
silently replaced. A second chat for the same identity is not a v1
feature. User IDs retain case and Unicode and must contain 1–256 characters,
including non-whitespace text and no control characters.

Both grant types use the same usage scopes: `assistant:use` to create or retrieve
a chat, `messages:submit` to send, `sessions:read` to read messages or
subscribe, `attachments:write` to upload, and `artifacts:read` to download.
Existing tokens do not gain file upload permission automatically; issue a grant
with explicit consent. Configuration and grant management remain separate
platform operations.

For published assistants, configure the owner's maintained assistant instance
in the platform using its existing model settings. Each business instance uses
that policy and its currently available published definition at new Run admission.
Existing Runs keep their accepted model and definition snapshots. Availability,
membership and plugin authorization are still checked. An unconfigured model
leaves saved messages pending until a usable policy is configured; acceptance
does not promise execution. The business API never picks the first catalog model.

## Requests

| Action | Method and path |
| --- | --- |
| Create or retrieve chat | `POST /api/v1/chats` |
| Send message | `POST /api/v1/chats/{id}/messages` |
| Read history | `GET /api/v1/chats/{id}/messages?limit=50` |
| Read one message | `GET /api/v1/chats/{id}/messages/{messageId}` |
| Subscribe or resume | `GET /api/v1/chats/{id}/events` |
| Upload files | `POST /api/v1/chats/{id}/files` |
| Download file | `GET /api/v1/chats/{id}/files/{fileId}` |

Create with `{ "agentId": "<configured target>", "businessUserId": "customer-42" }`.
The target is the grant's native Agent ID or published definition ID, as shown
in its generated integration guide. The response is `{ "data": { "id": "…",
"agentId": "…", "businessUserId": "customer-42", "createdAt": "…" } }`.
New creation returns 201; retrieval returns 200.

Send with `Idempotency-Key: business-request-001` and JSON:

```json
{ "text": "Check this document", "fileIds": [] }
```

The key is 1–64 ASCII letters, digits or `_.:-`. Persist the original key and
payload before sending. Retry uncertain responses with the same key and content
under the same grant; rotation preserves that grant. New acceptance returns 201,
replay 200, and conflicting content 409 `idempotency_conflict`. Files form a set:
order does not change submission identity and duplicates are rejected. `fileIds`
may be omitted. Text is retained exactly, is bounded by 65,536 UTF-8 bytes, must
not contain NUL, and may be blank only with attachments. Unknown fields fail.

Sending and looking up a message return the same `{ "data": Message }` envelope:

```json
{
  "data": {
    "id": "opaque-message-id",
    "chatId": "opaque-chat-id",
    "author": "user",
    "text": "Check this document",
    "fileIds": [],
    "createdAt": "2026-10-08T08:00:00.000Z",
    "readAt": null
  }
}
```

Agent output has `author: "agent"` and the same shape. `readAt` on user messages
comes only from verified committed intake evidence. It is null on Agent output.
Saved, read and completed are distinct facts. There is no automatic one-input /
one-answer correlation or task-completion inference. Output file IDs can be
downloaded; only uploaded chat files can be submitted as attachments.

History returns `{ "data": [Message], "nextCursor": "…", "hasMore": true }`.
The first page contains the most recent items, in retained order, and `cursor`
retrieves older pages. `limit` is 1–100. Keep following `nextCursor` while
`hasMore`; even an empty filtered page can have more records. At the end,
`nextCursor` is null. IDs and cursors are opaque and must be URL-encoded, retained
verbatim and never interpreted as timestamps or numbers. Cursors are bound to
their chat and endpoint. Keep Django signing-key fallbacks when rotating
the deployment key so already-issued resource IDs remain verifiable.

## Live updates and recovery

The SSE endpoint emits committed `message.created` and `message.updated` events.
Each has an opaque `id` and a complete Message JSON value in `data`. Save that
event ID after successfully applying the update. On reconnect, send it verbatim
in `Last-Event-ID`. Without a cursor the stream replays retained history, then
follows new records. Upsert by message ID: history and reconnect replay can
overlap. This endpoint streams committed messages, not token deltas or internal
tool traces. It does not synthesize a reply from a Run's final event.

Read updates are reconstructed from authoritative uptake records in the same
ordered database snapshot as message creation, including updates to old inputs.
Heartbeats carry the current scan cursor. Access is rechecked before each data
frame and during polling; withdrawal or credential rotation closes the stream.
The async response releases database leases between waits and closes its
iterator on disconnect. Execution already accepted under owner authority is not
cancelled by disconnecting or revoking a grant.

## Files

Upload multipart `files` (one or more). The response is
`{ "data": [{ "id": "…", "name": "report.txt", "contentType": "text/plain",
"sizeBytes": 123 }] }`. Submit those IDs in `fileIds`; output messages can also
carry downloadable IDs. File IDs are scoped to the chat, current source
authorization and captured content identity. Downloads recheck authority before
opening and use the existing bounded async storage stream. No storage paths,
runtime resource selectors or credentials appear in this contract.

## Errors and deployment

Errors use `{ "error": "code" }` (validation may additionally identify invalid
fields). 401 means invalid credentials; 403 means withdrawn authority or missing
scope; 404 means unavailable chat/message/file; 409 means conflicting
submission or changed file; 410 means deleted identity; 503 means transient
admission or storage capacity. Preserve retry identity on transient errors.

Migration `0007_business_definition_instances` marks existing business instances
without changing their Agent, Session, branch or message identities, and allows
distinct published instances per external user. It refuses downgrade once
published business instances exist. Back up before applying it. This change
does not migrate old generic published Sessions into business chats:
those Sessions have no authoritative external-user identity to reconstruct.
They remain platform records. Workspace/browser and diagnostic transports keep
their own contracts; application quickstarts use this public API.

Focused acceptance: `test_chats_api`, `test_conversation_migration`,
`test_agent_history`, business-branch and delegation suites, and the Web
`app-delegation-guide.test.ts` request-execution tests. Run the migration drift
check and the relevant Workspace release gate before deploying a candidate.
