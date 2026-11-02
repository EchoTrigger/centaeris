# Workspace API

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
Queue contention remains HTTP 503 `execution_admission_busy` with `Retry-After: 5`;
it commits no Run or receipt, and a retry retains the same operation identity.

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

Session deletion immediately denies subsequent workspace snapshot and checkpoint
access. Physical snapshot bytes are reclaimed by `gc_deleted_resources` after
its retention cutoff; deletion does not perform filesystem enumeration in the
HTTP request. On local POSIX storage an already opened authorized download can
finish after the file name is reclaimed. The collector only considers permanently
purged Sessions, so restorable trash retains its files.

`GET /api/workspaces/{workspaceId}/trash` orders entries by deletion time
descending, then kind and ID ascending. ID ordering and cursor comparisons use
PostgreSQL `C` collation, matching the Python merge independently of the database
default locale. IDs are tie-breakers, not timestamps. Each kind contributes at
most 51 candidates and the response contains at most 50 entries. Permissions,
filters, and cursor fields are unchanged by the choice of database locale.

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
fully projected. A bounded catch-up pass normally handles at most 128 source
events and 256 KiB. To guarantee cursor progress, one source event may exceed
the byte budget; Runtime processes that one event, records an explicit
`transcript projection progress exception` diagnostic with its byte size, and
then yields rather than looping on the same event. If more work remains, HTTP 409
`transcript_projection_not_ready` reports both `sourceHighWater` and
`projectedHighWater`, and a frozen-water retry advances another bounded pass.
An invalid generation fails explicitly rather than mixing pages. Empty Sessions
return a valid high-water-zero empty page without creating synthetic facts.
Runtime atomically overwrites one current-head recovery frontier with each
normal projection commit. Historical patch commits exclude that recovery
payload, so open-tool state is not copied into every retained patch. This
current recovery slot is not the periodic historical checkpoint policy owned by
Core; normal page and patch reads do not depend on a historical checkpoint
archive.

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
Both entries reject browser Origin headers. See
[Platform materials and MCP](../architecture/PlatformMaterials.md#mcp-connection-and-authorization)
for transport constraints and the platform processing lifecycle.

The authenticated host may send `X-Workspace-Tool-Call-Id` (one nonempty ASCII
value, at most 160 bytes), never a model argument. The server verifies the
authorized run's committed tool call, exact arguments and reserved
`workspace.materials` provider. Eligible ready results then include `receiptId`
and `citationIds` backed by immutable server receipts. These IDs are provisional:
only an exact matching durable successful MCP result can publish citation rows.
Missing headers allow diagnostic material access but create no receipt. The
Runtime registers the five first-party material tools for runs with declared
material inputs. There is no opt-in legacy reader or plugin activation. Runtime freezes the discovered catalog and verifies it before each
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
`execution_queue_full`. Contention on the cross-replica admission transaction
returns HTTP 503 with `execution_admission_busy`. These responses carry
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
