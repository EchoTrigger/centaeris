# Workspace API

## Agent Run API

Workspace owners and administrators manage API credentials through
session-authenticated `GET` and `POST /api/workspaces/{workspaceId}/api-credentials`
and `DELETE /api/workspaces/{workspaceId}/api-credentials/{credentialId}`.
Creation requires `name` and `modelConfigRef`. `pluginNames` and `sourceObjectRefs`
are optional and default to empty scopes. Each credential binds the issuing
membership, Workspace, model configuration and explicit plugin and material scopes.

Lists return metadata and `canCopy`. The issuer can retrieve the secret through
`GET /api/workspaces/{workspaceId}/api-credentials/{credentialId}/secret` when
the credential still authorizes execution. The response uses `Cache-Control: no-store`.
Secrets issued before this feature are available only to callers who saved them.
If creation times out, check the list before trying again.

Deletion returns `204`, permanently removes the credential and invalidates its
secret. Active runs are cancelled through the lifecycle reconciler. Internal
operations needed for cleanup are erased once cleanup completes. Routine
rotation settles and saves results before deletion. Delete a compromised
credential immediately.

New API keys use the prefix `app-` and 24 random letters or digits.
Previously issued keys continue to work. External callers send the complete key in `Authorization: Bearer <secret>`.
`POST /api/v1/agent/execute` accepts a required `prompt` and optional sorted, unique
`sourceObjectRefs` selected from the credential's current material scope. Its
required `Idempotency-Key` has the form `<Unix-seconds>.<UUIDv4>`. The key is valid
for 24 hours from its embedded timestamp, with at most five minutes of future clock
skew. Unknown fields and query parameters are rejected. Acceptance atomically
records the Run and receipt, then schedules the existing Runtime lifecycle.

Inspect `Content-Type` before parsing: a new accepted POST returns
`200 text/event-stream`; repeating an already accepted operation with the same
credential, key and input returns `200 application/json` containing only its
current receipt. A same-key retry after an uncommitted attempt can still be the
first accepted call and return SSE.
`Accept` does not select a response mode. HTTP `200` is not proof that the Run or
delivery completed. An accepted new call streams the following sequence.

1. `runAccepted` provides `operationId`, `runId`, `acceptedAt`, `deadlineAt`,
   `expiresAt`, `runStatus`, and `cleanupStatus`.
2. `runProgress` provides `runId` and a `stage` of `queued`, `running`, or
   `delivering`. Comment frames keep idle connections active.
3. `outputText` provides committed assistant text in ordered `text` chunks
   after execution reaches a terminal state; it is not a live token stream.
   A failed or cancelled Run may contain only the last partial assistant response.
4. Each artifact starts with `artifactStarted`, followed by indexed
   `artifactChunk` frames containing Base64 `data`, then `artifactCompleted` or
   `artifactFailed`. Start and completion metadata include `artifactId`,
   `filename`, `contentType`, `sizeBytes`, and `sha256`. Completion also includes
   `chunkCount`. Artifact chunks are numbered from zero. Verify length and SHA-256
   before publishing the received file locally. Completion metadata must match
   the start event. Keep partial files separate and never replace an existing
   caller file. A failed artifact can be followed by other valid artifacts;
   retain verified files while reporting the overall delivery failure.
5. `runFinished` separately reports `runStatus` and `deliveryStatus`. The Run may
   succeed while file delivery fails. A lost terminal frame leaves the caller's
   observation uncertain.

Each file is limited to 8 MiB, each Run to 16 MiB of raw artifact bytes and four
artifacts. Execution has a ten-minute deadline from acceptance, including queue
time. Terminal output delivery is bounded to 60 seconds. Connection loss closes
delivery while the original Run continues. Explicit cancellation uses
`POST /api/v1/agent/executions/{runId}/cancel` with an empty body. A `202` response records
the request, while a `200` response reports an existing terminal state. Query the
original operation to confirm cancellation; completion may win the race.
Cancellation does not undo external side effects.

For an already accepted operation, the same key and input return its minimal
JSON receipt. Changed input returns `409 operation_conflict`. Callers can inspect the original identity with
`GET /api/v1/agent/operations/{operationId}`. This endpoint returns receipt and
status fields, including a known terminal `reasonCode`. Text and files are
delivered only through the original response. An expired key returns
`410 operation_expired` even after its receipt has been erased. Timeouts, proxy
failures, and response loss require checking or resubmitting the same identity.
The Core tool-execution intent and receipt determine recovery of uncertain
side effects. Deduplication applies to Run acceptance within one credential's
scope, not exactly-once external plugin effects or identities across key rotation.
The request fingerprint uses the trimmed prompt and validated material IDs.

Persist the original endpoint, credential reference, operation ID and request
body before POST, and retain the Run ID and receipt timestamps after acceptance.
Use the same credential for lookup. A `404` immediately after a timed-out submit
can precede acceptance and is not proof that retrying with a new key is safe.
For an intentional submit retry, reuse the original key and request body. Never
change a timestamp or generate a new key merely to recover a lost response.

The client's wait, execution deadline, delivery lease and receipt expiry are
separate boundaries. `expiresAt` is not a result-download deadline. Generate a
key immediately before its first use: a nearly expired unused key can currently
be accepted even when its lookup window ends before the Run's deadline. Proposed
changes to this and to recoverable results are recorded separately in the
[Agent Run API design](../architecture/AgentRunAPI.md).

Agent Run API execution uses a private temporary Session, a signed v3 execution policy,
`perRun` allocation, disabled human questions, and explicit credential scopes.
Personal-library access is disabled. Artifact publication verifies the existing
publication identity and bytes into temporary Run-owned storage. After terminal
execution, settled physical cleanup and Host retirement, and closed or expired
delivery, the collector erases execution content, temporary files, snapshots,
authorization, and private Session records. The minimal receipt remains until
its key expires. Immutable physical-cleanup provenance remains available to the
Runtime's control contracts.

Run the backend example with `CENTAERIS_RUN_KEY` set in the caller's environment.
Save the operation identity printed before network access. The example verifies
artifact bytes and atomically creates each destination file.

```sh
python scripts/workspace/agent_run_client.py \
  --url https://workspace.example \
  --prompt 'Generate an editable Word project brief and publish the final docx.' \
  --output ./received
```

Inspect an uncertain call without submitting another Run:

```sh
python scripts/workspace/agent_run_client.py \
  --url https://workspace.example \
  --status --operation-id '<original-operation-id>'
```

Submission exits `0` only after complete successful delivery, `2` when it receives
a JSON receipt without output, and `1` for transport, protocol or delivery
failure. `--status` exits `0` for a successful lookup; inspect `runStatus`
separately. If deliberately retrying a submit, use the original `--operation-id`,
prompt and credential. No retry resumes a lost original stream.

In Workspace Web, **Settings → Developer** provides the API address, credential
management and a minimal request example in cURL and Node.js. Quickstart adds
a saved first request and status lookup for both choices.
The **Docs** tab bundles the integration guides and the generated request,
response and event fields with that Web build. The quickstart explains the
first-call and original-operation status examples. The first-call example saves
request identity before sending and refuses to overwrite an existing request directory.
The artifacts guide owns the Word example and file-verification instructions.
Link directly to a page using its URL, for example
`/w/{workspaceId}/settings/developer?view=docs&topic=events`.
After changing the public wire definitions, regenerate the reference with
`uv run --frozen --package api python scripts/workspace/agent-run-api-reference.py`.
The generated reference includes required headers, response media types and the
condition for each success response. Human guides explain recovery policy; they
must not present a receipt as output or invent a result-replay endpoint.

This document owns only Workspace transports. Exact fields are enforced by the
Django and Rust definitions in this repository.

Django JSON schemas inherit `StrictSchema`: ordinary Python field names map to
exact camelCase through Pydantic's shared alias generator. Exceptional wire names
such as `schema_id` to `schema` remain explicit. Python names are not alternate
input aliases; unknown fields and implicit type coercions remain rejected.
Operation validation and malformed JSON share one error-code mapping while
retaining their distinct defaults, method-specific codes and status codes.

Scalar query endpoints share `app_core/http/query.py` to reject unknown or
repeated fields and enforce required field names. Each handler retains its
value rules, error response and authorization order; Ninja still parses typed
parameters before entering the handler. Transcript citations and turn metadata
explicitly accept repeated `sequence` values and do not use the scalar check.

[Runtime HTTP](RuntimeHTTP.md) inventories the endpoints owned by the Rust
Runtime service, their purpose and the admission/rejection behavior preserved by
transport refactors. Django-owned internal routes remain described here.

Browser REST and SSE routes are rooted at `/api`. Postgres is truth; Redis holds
bounded transient live projection. Unknown fields and schemas fail. Public
Runtime event and tool semantics belong only to the exact public Cargo revision.

The Rust Host consumes Core's generic `ExecutionPolicy`,
`ExecutionCommandRequest`, and `ExecutionError` contracts. Docker runtime names
remain Host-private configuration/diagnostics. Host status uses `policyEnforced`;
process summaries retain `enforced` and no longer carry `sandboxType`.
Permission denial, policy enforcement unavailable, and Host unavailable are
explicit categories rather than inferred from an OCI backend name. Core and
these consumers must be upgraded together; no old-field aliases are accepted.
Historical Host details remain stored JSON facts; the durable tool error retains
the canonical `sandbox_unavailable` category for receipt recovery. Old results
are not replayed or rewritten. Container selection and authorization checks are
unchanged; nono is not introduced into hosted execution.

### Conversation messages and reset

`POST /api/v1/agent/messages` accepts the same prompt, material references and
headers as `/execute`, with an optional `conversationId`. The first message
creates a credential-owned conversation. Save the ID from `runAccepted` or the
operation receipt. Supply that ID to continue with a new operation key, or omit
it to start a new conversation. Output uses the original SSE response.

`POST /api/v1/agent/conversations/{conversationId}/close` takes an empty body
and returns `conversationId` and `closedAt`. Repeating a close preserves the
timestamp until the next accepted message. The run and delivery have their own
lifecycles.

Workspace owners and administrators can set `resetPolicy` when creating a key
or by sending `PATCH /api/workspaces/{workspaceId}/api-credentials/{credentialId}`
with `{"resetPolicy":"onReopen"}`. The supported values are `never` (the
default), `onReopen`, `after10Minutes`, `after15Minutes`, `after30Minutes`,
and `after60Minutes`. The current policy applies when a new message arrives after
an explicit close. Before the threshold, the original conversation resumes. At
or after the threshold, the accepted message starts a fresh conversation and
returns its new ID. A competing request for the superseded ID receives
`409 conversation_reset` with the replacement `conversationId`. An accepted
operation replay returns the original IDs.

Allow the preceding run and workspace release to finish before continuing the
same conversation. On `session_has_active_agent_run` or
`session_workspace_preservation_pending`, wait and retry the same request. Use
the original credential for messages, close, receipt lookup and cancellation.
An unrelated credential receives `404` for the conversation.

The service keeps the private context for resumable conversations and reports
`cleanupStatus: retained`. Context reclamation starts after replacement or
credential deletion and waits for every run, release and delivery in that
conversation. Operation lookup remains subject to the original key's expiry.
Conversation IDs do not provide transcript or artifact retrieval.

## Workspace and Session resources

Browser REST and SSE use the signed-in user's authority, current Workspace
membership and resource ownership. A resource ID selects a record; possession
of it grants no access. Agent profiles, definitions and dedicated coordination
Sessions are not entry points for ordinary Session work.

| Action | Method and path | Input |
| --- | --- | --- |
| Update Workspace profile (owner/admin) | `PATCH /api/workspaces/{workspaceId}/profile` | `name`; optional `avatarDataUrl` |
| Read Workspace avatar (current member) | `GET /api/workspaces/{workspaceId}/avatar/{digest}` | Exact current avatar digest |
| List the user's active Sessions | `GET /api/workspaces/{workspaceId}/sessions` | No query fields |
| Create an empty Session | `POST /api/workspaces/{workspaceId}/sessions` | `operationId`, optional `projectId` |
| Read a Session | `GET /api/sessions/{sessionId}` | No Agent selector |
| Update Session metadata | `PATCH /api/sessions/{sessionId}` | One or more of `title`, `isPinned`, `isUnread` |
| Create a Session project | `POST /api/workspaces/{workspaceId}/session-projects` | `name` |
| Submit a message | `POST /api/workspaces/{workspaceId}/sessions/{sessionId}/messages` | `operationId`, `modelConfigRef`; optional `text`, `thinkingMode`, `attachmentRefs`, `projectId`, `tailAction` |

Workspace profile updates require a trimmed, non-empty `name` of at most 160
characters. Omitting `avatarDataUrl` keeps the avatar; `null` removes it.
Uploads accept base64 PNG, JPEG or WebP data URLs, with at most 1 MiB of decoded
source bytes. The API verifies and decodes the image, applies EXIF orientation,
and stores a centered 256 × 256 PNG without source metadata. Name and avatar
changes commit together. The response is `{workspace: WorkspaceResponse}`;
every Workspace response includes required `avatarUrl` (a first-party path or
`null`). Avatar reads require current active Workspace membership and the exact
current digest, and return `image/png`, `X-Content-Type-Options: nosniff`, and
`Cache-Control: private, no-store`.

Session responses retain the existing Workspace, project, environment, title, origin, status,
pin, attention, active-Run and update metadata, without `agentId` or
`initialInputOrigin`. Projects contain `id`, `workspaceId`, `name` and
`createdAt`, without an Agent binding. Neither creation nor message input
accepts `agentId`.

Session metadata PATCH rejects an empty body, null values and unknown fields.

Message submission requires an explicit `modelConfigRef` from the models
available to that Workspace. The referenced model must be enabled and current.
An omitted or null `thinkingMode` uses that selected model's configured effort;
a nonempty explicit effort must be supported by that model. There is no Workspace model
fallback or catalog-first selection. The browser's saved model preference is
only a user selection aid.

`sessionId=new` creates a Session while accepting its first message. A
`projectId` is allowed only for this new-Session path. Existing Session messages
use explicit `attachmentRefs`; new-Session multipart messages can upload their
initial files. Existing upload, transcript, cancellation, operation recovery and
file access contracts continue to use Session ownership and current resource
authorization. Creating or selecting a Session does not mount an entire Library.

Workspace presentation uses
`GET /api/workspaces/{workspaceId}/presentation/events`, with only optional
`sessionId` and `Last-Event-ID` for recovery. Its frame identity is
`workspaceId` plus nullable `sessionId`; navigation carries the current Session
records. Unknown selectors, including `agentId`, fail validation. Browser title
and favicon belong to the Workspace layout, using its name and avatar (or its
name's first character when no avatar is set) through Session and settings navigation.

The feed emits `kind: "viewRevision"` initially and when its decimal-string
`contextRevision`, `historyRevision`, `workRevision`, or `subagentsRevision`
changes. A Session's subagent revision follows durable `subagent.run` Job
insertion, deletion, actual status changes, and committed child snapshots;
lease heartbeats do not advance it. Open child details refresh on this revision
while the Job remains running. The source mutation and presentation dirty key commit atomically. The
existing scoped notification listener and its 30-second durable reconciliation
recover changes after a lost notification.

`GET /api/sessions/{sessionId}/subagents` returns
`workspace.session.subagents.v1` with `sessionId` and `agents`, including terminal
history. Each agent has `runtimeJobId`, original `callId`, `childSessionId`,
`parentAgentRunId`, `parentTurnId`, `description`, `prompt`, raw Job `status`,
`createdAtMs`, and `updatedAtMs`. Appending `/{runtimeJobId}` returns
`workspace.session.subagent.v1` with `sessionId`, `agent`, and typed `messages`.
The parent Session's owner and current Workspace membership authorize both reads;
a child identity never authorizes a separate platform Session. Responses use
`Cache-Control: no-store`.

An active Run also emits an `activity` frame with exactly `schema:
"workspace.presentation.v1"`, `kind: "activity"`, `workspaceId`, `sessionId`,
`agentRunId`, `sourceHighWater`, and `activity`. The activity is `thinking` for
the current model phase, `working` for tools and other execution, or
`waitingInput` for a currently registered Core question wait bound to that Run
and its authorization. It follows the authority checkpoint from the same database
snapshot and carries no event cursor. Reconnecting restores the current value;
consuming the wait clears `waitingInput`, including when the transcript water is
unchanged. A terminal checkpoint clears active activity. Only applied checkpoints
acknowledge transcript delivery. These display states add no question or answer
submission by themselves.

When Core pauses for `request_user_input`, the same feed sends `kind: "question"`
with `workspaceId`, `sessionId`, `agentRunId`, `sourceHighWater`, and `question`.
The question is either null, clearing the pending card, or exactly
`{questionId, continuationId, turnId, question, options}`. `question` is the prompt
string and `options` is a string array. This frame follows the same-snapshot
authority checkpoint and activity, has no cursor, and restores the current
pending question after refresh. Runtime tool-call data and authorization digests
are not exposed. Accepting a response or consuming the current wait sends null; a terminal checkpoint
also clears the card.

`POST /api/sessions/{sessionId}/agent-runs/{agentRunId}/questions/{questionId}/responses`
accepts exactly `operationId`, `continuationId`, `action: "answer" | "skip"`, and
an optional `answer`. Answer actions require a nonempty answer string, which may
combine selected option numbers and free text. Skip actions omit `answer`.
HTTP 202 uses the ordinary hosted operation receipt with command
`respondQuestion`, preserving the original Run and its initial turn identity.
The response is accepted durably before waking that Run's original lifecycle Job.
Its receipt clears the question card and changes activity to `working`, including
after refresh while Core has not yet consumed the wait. Retry uncertain requests
with the same operation ID, or look up
`GET /api/workspaces/{workspaceId}/operations/respondQuestion/{operationId}`.
Reusing an identity with different content or question binding returns 409.
Current Session ownership, Workspace membership and original Run authorization
still apply to requests and receipt recovery.

Migration `0040_question_responses` enables the receipt command and rejects any
remaining `runtime.await_question.v1` wait. Those old records have no authoritative
question body to copy; they must be resolved or reconstructed from authoritative
records before upgrading. New pending records use `runtime.await_question.v2`.

## Workspace Issues

Issues are durable Workspace business records. Django owns their fields and
change history; Agent Run execution and Session events retain their separate
contracts. Completing a Run does not complete an Issue. These routes do not
create Runs, Reviews or business Projects. Assignment and status events create
personal Inbox notifications in the same transaction.

All Issue routes require a signed-in current member of an active Workspace.
Owner, admin and member roles can create and edit records in that Workspace.
An Issue identifier does not grant access across Workspaces. Mutation requests
use the existing browser CSRF contract. Assignees must be current members of
the same Workspace; `null` clears the assignment.

| Action | Method and path | Input / response |
| --- | --- | --- |
| List Issues | `GET /api/workspaces/{workspaceId}/issues` | `mine`, `archived`, `limit`, `offset`; `{issues, total}` |
| Create Issue | `POST /api/workspaces/{workspaceId}/issues` | `title`, optional `description`, `status`, `assigneeId`; HTTP 201 `{issue}` |
| Read Issue | `GET /api/workspaces/{workspaceId}/issues/{issueId}` | `{issue}` |
| Edit or archive Issue | `PATCH /api/workspaces/{workspaceId}/issues/{issueId}` | Required `version`; optional `title`, `description`, `status`, `assigneeId`, `archived`; `{issue}` |
| Read activity | `GET /api/workspaces/{workspaceId}/issues/{issueId}/events` | `limit`, `offset`; `{events, total}` |
| List assignees | `GET /api/workspaces/{workspaceId}/issues/assignees` | `{assignees: [{id, name}]}` |

An Issue contains `id`, `workspaceId`, `title`, `description`, `status`,
`assigneeId`, `creatorId`, `version`, `archivedAt`, `createdAt` and `updatedAt`.
Timestamps use ISO 8601. `assigneeId` and `archivedAt` can be null. Status is one
of `backlog`, `todo`, `inProgress`, `done`, `canceled`; creation defaults to
`todo`, an empty description and no assignee. Titles must be nonblank after
trimming and at most 240 characters. Unknown input fields are rejected.

List requests default to unarchived records and 50 results from offset zero.
`mine=true` selects records assigned to the signed-in user; `archived=true`
selects archived records only. Lists sort by update time descending, then ID.
Archive and restore retain the record and its history; there is no hard-delete
endpoint.

A successful change increments `version` and writes an immutable Issue event
in the same database transaction. A stale version returns HTTP 409 without
changing the record or history. A patch that changes no values creates no event
and does not increment the version. Events carry `id`, `issueId`, `actor`
(`id`, `name`), `type`, `changes` and `createdAt`. Types are `created`, `updated`,
`archived`, `restored`; changes map each affected field to `{before, after}`.
The activity endpoint returns chronological events with deterministic ordering.
No API permits editing or deleting an event.

Web Issues and My issues use these records and URL-selected details. Browser
refresh and navigation reload authoritative records; local browser storage is
not the business-data store.

## Personal Inbox notifications

Inbox notifications are durable per-recipient projections of Issue events.
Creating an Issue with an assignee, or assigning an existing Issue, notifies the
new assignee. Status changes notify the creator and current assignee. The actor
and users without current Workspace membership are excluded. When both rules
select the same recipient for the same event, one notification is stored, with
assignment taking precedence. Title-only edits, archive/restore and no-op edits
do not create notifications. Existing historical events are not backfilled.

Issue changes, their event and recipient notifications commit together. Each
notification retains the event reference and the Issue title at creation.
The database enforces one notification per event and recipient. Notifications
have independent read and archive timestamps; archiving a notification never
archives its Issue or changes another recipient's Inbox.

| Action | Method and path | Input / response |
| --- | --- | --- |
| List own notifications | `GET /api/workspaces/{workspaceId}/notifications` | `view`, `limit`, `offset`; `{notifications, total, unreadCount}` |
| Read own notification | `GET /api/workspaces/{workspaceId}/notifications/{notificationId}` | `{notification}` |
| Update receipt | `PATCH /api/workspaces/{workspaceId}/notifications/{notificationId}` | One or both of `read`, `archived` as booleans; `{notification}` |

`view` accepts `inbox` (default, unarchived), `unread` (unarchived and unread),
or `archived`. Lists sort newest first with an ID tiebreak. Pagination defaults
to 50 rows and offset zero, with a maximum limit of 100. `unreadCount` counts all
unarchived unread notifications for this recipient in this Workspace.

A notification contains `id`, `workspaceId`, `issueId`, `issueTitle`, `kind`,
`event`, `readAt`, `archivedAt` and `createdAt`. `kind` is `assigned` or
`statusChanged`; `event` uses the existing Issue event representation.
Receipt timestamps are generated by the server. Repeating an already-applied
boolean state preserves its timestamp. Empty, null and unknown patch fields are
rejected. All endpoints require current active Workspace membership and restrict
records to the signed-in recipient, including for Workspace administrators.
Mutations use the browser CSRF contract.

Web Inbox replaces sample notifications with these records. Selection and
filters live in the URL; acknowledgement changes appear after server acceptance.
Reloading or returning focus fetches current notifications. The linked Issue
keeps a return destination to the originating Inbox selection and filter.

## Hosted command acceptance

`POST /api/workspaces/{workspaceId}/sessions` (`createSession`) and
`POST /api/workspaces/{workspaceId}/sessions/{sessionId}/messages`
(`submitMessage`, including `sessionId=new`) require `operationId` in JSON or
the supported multipart form. It is 1–128 ASCII letters, digits, or `- _ . :`.
Unknown fields and missing operation identities fail validation. A caller creates
one identity for one intended command and retains it across uncertain responses.

An accepted create returns HTTP 201; an accepted submission returns HTTP 202.
Both return exactly `operationId`, `command`, `status: "accepted"`, `sessionId`,
`agentRunId`, and `turnId`. The last two fields are null for `createSession`.
The receipt records acceptance, not the current Run state or a Session snapshot.
Read the Session and Run separately; a replay after completion still returns the
original accepted identities.

`GET /api/workspaces/{workspaceId}/operations/{command}/{operationId}` retrieves
the same receipt with HTTP 200 and `Cache-Control: no-store` without starting execution.
Unknown query fields are rejected. A missing receipt
returns HTTP 404 `operation_not_found`. Loss of access also returns 404; neither
response authorizes a client to replace an uncertain operation identity.

Receipt identity is scoped by authenticated user, Workspace, command, and
`operationId`. The server hashes the validated command input, including its
target Session and upload content identities. Model defaults and server-generated
attachment IDs are not request identity. A matching replay returns the original
receipt without repeating current model selection, queue admission, or lifecycle
scheduling. Changed input under the same identity returns HTTP 409
`operation_conflict`, after current resource authorization. An unavailable
original resource returns HTTP 410 `operation_resource_unavailable`; retrying
does not recreate deleted resources.

Business rows and the receipt commit in one transaction. Failed admission does
not consume the identity. A scheduling failure after acceptance is reconciled
against the same Run. This is command acceptance deduplication, not an
exactly-once guarantee for model calls or external tool effects. Existing
commands without operation identities are not reconstructed from message text.

Public message submission pins Workspace and membership authorization with
shared row locks in each of its two transactions, including the recheck after
Runtime profile resolution. Other environments can retain the same authorization
pins while submission proceeds. Management writers retain exclusive Workspace locks, so revocation still
serializes with acceptance. A transaction-scoped operation mutex covers exactly
the receipt identity above; neither the payload digest nor target Session creates a separate mutex for a
conflicting reuse of that identity. Existing Session locks are acquired before
the global queue-admission decision.
Queue admission waits for the cross-replica transaction lock, then checks the
committed queue counts. The lock is released when that transaction ends.

The browser retains pending identity and input digests in tab-scoped storage,
without persisting prompts or file bytes. Uncertain retries keep the same ID,
including when a later retry is rejected. Explicit correction of an unresolved
message also keeps that ID: if the original input wins the race, the correction
conflicts and the original receipt is recovered. A receipt lookup alone does
not prove which input version was accepted; the user must inspect that Session.
Receipt recovery does not cancel execution or repeat external tool effects.

## Session environment reads

Session responses include `currentEnvironmentId` and `executionMode: "perRun"`.
`GET /api/sessions/{sessionId}/agent-runs/{agentRunId}/environment` reads the Run's
immutable logical environment binding under current Session access. It rejects query fields, performs no repair or admission, and returns
`Cache-Control: no-store`. An environment ID is not a capability or evidence of
physical process persistence. See [Session environments](../../development/SessionEnvironments.md)
for identity, authorization and migration boundaries.

## Model input images

Internal model requests consume Core's `prepared_prompt.v1`.
The authenticated `/internal/model-runs` JSON body is limited to
128 MiB (134,217,728 bytes), including base64 and history text; oversized bodies
return HTTP 413 with `model_run_request_too_large` before JSON parsing or provider
execution. This does not change Django's limits for other API endpoints.

The prompt supports optional `inputImages`. Each image has exactly
`messageId`, `contentType`, `placeholder`,
and `dataBase64`. Images bind to a unique placeholder in a user message. The API
validates canonical base64, PNG/JPEG/WebP headers, declared media type, positive
dimensions, at most 100,000,000 pixels, and at most 10 MiB (10,485,760 decoded
bytes) per image before provider execution. Header inspection is not full pixel
decoding. Unknown fields remain errors.

Provider adapters replace placeholders in text order with Chat Completions
`image_url`, Responses `input_image`, or Anthropic base64 `image` content blocks.
Core-generated image fixtures protect cross-language contracts and limit parity.

## Trash pagination

`DELETE /api/sessions/{sessionId}` confirms permanent deletion only after Run
termination, task cleanup, controller release and physical retirement are settled.
The API records permanent deletion intent under the Session lock before
cancellation; new Run admission then returns HTTP 409 `session_delete_pending`
and restoration is unavailable. Cancellation still in progress or an unconfirmed
purge returns HTTP 503; the Session remains visible and the same deletion can be
retried. Runtime erases the
parent and child Session bodies atomically with the permanent Session tombstone,
retaining authorization and cleanup receipts. A repeated confirmed deletion
returns HTTP 200.

Confirmed deletion denies subsequent workspace snapshot and checkpoint
access. Physical snapshot bytes are reclaimed by `gc_deleted_resources` after
its retention cutoff; deletion does not perform filesystem enumeration in the
HTTP request. On local POSIX storage an already opened authorized download can
finish after the file name is reclaimed. The collector only considers permanently
purged Sessions, so restorable trash retains its files.

`GET /api/workspaces/{workspaceId}/trash` lists the caller’s deleted private
library files, images, and saved artifacts. It orders entries by deletion time
descending, then ID ascending using PostgreSQL `C` collation. Each response
contains at most 50 entries. Session deletion marks the Session permanently
deleted and makes its workspace file tree eligible for the existing collector.

## Transcript page and committed patch reads

`GET /api/sessions/{sessionId}/transcript` returns the Core-owned
`transcript.page.v1` display read model. An initial request captures the current
Session source high-water and current projection generation on the server. A
retry of the frozen tail supplies exactly `sourceHighWater` and
`projectionGeneration`; an older-page request additionally supplies the opaque
`olderCursor`. The cursor is bound to the Session, projection version,
generation, high-water, and stable display order. Pages are ascending even
though PostgreSQL reads the page index in descending order.
`resumeCursors` contains one Session display stream (`workspace-transcript.v1`)
whose cursor is the canonical Session source sequence. It is deliberately not
an AgentRun SSE cursor, so the number of runs cannot exhaust the page cursor
budget.
Tool blocks contain exactly one of inline `summary` or bounded `summaryRef`;
large summaries use the same stable `session-event:<eventId>:<field>` reference
scheme as other transcript text and do not stall page cursors.

`GET /api/sessions/{sessionId}/transcript/content` accepts exactly
`projectionGeneration`, `refId`, `revision`, `byteLength` and `offset`. It returns
`transcript.content.range.v1` with at most 64 KiB and a UTF-8 continuation offset.
Every request rechecks ownership and membership and returns `Cache-Control: no-store`.
For `session-event:<eventId>:<field>`, the API delegates to Runtime's authenticated
`/internal/transcript/content` route. Runtime performs a session-scoped event lookup
bound to the current, valid projection generation; Core resolves the visible field
and verifies the reference. `tool-output:<callId>` reads serve complete inline
content from the committed tool result. Newly observed spills can also resolve
through a hosted immutable UTF-8 capture keyed by the committed event. The host
observes the successful create-only write, then publishes the matching byte range
after the fenced event commit; publication also verifies the event's Execution
against the existing execution-start ledger. Core event fields are unchanged.
Reads use at most two 64 KiB database chunks and verify each chunk's length and
SHA-256. No initial read or continuation falls back to a current workspace path
or snapshot. Old, missing, purged, corrupt, or not-yet-published captures return
HTTP 409 `transcript_content_unavailable`; the committed event and preview remain.
Archive failure, restart before publication, or exhausted capture budgets may
leave full text unavailable without changing tool success or replaying a tool.
Session deletion denies reads immediately and purges captured bytes, retaining
a tombstone. Membership loss
denies subsequent reads but does not delete shared history.
Message text loads automatically and renders as one Markdown
document. Tool detail navigation replaces the current range rather than appending
all previously loaded output.

`GET /api/sessions/{sessionId}/transcript/patches` reads committed display
patches strictly after `afterSourceHighWater` and through one frozen
`throughSourceHighWater`. The first request may omit the through-water and lets
the server capture it; continuations reuse the returned value. Results use
`transcript.patch.page.v1`, contain at most 128 patches, and never reconstruct
patches by scanning raw Session events.

Both routes recheck Session ownership and current Workspace membership on every
request and return `Cache-Control: no-store`. The Python API authorizes, binds,
and transports these contracts; it does not implement display projection.
Runtime reads the PostgreSQL display index through the public Core transcript
store port. A page or patch is returned only after the requested high-water is
fully projected. A bounded catch-up batch normally handles at most 128 source
events and 256 KiB, committing its patches, waterline and recovery together in
one transaction. The row limit applies before payload encoding and byte-window
calculation, so each query processes the bounded candidates rather than the
remaining history. Tokio drives successive blocking batches within a read request;
database wait time does not truncate a batch to a single event. To guarantee
cursor progress, one source event may exceed the byte budget; Runtime processes
that one event, records an explicit
`transcript projection progress exception` diagnostic with its byte size, and
then yields rather than looping on the same event. Between completed batches,
Runtime may return HTTP 409 `transcript_projection_not_ready`, reporting both
`sourceHighWater` and `projectedHighWater`; a frozen-water retry continues from
committed progress.
The presentation feed bounds each read independently and allows catch-up to
continue while that progress advances. Thirty seconds without advancement ends
the attempt; successful advancement does not incur a fixed polling delay.
An invalid generation fails explicitly rather than mixing pages. Empty Sessions
return a valid high-water-zero empty page without creating synthetic facts.
Runtime atomically publishes the batch's final head and one current-head
recovery frontier together with its event commits. Historical patch commits
exclude that recovery payload, so open-tool state is not copied into every
retained patch. This current recovery slot is not the periodic historical
checkpoint policy owned by Core; normal page and patch reads do not depend on a
historical checkpoint archive.

## Workspace citation presentation

The retired Session-history response is not a citation transport. Citation
summaries have exactly `citationId`, `inputRef`, `displayName`,
`sourceToolCallId`, and `sourceUrl`. Source URLs are first-party
`/api/citations/{citationId}` detail routes, not arbitrary model-supplied links.
Preview authorization is checked on every access.

`GET /api/sessions/{sessionId}/agent-runs/{agentRunId}/citations` returns a no-store
snapshot with exactly `schema: "workspace.citations.v1"`, `sessionId`, `agentRunId`,
`throughSequence`, and `citations`. It requires the run owner and current workspace
membership; unknown query parameters fail. The snapshot rebuilds verified
projections from committed events through a captured sequence, under the run
projection lock, so it works before terminal lifecycle reconciliation.

Citation snapshots remain an independently authorized resource API and do not
advance the Session stream cursor. Transcript blocks do not embed citations.
`GET /api/sessions/{sessionId}/transcript/citations?sequence=...` accepts 1–128
canonical Session source sequences (repeated `sequence` parameters). It returns
`{sessionId, bindings: [{sourceSequence, sourceToolCallId, snapshot}]}` with no-store
caching. Only tool-call events in the authorized Session are resolved. Each
binding uses that event's AgentRun snapshot and filters it by the recorded call ID;
non-tool/missing sequences are omitted and duplicate requests are collapsed.
Unknown parameters fail. A tool result retains the tool call's original order key.

The browser requests only loaded tool blocks, refreshes after committed transcript
changes/recovery, and discards superseded or disposed requests. Tool-source buttons
open the existing authorized detail/preview panel and retain its original locator.
Unavailable citations fail locally; refresh failures have an explicit retry.
These are tool sources, not sentence-level evidence for the final answer. The
current protocol has no answer-span binding; filenames, URLs and prose are never
scanned to manufacture one. API and web must be released together.

Internal REST calls require `X-Internal-Token`; there is no anonymous fallback.
The first-party MCP transport `/internal/mcp` is an exception: it accepts only a
short-lived scoped Bearer credential, not the global internal token. The host
issues one through `POST /internal/mcp/credential` with exact fields
`schema: "workspace.mcp.credential.issue.v1"`, `agentRunId`,
`authorizationDigest`, `processingSpecification`, and `specDigest`.
The no-store response has `schema: "workspace.mcp.credential.result.v1"`,
`accessToken`, and `expiresAt` (Unix seconds, five-minute lifetime).
Its `X-Workspace-Material-Catalog` header carries the SHA-256 identity of the
tool catalog bound to the signed credential.
Both entries reject browser Origin headers. See
[Platform materials and MCP](../architecture/PlatformMaterials.md#mcp-connection-and-authorization)
for transport constraints and the platform processing lifecycle.

Original-file transport uses four additional internal POST endpoints under
`/internal/agent-runs/materials/`. They require `X-Internal-Token`, reject unknown
fields and return successful JSON responses with `Cache-Control: no-store`. All requests bind `agentRunId` and
`authorizationDigest`; the accepted personal-library grant is a separate,
immutable platform record and does not change the signed authorization v2.

| Endpoint | Request and result |
| --- | --- |
| `context` | `schema: workspace.material.context.read.v1`; returns `workspace.material.context.result.v1`, `personalLibrary`, confirmed `inputs`, and `pendingInputs` requiring Host inspection. |
| `prepare` | `schema: workspace.material.prepare.v1`, `inputRef`, `callId`; reserves identity and capacity and returns `workspace.material.prepare.result.v1` with `resolvedInput`. This is not a delivery success. |
| `confirm` | `schema: workspace.material.confirm.v1`, `inputRef`, `sourceVersion`, `sha256`, `callId`; rechecks current authority and records the Host's completed installation. Returns `workspace.material.confirm.result.v1` and `resolvedInput`. |
| `reconcile` | `schema: workspace.material.reconcile.v1`, current `jobId`/`leaseOwner`, and bounded `inputs` containing exact identity, `state: present\|absent` and observed `admissionIds`. Returns `workspace.material.reconcile.result.v1` with `settledInputRefs`. |

Original bytes still use the existing authenticated `read-input` stream and its
version, length and digest checks. They are never embedded in MCP results. An
`execution_file` success is produced by Runtime only after installation, input
registration and confirmation; it is not a parsed-content evidence receipt.

The authenticated host may send `X-Workspace-Tool-Call-Id` (one nonempty ASCII
value, at most 160 bytes), never a model argument. The server verifies the
authorized run's committed tool call, exact arguments and reserved
`workspace.materials` provider. Eligible ready results then include `receiptId`
and `citationIds` backed by immutable server receipts. These IDs are provisional:
only an exact matching durable successful MCP result can publish citation rows.
Missing headers allow diagnostic material access but create no receipt. The
Runtime registers the six first-party material tools for Runs with attached
inputs or their accepted personal-library grant. There is no opt-in legacy reader or plugin activation. Runtime freezes the discovered catalog and verifies it before each
call, obtains a new short-lived credential per invocation, and isolates call
headers on separate connections. Configure API allowed hostnames for the private
connection explicitly; this checkpoint does not change deployed environments.

MCP read/search can now return durable `operations` with exact fields
`operationId`, `inputRef`, `status`, and nullable `errorCode`. Status is one of
`pending`, `running`, `completed`, `failed`, `cancelled`.
`get_operation(operation_id)` and `cancel_operation(operation_id)` are scoped to
the authenticated run. Cancellation withdraws that operation only, not shared
background processing. Completed operations require a new read/search call.
The dedicated platform material Worker claims these tasks directly under database
leases; Runtime has no material scheduling or processing endpoint.
`POST /internal/materials/processor` accepts exact
`{schema:"workspace.material.processor.v1"}` under `X-Internal-Token` and returns
`{schema:"workspace.material.processor.result.v1",processingSpecification,specDigest}`.
This identifies the processor registered by the platform Worker.
Retired Knowledge read/search/commit and material reconciliation routes return 404.
`GET /internal/model-catalog` returns exact
`{schema:"workspace.model_catalog.result.v1",catalog}` from the public Rust model
catalog crate. Django does not parse or duplicate catalog files. Other internal
AgentRun, workspace file, Skill, MCP, and Hook transports use the
exact v1 schemas in code.

Nonzero Session stream cursors use the exact `v1.<base64url>` wire prefix with
payload schema `session.stream.cursor.v1`; `0-0` is the only initial sentinel.
AgentRun authorization uses `workspace.agent_run_authorization.v2` and signature
domain `workspace:agent-run-authorization:v2`, signing `environmentProfile` and
`environmentProfileDigest`, including its network policy. Historical signed
records are preserved by explicit migrations. Administrators configure defaults through
`GET/PATCH /api/workspaces/{workspaceId}/environment-settings`; see
[environment network policy](../architecture/EnvironmentNetwork.md).
Knowledge processing
specifications use the immutable Centaeris processor version `1.0.0`.

Secrets never belong in ordinary responses, logs, documentation, or checked-in
examples. Explicit credential-delivery endpoints are restricted to their
authenticated caller and return no-store responses.

Hosted message submission atomically enforces the Workspace initial-queue budget
and the global initial-queue budget. A full Workspace returns HTTP 429 with
`workspace_execution_queue_full`; global saturation returns HTTP 503 with
`execution_queue_full`. Concurrent admissions wait for the transaction holding
the admission lock, then check the remaining capacity. Queue-full responses carry
`Retry-After: 5`; a rejected request creates no Session, AgentRun or authorization.
An initially queued run that expires records `execution_queue_expired` and follows
Runtime's normal cancellation protocol; logical waits do not expire this way.

`POST /internal/jobs/schedule` keeps `runtime.job.schedule.v1`. For
`agent_run.lifecycle`, `workspaceId` is required and commits with the job as an
immutable tenant binding. Other kinds omit it. Reusing an idempotency key with a
different job, session, payload reference or tenant fails. Hosted lifecycle claim
and wait routes enforce the shared execution limits; saturation returns an empty
claim result, not a failed job. Capacity release wakes existing job listeners.
Contention on the atomic claim transaction returns HTTP 503
`execution_claim_busy` with `Retry-After: 5`; worker slots back off before claiming
again rather than spinning on a due job whose claim transaction is still locked.

Runtime HTTP saturation returns HTTP 503 `runtime_busy` with `Retry-After: 5`.
An absolute handler response deadline returns HTTP 504
`request_deadline_exceeded`; a body deadline returns HTTP 408. Timeout does not
establish whether a write committed. Cancellation, heartbeat and job-status RPCs
use reserved HTTP capacity and the reserved database pool for their complete path,
including cancellation's terminal-state preflight reads.

`POST /internal/agent-run-lifecycle/reconcile` accepts
`runtime.agent_run_lifecycle.reconcile.v1`, `limit` (1–100), and optional
`activeAfter` / `deadLetterAfter` cursors. Each cursor is null (start a pass) or
`{createdAt, id}` with an offset-aware timestamp. Unknown fields and malformed
cursors fail. The response contains `scheduled`, `terminalized`, `pending`,
`activeNext`, and `deadLetterNext`. The two populations advance independently in
`(createdAt, id)` order, with at most `limit` attempts per population per call.
Null next cursors finish that population's pass; the next tick starts a new pass.
Failed individual attempts still advance the page and retry on a later pass.
The worker retains cursors across ticks, including when later waiter recovery
fails, and starts from null after process restart. These are scan-progress hints,
not execution checkpoints; durable jobs and session facts remain authoritative.

`POST /internal/jobs/reconcile` accepts `runtime.job.reconcile.v1` with `nowMs`
and returns `{reclaimed}` for expired job leases. It does not reactivate published
terminal notifications. Pending outbox deliveries persist until generation-checked
acknowledgement, which follows durable waiter wake handling. A crash before that
acknowledgement permits duplicate delivery; duplicate wake and acknowledgement
remain idempotent.

`POST /internal/job-outbox/reconcile-waiters` recovers late or missed wakes from
the durable waiting-relationship index and terminal source jobs, including
waiters registered after notification acknowledgement. Both this endpoint and
`POST /internal/job-outbox/wake-waiter` accept `after`: null or an exact object
`{checkpointId, toolCallId}`. Wake requests additionally retain `jobId` and
`generation`; their lookup selects only that source job's indexed relationships.

Responses contain exactly `disposition`, `checked`, `waiters`, and `next`.
`next` is null at the end of a pass, otherwise it is the cursor to send as
`after`. A request processes at most 256 relationships and yields after a 200ms
scheduling budget between operations. It always finishes at least one available
relationship; this budget does not interrupt database transactions or impose a
hard request deadline. Large checkpoints paginate within their tool-call set.

The worker retains independent cursors for reconciliation and each pending
notification generation. It does not acknowledge a notification until `next`
is null. Failed requests repeat their last page safely. A worker restart begins
new idempotent passes from null; durable pending notifications and waiting
relationships remain authoritative. Late registrations behind a cursor are
covered on the next reconciliation pass, without replaying terminal history.

Model completion results carry optional `reasoningContent` for display and
`continuationReasoningContent` for provider-approved plain-text continuation.
These fields are independent; Runtime must not reconstruct continuation from
display text. OpenAI-compatible adapters preserve their existing continuation
text, while Responses summaries and Anthropic thinking text are display only.
Neither field carries opaque reasoning, encrypted data, or provider signatures.
Both the ordinary result and `api.model.stream.v1` terminal `result` use these
exact camelCase names; unknown result fields and non-string, non-null reasoning
values fail. The stream emits answer `delta`, full-attempt `reasoning` snapshots
(`schema`, `type`, `text`), and terminal `result` or `error`. Ordinary error frames
have exactly `schema`, `type`, `reasonType`, and nullable `httpStatus`. The
`incomplete_output_token_limit` error additionally requires `truncatedToolCalls`,
an array of rejected call metadata (`callId`, `toolName`, `argsBytes`,
`argsSha256`); missing identities are null and unfinished arguments are never
included. Other failure reasons reject this field. Non-stream model failures
return HTTP 502 with `error: model_run_failed`, `reasonType`, nullable
`httpStatus`, and the same output-limit metadata when applicable. These envelopes
carry sanitized failure facts, never provider response text or credentials. Runtime
maps these hosted failure facts to Core's model error kinds. Configuration and
authentication failures stop immediately; transient provider failures retain
the existing retry behavior. An explicit `length` or `max_tokens` finish becomes
`incomplete_output_token_limit` before any unfinished tool arguments are parsed;
Runtime passes it to Core's existing continuation recovery instead of retrying
the original request. Empty finals without tool calls have the separate
`empty_final_response` identity and use the bounded model transport retry policy.
Token usage equalling a configured limit does not imply truncation. A failure after a delivered result does not emit
a second terminal frame. Runtime submits Core-owned
`reasoning_block` records on success (`done`), failure, or cancellation
(`interrupted`), keeping each retry under a separate Core request identity.
Postgres history and committed SSE carry these records through the existing
Session transport; Web reconstructs reasoning blocks in source sequence order.
The payload is exactly `blockId`, `requestId`, `text`, and `status`, with identity
and lifecycle validation owned by Core. No duration field is added.
Schema identifiers remain v1.

OpenCode Go requests carry the Workspace product User-Agent and stable
`x-opencode-session` routing header. Runtime requests resolve this identity from
the authorized AgentRun's retained Session and model binding; callers cannot
override it. Model-setting probes use a stable identity scoped to the ModelConfig.
Other provider templates retain their SDK headers.

Redis live snapshots and SSE live items carry optional `reasoning` alongside the
answer under one monotonic revision. Its exact fields are `blockId`, `requestId`,
and `text`. Atomic Redis writes update the cached snapshot and wake together;
browser restoration reads both body and reasoning from that snapshot. Committed
seals replace matching live blocks without changing their disclosure identity.
Runtime restart recovers available cached partial reasoning through Core before
settling the interrupted run. Missing or expired cache does not fabricate text.

The Redis signal stream retains at most 256 wake records per AgentRun with exact
trimming. A `live` signal has exactly `schema`, `kind`, `agentRunId`,
`afterSequence`, `revision`, `turnId`, and `messageId`; `commit_wake` has exactly
`schema`, `kind`, `agentRunId`, and `highWaterSequence`. Both use
`agent_run.transient.signal.v1`. Text and reasoning occur only in the current
cached snapshot, never in the wake history. A reader coalesces wakes and reads
that complete snapshot atomically; live SSE items retain their existing fields
and do not advance the committed cursor. Retained Redis data therefore grows
with the latest snapshot plus bounded wake metadata, rather than 256 copies of
the generated output. Snapshot content is complete and has no truncation limit;
the deployment's Redis memory budget still limits aggregate concurrent state.

Runtime producers and API readers must be upgraded together. The strict v1 wake
decoder rejects the earlier payload-bearing live signals; no compatibility
alias is supported. Restart active stream readers during the coordinated
upgrade so they capture a fresh Redis tail and restore the latest snapshot.
Earlier payload-bearing histories retain their memory until expiry or trimming;
if Redis is already full under `noeviction`, writes may fail before trimming.
During the planned upgrade, stop the old producers/readers and allow those
histories to expire, or have the operator remove only their transient signal
keys before restart. This cleanup does not remove PostgreSQL history.
PostgreSQL remains authoritative for durable replay and terminal reconciliation
when wakes are trimmed, absent, or unavailable.

Workspace and Session creation generate `ws_` and `session_`
identifiers followed by 16 case-sensitive Base64url characters (`A-Z`, `a-z`,
`0-9`, `-`, `_`), encoding 12 cryptographically random bytes without padding.
Other resource identifiers retain their own generation rules. Resource IDs are
opaque, immutable references: clients must not split, lowercase, or reinterpret
them. Creating these resources through the ORM retries generated primary-key
collisions up to three total attempts; explicit IDs and other integrity errors
fail. Possession of an ID does not grant access: ownership and workspace
membership checks still apply.

## Retired Agent and application transports

Agent profile/definition, coordination, private input/history, dispatch/return,
business-branch and user-app grant endpoints are no longer registered. The
`/api/v1/chats` Chat/Message/File API and its settings integration guide are
retired with those targets. Old bearer grants do not authorize the retained
Session endpoints. These routes have no compatibility aliases or replacement
embedding contract in this revision.

## BusinessApplication registration

Application records remain available through these management endpoints:

| Endpoint | Authority and result |
| --- | --- |
| `GET /api/business-apps` | Signed-in user; `{apps}` containing active records only |
| `GET /api/admin/business-apps` | Superuser; `{apps}` containing all registered records |
| `POST /api/admin/business-apps` | Superuser; `{name}` creates a pending record; HTTP 201 `{app}` |
| `PATCH /api/admin/business-apps/{appId}` | Superuser; `{status: "active"}` or `{status: "revoked"}`; `{app}` |

An app contains exactly `id`, `name` and `status`; status is `pending`, `active`
or `revoked`. Name is nonempty and at most 160 characters. Updating an unknown
record returns 404 `business_app_not_found`; updating a revoked record returns
409 `business_app_revoked`. Registration issues no access token and grants no
Session, model or file authority. See [Business API](BusinessAPI.md) for the
retired usage boundary.

## Execution recovery scheduling

The internal `runtime.agent_run.step.result.v1` response requires `retryAtMs`
(a nonnegative integer Unix timestamp in milliseconds) when `transitionReason`
is `execution_recovery_checkpoint_committed`. This response has `disposition`
`waiting` and `terminalState` null. Other step outcomes omit `retryAtMs`.
The worker yields its current lease until that deadline while keeping the
AgentRun running. The Runtime derives the deadline from committed recovery
facts, reserves an attempt under the lifecycle lease before preparation, and
terminalizes exhaustion as `execution_recovery_exhausted`. Retry scheduling
does not permit replaying tool calls beyond a checkpoint.

Runtime store schema v3 adds nullable `wait_handoff_json` to
`runtime.checkpoints` with a unique `handoffKey` index. The hosted attachment uses
schema `workspace.runtime_job_wait_handoff.v1` and exact fields `schema`,
`handoffKey`, `sourceSessionSequence`, `modelRequestId`, `waitCheckpoint` and
`snapshotJson`. The last two retain Core's existing checkpoint and full Session
snapshot formats; Runtime does not redefine their private pending state.
Recovery checkpoints with a `waitcp:` identity require that bound attachment.
Existing checkpoints migrate with a null attachment and are not promoted to new
wait recovery boundaries. This storage contract adds no public Runtime endpoint
or model-visible tool.

## Plugin management isolation

`GET /api/workspaces/{workspaceId}/plugins` returns the validated inventory and
per-package interface errors without calling Runtime. `mcpServers` and `hooks`
are `null` until inspected, not empty success results. The required `errors`
array is scoped to each plugin. `GET .../plugins/{pluginName}` independently
inspects that package through the existing exact v1 MCP and Hook projections;
unavailable or invalid contributions remain `null` with explicit error codes.

Enabling validates only the target package and rejects errors with
`workspace_plugin_unavailable`. A package changed during validation requires a
new inspection. Disabling does not contact Runtime. Global catalog integrity
errors still fail the request; missing package files are isolated in management
but remain errors for execution. Enabled invalid packages are never silently
removed from AgentRun activations.

Bearer credential management loads independently. `mcpCredentialRefs` contains
deduplicated references read from digest-verified installed v1 transport metadata,
independent of full tool contract validation or Runtime availability. Unreadable
credential metadata returns `null` with `plugin_credentials_unavailable`, not an
invented reference. The UI supplies references automatically and shows one Token
input per reference; shared references do not create duplicate inputs.

Create and rotate accept either a bare Token or `Bearer <Token>`, trim surrounding
whitespace, and encrypt only the Token. Empty values, embedded whitespace/control
characters, and full `Authorization:` header lines are rejected. Saved credentials
remain manageable if declarations fail. Saving credentials does not establish
tool availability or bypass contract validation; execution remains strict v1.

### Read-only Office preview

`GET /api/office-preview/{ownerKind}/{objectId}?lang=zh-CN|en` provides an
authenticated, read-only PDF preview for a DOCX, XLSX or PPTX
`userLibraryObject`, `sourceObject` or `artifact`. If the exact content
generation, SHA-256 digest and document processing specification have already
produced a representation, the response streams it as `application/pdf`.
Otherwise the endpoint atomically queues the existing LibreOffice-backed
document processing task and returns a small `202` loading page that refreshes
until the representation is ready.

Every request repeats user and owner permission checks. A changed generation or
digest selects a new immutable representation instead of reusing stale output.
Responses are not cached; unsupported formats return 415, inaccessible objects
404, and unavailable or invalid processing configuration returns 503. The
endpoint does not expose a write or save route. Original files remain available
through their existing authenticated download endpoints.

### Bounded material tool results

`read_material` and `search_materials` preserve each completed request as an immutable, session-owned result snapshot before returning a bounded page. The model receives one structured result, without a duplicated text projection. English `message` text reports delivered line and UTF-8 byte ranges; byte-range ends are exclusive. `completeResultSaved` describes storage completeness, not model reading coverage.

The response carries `resultRef`, `resultSha256`, and `continuation` (either null or an exact `tool` / `arguments` pair). `read_material_result(result_ref, cursor)` retrieves the next page of the same saved result. Treat its cursor as opaque. It does not rerun a search. When a document window is exhausted, continuation can point to `read_material` with the next zero-based line offset. Single long lines are paged at UTF-8 character boundaries. Only the returned content is eligible for a citation.

Snapshots remain with the source SessionEvent and survive API/runtime restarts. Every continuation rechecks the current run's source permissions, generation, processing identity, and session ownership. Corrupt snapshots, revoked access, and invalid cursors fail explicitly; they are not reported as a successful partial result. Existing citation projections are retained during the schema migration. Roll out API migrations and the matching Runtime tool catalog together, after active runs drain.

## Workspace guide

`GET /api/workspaces/{workspaceId}/guide` returns
`{ "guide": { "dismissed": false, "steps": [{ "id": "materials", "completed": false }] } }`.
Owners and administrators receive steps in this order: `models`, `materials`,
`plugins`, `connections`, `conversation`, `issue`. Members receive `materials`,
`conversation`, `issue`. Both methods require browser session authentication and
current membership in an active workspace; missing membership returns
`404 workspace_not_found`, including for platform operators.

`PATCH` on the same URL accepts `dismissed` and/or `manualSteps` (for example
`{ "manualSteps": { "materials": false } }`) and requires CSRF. It returns the
same envelope. Unknown fields or step IDs, query parameters, empty requests and
non-boolean values are rejected. Members cannot change administrator-only steps.
Manual values, including false, persist and override automatic completion. The user is
always the authenticated recipient. Dismissal persists independently for each
user/workspace and survives application updates. Repeated identical PATCHes
preserve the dismissal timestamp. GET never creates a state record.

Completion is recomputed from authoritative records:

- `models`: a current, enabled workspace model with an enabled, unarchived
  provider and a saved provider credential; diagnostic models do not count.
- `materials`: a non-deleted personal file/image owned by the recipient, or a
  non-deleted file in a ready workspace source that the recipient may read.
  Owner/admin source access and member group grants follow existing source rules.
- `plugins`: an installed, enabled package with readable, valid package and
  connector metadata.
- `connections`: an installed service whose declaration requires no credentials,
  or whose required credential is saved in this workspace. This indicates
  configuration only; no live service connection is tested.
- `conversation`: an interactive AgentRun by the recipient in an active session
  they own in this workspace. Empty sessions and Agent Run API sessions do not count.
- `issue`: an issue created by the recipient in this workspace.

Invalid package metadata returns `503 plugin_catalog_invalid`; an enablement
without an installation returns `409 workspace_plugin_enablement_invalid`.
Failed completion checks or persistence do not acknowledge a dismissal.


## Issue executions

`POST /api/workspaces/{workspaceId}/issues/{issueId}/executions` accepts strict
`{operationId, version, modelConfigRef, thinkingMode?, instructions?}` and returns the existing
202 hosted operation receipt (`submitMessage`, `sessionId`, `agentRunId`, `turnId`).
It requires browser authentication, CSRF and current workspace membership. Issue
version conflicts and archived Issues reject new acceptance. Replaying accepted
input returns its original receipt despite later Issue edits; changed input
conflicts. The immutable Issue snapshot, caller-owned interactive Session,
AgentRun, authorization and receipt commit together. The shared message admission
path owns execution and lifecycle scheduling. Issue status is not completed by
starting or finishing an AgentRun.

`GET` on the same URL accepts `limit` and `offset`, returning `{executions,total}`.
Each entry contains `id`, `sessionId`, `agentRunId`, `latestAgentRunId`, `title`,
`status`, `waitingForInput`, `createdAt`. `agentRunId` identifies the immutable
initial execution; status and `latestAgentRunId` describe the latest Run in the
same conversation. `waitingForInput` reuses the current Runtime question authority
and response receipt; a historical pause event does not establish a current wait.
Only the requesting user's accessible active, unarchived linked conversations are
returned. Issue visibility does not grant access to another member's transcript.

Execution creation also accepts optional `instructions`, at most 8192 characters,
appended to the accepted Issue context. Empty instructions preserve the existing
operation identity. Ordinary follow-up messages and question responses use the
existing conversation endpoints, retaining the same Session and Issue origin.
Agent results are readable through the linked execution without submitting a
deliverable. Sharing a selected result remains an optional separate action.

`GET /api/workspaces/{workspaceId}/sessions/{sessionId}/issue-origin` returns
`{origin:{issueId,executionId,title}}`, or `{origin:null}` for an unrelated or no
longer visible Issue. It requires browser authentication, current membership and
an owned active, unarchived interactive Session. The title is read from the
currently visible Issue, not from an old snapshot. Web uses this origin to return
to the Issue and expand the corresponding execution's results after refresh.

## Workspace model defaults

`GET /api/workspaces/{workspaceId}/model-defaults` returns
`{defaults:{titleModelConfigRef:null,titleThinkingMode:"low"}}` when unconfigured.
`PATCH` accepts those two fields and returns the same envelope. Both operations
require current owner/admin membership and browser authentication; PATCH also
requires CSRF. References must select an available authorized current model and
supported reasoning mode. Stored selection follows the same model family's
revisions, never a different model or provider. Web exposes one optional
**Default purpose** selector, using low reasoning (or no reasoning for models
without that capability), independently of chat preferences.

Null selection creates no title job or provider call. The first accepted message
can create one asynchronous job after commit; it reads at most 2048 characters
from that prompt, has no tools, and preserves its language/script. Queue wait and
model generation share the ten-second deadline. Expiry or failure retains the
initial title. Clearing the selection, losing authority or manually renaming the
session prevents pending publication. Successful title publication updates the
existing navigation stream without changing session ordering. Existing named
sessions are protected by the forward migration.

The title worker runs `celery -A api.title_queue:app worker -Q session_titles`,
using `MATERIAL_BROKER_URL` and its own queue. `SESSION_TITLE_TIMEOUT_SECONDS`
defaults to 10. It requires the same database and credential-encryption settings
as the API; it does not start agent execution environments.
Outcome logs include the job ID, status, reason, queue wait and processing time;
failures include the exception class and HTTP status when available. Prompts,
generated titles, credentials and raw provider errors are excluded.

## Workspace work forms and submitted results

`GET /api/workspaces/{workspaceId}/issue-templates` lists installed forms.
Owner/admin members can install one with `POST`, supplying `key`, `name` and a
strict `definition`. Definitions support text, textarea, date, decimal money and
bounded item lists, required fields, money minima and date ordering. Business
definitions are Workspace data, not built-in financial policy. Issue creation
accepts `templateKey` and `properties`, captures the definition revision, validates
the values and derives record visibility. Later edits use that captured definition.
`participants` records are visible only to their creator and current assignee;
ordinary records retain Workspace visibility. Listing, activity, notifications,
material links and execution entry points use the same record visibility.
Execution prompts include the captured form and its instructions.

`GET /api/workspaces/{workspaceId}/issues/{issueId}/deliverables` lists submissions
visible to the caller. `POST` accepts `version`, `executionId`, `agentRunId`,
`summary`, selected `artifactIds`, `recipientId` and optional `replacesId`.
The caller must own the linked execution and its Run; selected artifacts must be
published current files from that Run. A submission captures immutable summary,
file identities, recipient and revision. A replacement preserves author and
recipient. Participants-only records restrict recipients to their participants.

`POST .../deliverables/{deliverableId}/responses` accepts `version`, `action`
(`feedback` or `accept`) and `text`. Feedback requires text; acceptance belongs
to the recipient. Replaced submissions cannot receive new responses. These
operations advance the Issue version and notify the other participant without
marking the Issue done. Concurrent writes return a version conflict.

`GET .../deliverables/{deliverableId}/artifacts/{artifactId}` checks current
membership, record visibility, submission author/recipient and captured file
identity before streaming. Sharing a submission grants no access to the private
execution conversation or its unselected files. Deleted or changed files are
unavailable. Artifact references do not create a new independent file copy.

## Authorized browser resource search

`GET /api/workspaces/{workspaceId}/search?query=...&limit=20&cursor=...` searches
current personal materials, granted Workspace source objects and summaries
explicitly submitted to the caller. Responses contain `matches` (`id`, `kind`,
`title`, bounded `snippet`, `href`) and `nextCursor`. Cursors bind caller,
Workspace, query and page size. Every page checks current authorization.
Existing private conversation search remains a separate authorized endpoint.

Body matches use existing `DerivedRepresentation` and `KnowledgeSegment` data
bound to the owner's current content generation and hash. Unindexed objects can
match names; search neither scans original files nor creates an Agent Run or a
synchronous extraction task. Submitted summaries retain their explicit audience.

`GET /api/workspaces/{workspaceId}/search/materials/{kind}/{objectId}` accepts
optional `representationId` and `offset`. It rechecks membership, ownership or
source grants and current content identity, then reads verified canonical text.
Pages contain at most 200 lines and 50 KiB, with `nextOffset`. Search links carry
real segment line offsets where available. Stale versions, denied sources and
missing representations are rejected rather than rendered from a cached result.
