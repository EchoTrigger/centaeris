# Local Runtime protocol

The Local Runtime protocol is the versioned JSON-RPC interface exposed by
`cet --mode rpc`. The TUI and headless modes use the same app service in process.
Core owns Runtime semantics. This interface owns transport, client registration,
command routing and projections.

The v1 identity is:

```text
protocol = centaeris.runtime
protocolVersion = 1
coreProtocolVersion = 1.0.0
```

## Server and transport

RPC runs on the process standard input and output. The RPC process acquires the
exclusive profile lock before opening writable state. Launch the process with
`cet --mode rpc` and initialize the connection before sending application requests.

Frames are UTF-8 JSON-RPC 2.0 objects separated by `LF`. JSON strings escape
embedded newlines, so one frame occupies exactly one JSONL line. Empty lines are
ignored by the server. The server accepts requests only; an inbound response or
notification closes that client connection.

The local transport has explicit process-memory boundaries. Each input line
is limited to 16 MiB; encoded output frames are limited to 160 MiB (including LF).
The output limit accommodates the existing 25 MiB local preview payloads even
with JSON's worst-case six-byte escaping, with room for its envelope. These finite
resource caps are not throughput targets. Previously unbounded `session/load`
and `session/list` results exceeding the output limit return the typed error
`runtime_rpc_response_too_large`, without truncation or closing the connection.
Serialization writes directly into a capped buffer and stops before adding bytes
beyond the output limit, including the final LF. Domain-result construction is
separate and is not claimed to have a process-wide memory bound.
The TUI restores transcripts through bounded `transcript/page`, patches
and content ranges; the Session metadata list itself currently has no pagination. Oversized input closes that connection before JSON
parsing.

Request admission happens before spawning: at most eight ordinary requests per
connection and 32 across the server. Only initialization, AgentRun cancel/detach/
detach-viewer, app exit, Runtime shutdown and sidecar stop use separate lifecycle
control headroom of four per connection and eight globally. Reads use ordinary
capacity, even though they remain allowed during shutdown drain. This keeps slow
reads from occupying cancellation headroom. Control requests can still exhaust
their own finite lane. These are stability defaults, not measured throughput optima.
There is no extra queue of spawned tasks waiting for permits. Exhaustion closes
the offending connection; an admitted action continues and retains its capacity
until its handler and response enqueue finish. Reconnect to observe/reconcile
uncertain outcomes; never automatically retry a one-shot action solely because
its connection closed.

Each client has at most 128 queued messages and 256 MiB of output bytes; all clients
share a 512 MiB byte budget. Byte accounting includes the frame currently being
written, and is released on complete write or discard during disconnect. Queue message
or byte exhaustion signals connection closure and makes direct response delivery
fail; broadcasts continue to other clients. An oversized broadcast fails during
encoding before any client enqueue. No blocking send
runs while holding the client registry lock. Disconnect removes the viewer
binding and queued output, but does not cancel Runtime-owned AgentRuns or already
admitted host actions. Clients must resynchronize through the existing durable
read methods after reconnecting. Broadcast delivery is transient, not a durable
acknowledgement.

## JSON-RPC envelope

A request has the exact envelope:

```json
{
  "jsonrpc": "2.0",
  "id": "client-1",
  "method": "session/list",
  "params": {
    "request": {}
  }
}
```

- `id` is a non-empty string or signed JSON integer. `null` is invalid for a
  request.
- `method` is non-empty and has no surrounding whitespace.
- Clients emit `params` as an object containing `request`. If a method
  has an empty request type, the canonical client form is `request: {}`.
- The current server decoder also accepts an omitted or `null` `params`, maps
  `null` to an empty object, and supplies `{}` when `request` is absent. It reads
  the `request` member and does not reject its sibling members. Clients should
  emit the canonical form above rather than depend on those decoder details.
- The JSON-RPC envelope and each typed method request reject unknown fields;
  `command` and `kind` sidecar envelopes are not aliases.
- Public request and response fields use exact `camelCase`.

A successful response contains exactly one `result`:

```json
{
  "jsonrpc": "2.0",
  "id": "client-1",
  "result": {}
}
```

A `result` may be JSON `null`; it remains a present success result. The decoder
rejects missing result/error, both fields present (even if either is null), and
`error: null`. Direct typed deserialization and JSONL decoding share this rule.

A failed response contains exactly one `error`:

```json
{
  "jsonrpc": "2.0",
  "id": "client-1",
  "error": {
    "code": -32603,
    "message": "request failed",
    "data": {
      "code": "domain_error_code",
      "message": "request failed"
    }
  }
}
```

JSON-RPC codes are `-32700` for JSON parse failure, `-32600` for an invalid
request or method payload, `-32601` for an unknown method, and `-32603` for other
Runtime or domain failure. v1 does not emit a separate `-32602` invalid-params
code. Domain failures place the stable Runtime `code` and diagnostic `message`
in `error.data`. Clients make retry decisions from the domain code and operation
contract, not from `-32603` alone.

Batch arrays are not accepted. One JSON object is carried by each JSONL frame;
an array frame fails as an invalid request.

## Initialization

`initialize` is the first request on every connection. Every other method fails
until initialization succeeds.

```json
{
  "jsonrpc": "2.0",
  "id": "client-1",
  "method": "initialize",
  "params": {
    "request": {
      "clientKind": "tui",
      "viewerId": "rpc-client"
    }
  }
}
```

`clientKind` is exactly `desktop` or `tui`. `viewerId` is non-empty and unique
among connected clients. Repeating the identical registration on the same
connection is idempotent; changing either field or reusing a connected
`viewerId` fails.

The initialize request does not declare a desired Runtime or Core protocol
identity. The server returns its descriptor and the current client
clients validate that descriptor after receiving it. This is a
server-advertised, client-validated v1 handshake, not version negotiation.

The result has this exact field set:

| Field | v1 meaning |
| --- | --- |
| `status` | Exact value `ok`. |
| `runtime` | Exact value `centaeris-runtime`. |
| `protocol` | Exact value `centaeris.runtime`. |
| `protocolVersion` | Integer `1`. |
| `capabilities` | Supported protocol capabilities listed below. |
| `events` | Supported notification methods listed below. |
| `projections` | Supported projection identities listed below. |
| `buildId` | `sha256:` digest of the running Runtime executable bytes. |
| `coreProtocolVersion` | Exact Core protocol version, currently `1.0.0`. |
| `profileId` | Non-empty identity of the user-data profile. |
| `storeId` | Non-empty identity of the Runtime store. |
| `storeSchemaVersion` | Positive storage schema version, currently `4`. |
| `layoutSchemaVersion` | Positive user-data layout version, currently `2`. |

The v1 descriptor publishes these arrays:

```text
capabilities:
  json_rpc_2_over_jsonl
  session_log
  session_fork
  question_resume
  agent_run_intervention
  stream_replay
  runtime_store_actor

events:
  session/update
  runtime/config-changed

projections:
  runtime_event
  session_event
  session_projection
  agent_state
  headless_transcript
```

A packaged client also compares `buildId` with the SHA-256 digest of its bundled
Runtime executable. A mismatch means another build owns the profile endpoint;
the client must not continue against it.

## Fork before a user turn

`_centaeris/session/fork` creates a Session in the active profile from the
committed history before a verified user turn. The `request` object contains
`operationId`, the source `sessionId`, and `beforeTurnId`. The prefix excludes
the selected opening and all subsequent records. Forking before the first
opening creates a Session with empty history. The source history stays intact.

The Runtime returns `session_fork_active` for an active source and
`session_fork_stale` for a missing, stale, or ambiguous opening.
The result contains `operationId`, `sourceSessionId`, `sourceTurnId`, and the
child `sessionId`. A separate `session/prompt` request sends a prompt to the
child. Retry with the same fields to retrieve the same child after publication,
even if the source changes. Reusing an admitted operation ID with different
fields returns `operation_id_conflict`.

Set `params.recoverOnly` to `true` beside `params.request` to retrieve an
already published child. If no child was published, the Runtime returns
`session_fork_stale`. The recovery flag does not change the operation identity.

## Session model request ledger

`_centaeris/session/model_requests` pages model request starts and provider
usage recorded by the requested Session. Forked history stays in the child
conversation but does not count as a new request or charge. Editing a message
does not remove the original Session's usage records. Pass `nextSequence` as
`afterSequence` to read the next page.

## Typed model evaluation receipts

`_centaeris/model/evaluate_typed` records the operation before sending the model
request. Retry with the same session, operation ID, and input to recover a
completed receipt. If the request was admitted and a receipt is unavailable,
the Runtime returns `typed_model_operation_indeterminate`. Query
`_centaeris/model/typed_operation` for the recorded request, provider response,
and failure or receipt. A retry does not send another model request.

## Transcript page and committed patch reads

`transcript/page` reads the persistent transcript read model. Its strict
camelCase request contains `sessionId`, optional `projectionGeneration`,
optional `sourceHighWater`, and optional `olderCursor`. The local Runtime owns
the generation identity; an omitted generation selects the current local
generation and a supplied value must match it exactly.

An initial request without `sourceHighWater` captures the JSONL source tail as
`targetSourceHighWater`. The response separately reports
`projectedSourceHighWater` and `targetReached`; `page` is null while bounded
background projection is catching up. Later page requests keep the original
target so appends cannot move the initial view waterline.

`transcript/patches` accepts `sessionId`, optional matching
`projectionGeneration`, exclusive `afterSourceHighWater`, and optional frozen
`throughSourceHighWater`. It returns committed patches in ascending source
order plus `nextSourceHighWater` and `hasMore`. Reads come from projection
commits and never revisit raw session events. Empty patches intentionally
advance the cursor across committed events with no display change.

Both responses include their projection version and generation. A tombstone or
projection-generation change invalidates old cursors explicitly; it must not
silently fall back to full client-side event projection.

Oversized user, assistant, reasoning, notice, and tool-summary text is represented
by a stable `session-event:<eventId>:<field>` content reference, so one large block
cannot prevent a page cursor from advancing. `transcript/content-range` reads both
these references and `tool-output:<callId>` references using
`transcript.content.range.read.v1`. Each response contains at most 64 KiB of UTF-8
text and an exact continuation offset. Core validates the source event, visible
field, revision and byte length; the host binds the read to the session and current
projection generation. Clients automatically assemble referenced message text for
normal Markdown rendering; tool details retain one range with backward/forward
navigation instead of accumulating all previously read output.

The local SQLite adapter keeps one replaceable current-recovery slot per session,
projection version, and generation. That slot contains only the control frontier
and unresolved tool blocks; immutable projection commits and block versions do not
embed it. Historical checkpoint cadence remains a separate policy concern.

## Method registry

The registry separates shared Runtime semantics from execution and native Host
surfaces. This classification controls dependency ownership; it is not an
authorization grant.

<!-- BEGIN GENERATED:RUNTIME_METHOD_REGISTRY -->

`operationKind` classifies the observable effect: `read`, `desiredStateWrite`, `identityMutation`, `creation`, or `oneShotAction`. `retryPolicy` is one of `safeRetry`, `sameOperationId`, or `noAutomaticRetry`. A non-empty `reconcileMethod` names the registered method that observes or resumes an uncertain outcome.

### Shared Runtime

| Method | Operation kind | Retry policy | Reconcile method |
| --- | --- | --- | --- |
| `agent_context_usage_get` | `read` | `safeRetry` | — |
| `_centaeris/session/model_requests` | `read` | `safeRetry` | — |
| `_centaeris/session/compact_context` | `oneShotAction` | `noAutomaticRetry` | — |
| `agent_dead_letter_dismiss` | `identityMutation` | `noAutomaticRetry` | `agent_dead_letter_get` |
| `agent_dead_letter_get` | `read` | `safeRetry` | — |
| `agent_dead_letter_list` | `read` | `safeRetry` | — |
| `agent_dead_letter_replay` | `identityMutation` | `noAutomaticRetry` | `agent_dead_letter_get` |
| `_centaeris/session/activate` | `desiredStateWrite` | `noAutomaticRetry` | `workspace_get` |
| `_centaeris/session/answer_now` | `oneShotAction` | `noAutomaticRetry` | — |
| `_centaeris/session/answer_question` | `oneShotAction` | `noAutomaticRetry` | — |
| `_centaeris/session/delete` | `identityMutation` | `noAutomaticRetry` | `session/list` |
| `_centaeris/session/diagnostics` | `read` | `safeRetry` | — |
| `_centaeris/session/fork` | `creation` | `sameOperationId` | `_centaeris/session/fork` |
| `_centaeris/session/reorder` | `desiredStateWrite` | `noAutomaticRetry` | `session/list` |
| `_centaeris/session/search` | `read` | `safeRetry` | — |
| `_centaeris/session/agent-runs` | `read` | `safeRetry` | — |
| `_centaeris/session/agent-runs/live-snapshot` | `read` | `safeRetry` | — |
| `_centaeris/session/agent-runs/attach` | `identityMutation` | `noAutomaticRetry` | — |
| `_centaeris/session/agent-runs/detach` | `identityMutation` | `noAutomaticRetry` | — |
| `_centaeris/session/agent-runs/detach-viewer` | `identityMutation` | `noAutomaticRetry` | — |
| `_centaeris/session/agent-runs/cancel` | `identityMutation` | `noAutomaticRetry` | `_centaeris/session/agent-runs` |
| `_centaeris/session/supplement` | `oneShotAction` | `noAutomaticRetry` | — |
| `_centaeris/session/update_metadata` | `desiredStateWrite` | `noAutomaticRetry` | `session/load` |
| `agent_runtime_config_get` | `read` | `safeRetry` | — |
| `agent_runtime_config_reset` | `desiredStateWrite` | `noAutomaticRetry` | `agent_runtime_config_get` |
| `agent_runtime_config_set` | `desiredStateWrite` | `noAutomaticRetry` | `agent_runtime_config_get` |
| `_centaeris/model/typed_operation` | `read` | `safeRetry` | — |
| `_centaeris/model/evaluate_typed` | `oneShotAction` | `sameOperationId` | `_centaeris/model/evaluate_typed` |
| `agent_runtime_model_test` | `oneShotAction` | `noAutomaticRetry` | — |
| `mcp/catalog` | `read` | `safeRetry` | — |
| `mcp/configure` | `desiredStateWrite` | `noAutomaticRetry` | `mcp/catalog` |
| `agent_runtime_garbage_collect` | `oneShotAction` | `noAutomaticRetry` | — |
| `agent_runtime_job_get` | `read` | `safeRetry` | — |
| `agent_runtime_job_list` | `read` | `safeRetry` | — |
| `agent_state_get` | `read` | `safeRetry` | — |
| `transcript/project` | `read` | `safeRetry` | — |
| `transcript/page` | `read` | `safeRetry` | — |
| `transcript/patches` | `read` | `safeRetry` | — |
| `transcript/content-range` | `read` | `safeRetry` | — |
| `plugin/catalog_state` | `read` | `safeRetry` | — |
| `skill/source/list` | `read` | `safeRetry` | — |
| `skill/source/add` | `creation` | `noAutomaticRetry` | `skill/source/list` |
| `skill/source/remove` | `identityMutation` | `noAutomaticRetry` | `skill/source/list` |
| `skill/source/set_enabled` | `desiredStateWrite` | `noAutomaticRetry` | `skill/source/list` |
| `skill/source/ref` | `read` | `safeRetry` | — |
| `skill/catalog` | `read` | `safeRetry` | — |
| `skill/detail` | `read` | `safeRetry` | — |
| `skill/set_enabled` | `desiredStateWrite` | `noAutomaticRetry` | `skill/detail` |
| `skill/reload` | `oneShotAction` | `noAutomaticRetry` | `skill/catalog` |
| `plugin/detail` | `read` | `safeRetry` | — |
| `plugin/list` | `read` | `safeRetry` | — |
| `plugin/reload` | `oneShotAction` | `noAutomaticRetry` | `plugin/catalog_state` |
| `plugin/set_enabled` | `desiredStateWrite` | `noAutomaticRetry` | `plugin/detail` |
| `plugin/source_ref` | `read` | `safeRetry` | — |
| `session/catalog` | `read` | `safeRetry` | — |
| `session/list` | `read` | `safeRetry` | — |
| `session/load` | `read` | `safeRetry` | — |
| `session/new` | `creation` | `sameOperationId` | `session/new` |
| `session/prompt` | `creation` | `sameOperationId` | `session/prompt` |
| `runtime/shutdown` | `oneShotAction` | `noAutomaticRetry` | — |
| `terminal_manage` | `oneShotAction` | `noAutomaticRetry` | — |
| `schedule_manage` | `oneShotAction` | `noAutomaticRetry` | `schedule_manage` |
| `ssh_start` | `creation` | `sameOperationId` | — |
| `process_session_start` | `creation` | `sameOperationId` | — |
| `process_session_list` | `read` | `safeRetry` | — |
| `process_session_get` | `read` | `safeRetry` | — |
| `process_session_read` | `read` | `safeRetry` | — |
| `process_session_stop` | `desiredStateWrite` | `safeRetry` | — |

### Execution Host

| Method | Operation kind | Retry policy | Reconcile method |
| --- | --- | --- | --- |
| `process_capture` | `oneShotAction` | `noAutomaticRetry` | — |
| `sidecar_list` | `read` | `safeRetry` | — |
| `sidecar_start` | `creation` | `noAutomaticRetry` | `sidecar_list` |
| `sidecar_stop` | `identityMutation` | `noAutomaticRetry` | `sidecar_list` |
| `workspace_file_tree` | `read` | `safeRetry` | — |
| `workspace_read_file` | `read` | `safeRetry` | — |

### Native Host surface

| Method | Operation kind | Retry policy | Reconcile method |
| --- | --- | --- | --- |
| `app_exit` | `oneShotAction` | `noAutomaticRetry` | — |
| `initialize` | `desiredStateWrite` | `noAutomaticRetry` | — |
| `plugin/install` | `creation` | `noAutomaticRetry` | `plugin/list` |
| `plugin/remove` | `identityMutation` | `noAutomaticRetry` | `plugin/list` |
| `workspace_activate` | `desiredStateWrite` | `noAutomaticRetry` | `workspace_get` |
| `workspace_get` | `read` | `safeRetry` | — |
| `workspace_git_diff_get` | `read` | `safeRetry` | — |
| `workspace_git_file_diff_get` | `read` | `safeRetry` | — |
| `workspace_git_github_cli_status_get` | `read` | `safeRetry` | — |
| `workspace_git_view_get` | `read` | `safeRetry` | — |
| `workspace_git_review_get` | `read` | `safeRetry` | — |
| `workspace_git_review_diff_get` | `read` | `safeRetry` | — |
| `workspace_git_stage` | `desiredStateWrite` | `noAutomaticRetry` | `workspace_git_review_get` |
| `workspace_git_unstage` | `desiredStateWrite` | `noAutomaticRetry` | `workspace_git_review_get` |
| `workspace_git_commit` | `oneShotAction` | `noAutomaticRetry` | `workspace_git_review_get` |
| `workspace_git_status_get` | `read` | `safeRetry` | — |
| `workspace_open_folder` | `oneShotAction` | `noAutomaticRetry` | — |
| `workspace_remove` | `identityMutation` | `noAutomaticRetry` | `workspace_get` |
| `workspace_reset` | `desiredStateWrite` | `noAutomaticRetry` | `workspace_get` |
| `workspace_rename` | `desiredStateWrite` | `noAutomaticRetry` | `workspace_get` |
| `workspace_reveal_folder` | `oneShotAction` | `noAutomaticRetry` | — |

<!-- END GENERATED:RUNTIME_METHOD_REGISTRY -->

Method-specific payloads are the strict v1 request and response structures
owned by the modules routed from `packages/cli/runtime/src/handlers.rs`. The method
name, request structure, response structure, error behavior, and command scope
change together; a Host must not infer a generic CRUD schema from similarly
named methods.

The generated registry above and
`packages/cli/runtime/src/generated/runtime-methods.json` come from the declarative
Rust source in `packages/cli/runtime/src/runtime_command_registry.rs`. Run
`cargo run --locked -p centaeris-cli-runtime --bin
centaeris-runtime-protocol-docs -- --write` after changing that source; the
local CI gate runs the generator with `--check` and rejects stale artifacts.

The machine-readable registry contains method names and command scopes. This
repository does not currently publish a standalone JSON Schema catalog for
every method. `initialize` appears in the v1 Host-surface registry even though
it also controls connection registration; the classification describes the
current registry rather than an authorization boundary.

## Notifications and replay

The Runtime Server broadcasts notifications to every successfully initialized
client. A connected client receives no broadcast notifications before its
`initialize` request has registered the connection:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "session-id",
    "agentRunId": "agent-run-id",
    "payload": {}
  }
}
```

`payload` is one canonical stream projection. A notification is a wake-up and
live-delivery path, not the only copy of durable Session truth. After a missed
connection or uncertain delivery, clients obtain the projection and replay by
Session and AgentRun identity. See [Session events](SessionEvents.md).

`runtime/config-changed` always carries exact empty parameters:

```json
{"jsonrpc":"2.0","method":"runtime/config-changed","params":{}}
```

It tells clients to reload configuration through the normal read method; it
does not embed credentials or configuration values.

## AgentRun ownership and shutdown

At most one AgentRun is active for a Session. Starting one creates a lease owned
by the Runtime service. Attach/detach operations must use the initialized
connection's `viewerId`; clients cannot detach another viewer by supplying its
identifier.

`app_exit`, viewer detach, and an unclean connection loss end observation only.
They do not cancel an AgentRun or transfer execution to another client. A later
client discovers the same Session and AgentRun, reads its durable projection,
and attaches for further updates without repeating the original prompt.

Explicit AgentRun cancellation returns `cancelAccepted` and an `agentRun`
summary. Acceptance does not establish a terminal state: clients continue
observing until the authoritative AgentRun status is terminal. Natural
completion may win the cancellation race. An interrupted lease remains active
until Core closes input admission and the task releases its lease; a second
turn cannot race that cleanup. Failed input closure retains the lease.

An initialized client can request service shutdown with `runtime/shutdown` and
exact empty params `{}`. Its response is `{"disposition":"requested"}`; it
acknowledges the request, not process termination.
Shutdown stops admission,
allows five seconds for active work to finish, then requests Core interruption
with `reasonType: "shutdown"` and allows five seconds for cleanup. A committed
shutdown interruption projects as `stopped`, distinct from user cancellation.
These budgets bound service teardown; they do not prove external effects were
undone or that every task committed a terminal before exit.

After an abnormal exit, startup reconciles unfinished runs as `stopped` with
`runtime_server_recovered_interrupted`, preserves already committed terminals,
and uses Core's tool intent/receipt recovery without replaying unknown external
effects. A completed assistant message alone never proves AgentRun success.

Closing stdio ends the RPC service and starts bounded cleanup. The exclusive
profile lock remains held until the process exits.

## Failure and retry behavior

Malformed envelopes, unknown methods, unknown typed-request fields, stale
identities, missing credentials, ownership conflicts, and contract mismatches
are deterministic failures. A client that rejects the advertised initialize
descriptor treats that as a deterministic compatibility failure rather than a
transport retry. Clients do not retry these failures as disconnects.

Connection loss and explicitly retryable provider or MCP availability failures
may be retried only at the owning operation's defined boundary. A timed-out
client request does not prove the Runtime failed to commit it; callers recover
through durable identity and projection before repeating a mutating request.

## Change policy

This page describes the current clean-slate v1 implementation. A later protocol
change updates the owning Core or Host type, protocol tests, and this reference
together. Documentation does not require an unimplemented stricter handshake,
transport policy, error taxonomy, or schema catalog from the current release.

### Transcript display facts

Newly projected blocks may include `presentation` with the source `agentRunId`,
`sourceType`, `observedAtMs`, tool `displayTarget`, `durationMs`, and bounded tool
`operation` facts. Absence means unknown; existing projections are not repaired.
`run_boundary` notices carry authoritative run start/terminal timing and have no
visible body. Clients pair the same run identity and never sum parallel tool latency.

For recognized file/directory reads, tool operations may contain `contentStartByte`
and `contentByteLength`: a UTF-8 byte range within the unchanged raw tool output.
Readers show this range as the readable body and retain an explicit raw-output view.
Unknown result formats retain their original text without heuristic header removal.


## Runtime-owned process sessions

These local Host methods use the existing `{ "request": { ... } } parameter
wrapper. They do not change the model-visible `bash` tool or Core execution
semantics. The Runtime owns the OS process tree; the conversation `sessionId`
owns its logical record. Client connections and UI tabs are observers, so closing
them does not stop a process. Manual Host starts have `source: "hostCommand"`;
the native Agent adapter uses `source: "agentTool"` and retains the originating
Session, AgentRun and tool-call identities in its completion outbox.

| Method | Required request fields | Result |
| --- | --- | --- |
| `process_session_list` | `sessionId` | `serviceInstanceId`, ordered `processes` |
| `process_session_start` | `sessionId`, `serviceInstanceId`, `operationId`, `program`, `args`, `timeoutMs` | Process snapshot |
| `process_session_get` | `sessionId`, `processSessionId` | Process snapshot |
| `process_session_read` | `sessionId`, `processSessionId`, `cursor`, `waitMs` | Snapshot and output page |
| `process_session_stop` | `sessionId`, `processSessionId` | Current snapshot after stop request |

Call list before starting and retain its `serviceInstanceId` with the start
request. Reusing the same operation ID with identical arguments in the same
service instance returns the original process; different arguments conflict.
Spawn failures are also remembered. A service-instance mismatch rejects the start:
a client must not blindly replace the instance ID and retry an uncertain command.
Receipts last for the service lifetime, including after output/record eviction;
retrying an evicted successful operation reports expiration without spawning again.
These Host receipts and live process records are service-local. There is no
OS-PID adoption or process restart. Agent completion/output retention is described below.

`cwd` comes from the owning Session. `program` and `args` are an argv invocation;
shell syntax requires explicitly invoking the shell. Existing local execution
policy and process-tree ownership are reused. Standard input is closed. PTY,
interactive input and resizing are outside this slice.

Snapshots contain `processSessionId`, `sessionId`, `source`, `program`, `args`,
`cwd`, `state`, nullable `exitCode`, nullable `terminationReason`,
`cleanupComplete`, `outputComplete`, and nullable `error`. Stop is idempotent and
requests tree termination: `running -> stopping -> exited`. Only observed root
exit sets `exited`; pipe drainage and descendant cleanup are separate facts.
Natural exit may go directly to `exited`. A stop/timeout racing natural exit may
observe natural completion. Termination reasons are `stopped` or `timedOut` when
termination was requested successfully; natural exit has no termination reason.
A nonzero exit code is preserved. `cleanupComplete` records successful tree
cleanup, not a durable guarantee against deliberately escaping OS containment.

`timeoutMs` is the execution deadline (0 disables it, maximum 24 hours).
`waitMs` only bounds one output read (0–30,000 ms); it never kills the process.
Each observer maintains its own canonical decimal-string chunk cursor, initially
`"0"`. A page has `process`, `chunks`, `nextCursor`, `earliestCursor`, `gap` and
`hasMore`. Chunks contain `cursor`, `stream` (`stdout`/`stderr`) and `dataBase64`.
Decode incrementally per stream: bytes can be non-UTF-8 or split across chunks.
Ordering is pipe-observation order, not a claim of exact cross-stream OS ordering.
Use `nextCursor` for the next read. An evicted prefix sets `gap: true`; a future
cursor is rejected. `outputComplete` means the producer has finished draining,
while `hasMore` means retained chunks remain after this page.

Limits per Runtime: 32 active processes, 64 retained in-memory records,
1 MiB / 4,096 chunks retained output per process (whichever limit is reached
first), and 64 KiB per read. Completed records are evicted
oldest-first when admitting new records; active/incompletely-cleaned records are
never evicted. Start receipts are stored by service identity and operation key
in `runtime/work-receipts.sqlite3`, without a cumulative count ceiling. A compact
request digest and result preserve retry protection after entry eviction. Active processes prevent idle shutdown. Explicit service shutdown
closes admission and stops the owned process trees; failure to confirm cleanup is
reported. Deleting a Session or its subtree is rejected while it owns active
processes. Start and deletion share an admission gate; successful deletion discards
closed process records but preserves receipt tombstones.

Run `cargo test --locked -p centaeris-cli-runtime process_sessions`.
The wire fixtures are in `packages/cli/runtime/src/generated/process-session-samples.json`.

### Agent background commands and completion delivery

The Local Runtime registers `process_start`, `process_list`, `process_read` and
`process_stop` through Core's existing dynamic-tool provider port. Model arguments
are exact lower_snake_case: start takes `program`, `args`, `timeout_ms`; read
takes `process_session_id`, `cursor`; stop takes `process_session_id`; list takes
no arguments. Session and AgentRun identities are supplied by the Host, never
selected by tool arguments. The original foreground `bash` tool is unchanged.

Start writes an admission intent before creating a process. Its identity derives
from the originating AgentRun and tool call. Retries cannot launch another command,
including after a Runtime restart. Uncertain starts/outcomes are reported explicitly;
the Runtime never reconstructs a process by PID or guesses its exit code.

The completion worker waits for observed exit and drained output, then atomically
stores the result and bounded output pages below `runtime/process-completions`.
Output uses a separate file. `index.sqlite3` stores completion metadata; workers
query only its pending index. Existing JSON records are imported atomically once
and remain as migration backups. Completed history does not enter the worker
query and has no cumulative admission ceiling. Successful Session deletion
cleans up its retained records and output. Manual Host/TUI starts do not opt into
automatic Agent follow-up.

Delivery waits until the owning Session is idle. A succeeded originating AgentRun
permits follow-up; a missing, cancelled or failed origin suppresses it. A newer
cancelled/failed run also suppresses automatic follow-up. Cancellation of an Agent
does not stop its background processes: use `process_stop` for that separate action.
Each result is submitted through normal idempotent run admission with a deterministic
operation ID. The input is visibly labelled **Runtime background task completion —
automatic notification, not a user request**. It uses the existing Session input
representation; it is not a new Core event type. The message contains bounded
status facts and an output reference, never an entire command log. `delivered`
means a follow-up run was admitted, not that the model successfully interpreted it.
A lost outbox acknowledgement reuses the same receipt and does not create a second run.

Internal admission is independent of connected clients but shares the existing
Session lease, deletion fence and service-draining lock. Clients observe unknown
same-Session runs by reloading the authoritative transcript and attaching to its
active replay; workspace changes invalidate outstanding reloads. TUI ignores other
Sessions' updates and attaches to an idle visible Session's new run. Closing a Task
tab, switching workspaces or disconnecting clients does not control delivery.
Service shutdown stops owned processes and flushes terminal results without starting
new work. Restart preserves already-recorded results; unrecorded process outcomes
remain explicitly unknown. Completed output can be read by cursor even after the
live record has expired. Model-facing text is a lossy UTF-8 display; exact bytes
remain in the retained output records.

Run `cargo test --locked -p centaeris-cli-runtime process_agent` for focused checks.

## Local automation methods

`ssh_start` returns an ordinary process-session snapshot and uses the same
service-instance/operation identity boundary as `process_session_start`.
`schedule_manage` accepts a strict action-tagged request; action-specific replay
and revision rules are documented in [Local automation](LocalAutomation.md).
It is conservatively classified as a non-automatically-retried Host action.
Core JSON/events and the public `session/prompt` shape are unchanged.

### Bounded Session catalog

`session/catalog` receives `{ "request": { "mode": "recent", "cwd": "...", "limit": 50 } }`.
Modes are `recent`, `pinned`, `changes`, and `lookup`. `cwd` is optional for recent
and pinned pages; `lookup` requires `sessionId`. `limit` is 1–100 (default 50).
Unknown fields or modes fail. Responses contain `items`, `deletedIds`, `revision`,
`nextCursor`, and `reset`. Items have the existing Session summary shape.

Begin observation with changes and no cursor, retain its revision, then load
initial recent/pinned pages. Pass revision as cursor to changes; when nextCursor
is present, continue from the returned revision. Recent/pinned nextCursor values
are opaque and scoped. If reset is true, discard that cursor and reload the scope.
A reconstructed index or expired changes cursor also returns reset. Cursors are
local navigation state, not authoritative Session identities.

`_centaeris/session/delete` returns `deletedSessionId` and `deletedSessionIds`.
The latter enumerates the deleted parent and children for client view cleanup.
