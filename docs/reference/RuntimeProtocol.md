# Local Runtime protocol

The Local Runtime protocol is the versioned boundary currently used by Desktop
and TUI to share one profile-scoped Runtime. Runtime semantics remain in Core;
this protocol owns local transport, connection identity, command routing,
projections, and Host lifecycle. This page records the v1 implementation that
ships in this repository. It does not turn possible later handshake or schema
work into a requirement for the current release.

The v1 identity is:

```text
protocol = centaeris.runtime
protocolVersion = 1
coreProtocolVersion = 1.0.0
```

## Server and transport

One Runtime Server owns one user-data profile. A profile-wide writer lock
prevents a second server from opening the same writable state. The endpoint is
scoped by the profile identity and Core protocol version:

- Windows uses a local named pipe.
- Unix builds use a Unix domain socket below the Runtime data directory.

Only Windows x64 packages are currently built and release-tested. The Unix
transport implementation does not by itself advertise a macOS or Linux
artifact.

The endpoint name and `viewerId` are not credentials. v1 has no application
authentication exchange on this connection and relies on the operating
system's local endpoint protections. Tokio rejects remote Windows named-pipe
clients by default. Every Windows pipe instance also uses an explicit DACL that
grants access only to the profile-owning user and LocalSystem. The Unix socket
inherits the permissions of the Runtime data directory and process environment;
only Windows x64 is currently release-tested.

Frames are UTF-8 JSON-RPC 2.0 objects separated by `LF`. JSON strings escape
embedded newlines, so one frame occupies exactly one JSONL line. Empty lines are
ignored by the server. The server accepts requests only; an inbound response or
notification closes that client connection.

The Runtime executable exposes two transport entry points:

```text
centaeris-runtime --runtime-server-endpoint
centaeris-runtime --runtime-server
```

The first prints one JSONL object containing `endpoint`. The second acquires the
profile writer lock and serves that endpoint.

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
- Desktop and TUI emit `params` as an object containing `request`. If a method
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
      "clientKind": "desktop",
      "viewerId": "desktop-main"
    }
  }
}
```

`clientKind` is exactly `desktop` or `tui`. `viewerId` is non-empty and unique
among connected clients. Repeating the identical registration on the same
connection is idempotent; changing either field or reusing a connected
`viewerId` fails.

The initialize request does not declare a desired Runtime or Core protocol
identity. The server returns its descriptor and the current Desktop and TUI
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
| `layoutSchemaVersion` | Positive user-data layout version, currently `1`. |

The v1 descriptor publishes these arrays:

```text
capabilities:
  json_rpc_2_over_jsonl
  session_log
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
| `_centaeris/session/reorder` | `desiredStateWrite` | `noAutomaticRetry` | `session/list` |
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
| `session/list` | `read` | `safeRetry` | — |
| `session/load` | `read` | `safeRetry` | — |
| `session/new` | `creation` | `sameOperationId` | `session/new` |
| `session/prompt` | `creation` | `sameOperationId` | `session/prompt` |
| `runtime/shutdown` | `oneShotAction` | `noAutomaticRetry` | — |

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
| `desktop_file_preview_read` | `read` | `safeRetry` | — |
| `initialize` | `desiredStateWrite` | `noAutomaticRetry` | — |
| `plugin/install` | `creation` | `noAutomaticRetry` | `plugin/list` |
| `plugin/remove` | `identityMutation` | `noAutomaticRetry` | `plugin/list` |
| `workspace_activate` | `desiredStateWrite` | `noAutomaticRetry` | `workspace_get` |
| `workspace_get` | `read` | `safeRetry` | — |
| `workspace_git_diff_get` | `read` | `safeRetry` | — |
| `workspace_git_file_diff_get` | `read` | `safeRetry` | — |
| `workspace_git_github_cli_status_get` | `read` | `safeRetry` | — |
| `workspace_git_status_get` | `read` | `safeRetry` | — |
| `workspace_open_folder` | `oneShotAction` | `noAutomaticRetry` | — |
| `workspace_remove` | `identityMutation` | `noAutomaticRetry` | `workspace_get` |
| `workspace_reset` | `desiredStateWrite` | `noAutomaticRetry` | `workspace_get` |
| `workspace_rename` | `desiredStateWrite` | `noAutomaticRetry` | `workspace_get` |
| `workspace_reveal_folder` | `oneShotAction` | `noAutomaticRetry` | — |

<!-- END GENERATED:RUNTIME_METHOD_REGISTRY -->

Electron-only actions such as opening a directory picker, revealing a local
Plugin source, and tray events remain outside this registry. They do not become
Core or Runtime methods merely because Desktop exposes them.

Method-specific payloads are the strict v1 request and response structures
owned by the modules routed from `packages/runtime/src/handlers.rs`. The method
name, request structure, response structure, error behavior, and command scope
change together; a Host must not infer a generic CRUD schema from similarly
named methods.

The generated registry above and
`packages/runtime/generated/runtime-methods.json` come from the declarative
Rust source in `packages/runtime/src/runtime_command_registry.rs`. Run
`cargo run --locked -p centaeris-runtime --bin
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
The Desktop renderer has no route for this service command; its `app_exit`
continues to close only the application client. Shutdown stops admission,
allows five seconds for active work to finish, then requests Core interruption
with `reasonType: "shutdown"` and allows five seconds for cleanup. A committed
shutdown interruption projects as `stopped`, distinct from user cancellation.
These budgets bound service teardown; they do not prove external effects were
undone or that every task committed a terminal before exit.

After an abnormal exit, startup reconciles unfinished runs as `stopped` with
`runtime_server_recovered_interrupted`, preserves already committed terminals,
and uses Core's tool intent/receipt recovery without replaying unknown external
effects. A completed assistant message alone never proves AgentRun success.

The Runtime Server exits only after it has no connected clients, no active
AgentRuns or admitted host actions, and no queued, leased, or running background Runtime jobs for one
continuous idle window. The current implementation uses five seconds; that
duration is not a protocol identity and clients must not synchronize behavior
to it. Active work keeps the service alive even when no client is connected.
The profile writer lock remains held until the server process exits, including
bounded executor teardown after a shutdown timeout.

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
