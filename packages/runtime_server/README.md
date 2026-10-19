# Runtime Server

Runtime Server composes the public `core::runtime::AgentRuntime` for hosted
Workspace use. It adapts PostgreSQL persistence, Redis live delivery, Docker
execution, API model calls, authorized files, Agent Memory, and Plugin resources
without redefining Core semantics.

See [Workspace architecture](../../docs/architecture/Architecture.md),
[Workspace API](../../docs/reference/API.md), and the
[security model](../../docs/workspace/security/Model.md) for the complete boundary.

## Execution boundary

A Session owns one environment profile and controller. AgentRuns and maintenance
operations use the same allocation, host task protocol and cleanup receipts.
The profile selects the immutable image, workload user, workspace directory,
network and declared resources. Physical lifetime is either `perRun` or
`retained`.

The native host supervisor attests the Docker/runc target and owns process
launch, journals and output. Commands, files, inputs, artifacts, snapshots, MCP
and Hooks use that task channel. Workloads receive no Docker socket, host control
socket, control-plane credentials or writable host cgroup mount. Fixed helpers
come from the reviewed static binary and run through a sealed descriptor.
Agent Memory is resolved by Runtime outside the container.

The current native backend supports `OCI_RUNTIME=runc`. The repository keeps
its existing `runsc` default; connecting that backend to the same supervisor
contract is remaining integration work. There is no automatic runtime fallback.
See [native supervisor installation](../hosted_execution/deploy/README.md).

Plugin Skills, CLI programs, MCP servers and Hooks use the frozen activation
and Core tool/receipt contracts. Authorized attachments use `read(input_ref)`.
`PLUGIN_VOLUME_NAME` and `AGENT_MEMORY_VOLUME_NAME` remain required canonical
volume names; only the projected Plugin data enters workload containers.

## Internal service surface

- `POST /agent-runs/step` starts or resumes the exact authorized lifecycle job.
- `POST /agent-runs/cancel` requests cooperative cancellation.
- `POST /agent-runs/teardown` closes the Run scope, confirms cleanup and applies
  the environment's physical lifetime policy.
- `POST /internal/environments/provision` allocates an environment independently
  of an AgentRun.
- `POST /internal/environments/observe` reads allocation and physical facts.
- `POST /internal/environments/maintenance/task` and `/filesystem` execute
  independently signed maintenance operations.
- `POST /mcp/catalog` and `POST /hooks/catalog` inspect frozen package bytes
  without connecting to an MCP server or executing a Hook.

Different Sessions may execute concurrently. The API permits only one active
AgentRun per Session, and every request remains bound to its current lifecycle
lease.

## Persistence and live delivery

`PostgresSessionLog` implements Core `SessionLogPort`, and
`PostgresRuntimeStore` implements the same Core storage ports used by local
hosts. Django consumes those facts to build product projections; it does not
provide a second Session-log writer.

Redis carries bounded live state and is never durable truth. Terminal and
history projections converge from PostgreSQL. `DATABASE_URL` and
`RUNTIME_STATE_ROOT` are required.

## Focused gate

```powershell
cargo test --locked -p runtime_server
```
