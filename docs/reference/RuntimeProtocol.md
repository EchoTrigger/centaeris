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
| `storeSchemaVersion` | Positive storage schema version, currently `1`. |
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

## Method registry

The registry separates shared Runtime semantics from execution and native Host
surfaces. This classification controls dependency ownership; it is not an
authorization grant.

### Shared Runtime

```text
agent_context_usage_get
_centaeris/session/compact_context
agent_dead_letter_dismiss
agent_dead_letter_get
agent_dead_letter_list
agent_dead_letter_replay
_centaeris/session/activate
_centaeris/session/answer_now
_centaeris/session/answer_question
_centaeris/session/delete
_centaeris/session/diagnostics
_centaeris/session/project
_centaeris/session/reorder
_centaeris/session/agent-runs
_centaeris/session/agent-runs/replay
_centaeris/session/agent-runs/attach
_centaeris/session/agent-runs/detach
_centaeris/session/agent-runs/detach-viewer
_centaeris/session/agent-runs/cancel
_centaeris/session/supplement
_centaeris/session/update_metadata
agent_runtime_config_get
agent_runtime_config_reset
agent_runtime_config_set
agent_runtime_model_test
mcp/catalog
mcp/configure
agent_runtime_garbage_collect
agent_runtime_job_get
agent_runtime_job_list
agent_state_get
transcript/project
plugin/catalog_state
skill/source/list
skill/source/add
skill/source/remove
skill/source/set_enabled
skill/source/ref
skill/catalog
skill/detail
skill/set_enabled
skill/reload
plugin/detail
plugin/list
plugin/reload
plugin/set_enabled
plugin/source_ref
session/list
session/load
session/new
session/prompt
```

### Execution Host

```text
process_capture
sidecar_list
sidecar_start
sidecar_stop
workspace_file_tree
workspace_read_file
```

### Native Host surface

```text
app_exit
desktop_file_preview_read
initialize
plugin/install
plugin/remove
workspace_activate
workspace_get
workspace_git_diff_get
workspace_git_file_diff_get
workspace_git_github_cli_status_get
workspace_git_status_get
workspace_open_folder
workspace_remove
workspace_reset
workspace_rename
workspace_reveal_folder
```

Electron-only actions such as opening a directory picker, revealing a local
Plugin source, and tray events remain outside this registry. They do not become
Core or Runtime methods merely because Desktop exposes them.

Method-specific payloads are the strict v1 request and response structures
owned by the modules routed from `packages/runtime/src/handlers.rs`. The method
name, request structure, response structure, error behavior, and command scope
change together; a Host must not infer a generic CRUD schema from similarly
named methods.

The registry is code navigation for the bundled clients. This repository does
not currently publish a generated, standalone JSON Schema catalog for every
method. `initialize` appears in the v1 Host-surface registry even though it also
controls connection registration; the classification describes the current
registry rather than an authorization boundary.

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
by the initialized connection. Attach/detach operations must use that
connection's `viewerId`; clients cannot detach another viewer by supplying its
identifier.

`app_exit` and an unclean connection loss follow the same ownership cleanup:

- If a TUI-owned run loses its owner while exactly one Desktop connection is
  available, the same lease transfers to that Desktop owner.
- Otherwise the Runtime requests interruption of each run owned by the lost
  connection.
- An interrupted lease remains active until its Session actor persists the
  terminal state and releases the lease. A second turn cannot race cleanup.
- Runs owned by other connected clients are unaffected.

The Runtime Server exits only after it has no connected clients, no active
AgentRuns, and no queued, leased, or running background Runtime jobs for one
continuous idle window. The current implementation uses five seconds; that
duration is not a protocol identity and clients must not synchronize behavior
to it. A client process ending does not permit an unowned run or child operation
to remain indefinitely.

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
