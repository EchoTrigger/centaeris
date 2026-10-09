# Runtime Server

Runtime Server composes the public `core::runtime::AgentRuntime` for hosted
Workspace use. It adapts PostgreSQL persistence, Redis live delivery, Docker
execution, API model calls, authorized files, Agent Memory, and Plugin resources
without redefining Core semantics.

See [Workspace architecture](../../docs/architecture/Architecture.md),
[Workspace API](../../docs/reference/API.md), and the
[security model](../../docs/workspace/security/Model.md) for the complete boundary.

## Execution boundary

- `OCI_RUNTIME` is exactly `runc` or `runsc`; Runtime verifies that Docker has
  registered the selected runtime and does not fall back to another runtime or
  a host process.
- Compose defaults to `runsc`; gVisor must be installed and registered on the
  Docker host. Local Docker Desktop tests on hosts without gVisor explicitly
  set `OCI_RUNTIME=runc`. There is no automatic fallback. Performance evidence
  must identify the actual platform, such as systrap; it does not establish
  latency or capacity for an untested KVM deployment.
- Every AgentRun receives one temporary container with explicit mounts, work
  directory, UID/GID, network, CPU, memory, PID, and temporary-space limits.
- `read`, `bash`, `edit`, and `write` use that same container. Fixed helpers run
  through attached Docker Engine exec streams and expose no network listener.
- Runtime owns the container lifecycle and Docker socket. AgentRun containers
  receive neither the socket nor control-plane credentials.
- Plugin Skills, CLI programs, MCP servers, and Hooks use the frozen AgentRun
  activation and the existing Core tool, receipt, and lifecycle paths.
- Authorized attachments use Core `read(input_ref)`. `search_knowledge` is not a
  Workspace model tool.

`PLUGIN_VOLUME_NAME` and `AGENT_MEMORY_VOLUME_NAME` are required canonical
Docker volume names. Agent Memory is mounted through the Workspace-owned memory
boundary and remains outside the public Session workspace model.

## Internal service surface

- `POST /agent-runs/step` starts or resumes the exact authorized lifecycle job.
- `POST /agent-runs/cancel` requests cooperative cancellation.
- `POST /agent-runs/teardown` idempotently removes the matching terminal
  AgentRun container.
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
