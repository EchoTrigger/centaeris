# Web presentation delivery

Web commands and display updates cross an unreliable network. The browser does
not infer execution failure from an HTTP timeout, an SSE disconnect, or missing
text. Core owns execution semantics; the hosted API adapts durable Session
facts and the existing Runtime transcript projection for browser delivery.
Desktop and Web share presentation primitives without sharing transport lifetime.

## Ownership and normal path

| Boundary | Authority | Browser responsibility |
| --- | --- | --- |
| Command acceptance | Persisted operation receipt and request fingerprint | Retain the same `operationId` until acceptance is established. |
| Execution | Core/Runtime facts and the hosted execution projection | Display the observed Run state; a disconnected tab cannot cancel or fail it. |
| Delivery | Runtime transcript generation and source water | Apply ordered, replayable display updates before acknowledging a cursor. |
| Read attention | Session owner's persisted attention/read watermarks | Acknowledge content actually displayed in the visible current view. |

Creation and submission remain ordinary authenticated HTTP commands. A lost
response keeps its operation identity; an exception-triggered receipt lookup
can recover the accepted Session/Run. A missing receipt is not proof that a
previous request failed. Automatic transport recovery does not issue another
submission. An explicit retry retains the same idempotency identity.

Normal display changes use one subscription:

```text
GET /api/workspaces/{workspaceId}/presentation/events?agentId=...&sessionId=...
Accept: text/event-stream
Last-Event-ID: <last successfully applied cursor, when available>
```

The binding is workspace + agent + optional selected Session. Without
`sessionId`, the stream carries navigation updates. Selecting another Session
retires the old binding. It does not create another execution loop. One Run's
completion does not close the subscription; later Runs can use the same feed.

The browser receives updates directly rather than fetching transcript patches
after each notification. Older-history pages, attachments, evicted history
content, and explicitly expanded tool details remain demand reads. The hosted
adapter reads existing PostgreSQL/Runtime projections on scoped invalidation.
Navigation, metadata and unchanged transcript state are not fetched on a per-tab
timer. Agent history and work lists retain their existing demand-read endpoints;
one agent-scoped feed invalidates those reads without replacing their UI logic.

## Stream and recovery contract

Every data frame uses `workspace.presentation.v1` and exact camelCase fields.
Unknown fields, unsupported kinds and a mismatched binding are rejected.
The checked-in [wire fixture](../../../tests/fixtures/workspace-presentation.json)
is consumed by the server and browser tests.

| Frame | Meaning | Replay cursor |
| --- | --- | --- |
| `navigation` | Contiguous chunks of one scoped Session-list snapshot | None |
| `snapshot` | A bounded transcript tail at a frozen generation and source water | None |
| `patches` | Existing Runtime projection upserts/removals through the chosen water | None |
| `content` | Ordered UTF-8 ranges belonging to a referenced normal display block | None |
| `operation` | Bounded tool call/result display data | None |
| `live` | Replaceable, complete ephemeral text/reasoning snapshot | None |
| `viewRevision` | Decimal-string context, Agent history and work invalidation versions | None |
| `checkpoint` without SSE `id` | Current authoritative Run state and its target water | None |
| `checkpoint` with SSE `id` | Delivery boundary after preceding projection/content application | Scoped durable cursor |
| `reset` | Explicit `cursorInvalid` or `projectionInvalidated` recovery | Clears the old cursor |

The cursor schema is `workspace.presentation.cursor.v1`. It binds the workspace,
agent, selected Session, projection generation and source high-water mark.
Encoding is not authorization: every subscription and subsequent emission must
still pass the normal authorization boundary. Source and attention watermarks
travel as canonical nonnegative decimal strings, preserving integer identity in
JavaScript.

Initial bootstrap freezes source water `H`, reads the tail at `H`, sends its
normal display content and tool display metadata, then emits the cursor-bearing
checkpoint. A valid resume reads patches after the applied cursor through the
new frozen target. Projection rebuilds or invalid cursors produce an explicit
reset and replacement tail; readable content remains until that replacement can
be applied. The adapter does not silently substitute another projection format.

The browser applies frames serially. It advances `Last-Event-ID` only after the
owning projection and its pushed display-content prefix are ready. A disconnect
between a patch and its checkpoint therefore replays from the previous cursor.
Duplicate patches/ranges are idempotent; conflicts, gaps and binding changes do
not earn an acknowledgement. Retired view epochs reject late asynchronous data.

An authoritative terminal checkpoint can stop the Run timer before its final
content is available. That early checkpoint has no `id` and cannot skip missing
text on reconnect. The final applied checkpoint confirms the corresponding
content water. Live text remains visible until its committed replacement is
ready; live revisions never advance a durable replay cursor. Terminal state and
source water are observed in one PostgreSQL snapshot.

Navigation snapshots use `snapshotId`, contiguous `offset`, and `hasMore`.
The browser publishes the list only after the final chunk. An interrupted list
snapshot leaves the previous complete sidebar available; it cannot be mistaken
for Session deletion or a completed replay checkpoint.

Normal referenced content is pushed in sequential ranges of at most 64 KiB.
Display prefixes and the browser content cache are bounded; very large content
retains an explicit limited prefix and its original reference. Evicted historical
content can use the existing demand reader. Full tool model output is not eagerly
expanded. Tool display DTOs remain bounded, and a truncated operation can be
retrieved as an authenticated attachment through the operation download route.
These limits bound delivery memory; they do not truncate authoritative storage.

A single ephemeral live snapshot has a 4 MiB frame limit. A larger accumulated
snapshot explicitly ends delivery and enters bounded reconnect recovery; it is
not silently truncated. Committed normal content still recovers through the
range stream. Arbitrarily large live-snapshot fragmentation is outside this
contract. Navigation discovery also reads the current scoped list and retains
authorization checks; chunking bounds wire frames, not the cost of evaluating
and serializing an actual navigation snapshot.

## Shared invalidation and bounded database work

Migration `0028_presentation_changes` installs PostgreSQL triggers that append
scoped keys to `PresentationChangeOutbox` in the same transaction as source
changes. Runtime's direct event writes are covered without a second execution
loop. Keys contain identities and change kinds, never message bodies. Source
writes do not take locks on shared version rows; a separate relay advances
`PresentationChange` versions and consumes its bounded batch atomically in a
stable key order. These versions are display projections, never authorization
fences or substitutes for transcript source water.

The active invalidation axes are `navigation`, `state`, `context`, `history` and
`work`. Migration `0029_remove_presentation_authority` removes the unused global
permission-change axis and its dedicated source triggers. It removes only that
axis's derived version and pending rows; scoped versions, source facts and read
watermarks remain intact. Permission-only changes do not broadcast display
invalidations. Full authorization remains independent on its existing timer and
before each payload, including while producer reads are blocked.

Each ASGI application owns one lazy dedicated LISTEN connection, independent of
the ORM transaction pool. The configured database endpoint must preserve the
listener session; a transaction-pooling proxy cannot stand in for that session.
Source notifications and the shared 30-second fallback
consume at most four batches of 256 pending keys per source hint or reconciliation. Already
projected notifications only refresh versions, so relay notifications cannot
recursively trigger more draining. Lost notifications recover from durable
outbox/version state. Large backlogs do not block a fresh authoritative tail;
remaining rows wait for another bounded observation or maintenance invocation.

The existing resource-GC command consumes at most 4096 pending rows per run;
its dry-run does not mutate them. `drain_presentation_changes --limit 4096`
provides the same explicit maintenance boundary. Deployments must run resource
maintenance even when no browser is connected. These bounds limit one invocation;
they do not guarantee drain capacity for an arbitrarily high write rate.

A hub has at most 128 exact-grant observations, 512 subscriptions and 16
subscriptions per owner per process. Identical bindings share initial navigation,
state, versions and one blocking Redis signal waiter; different user, delegation,
credential version, branch and browser-session identities cannot share that
cache. The last subscriber cancels its owned tasks and closes the listener.
Listener recovery permits five reconnects; source SQL, connects, LISTEN and
cleanup have finite deadlines. Redis failure leaves committed recovery active.

A quiet hub reconciles version rows once per active scope every 30 seconds.
It does not reload unchanged navigation/transcript business data. Original full
authorization reads remain independent every five seconds per subscription and
before each payload; they are neither cached by a projected version nor merged
across frames. SSE still uses a long-lived HTTP request and a dedicated database
listener. Hosting it on request-billed serverless infrastructure is therefore not
a zero-idle-cost design; pool sizing and provider request-duration limits require
deployment acceptance.

`viewRevision` invalidates context usage, Agent history and the visible work-list
window. Duplicate/stale revisions do not fetch data. Hidden views pause reads;
returning visible calibrates once. In-flight invalidations collapse into a
following read. Errors await a new observation or explicit retry. A work refresh
visits only the user's already opened work-list page window. Agent history
catch-up reads at most four pages per invalidation or explicit continuation,
stops between pages while hidden, and retains a visible continuation control
and its cursor when newer pages remain. Note-save receipt retention and S3
object/history lifecycle remain separate storage changes.

## Attention and explicit read acknowledgement

Session is currently private to its owner. Its read state therefore belongs on
the existing Session row. A future shared-reader product needs an explicit
per-reader migration; it cannot reuse the owner's watermarks for every member.

| Session field | Meaning |
| --- | --- |
| `latestAttentionSequence` | Greatest durable Session sequence requiring attention |
| `readThroughSequence` | Greatest observed sequence acknowledged by the owner |
| `attentionRevision` | Monotonic version of attention, read, or manual-unread changes |
| `isManuallyUnread` | Independent explicit owner choice |
| `isUnread` | `isManuallyUnread || latestAttentionSequence > readThroughSequence` |

Attention derives from canonical `agent_run_completed`, `agent_run_failed`,
`agent_run_interrupted`, and `checkpoint_ref` with `status=paused_question`.
Ordinary running/interim assistant output does not create a blue dot. Question
checkpoints count even when excluded from the former per-Run event stream.
Observing a question is separate from answering it.

```text
POST /api/sessions/{sessionId}/read
{"throughSequence":"123"}
```

The acknowledgement requires the current owner and workspace membership, rejects
future water, and applies `MAX(readThroughSequence, throughSequence)` in a locked
transaction. It never clears `isManuallyUnread`. Duplicate or stale acknowledgements
cannot clear a newer notification. Reprojecting a terminal fact never resets read
state. Unchanged attention does not write; sidebar discovery aggregates candidate
facts in one grouped read rather than one query per Session.

The browser sends an automatic acknowledgement only while the selected Session
is visible, reading the latest content, and applied through its attention water.
Latest-content visibility is measured independently of automatic scroll following:
pausing following for a disclosure does not make a visible answer unread, and
content contraction can reveal the latest answer without resuming following.
Concurrent observations are coalesced. An unsuccessful acknowledgement waits for
another observation rather than starting a polling loop. Late sidebar/HTTP
responses merge attention fields by `attentionRevision`; older responses cannot
reverse a newer manual choice or read acknowledgement.

`PATCH /api/sessions/{sessionId}` with `isUnread=true` remains an explicit manual
mark. `isUnread=false` explicitly clears that mark and advances read state through
the current durable high water. Opening a manually marked Session can perform
that existing explicit-read action after its visible content is ready; merely
receiving background updates cannot.

Migration `0027_session_attention_watermarks` reconstructs attention from existing
Session facts. Previously read Sessions acknowledge their existing durable water.
The legacy boolean did not distinguish automatic from manual unread, so every
legacy true value is preserved as manual unread. First explicit entry or marking
read can clear it. Identities, events, owner, and recency are preserved. Once
nonzero watermark/revision history exists, rollback is rejected in favor of a
forward migration.

Migration `0030_session_manual_unread` renames the stored Session boolean to
`isManuallyUnread` without reconstructing events or changing stored choices,
watermarks, revisions or recency. The public derived `isUnread`, response
`isManuallyUnread` and explicit `PATCH isUnread` command retain their meanings.
The atomic migration also replaces the Session change-capture function, whose
column references PostgreSQL does not rewrite when a column is renamed.

Stop API writers and drain active API requests before applying this physical
column rename, then start the matching API revision. Old and new API versions
cannot run concurrently against the renamed column. Apply the migration chain
through `0030` before accepting new API traffic; upgrading only the Web image
cannot install these database and API changes. Historical migrations retain their
original field names, while current ORM code uses only the new stored name.

## Failure detection, authorization and resource ownership

HTTP commands/JSON reads have finite waits. The SSE client separately bounds
response headers and byte silence. Heartbeat comments demonstrate connection
liveness only; they do not mean execution progressed and do not acknowledge data.
A clean EOF is an interruption of this long-lived subscription, including after
an individual Run has completed.

Recoverable interruptions resume from the last applied cursor with jittered
exponential backoff. The default permits at most five automatic reconnects in
one minute and five consecutive unsuccessful reconnects. Duplicate navigation,
checkpoints and immediate EOF do not replenish that budget. Only a connection
that stays live for at least 30 seconds can reset consecutive failures.
Online/visibility wakeups use the same budget. Offline tabs wait for an online
signal. Retry-After is respected; a server delay above 60 seconds stops automatic
recovery. Authorization, protocol rejection and an exhausted budget surface an
explicit retry instead of an automatic request loop.
The producer has one pending frame, and the fetch reader awaits each application,
so slow rendering cannot create an unbounded application event queue.

The API rechecks authorization before emission and independently while producer
reads are pending. Projection/database reads have finite deadlines. Heartbeats
are independent of producer waits; a stuck producer eventually ends instead of
sending heartbeats forever. Ephemeral Redis reads have their own shorter bound;
Redis loss does not replace durable PostgreSQL authority. Disconnect cancels the
subscription's reads and returns connections; it does not cancel an AgentRun.

HTTP Session listing and navigation-only stream authorization share
`prepare_workspace_session_list`. It rechecks workspace membership, current
delegation credentials and, when `agentId` is supplied, the owned active Agent.
It returns the complete filtered Session QuerySet without evaluating it. Native
branch and published definition filters remain in that shared scope. Original
rejection order, statuses and request grant/branch refreshes are preserved.
An empty list still authorizes navigation; a row-existence check is not an
access requirement.

The navigation-only stream also retains its fresh active-user and definition
visibility checks. A selected Session continues through the existing Session
authority helper. Credential versions and business-branch identities stay bound
to initial authentication while current grants and branch state are rechecked;
a cached grant cannot bypass rotation, expiry or revocation.

Authorization checks do not load the Session list, refresh attention projections
or serialize sidebar rows. The actual `navigation()` read performs those steps
and sends a changed snapshot through the same filtered listing path. This removes
duplicate display work from authorization without changing which rows navigation
can return. Actual navigation discovery still has full-list query and
serialization costs; this contract does not establish a latency or capacity
improvement.

Responses use `text/event-stream`, `Cache-Control: no-store, no-transform`, and
`X-Accel-Buffering: no`. These headers do not prove that an external WAF forwards
bytes promptly. Proxy buffering, idle limits, compression and reconnection through
the school's actual deployment still require separate end-to-end acceptance.

## Focused validation

Use an isolated synthetic PostgreSQL instance with `CREATEDB` privileges. Configure
`TEST_POSTGRES_HOST`, `TEST_POSTGRES_PORT`, `TEST_POSTGRES_DB`,
`TEST_POSTGRES_USER`, and `TEST_POSTGRES_PASSWORD`; never point these commands at
an application or production database. Migration tests create/drop their own
old-schema databases. Set `PGCONNECT_TIMEOUT` to bound a missing test service.

```sh
uv run --frozen --package api python packages/api/manage.py test \
  app_core.test_presentation_stream app_core.test_presentation_authority \
  app_core.test_presentation_changes app_core.test_presentation_changes_migration \
  app_core.test_presentation_authority_removal_migration \
  app_core.test_presentation_observer app_core.test_presentation_notifications \
  app_core.test_presentation_service \
  app_core.test_session_attention \
  app_core.test_session_attention_migration app_core.test_session_attention_characterization \
  app_core.test_session_manual_unread_migration app_core.test_schema_wire_contract \
  app_core.test_workspace_session_list_queries app_core.test_session_projection_concurrency \
  app_core.test_http_query_contract app_core.test_http_error_contract \
  --settings=api.test_settings --noinput

pnpm --filter web exec node --test \
  tests/unit/workspace-presentation-stream.test.mjs \
  tests/unit/workspace-presentation-reconnect-budget.test.mjs \
  tests/unit/presentation-view-revision.test.mjs \
  tests/unit/api-deadlines.test.ts tests/unit/operation-receipts.test.mjs \
  tests/unit/session-read-state.test.ts
pnpm --filter web run test:components
```

| Injected condition | Protected result |
| --- | --- |
| Accepted command loses its response | Original receipt is recovered without a second automatic submission. |
| Silent headers/body, clean EOF, or lost final checkpoint | Bounded recovery resumes from applied water; no invented Run failure. |
| Terminal metadata arrives before a hung projection/content read | Timer can end; missing content is not acknowledged or skipped. |
| Patch applied but its checkpoint lost; duplicate/conflicting UTF-8 ranges | Safe replay succeeds; contradictory bytes cannot advance a cursor. |
| Invalid binding/generation or projection rebuild | Explicit reset; no cross-Session application. |
| Redis unavailable, slow consumer, or cancellation | Durable state remains authoritative; bounded work and owned resources are released. |
| Grant revoked during a pending read | No subsequent protected payload is emitted. |
| Duplicate successful frames followed by EOF | The shared reconnect budget still exhausts. |
| Quiet or duplicate-hint subscriptions | Business data is read once until its scoped version changes. |
| Concurrent relay, source rollback, missed notification or forward migration | Durable invalidation commits atomically and resumes without cross-source lock ordering. |
| Two readers switch active Runs concurrently | One owned live waiter survives, and old-Run text cannot populate the new-Run cache. |
| Navigation authorization repeated with empty or growing Session lists | Access decisions and rejection contracts hold without loading rows or updating attention. |
| Cached credential rotates, membership is replaced, or a business branch changes | Fresh authority checks preserve the original credential, definition and branch boundaries. |
| Split navigation interrupted before its last chunk | Existing complete list remains; no fabricated truncation. |
| Current view, background view, manual unread, or reading older text | Only eligible visible applied attention is automatically acknowledged. |
| Concurrent/stale read ACK, repeated terminal projection, late HTTP metadata | Read water and revisions remain monotonic; newer attention/manual choice survives. |
| Upgrade from existing read/unread history | Facts and owner choices survive reconstruction without recreating history. |

These are local synthetic protocol, HTTP/ASGI, persistence, and rendering checks.
Passing them is not production deployment acceptance or a school-WAF validation.
