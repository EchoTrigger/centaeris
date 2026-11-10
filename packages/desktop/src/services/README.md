# Centaeris Desktop Services

`centaeris-desktop-services` owns the local product host shared by Desktop and CLI.
It composes the [Rust Agent SDK](../../../agent/README.md) and Core with profile
configuration, credentials, SQLite storage, workspace and Git services, resource
activation, schedules, process sessions and terminals. Core owns Session, Run,
tool and continuation semantics.

## Native host

`LocalAgentHost::start()` starts an embedded host using the selected local
profile. It owns recovery, background workers and the process-lifetime profile
writer lock. `CENTAERIS_DESKTOP_DATA_DIR` selects the profile. The background
service entry point starts the local RPC listener over the same lifecycle.

Embedded entry points call `dispatch_execution_helper()` before opening the
profile. When it returns an exit code, the entry point exits with that code.
This dispatches the existing Windows execution-helper invocation through the
SDK runner. The runtime executable and native session example use this bootstrap.

`host.connect(viewer_id)` registers a native viewer and supplies an SDK
`AgentClient` through the `SessionHost` adapter. Its Session operations use the
same admission, operation receipts and execution owners as RPC requests.
Desktop and TUI share these services through their existing entry points.

Dropping the client detaches its viewer. Accepted Runs stay host-owned.
`host.shutdown()` stops admission and awaits Run, worker, process and storage
drain. Native event delivery is bounded to 128 pending notifications. A closed
connection is reconciled through durable Session and Run state.

The [session example](examples/session.rs) creates and loads a Session. An
optional prompt also sends input, subscribes and waits for a Run using the stored
model. It awaits host shutdown after successful and failed Session operations.

```sh
cargo run -p centaeris-desktop-services --example session
cargo run -p centaeris-desktop-services --example session -- "Explain this repository"
```

## Product entry points

`rpc_client::RuntimeClient` provides the local product client used by CLI modes.
`run_rpc_stdio()` bridges standard I/O to the profile service and preserves
request IDs, notifications and lifecycle-control admission. The TypeScript
client lives in [sdk](../../../sdk/README.md).

The `centaeris-runtime` executable serves the local background service. It
accepts `--runtime-server` and `--runtime-server-endpoint`.
`centaeris-package-catalog`, `centaeris-mcp-contract` and
`centaeris-runtime-protocol-docs` provide catalog and contract utilities.
Hosted Workspace owns its PostgreSQL, Redis, authorization and sandbox
composition in [Workspace Runtime](../../../workspace/runtime/README.md).

See [Architecture](../../../../docs/architecture/Architecture.md),
[Runtime protocol](../../../../docs/reference/RuntimeProtocol.md) and
[ExecutionHost](../../../../docs/architecture/ExecutionHostContract.md).
