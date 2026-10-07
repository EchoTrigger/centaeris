# Workspace API

This document owns only Workspace transports. Exact fields are enforced by the
Django and Rust definitions in this repository.

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

The browser retains pending identity and input digests in tab-scoped storage,
without persisting prompts or file bytes. Uncertain retries keep the same ID,
including when a later retry is rejected. Explicit correction of an unresolved
message also keeps that ID: if the original input wins the race, the correction
conflicts and the original receipt is recovered. A receipt lookup alone does
not prove which input version was accepted; the user must inspect that Session.
Receipt recovery does not cancel execution or repeat external tool effects.

## Private Agent message transport

Browser-authenticated `POST /api/agents/{agentId}/coordination-session` accepts
exactly `{}`. It creates a fresh private coordination Session and returns HTTP
201, or HTTP 200 for the existing active binding. The response has exactly
`schema: "agent.coordination_session.v1"`, `agentId` and `sessionId`.
`GET` at the same path returns that binding with HTTP 200 under current owner
authority. A missing/inaccessible binding returns 404; POST on a deleted bound
Session returns 410 `coordination_session_deleted` rather than replacing it.
Fields selecting an existing Session or recipient are rejected. Workspace
Session lists, including `agentId` filtering, exclude these bound Sessions.

`GET /api/agents/{agentId}/messages` requires the current private owner's
authority for the bound Session. Query `afterSequence` defaults to 0 and must
be nonnegative; `limit` defaults to 50 and must be 1–100. Unknown or repeated
query fields are rejected. The response has
exactly `schema: "agent.messages.v1"`, `agentId`, `sessionId`, `messages` and
`nextAfterSequence` (integer or null). Each message contains `id`, `agentRunId`,
`turnId`, `toolCallId`, `sourceSequence`, `createdAtMs`, `body`, `sessionRefs`
and `fileRefs`. `id` is the committed successful result event ID. Pages are
bounded by successful source result rows; ignored foreign rows can yield an
empty page with a next cursor. Continue from that cursor until null. No Final
message is added to this feed. Revoked or unavailable source authority returns
404, including for previously committed messages.

The model-visible tool is `send_message` with exactly the required fields
`body`, `session_refs` and `file_refs`; the ref arrays may be empty. The body
must contain non-whitespace text and fit 65,536 UTF-8 bytes. Each array contains
at most eight opaque resource identities. There is no recipient parameter or
compatibility alias. This first-party dynamic tool has `turnBehavior:
"continueTurn"`, `concurrencySafe: false` and empty extension scopes. Only a
bound coordination Run receives it. This uses the pinned public Core dynamic
tool contract without changing Core's protocol.

Runtime's internal-token-authenticated
`POST /internal/agent-messages/validate` accepts exactly
`schema: "workspace.agent_message.validate.v1"`, `agentRunId`,
`authorizationDigest`, `coordinationSessionId`, `toolCallId`, `body`,
`sessionRefs` and `fileRefs`. These source identities come from hosted execution,
not model arguments. A successful response has exactly
`schema: "workspace.agent_message.validated.v1"`, `agentId`, `sessionId`,
`agentRunId`, `toolCallId`, `authorizationDigest` and `inputDigest`. Runtime
requires all identities/digests to match before returning tool success.
Invalid input fails validation; failed current authorization returns 403
`agent_message_authority_rejected`.

A successful validation authorizes that single in-flight call. A later revocation
does not retract its authorization, so it may still commit; cancellation, an
invalid lifecycle lease or storage failure can still prevent that commit. Only
the successful Session commit makes sending effective, and replay retains the
same message identity. New calls after revocation recheck current identity,
operation and ref permissions and reject unavailable authority.

Every message-history read checks current coordination Session authority.
Associated Sessions and files require their own current permissions on every
read or download. A retained message or old Run authorization is not an access
grant. The feed can retain opaque refs after their resources become inaccessible.

This internal response validates current authority without writing a message.
Only a matching successful durable Session commit produces the public
projection. Session refs are currently readable associations; file refs are
explicit authorized SessionAssetLink inputs of the executing Run, rechecked
through existing attachment resolution. Neither creates a grant, copies an
attachment nor enables cross-Agent delivery. Replayed committed event IDs retain
their identity; an internal validation retry alone cannot send a message.

## Committed private work materialization

Native private coordination Runs receive `nativeCoordinationSessionId` in their
hosted Run start, equal to both `coordinationSessionId` and the authorized Session.
Ordinary, managed and application-origin Runs omit it. Runtime registers
`dispatch_work` only for that native identity, using the pinned Core
`continueTurn` contract. Its model arguments are exactly `objective`,
`session_refs` and `file_refs`; execution supplies the source identities.

Internal-token-authenticated `POST /internal/agent-work/validate` accepts exactly
`schema: "workspace.agent_work.validate.v1"`, `agentRunId`,
`authorizationDigest`, `coordinationSessionId`, `toolCallId`, `objective`,
`sessionRefs` and `fileRefs`. Success returns exactly
`schema: "workspace.agent_work.validated.v1"`, `agentId`, `sessionId`,
`agentRunId`, `toolCallId`, `authorizationDigest` and `inputDigest`.
Runtime checks every identity and digest before returning validation success.
This call rechecks current signed Run binding, original membership, native
private source authority and each explicit resource. Managed/delegated sources,
Session Artifact inputs, unsigned inputs and unavailable resource access fail
with 403; malformed or extra transport fields fail validation.

Provider success means awaiting Session commit, not admitted. It writes no child
Session/Run, work binding or hosted operation receipt, and emits no admission
fact. Core commits its successful call/result using the existing Session safe
point; a failed source commit cannot create work. After a successful fenced
append, Runtime best-effort posts each native `dispatch_work` success event ID
from the returned `SessionCommitReceipt` to the materializer below. This happens
after the append transaction/guard ends and before downstream stream/capture
processing. It does not await HTTP or change the committed result or Core's
`continueTurn` behavior. API remains the owner of admission and current authority.

In-flight trigger HTTP requests share a process-wide non-waiting semaphore sized
by the existing `EXECUTION_GLOBAL_LIMIT` (default 8), and reuse the work API
client's 30-second timeout and disabled redirects. Full capacity, setup failure,
HTTP error or timeout only emits a diagnostic; there is no trigger queue or
retry. HTTP success is not an admission fact in Runtime; the read-only query
below reads the existing API receipt. Duplicate wakes and lost responses use the
materializer's existing idempotency. Empty resumed batches and terminal batches
without a new dispatch result do not recover earlier pending work. The independent
worker source scan below recovers skipped wakes from committed facts.

Internal-token-authenticated `POST /internal/agent-work/discover` accepts exactly
`schema: "workspace.agent_work.discover.v1"`, `limit` (integer 1–100), and nullable
`after`/`through` cursors containing offset-aware `insertedAt` and `eventId`.
An initial request freezes the maximum visible qualifying source key. Pages read
at most `limit + 1` raw source keys in `(insertedAt,eventId)` order within that
upper bound, and return `schema: "workspace.agent_work.discovered.v1"`, `entries`,
`through` and `next`. Each entry contains its `cursor` and a nullable
`sourceEventId`; null means skip this entry, not proof of admission. Discovery
validates committed call/result and signed native identity and batch-checks the
exact user/Workspace/command/operation receipt. It writes no business facts and
does not repair bindings or require a currently running model caller.

One worker control thread reuses the existing five-second reconciliation loop
and HTTP client. Each round examines at most one 100-entry source page, attempts
multiple pending sources within a five-second soft budget, and advances past
errors or lost responses. Budget expiry retains the last actually processed
cursor; an unprocessed tail is fetched again, without a retained task queue.
Completed passes reset both cursors on the next periodic round, revisiting old
failures and late commits behind an earlier cursor. `insertedAt` is insertion
time, not a commit sequence. Continuous new sources cannot extend the frozen
upper bound. Cursors are volatile progress hints, not business facts; restart
begins an idempotent pass from the head. Dense admitted history still costs
pages, and arrivals exceeding scan/admission capacity have no latency guarantee.

The scan thread is independent of lifecycle/waiter RPCs, shares service stop and
join handling, and starts no new call after observing stop. Requests use the
remaining soft budget as socket timeout. The existing client has no strict total
deadline, so a slow streaming response can exceed the budget or delay scan-thread
shutdown; it does not make lifecycle/waiter wait for that thread. Admission uses
the existing materializer, source lock, current permission and input-version
checks. An existing receipt prevents rebuilding deleted child work or a missing
binding. Admission followed by scheduling failure remains the responsibility of
the original queued-Run lifecycle reconciler.

With original identity and authorization binding intact, temporary loss of
access leaves an unadmitted committed request pending. Restored conditions permit
admission only after all current checks pass again. Deleting the original
membership and rejoining creates a new identity: the old Run's `membership_ref`
remains invalid, and recovery never changes it or replaces its authorization.
Restored resource access does not authorize a changed input generation/hash or
new link. Rewritten sources and permanently deleted resources are not revived;
retained facts remain available for audit. This adds no cancellation cascade,
task-result notification or Session UI.

The same native identity also receives the read-only `get_work_request` tool,
with exactly `source_agent_run_id` and `source_tool_call_id` model arguments.
Runtime supplies current caller identity to internal-token-authenticated
`POST /internal/agent-work/query`: exactly `schema: "workspace.agent_work.query.v1"`,
`agentRunId`, `authorizationDigest`, `coordinationSessionId`, `toolCallId`,
`sourceAgentRunId` and `sourceToolCallId`. The query call identity is distinct
from the source dispatch locator. No client supplies or guesses a source turn.
The API resolves one full source Run/turn/call identity from retained events and
work bindings; multiple turns return 409 `agent_work_identity_ambiguous`.

Success returns `schema: "workspace.agent_work.query_result.v1"`, caller echoes
`agentId`, `sessionId`, `agentRunId`, `toolCallId`, `authorizationDigest`, locator
echoes `sourceAgentRunId`, `sourceToolCallId`, and `status`, `source`, `operation`,
`work`. `source`, when known, contains `sourceAgentRunId`, `sourceTurnId`,
`sourceToolCallId`, nullable `sourceEventId` (the original committed success) and
nullable `projectsToAgentRunStream`. `notRecorded` means no trusted committed
success was found in this snapshot, including a call without success. It makes
no promise about a future commit and grants no permission to repeat or rebuild.
`pending` means a committed success has no admission receipt. A rewritten source
can remain pending: its projection flag is false and materialization rejects it.
`admitted` returns the existing hosted operation receipt and `work` containing
`available`, nullable `agentRunStatus`, `returns` and nullable `output`; admission is not completion. Deleted
or permanently purged child Sessions retain admission, with availability false
and no child status exposed, with an empty `returns` list and null `output`. Each available return
contains its retained `notice`, nullable `attempt` (`attemptId`, nullable
`agentRunId`), `handled` and nullable `confirmation`. Confirmation identifies the
committed `eventId`, `callEventId`, `sequence`, `agentRunId`, actual `turnId` and
`toolCallId`. The existing source Run/call locator already selects one child
work; there is no additional notice query argument or new get tool. A completed,
available child can return its actual retained Final under `output`, exactly
`{schema:"workspace.agent_work.output.v1",source:"untrustedWorkOutput",sessionId,
agentRunId,eventId,throughSequence,body}`. The body comes from the latest verified
assistant Final before that Run's completed terminal event. It is untrusted work
output, and neither admission nor a return-notice index substitutes for it.
Files and Artifacts keep their existing separately authorized read paths.

The API verifies current signed native caller authority and original membership,
historical source signature/binding, same private coordination Session and current
source/child read authority. Permission rejection returns 403. Rewrite retains
the original event rows; public deletion and expiration retain Session tombstones
and work bindings. Thus supported lifecycle operations preserve the locator.
Receipt lookup uses only the exact resolved full identity, never unrelated opaque
hashes. Known conflicting/incomplete evidence returns 409. Query reads run in a
consistent PostgreSQL read-only snapshot; they never materialize, repair a binding,
request an execution profile or schedule work. Normal Core query call/result
recording does not constitute dispatch success or invoke admission.

Native private coordination additionally exposes `confirm_work_return`, with
exactly required `notice_id` and `attempt_id` model arguments and `continueTurn`.
Runtime supplies its accepted Run/authorization/Session and call identity to
internal-token-authenticated `POST /internal/agent-work/returns/confirm/validate`:
`schema: "workspace.agent_work.return_confirm.validate.v1"`, `agentRunId`,
`authorizationDigest`, `coordinationSessionId`, `toolCallId`, `noticeId`, `attemptId`.
The API resolves the original dispatch/child/fact identity from retained notice
and attempt bindings. Pending attempts and attempts owned by another Run fail;
current native ownership, ACL and original execution membership are required.
Validation is read-only and returns `workspace.agent_work.return_confirm.validated.v1`
with caller echoes, `noticeId`, `attemptId`, canonical `inputDigest` and
server-derived `noticeIdentity`. It does not mark the notice handled.

Handled is a rebuildable read-only projection of the matching effective canonical
confirmation call and successful `successWithOutput` result committed to Session.
The call must use the exact first-party provider, contract digest and normalized
arguments; the structured result must match that validation identity. The call
may have committed in an earlier batch. Only this tool's successful result receives
the additional authority/identity check immediately before a lease-fenced commit,
with admission locks acquired before the existing Session/job locks. No handled
table or scanner is added. Failed, denied, aborted, rolled-back or uncommitted
results, call-only records, ordinary Final, `send_message`, input ACK and Read do
not handle a return. Repeated reads retain the same first valid committed source.

Internal-token-authenticated `POST /internal/agent-work/materialize` accepts
exactly `schema: "workspace.agent_work.materialize.v1"` and `sourceEventId`.
The source must be an active committed `dispatch_work` success with its preceding
matching first-party `workspace.agent_work` call and exact contract digest. The
consumer input has exactly `objective`, `session_refs` and `file_refs`: a complete
non-whitespace objective within 65,536 UTF-8 bytes and at most eight opaque refs
per array. Unknown identities/aliases, failed results and foreign contracts fail.
The consumer validates the same first-party contract registered for native
coordination Runs; ordinary Session tool registries do not include it.

HTTP 201 means one new work admission committed; HTTP 200 returns the same
admission on replay. Both return exactly `schema: "agent.work.materialized.v1"`,
`sourceEventId` (the original first success identity) and `operation`, using the
existing hosted `submitMessage` acceptance receipt fields. The source Run/turn/call
tuple produces a deterministic scoped operation identity; changed input conflicts.
The request digest also binds the canonical first success event: if that origin
is erased, a later duplicate cannot reconstruct a different origin/receipt pair.
This receipt is independent of request/tool success and child execution state.
Accepted means admitted, not processed. Replay does not reschedule or retry a
failed child. A deleted/unavailable accepted work resource returns 410 rather than
creating a replacement; missing source returns 404 and untrusted/conflicting
source returns 409. Calls inside the source transaction are rejected.
Source validation and admission hold the source Session lock in existing hosted
Workspace/membership/Agent/Session order. A busy source returns retryable 503
`agent_work_source_busy` with no new work rows; the attempt rolls back rather than
waiting while holding Workspace/Agent locks. A later attempt rechecks committed
source facts, so successful rewrite invalidation rejects the old source with 409.

Admission rechecks original membership, private source authority, signed source
Run binding and explicit resource access. Only native private Agents are supported;
managed or delegated sources return 403. The fresh work Session/Run belongs to the
same user, Workspace and Agent, with its own authorization and current execution
profile.
New admissions require the selected model to remain enabled/current and its
explicit thinking mode to remain supported; receipt replay does not reselect it.
Explicit Library/SourceObject inputs get new Session links after current
ACL/generation/storage validation; no default parent input, workspace snapshot or
credential inheritance occurs.
The new input preserves the validated source generation/digest; a change after
validation does not upgrade that identity and remains subject to current reads.
Source-Session Artifact inputs return 403 `agent_work_file_scope_not_supported`.
Session refs remain source associations and
do not grant read access or enter child model context through this endpoint.
No request producer, background scan, Inbox, notice consumption or wake policy is
registered by this path. Existing lifecycle scheduling occurs only after commit.

## Delivered private work returns

The API retains an immutable `AgentWorkReturn` for the original admitted work Run.
The work binding and acceptance receipt identify the child; a later manual Run in
the same Session is a different input. Delivery uses committed ledger facts, never
an admission receipt, `AgentRun.status` alone, model supplement ACK or Job success.
Its sources are `sessionTerminal` (the canonical Core completed/failed/interrupted
tail record), `preAdmissionCancellation` (Runtime's fenced, cancellation-checked
hosted receipt without Session admission), or `lifecycleFailure` (the original
lifecycle Job failed/dead-lettered plus the corresponding hosted fault projection).
The latter has state `executionFailed`; it does not certify a Core terminal or work
success. A later committed Core terminal produces a separate immutable notice.

Internal-token-authenticated `POST /internal/agent-work/returns/materialize` accepts
exactly `schema: "workspace.agent_work.return_materialize.v1"` and `workAgentRunId`.
It rejects entry inside an application transaction. HTTP 201 is a delivered fact,
200 is the identical duplicate or `notWork`, and 202 is `pending` with null notice.
The response has `schema: "workspace.agent_work.return_materialized.v1"`,
`disposition` and `notice`. Missing Runs return 404; invalid binding/terminal or
changed fact identity returns 409. No caller-supplied outcome or text is accepted.
The scoped notice ID binds the original source Run/turn/tool call/event, operation,
work/coordination Sessions, user/Workspace/Agent and terminal fact digest.
`workspace.agent_work.return_notice.v1` preserves these under `identity`, with
`source: "workspace.agent_work.return"`, `terminal` and `content`. The content has
`source: "untrustedWorkOutput"`, `sessionRef` and `throughSequence`; child prose is
not embedded as a user message or privileged instruction. Normal ledger access
still governs reading referenced output.

Internal-token-authenticated `POST /internal/agent-work/returns/consume` accepts
exactly `schema: "workspace.agent_work.consume.v1"`, `noticeId` and `operationId`.
HTTP 201 commits the notice's first consumption attempt, native coordinator Run,
existing signed authorization and `consumeWorkReturn` operation receipt together.
HTTP 200 replays the same first attempt; a different operation cannot create a
second root for that notice. Reusing an operation for another notice returns 409.
HTTP 202 with `disposition: "busy"` retains the notice without allocating an attempt
or receipt. Source/child membership, current Session authority and native ownership
are checked again under the same Session lock used by ordinary message admission.
Scheduling occurs after acceptance; a lost response leaves the accepted queued Run
for the existing lifecycle reconciler. Admission does not imply model access,
Read, handling or success. Explicit retry remains outside this endpoint.

Internal-token-authenticated `POST /internal/agent-work/returns/consume/discover`
accepts exactly `schema: "workspace.agent_work.consume_discover.v1"`, `limit`
(integer 1–100), `after` and `through` (nullable notice ID cursors). `after`
requires a non-null upper bound and must not exceed it. The repeatable-read,
read-only response has `schema: "workspace.agent_work.consume_discovered.v1"`,
`entries`, `through` and `next`. Entries contain exactly `cursor`, `noticeId`
and the API-derived stable automatic `operationId`. The upper notice ID is frozen
for a pass; ordering uses the existing notice primary key. Discovery performs no
Runtime HTTP and creates no admission. Any first attempt excludes its notice,
regardless of that attempt's Run outcome.

The existing return worker control thread runs delivery and automatic consumption
passes with independent budgets and progress hints. It forwards each candidate
to the existing consume endpoint; busy or failed requests retain first eligibility,
while committed attempts remain excluded after a lost response or restart. Current
authority is checked at admission, not granted by discovery. Acceptance followed
by scheduling failure remains owned by the existing lifecycle reconciler and
preserves the original Run. Automatic admission does not establish Read or handling.

`workspace.agent_run.start.v2` requires `initialInput`: exactly
`{"type":"userMessage"}` or `{"type":"hostEvent","attemptId":...,"noticeId":...,"input":...}`.
The latter carries Core's exact `HostEventInput` JSON, generated by Runtime's
internal-token-authenticated `/internal/host-event-input` factory. The API does
not calculate its message ID. `prompt` remains the accepted objective and
`agentInstructions` remains the trusted Agent instruction snapshot; notice content
is non-authoritative data. v1 and unknown fields are rejected. Existing lifecycle
jobs remain `record:agent_run:<id>` references and rebuild the typed UserMessage
start from persisted facts without changing Run, Turn, job or authorization identity.

`POST /internal/agent-work/returns/query` accepts exactly
`schema: "workspace.agent_work.return_query.v1"`, `agentRunId`,
`authorizationDigest`, `coordinationSessionId`, `toolCallId` and `noticeId`.
`workspace.agent_work.return_query_result.v1` echoes the caller identities plus
`notice`. The active signed native coordination caller, original source/child
membership identities, current user/Session authority and retained binding are
checked in a repeatable-read read-only transaction. Foreign/deleted/revoked
resources return 403 without content. Restoring a condition of the same identity
may restore access; membership deletion/rejoin cannot. This query creates no
notice, binding, profile or Run. A retained acceptance binding remains historical
evidence after source projection rewrite or source event erasure.

After a terminal projection commits, the existing transition endpoint attempts
delivery without undoing that projection or blocking teardown on failure.
`POST /internal/agent-work/returns/discover` accepts exactly
`schema: "workspace.agent_work.returns.discover.v1"`, integer `limit` (1–100), and
nullable `after`/`through` work Session IDs. A null upper freezes the current maximum
binding key. Pages are lower-exclusive, upper-inclusive and inspect at most the
requested number of bindings, with one lookahead row. The result schema is
`workspace.agent_work.returns.discovered.v1`, with `entries`, `through` and `next`;
each entry has `cursor` (work Session ID) and `workAgentRunId` (original receipt Run).
Discovery is repeatable-read read-only and performs no delivery or Runtime HTTP.
The separate worker control thread then attempts individual materialization
requests within the existing five-second soft budget. Each attempted source
advances its cursor, including an error or lost response; unattempted tail rows
retain the same frozen upper bound for the next tick. Completed passes revisit
pending, failed and late-committed sources. This source walk is independent of
published Job outbox entries; volatile cursor loss restarts from the head.
Socket timeouts are not a strict total deadline or a throughput guarantee.

The initial schema includes the delivered-fact relation. Delivery and query do not assert input
access, consumption, handling or success of coordinator business work. No model
query tool, automatic coordination wake, consumption attempt, confirmation or
retry entry is registered at this boundary. Future Read projection requires an
actual loop input-access record and timestamp; persistence, queueing, admission
and supplement ACK cannot supply that fact. Read and explicit handled confirmation
are separate outcomes.

## Agent input facts

The Agent page has an independent read-only work index:
`GET /api/agents/{agentId}/work-sessions?limit=50&afterSessionId=...` returns exactly
`{schema:"agent.work_sessions.v1",agentId,workspaceId,sessions,nextAfterSessionId,hasMore}`.
The optional cursor is an opaque child Session ID from this list. Pages follow
descending creation time and ID, with limits 1–100. Only authoritative
`AgentWorkSession` bindings in the current coordination tree are eligible;
unrelated Sessions and internal native subagents do not become work Sessions.
Owner-session authentication and current read access are required.

Session responses include `initialInputOrigin`, null for ordinary human input or
exactly `{messageId,agentId,agentName}` for a verified dispatched first message.
Clients attach `Send By AgentName` only to that exact transcript message ID.
Dispatch-created work references attach to the first trusted public reply after
their dispatch; each reply keeps its own explicit references.

The prepared hosted input boundary is separate from the committed Agent output
feed. `POST /api/agents/{agentId}/inputs` requires the current owner session and
exactly `schema: "agent.input.submit.v1"`, `inputId`, `body` and `attachmentRefs`.
The references are sorted, unique coordination-Session asset-link IDs (maximum
50); use an empty array for text-only input. Uploaded images are attachments.
The server captures each selected asset's immutable identity and metadata at
acceptance and rechecks access; later material changes cannot replace its bytes.
The caller chooses
a stable input ID of 1–64 ASCII letters, digits or `_.:-`; the Agent scopes that
identity. Preserve the ID across uncertain responses. Body text is retained
exactly, including whitespace and Unicode, within 65,536 UTF-8 bytes; blank text
requires an attachment, and NUL-containing text is rejected. Clients cannot choose a Run, Session, server
sequence, timestamp or Read state.

HTTP 201 commits a new input fact. HTTP 200 replays the same fact for the same
ID/body/attachmentRefs. Different content for the same scoped ID returns 409
`agent_input_conflict`. Current authority is checked before replay. Deleting and
rejoining membership cannot revive the old acceptance identity; such a replay
returns 403. A short coordination Session lock conflict returns 503
`agent_input_admission_busy` without accepting a partial input.

The accepted response has exactly `schema: "agent.input.accepted.v1"`, `agentId`,
`sessionId` and `input`. Each input has exactly `inputId`, `sequence` (positive
integer), `createdAtMs` (server milliseconds), `body`, `attachments` and `read`.
Each attachment is exactly `{inputRef, displayName, contentType}`. Acceptance does
not imply execution intake or Read.

`GET /api/agents/{agentId}/inputs?afterSequence=0&limit=50` returns exactly
`schema: "agent.inputs.v1"`, `agentId`, `sessionId`, `inputs` and
`nextAfterSequence` (integer or null). Input rows use the same shape as acceptance
and follow the server input sequence, independently of lexicographic IDs. Limits
are 1–100; unknown, repeated and snake_case query aliases fail. The existing
`/messages` feed continues to contain committed `send_message` output. A client
may show both feeds without equating their independent sequence domains or
inventing a one-input/one-reply relation.

Read is null until the owning coordinator Run commits a main model-request
fact whose `input_uptake` observation contains the original `inputId`. Its
positive shape is exactly
`{"agentRunId": "…", "eventId": "…", "requestId": "…", "createdAtMs": 0}`,
identifying the first such committed fact in Session ledger order. The event
must belong to the input's admitted Run, Session, Workspace and original
membership, with the queue's admitted authorization digest. Model-request facts
are internal ledger records; their UI stream projection flag is not a Read gate.

Acceptance, claim, queue ACK, compaction requests, summaries and assistant output
do not establish Read. The committed intake fact remains authoritative if a
subsequent ACK or model dispatch fails. Read does not certify a successful model
response, one-input/one-reply correspondence or handled confirmation. Production
recovery and the public-pin release gate remain required
before this candidate's execution delivery is certified.

The first-release initial schema includes immutable input carriers, hosted queue
ownership and claim/ACK bookkeeping. Initialization creates no carriers, Runs,
Read facts or inferred owners. Native deliveries require a non-null delivery
sequence and retain the queue's explicit initial host owner. Runtime schema
version 4 admits Core's `input_uptake` observation kind. Reopening current storage
retains observation content, manifests and Session facts; unsupported or altered
schemas reject without writes, with no development-version conversion.

The owner-set Agent model policy is captured when a new coordinator Run is
admitted; active Runs retain their admitted model and effort. Idle admission,
active batched uptake and serialized Final-race handoff require the remaining
Core/Runtime integration gates. Ordinary text does not approve an approval wait,
answer a question wait or release a runtime-job wait.

### Unified Agent history

`GET /api/agents/{agentId}/history?limit=50` accepts `beforeCursor`, `afterCursor` and
`limit` (1–100). The two cursor parameters are mutually exclusive. With no
cursor, it returns the newest page; `beforeCursor` loads older rows, while
`afterCursor` polls newer rows. Each page remains in chronological order. The exact response is `schema: "agent.history.v1"`, `agentId`,
`sessionId`, `items`, `nextCursor`, `newestCursor` and `hasMore`. An item is exactly
`{cursor, kind: "input", input}` or `{cursor, kind: "message", message}`, using
the existing input/output fact shapes. Render this server order; independent
input `sequence` and output `sourceSequence` must not be merged numerically or
sorted by timestamps.

Input acceptance captures the committed Session ledger waterline under the same
Session row lock used by production Runtime event append. This immutable anchor
orders an input after existing source facts and before future source facts;
the input sequence orders inputs sharing an anchor. Source outputs sort by their
committed result sequence. The opaque cursor contains the Session identity and
all ordering components, with strict scope/shape validation on each authorized
request. The server reads inputs, outputs and each matching call in one SQL
snapshot, then applies existing trusted `send_message` validation. It does not
write synthetic Core events or add a second history table.

`nextCursor` records the last candidate scanned in the requested direction,
including ignored output rows. `newestCursor` records the newest scanned
candidate. Preserve the older-page cursor while polling with `newestCursor`;
prepending older pages must not change the newer polling boundary. Empty pages
preserve the requested cursor (initially null); `hasMore` describes remaining
candidates in the requested direction. POST confirms acceptance identity; reconcile timeline placement with
history rather than inserting it ahead of unseen earlier output. Future Read
updates refresh the same input row without moving its cursor. Fetching a cursor
does not establish Read.

The first-release initial schema requires every accepted input's source anchor
and includes the saved owner model policy. Initialization guesses no history
order and creates no input facts or policy choices. Fresh-schema, history and
concurrency validation remain required for a changed candidate.

### Agent Run and file previews

`GET /api/sessions/{sessionId}/preview?limit=50` is a private, read-only
projection. It accepts only `limit` (1–100) and `afterArtifactId`. The exact
response is `{schema: "session.preview.v1", sessionId, runFact, outputs,
nextAfterArtifactId, hasMore}`. The cursor is the last returned Artifact ID
when another page remains; otherwise it is null. Only currently authorized,
published, undeleted Artifacts from this Session are outputs. They retain
`ownerKind: "artifact"` and a null `inputRef`; previewing does not create a
Library object.

`runFact` is null when no current Run fact is available, or exactly
`{agentRunId, status, sourceType, eventId, createdAtMs}` for this Session's
latest Run. Queued/running facts use `sourceType: "hostedRun"` and a null
event ID. Completed/failed/cancelled facts reuse committed terminal Session
events (`sourceType: "sessionEvent"`); an existing pre-admission cancellation
uses `sourceType: "preAdmissionCancellation"` and a null event ID. The Agent
overview reads its coordination Session; a work Session reads its own facts.
Child completion, Read, handled and absence of an active Run do not establish
Agent completion. No waiting/paused states or execution controls are exposed.

`GET /api/agents/{agentId}/messages/{messageId}/files` accepts no query fields.
Its exact response is `{schema: "agent.message.files.v1", agentId, sessionId,
messageId, agentRunId, files}`. Each file corresponds, in first-reference order,
to a distinct `fileRef` from the original trusted, committed `send_message`.
It reuses that Run's signed authorization and captured input resolver; an owned
object outside that binding cannot be substituted.

Both projections use the exact file shape `{inputRef, agentRunId, ownerKind,
objectRef, sourceVersion, sha256, displayName, contentType, sizeBytes,
previewUrl, downloadUrl}`. Owner kinds remain `artifact`, `sourceObject` or
`userLibraryObject`. Metadata and content are private `no-store` responses.
Content URLs are `/api/sessions/{sessionId}/outputs/{artifactId}/{action}` or
`/api/agents/{agentId}/messages/{messageId}/files/{inputRef}/{action}`, where
action is `preview` or `download`. They require exactly `sourceVersion`,
`sha256` and `lang` (`en` or `zh-CN`). Current access and the original selected
version are checked again at actual storage open. Missing/revoked access is
denied; changed versions return 409 rather than falling back to current bytes.
Office preview loading preserves the same bound URL and version. Refresh and
retry reload preview content only; they do not retry, wake or rerun an Agent.
The Agent client propagates actual content responses with status 401, 403,
404, 409 or 410 to the owning metadata cache and clears the matching file
selection. The authorized Session remains available. Network and 5xx errors
retain metadata and preview retry; a late failure cannot clear a newer file
selection. Shared Session preview consumers without the optional error callback
retain their existing presentation.

### Agent model settings

Private-owner `GET /api/agents/{agentId}/model-settings` returns exactly
`{schema: "agent.model_settings.v1", agentId, modelConfigRef, thinkingMode,
status}`. Status is `unconfigured`, `configured` or `unavailable`. The first has
null selection/effort and requires configuration before a new idle Run;
unavailable preserves an unusable saved selection and never substitutes another
model. Configured means a current enabled ModelConfig and effective effort are
selected, not that credentials, quota or provider connectivity have passed.

`PATCH` accepts exactly `schema: "agent.model_settings.update.v1"`,
`modelConfigRef` and `thinkingMode`. Both latter fields are required and nullable.
A selected ref with null effort explicitly resolves the selected ModelConfig's
default and persists that effective value. A null ref requires null effort and
clears the setting. Explicit effort reuses existing supported-mode validation.
GET/PATCH accept no query fields. The same response shape returns the saved
policy; a null response effort represents an empty saved effective effort.

The policy belongs to the owned Agent instance, including definition-backed
instances; managed definition content remains separate. Existing and default
Agents begin unconfigured. Browser preferences may assist an explicit setting
choice but do not supply authority. New coordinator admission must snapshot the
setting under the Agent admission lock into the existing Run/authorization.
Changing or clearing settings does not update, restart or cancel an active Run.
Idle native work-return admission now snapshots this Agent policy under the
existing Agent/Session admission locks. It rechecks after the Runtime profile
RPC and returns 409 `agent_model_not_configured` or `agent_model_not_available`
without spending the notice when no usable policy exists. The source/child Runs
retain independent signed authorization and original membership checks; their
model does not override the new coordinator selection. Accepted coordinator
replay verifies its own signed model/effort snapshot rather than later settings.
These behaviors remain untested on this prepared candidate. Ordinary typed user
admission and active native intake still require the Core adapter.

Generic Session message submission to the dedicated coordination Session returns
409 `coordination_session_requires_agent_input`, before spending an operation ID.
The server rechecks the binding after the Runtime profile RPC under the same
Workspace lock as binding creation. Use the Agent input endpoint; matching the
configured model does not grant a second admission path. Ordinary work Sessions,
including those owned by the same Agent, retain their explicit model behavior.
No directory-first or recent-Session fallback is permitted for coordinator Runs.

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
a tombstone. Agent trash expiration also collects its captures. Membership loss
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
AgentRun authorization uses schema `workspace.agent_run_authorization.v1` and
signature domain `workspace:agent-run-authorization:v1`. Knowledge processing
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
(`schema`, `type`, `text`), and terminal `result` or `error`. Error frames have
exactly `schema`, `type`, `reasonType`, and nullable `httpStatus`; they carry a
sanitized failure code, never provider response text or credentials. Runtime
maps these hosted failure facts to Core's model error kinds. Configuration and
authentication failures stop immediately; transient provider failures retain
the existing retry behavior. A failure after a delivered result does not emit
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

Redis live snapshots and SSE signals carry optional `reasoning` alongside the
answer under one monotonic revision. Its exact fields are `blockId`, `requestId`,
and `text`. Atomic Redis writes update the cached snapshot and signal together;
browser restoration reads both body and reasoning from that snapshot. Committed
seals replace matching live blocks without changing their disclosure identity.
Runtime restart recovers available cached partial reasoning through Core before
settling the interrupted run. Missing or expired cache does not fabricate text.

Workspace, Agent, and Session creation generates `ws_`, `agent_`, and `session_`
identifiers followed by 16 case-sensitive Base64url characters (`A-Z`, `a-z`,
`0-9`, `-`, `_`), encoding 12 cryptographically random bytes without padding.
Other resource identifiers retain their own generation rules. Resource IDs are
opaque, immutable references: clients must not split, lowercase, or reinterpret
them. Creating these resources through the ORM retries generated primary-key
collisions up to three total attempts; explicit IDs and other integrity errors
fail. Possession of an ID does not grant access: ownership and workspace
membership checks still apply.

## Shared Agent definitions

Workspace owners and admins manage definitions under
`/api/workspaces/{workspaceId}/agent-definitions`:

| Method and suffix | Behavior |
| --- | --- |
| `GET` / `POST` collection | List definitions / create a draft |
| `GET` / `PATCH` `/{definitionId}` | Read / update draft configuration or active/disabled status |
| `GET` / `POST` `/{definitionId}/versions` | List immutable versions / publish the current draft |
| `PUT` `/{definitionId}/availability` | Replace availability using `{scope, membershipIds}` |

Draft configuration uses exact public fields `name`, `description`, `instructions`,
`avatarKind` and `pluginNames`. `pluginNames` defaults to `[]`, rejects duplicate
or unknown identities, and selects complete Workspace-enabled packages; it does
not select individual Skills or tools. Responses return sorted names. Status is
`active` or `disabled`. Availability scope is `none`
(the default), `workspace` or `members`; member grants refer to current
WorkspaceMembership identities. Foreign Workspace members and unknown scopes
are rejected. A new membership after rejoining does not inherit an old grant.
Published versions freeze configuration plus the complete `pluginActivation`,
their version number, publisher and publication time. Public version responses
show the selected `pluginNames`. Publication requires a strictly empty JSON object
`{}`; published content cannot be edited.

Members use `GET /api/workspaces/{workspaceId}/available-agent-definitions` to
discover only active, published definitions available to them, with published
configuration rather than draft content. `POST` to
`/api/workspaces/{workspaceId}/available-agent-definitions/{definitionId}/instance`
also requires strictly `{}`. It returns `{agent}` with 201 for a new private
instance or 200 for the existing instance. Concurrent first use creates one Agent
per Workspace, owner and definition. Definition management conveys no permission
to read other users' private instances or conversation data.

Agent responses add nullable `definitionId` and `definitionVersionId`. Managed
configuration is displayed from the current published version; attempts to PATCH
definition-sourced configuration return 409 `agent_configuration_managed`.
Private Agents retain their own editable configuration and null definition fields.
Clients reuse the existing Session and message endpoints with the returned
`agentId`; no `definitionId` message argument is added.

New managed Runs, including continued Sessions and tail rewrites, recheck scope
and active status and capture the current published version plus the existing
instructions snapshot in the acceptance transaction. Unavailable definitions
return 403 `agent_definition_not_available`. Existing Session and Agent IDs stay
unchanged. Old Runs, historical reads and accepted operation receipt retries keep
their version and snapshot, subject to existing ownership and membership checks.
Stopping availability does not cancel or reinterpret accepted Runs.

Managed Run authorization freezes the selected version's exact Plugin activation,
covering Skills, CLI contributions, MCP servers and Hooks. Run acceptance requires
that selection to remain Workspace-enabled and match the available package
digests. Empty selection remains supported. Credential authority is a current
scoped binding, independent of the immutable configuration snapshot.

### Scoped connector approvals and bindings

Browser routes below use `/api`. Workspace routes require the owner/admin role;
approval creation and revocation require a superuser who owns the source approval.
Creation additionally requires that user to be the credential record's creator
(`created_by`), the current custodian. These controls reference existing encrypted
credentials; they do not create, import or rotate secrets.

| Method and route | Required body and controlled response |
| --- | --- |
| `POST /admin/mcp-bearer-credentials/{credentialId}/assistant-approvals` | `{workspaceId, definitionId, serverId}`; 201 `{approval}` for the source plugin, current source version and frozen resource |
| `DELETE /admin/mcp-assistant-credential-approvals/{approvalId}` | Permanently revoke the caller's approval; 204 |
| `GET /workspaces/{workspaceId}/agent-definitions/{definitionId}/credential-approvals` | `{approvals}` containing only unrevoked approvals of the current source version for this exact definition |
| `GET /workspaces/{workspaceId}/agent-definitions/{definitionId}/connector-bindings` | `{bindings}` for this exact definition |
| `PUT /workspaces/{workspaceId}/agent-definitions/{definitionId}/connector-bindings/{pluginName}/{serverId}` | `{approvalId}`; `{binding}` after exact scope, resource and current source-version checks |
| `DELETE /workspaces/{workspaceId}/agent-definitions/{definitionId}/connector-bindings/{pluginName}/{serverId}` | Remove this binding; 204 |

Approval responses have exactly `id`, `pluginName`, `serverId`, `resourcePath`,
`resourceDigest`, `displayName` and `approvedAt`. Binding responses have exactly
`id`, `pluginName`, `serverId` and `approvalId`. They contain no token, secret
reference, encrypted secret or authorization signature. There is no Workspace
administrator route for global secret enumeration or arbitrary secret binding.
Unknown fields fail; inaccessible scope returns 404, and invalid approval scope
returns `mcp_credential_approval_scope_invalid`. These interfaces support a scoped
approval picker and binding controls; a new real-secret management UI is outside
this delivery.

An approval fixes Workspace, definition, plugin, server, declaration resource path
and digest, plus credential identity and version. The declaration file digest
binds its endpoint URL. Different assistants may use different credentials for
the same plugin. Rotation makes old approvals and cached fingerprints unusable;
the custodian must explicitly approve the new version before rebinding. Approval
references protect their source record, including after revocation: deleting a
protected source through credential management returns 409
`mcp_bearer_credential_in_use`. Existing global
credentials and migrated records receive no automatic assistant authorization.
A shared service account authorizes the configured business capability; these
routes do not assert each user's downstream personal ACL.

`POST /internal/mcp-connectors/authorize` requires `X-Internal-Token` and exactly
`schema: "runtime.mcp_connector.authorization.v1"`, `agentRunId`,
`authorizationRef`, `authorizationDigest`, `pluginName`, `serverId`,
`resourcePath`, `resourceDigest`, `operation` and `bindingDigest`. Operation is
`connect` or `dispatch`; `bindingDigest` is required and nullable. Runtime obtains
server/resource identity from frozen package declarations, never model arguments;
the request has no `secretRef` or caller-selected credential reference.

The strict no-store response requires exactly
`schema: "runtime.mcp_connector.authorized.v1"`, `scope: "private" | "managed"`,
`bindingDigest` and nullable `token`. Dispatch always returns null token; connect
may release a bearer token only to the authenticated adapter. All MCP transports
check current membership identity, ownership and lifecycle, managed definition
availability, Workspace plugin enablement, frozen resource, approval, binding and
source version on every call. Lazy connect fixes the first fingerprint before
receiving a token. Runtime checks again on the real provider after initialization
and connection-queue waiting, and on every cached call. Revocation, mismatch or API
failure prevents provider execution without fallback or automatic reconnect.
This decision is not an atomic fence against a later revocation commit and does
not withdraw an external call already dispatched.

Private credential resolution requires persistent null Agent definition and null
Run definition version. Missing managed version data never selects that path.
The old `/internal/mcp-bearer-credentials/resolve` endpoint remains private-only;
the new adapter uses connector authorization. Delegated Runs additionally require
their current user-app grant before connector dispatch.

## User-delegated business applications

Applications act for an existing signed-in user. They use opaque bearer tokens,
not a separate service identity. Platform registration and user consent remain
browser-only and retain CSRF protection. These `/api` routes use strict camelCase
schemas; unknown fields and scopes are rejected.

| Method and route | Behavior |
| --- | --- |
| `GET /business-apps` | Signed-in user lists active applications. |
| `GET /admin/business-apps` | Platform superuser lists registered applications. |
| `POST /admin/business-apps` | Superuser registers `{name}`; status defaults to `pending`. |
| `PATCH /admin/business-apps/{appId}` | Superuser sets `{status: "active"}` or `{status: "revoked"}`; revoked applications cannot be reactivated. |
| `GET /account/app-delegations` | User lists their own grants, without access tokens. |
| `POST /account/app-delegations` | User consents to an application, Workspace, scopes and expiry, with exactly one non-null `definitionId` or `agentId` target. |
| `POST /account/app-delegations/{delegationId}/rotate` | User rotates their grant's credential with `{expectedCredentialVersion}`; response shows the new token once. |
| `DELETE /account/app-delegations/{delegationId}` | User permanently revokes their own grant; repeat revocation returns 204. |

Consent requires an active application and exactly one target: a currently
available published `definitionId`, or an active native `agentId` belonging to
the consenting user in that Workspace. A native target cannot have a shared
definition or already be a business user's branch. `scopes` must be a nonempty
list of unique values from the table below.
`expiresInSeconds` defaults to 3600 when omitted; a finite value ranges from
300 to 86400. Explicit `null` creates a grant that does not expire automatically.
The 201 response contains `{delegation, accessToken, tokenType:
"Bearer"}` with `Cache-Control: no-store`. The token is shown only once and only
its SHA-256 digest is stored. Grant metadata includes `id`, `appId`, `appName`,
`workspaceId`, `workspaceName`, nullable `definitionId`/`definitionName`, nullable
`agentId`/`agentName`, `scopes`, `issuer`, `audience`, `createdAt`, nullable
`expiresAt`, `revokedAt` and positive `credentialVersion`. Exactly one target is
present. The server fixes
issuer `centaeris-workspace` and audience `centaeris-workspace-api`; callers cannot
set these values, another `userId`, origin fields or a token digest.

Use `Authorization: Bearer {accessToken}` on the existing usage routes:

| Scope | Existing operations |
| --- | --- |
| `assistant:use` | List/get the granted assistant, list its available definition, obtain the user's private definition instance. |
| `sessions:create` | Create a Session for that instance. |
| `sessions:read` | List/get its Sessions, operation receipts, transcript pages/patches/content, turn metadata, active Run, context usage and citation details. |
| `messages:submit` | Submit messages/supplements and list usable models. A `sessions/new/messages` request also requires `sessions:create`. |
| `attachments:write` | Upload files to its Session or delete its attachment links. Inline message uploads additionally require this scope. |
| `events:read` | Subscribe to the existing Session event stream. |
| `artifacts:read` | Download artifacts and read file/citation previews or downloads associated with the granted assistant's Sessions. Existing source/file ACLs still apply. |
| `runs:cancel` | Request cancellation of an owned Run in its Session. |

Each request validates current application status, grant issuer/audience,
expiry/revocation, exact membership identity, definition availability, scopes and
object ownership. Malformed or unknown tokens return 401 `delegation_invalid`;
unavailable grants return 403 `delegation_not_available`, and missing scopes
return 403 `delegation_scope_forbidden`. Out-of-bound resources return 404.
Cookie identity combined with any Authorization header returns 400
`authentication_mixed`; a bearer token never falls back to a browser session.
Definition, Plugin, credential, global Library and delegation management routes
do not accept app bearer authentication. Rotation requires the owning browser
identity and CSRF. Its 200 response has the same envelope as issuance, with an
incremented credential version and one new token. A stale expected version
returns 409 `delegation_credential_conflict`; withdrawn or expired authority
cannot be renewed by rotation. Grant target, owner, membership, scopes and expiry
remain fixed. Rotation retains the grant ID, Agent, history and accepted work.
Old tokens fail authentication, including an old authenticated request that
crosses rotation before admission. Open delegated streams also recheck the
captured credential version. Issuance, rotation and first revocation append
audit metadata without token values or digests.

### Native business Agent applications

A native Agent grant supports only `assistant:use`, `messages:submit`,
`sessions:read` and `artifacts:read`. Its target is the owner's maintained business
root Agent. The trusted business backend authenticates its external users; this
grant does not make their browsers account administrators or assign them platform
accounts. Keep the application bearer credential in that backend.

`POST /agents/{rootAgentId}/business-branches/resolve` accepts exactly
`{businessUserId}` under app bearer authentication and `assistant:use`. The stable
external identity is an exact string of 1–256 characters, with non-whitespace
content and no control characters; case and Unicode are retained. This is the
only native usage request without `X-Centaeris-Business-Branch-Id`; supplying that
header here is rejected. It returns HTTP 201 for a fresh branch or HTTP 200 for
the existing branch, with exactly `schema: "agent.business_branch.v1"`, `branchId`,
`rootAgentId`, `businessUserId`, `agentId` and `sessionId`.

The server uniquely maps application, root Agent and business user identity to
one permanent private Agent and its coordination Session. The same identity
returns the same branch across elapsed time, grant reissuance and credential
rotation. Another application or root uses a different namespace. Resolution
copies configuration into a fresh private Agent; it copies no root conversations,
memory, files, snapshots or artifacts. A deleted branch or coordination Session
is not silently replaced.

For every subsequent native usage request send both `Authorization: Bearer
{accessToken}` and `X-Centaeris-Business-Branch-Id: {branchId}`, using the returned
private `agentId` in routes. The server fixes and revalidates that branch along
with the authenticated credential version. A missing branch returns 400
`business_branch_required`; an inaccessible or mismatched branch returns 404
`business_branch_not_found`. A branch ID is a resource identity, not a credential;
the application's authorization is still required.

| Route | Required scope and behavior |
| --- | --- |
| `POST /agents/{rootAgentId}/business-branches/resolve` | `assistant:use`; resolve the external user's durable isolated branch. |
| `GET /agents/{agentId}/coordination-session` | `assistant:use`; read that branch's existing binding. |
| `POST /agents/{agentId}/inputs` | `messages:submit`; submit the existing strict `agent.input.submit.v1` contract. |
| `GET /agents/{agentId}/inputs`, `/history`, `/messages` | `sessions:read`; read that branch's accepted inputs, actual Read evidence and committed messages. |
| Agent message file and child-work output routes | `artifacts:read`; retain the original Run/file association, source authorization and current access checks. |

Native grants can read the current branch's bound coordination Session and work
Sessions linked through the actual AgentWorkSession relation. The root's private
history, sibling business users and other applications are outside that branch.
Ordinary Sessions using the same Agent are also outside these app read routes.
The ordinary Session message/create routes are unavailable to native grants.
The owner maintains instructions and model policy on the root. New coordinator
Runs snapshot the current root policy; existing Runs and their child work retain
the source Run's accepted instructions. Per-branch configuration writes are
rejected. Owner-cookie-only `GET /agents/{rootAgentId}/business-branches?appId=...`
returns a read-only maintenance tree with branch, user, Agent, coordination
Session, status, Session count and creation time. Its envelope is
`{branches, nextAfterId}`; pass the non-null cursor as `afterBranchId` to continue.
`limit` defaults to 100 and ranges from 1 to 200; unknown or repeated query fields
fail validation. It does not resolve users or
create work. Ordinary owner Agent lists show the maintained roots rather than
duplicating every business user's private instance.

Owner-cookie-only
`GET /agents/{rootAgentId}/business-branches/{branchId}/sessions?appId=...`
expands a retained branch using its authoritative coordination binding and
actual `AgentWorkSession` relationships. It returns exactly
`{branchId, coordinationSession, workSessions, nextAfterSessionId}` with
`Cache-Control: no-store`. The non-null coordination object contains
`{sessionId, title, status, createdAt}`; each work item adds `sourceAgentRunId`.
`status` is the retained Session's `active` or `deleted` value, not the Run's
completion state. No prompts, file contents or execution status are returned.
Pass a non-null `nextAfterSessionId` as `afterSessionId` to continue the work
list. `limit` defaults to 50 and ranges from 1 to 100. Unknown, repeated or
invalid query fields and out-of-tree cursors return 400
`business_branch_request_invalid`. Missing, inaccessible or inconsistent
coordination bindings return 404 `business_branch_not_found` and are never
created or repaired by this read. Current owner membership remains required,
including inspection of retained deleted metadata. Bearer credentials cannot
use this maintenance endpoint. The outer list's `sessionCount` includes all
retained Sessions using the branch Agent; it is not a count of child work.

### Settings and backend integration guide

Settings → Applications groups grants by application. Each detail selects an
exact `delegationId` and exposes overview, API access, business users (native
targets only), and credentials. Root configuration remains a separate owner
Agent setting. API addresses come from the Web deployment's configured
`API_BASE_URL`, including any path prefix. Guide snippets use a backend
`CENTAERIS_TOKEN` environment variable; they never insert an issued credential.
Creation and rotation show an actual token only once. Rotation keeps the grant
and branch identities while immediately invalidating its old credential.

Native quickstart examples resolve a stable authenticated external user, submit
one retained input, and inspect the first input and message pages. They are not
a complete polling loop. Keep separate input and output cursor state, drain
non-null `nextAfterSequence` pages, and retain scan progress when the terminal
cursor is null. Advance processed messages using actual `sourceSequence` and
deduplicate by message `id`. A pending input's Read evidence updates that same
row: requery a window containing its original sequence, for example
`afterSequence=input.sequence-1&limit=1`. Continually advancing the input cursor
alone cannot observe those updates. Acceptance, Read and committed output are
different facts; one input does not imply one answer.

Published-assistant examples obtain the owner's definition instance, list
`GET /api/models` and select an actual `models[].id` as `modelConfigRef`, create
a Session, submit a message and open that accepted Run's SSE endpoint. Empty
or display-name model references are not usable. Keep the original
`operationId` and request body when acceptance is uncertain. SSE reconnection
uses the original opaque frame `id` as `Last-Event-ID`, separately for each Run;
neither native input sequences nor numeric source sequences are substitutes.
Live frames are transient; committed frames provide retained facts. Published
instances reuse the platform owner's definition instance and do not provide
native external-user branch isolation.

For a Dify workflow using a native grant:

1. Keep the token in a Secret environment variable. Configure each HTTP node's
   authorization as Bearer and select that variable without adding another
   `Bearer ` prefix. Use the configured Centaeris API address.
2. Authenticate the external user in the trusted business backend and pass its
   stable ID as Dify Service API `user`. Use the corresponding `sys.user_id`
   rather than `sys.conversation_id` or a model-generated identifier.
3. Build a JSON object in a Code node, for example
   `request_body = {"businessUserId": stable_id}`. In the resolve HTTP node,
   use POST and JSON body with the complete object variable
   `{{#encode.request_body#}}`. Do not interpolate an identity into a quoted
   JSON string. The resolve request has no business-branch header.
4. Parse the HTTP node's string `body` with `json.loads` in a Code node and
   extract `branchId` and `agentId`. Construct the strict
   `agent.input.submit.v1` object with a retained business request `inputId`,
   and send it to the returned Agent with
   `X-Centaeris-Business-Branch-Id`. Follow the input/message polling contracts.
5. Return actual committed messages according to the business application's
   response policy. A receipt or Read record is not the assistant's answer.
   These HTTP nodes are an integration flow, not an OpenAI-compatible or Dify
   chat-provider endpoint.

The API records accepting application, grant and credential version on each
input; clients cannot choose these source fields. Stable `inputId` retries under
the same grant retain the original input and credential version. Another browser,
application or grant cannot replay that input identity. Changed content retains
the existing 409 conflict behavior. Acceptance rechecks the current grant and
authenticated credential version under the admission lock.

Accepted inputs become durable tasks in the user's isolated branch. They use
the existing owner execution, child-work dispatch, Read, result-return and
recovery paths. A coordinator Run is not attributed to a single submitting grant.
Credential rotation, browser logout and withdrawal of application access do not
discard accepted inputs or terminate owner execution. Withdrawal blocks new
application requests; current owner, membership and resource authorization still
protect execution. This differs from definition-delegated Runs, which retain
their original Run-level grant checks before connector dispatch.

Within a branch, all Sessions share that private Agent's memory namespace; another
business user's Agent has a different namespace. Session snapshots and artifacts
keep their existing Session ownership and signed authorization. Message and work
tools reject cross-branch `session_refs`, even when both branches share the same
platform owner. Material tools still read only explicit inputs authorized for
the current signed Run. Neither root ownership nor knowledge of a file ID grants
access to sibling files. Sharing business source material requires explicit
authorized attachment links; branches do not inherit the root's private files.
Child work continues to start with a fresh Session workspace and explicitly
selected inputs; it does not mount every file in the branch tree automatically.

A backend integration resolves the authenticated business user's stable ID,
then uses that branch for intake and polling. For example, the following requests
all originate in the trusted backend (values are placeholders):

```http
POST /api/agents/{rootAgentId}/business-branches/resolve
Authorization: Bearer {accessToken}
Content-Type: application/json

{"businessUserId":"customer-42"}
```

```http
POST /api/agents/{returnedAgentId}/inputs
Authorization: Bearer {accessToken}
X-Centaeris-Business-Branch-Id: {returnedBranchId}
Content-Type: application/json

{"schema":"agent.input.submit.v1","inputId":"unique-business-input-id","body":"Process this user's request.","attachmentRefs":[]}
```

Read `/api/agents/{returnedAgentId}/inputs` and `/messages` with the same two
headers to observe acceptance, actual Read evidence and committed responses.
Retain the same `inputId`, body, branch and grant for uncertain retries of the
same input. Rotation preserves the grant ID; issuing another grant preserves
the branch but does not let it replay the old input's source identity. Resolution is
idempotent; do not invent a new business user identity when a response is lost.

`nextAfterSequence` is a continuation cursor only when non-null; the last page
returns null. Persist the last valid scan cursor and processed message
`sourceSequence`, deduplicate by message `id`, and retain progress when a page
ends. Input Read updates the original input row rather than adding a new input:
retain pending input sequences and requery a window containing those rows to
observe later Read evidence. Advancing beyond an input does not subscribe to
changes of that input.

Input acceptance, actual Read and committed messages are different facts. The
native API provides a persistent input/output stream, without a one-input,
one-answer correlation guarantee. A synchronous workflow integration must define
its own waiting, message association, concurrency and recovery behavior; an
acceptance receipt is not an answer.

### Published assistant application transport

Definition-target applications reuse the original Session ID, `operationId`/digest receipts, 409
concurrency outcomes, cancellation requested/terminal behavior and history
contracts. Browser and app retries share the same user-owned operation namespace;
receipt replay keeps the original acting application and grant. Multipart
`POST /sessions/{sessionId}/uploads` returns the existing `libraryObjects` and
`assets`; `assets[].id` is a `SessionAssetLink` used in JSON `attachmentRefs`.
Run `assetRefs` includes all Session attachments, whereas `messageAssetRefs`
includes only the submitted message's refs. Artifact references and authenticated
download URLs remain stable; citations describe tool sources rather than answer
spans.

An open event stream rechecks authority before each item and every five seconds
while waiting. Checks time out after five seconds and fail closed, so committed
revocation closes the stream within fifteen seconds. This adds no event type:
`session.stream.item.v1` live text remains a full snapshot with a revision, and
committed items retain `sourceSequence` and the Core event. Connector dispatch
also checks the Run's current grant after lazy initialization and queue waiting.
Accepted configuration snapshots do not preserve withdrawn security authority;
already dispatched external effects cannot be recalled by these checks.

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
