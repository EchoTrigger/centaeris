# Configuration

Copy `.env.example` to `.env` for local development. Empty required secrets
fail at startup; the example file intentionally contains no usable secret.

Ordinary multipart upload envelopes, shared temporary spool bytes and concurrent upload slots
also require explicit deployment values; `.env.example` leaves them blank.
See [Upload and temporary-space limits](../operations/Uploads.md) for the four
inputs, physical volume requirements and verified offline recovery procedure.
The API upload pool excludes JSON/control requests and the exact internal signed
snapshot paths; their original transport and authorization remain unchanged.

## Required secrets

| Variable | Purpose |
| --- | --- |
| `DJANGO_SECRET_KEY` | Django signing and security state |
| `INTERNAL_API_TOKEN` | API-to-Runtime and worker internal authentication |
| `AGENT_RUN_AUTHORIZATION_SIGNING_KEY` | Immutable AgentRun authorization signatures |
| `CREDENTIAL_ENCRYPTION_KEY` | Encryption of stored model and MCP credentials |
| `POSTGRES_PASSWORD` | PostgreSQL service authentication |
| `BOOTSTRAP_SUPERADMIN_PASSWORD` | Initial administrator bootstrap |

Do not reuse these values across environments. They must not enter Git, image
layers, browser responses, Session logs, Plugin packages, or test artifacts.

## Browser login

Browser authentication uses Django's database session lifecycle with a persistent,
HTTP-only, SameSite=Lax cookie. Authenticated server credentials have no time-based
expiry. Explicit logout, credential deletion, disabled accounts and the existing
password-change/reset rules invalidate authentication. An unchanged authenticated
request renews the cookie without rewriting the server record. The cookie is
reissued with a 400-day lifetime on requests; browser retention policies, clearing
site data and losing a browser profile can remove the local cookie independently.
Closing the browser does not itself expire a credential.

The forward browser-session migration preserves old signed bytes, keys and finite
deadlines. An unexpired old credential adopts permanent server validity on its next
authenticated request; an already expired credential is not revived. Explicit
session-specific expiry and anonymous sessions remain finite. Browser sessions do
not determine business application grant expiry or the lifetime of Agents and
accepted work.

## Service addresses

The bundled Compose deployment owns internal service addresses: PostgreSQL is
`postgres:5432`, Redis is `redis:6379`, and API is `api:8000`. Both API and worker
Runtime URLs derive from `RUNTIME_PORT`. These internal addresses are not `.env`
knobs. `POSTGRES_DB`, `POSTGRES_USER`, published host ports, `API_BASE_URL`, and
`WEB_ORIGIN` remain deployment inputs.

`WEB_ORIGIN` is the canonical address used in generated links. Optional
`WEB_EXTRA_ORIGINS` lists additional exact, comma-separated browser origins
allowed by both CORS and CSRF checks. Wildcards and URL paths are rejected.
Set `API_BASE_URL=/` to use the Web server's same-origin `/api/` proxy; this
keeps browser cookies on the hostname used to open the page.

Standalone processes still accept the application environment variables
`POSTGRES_HOST`, `POSTGRES_PORT`, `REDIS_URL`, `RUNTIME_URL`, and
`API_INTERNAL_URL`. External databases or custom network layouts require an
explicit Compose override updating every affected consumer; the bundled topology
gate does not certify those custom overrides. Production uses an external HTTPS
reverse proxy. Do not expose internal service tokens or database ports through it.

## Storage and concurrency

- Compose shares one storage-root anchor between API/GC configuration and their
  named-volume targets, and one Plugin-root anchor between API/init configuration
  and API/init/Runtime mounts. They are not independently configurable in `.env`.
  Standalone API processes still require `STORAGE_ROOT` and `PLUGIN_CATALOG_ROOT`.
- `STORAGE_STREAM_LANES` and `STORAGE_STREAM_CHUNK_BYTES` bound file streaming.
- `REDIS_BROWSER_MAX_CONNECTIONS` bounds browser streaming connections per API
  process.
- `API_WORKERS` must remain within the validated range.
- Bundled Compose sets `API_POSTGRES_POOL_MAX_SIZE=8` on each ASGI API process;
  standalone Django processes default to `0` unless explicitly configured.
  `0` disables pooling. Django keeps `CONN_MAX_AGE=0`; its PostgreSQL
  backend uses `psycopg_pool` for connection reuse. The setting does not reach
  `api-init`, material processing, GC, or mail processes. Quota advisory locks
  use separate dedicated sessions, and Runtime has its own connection budgets.
  Before increasing API workers or replicas, budget
  `API_WORKERS × API replicas × API_POSTGRES_POOL_MAX_SIZE` together with the
  Runtime pools, quota sessions, other database clients, and operational reserve
  against PostgreSQL `max_connections`. Set the value to `0` and replace only
  the API to return to direct Django connections.
- `RUNTIME_POSTGRES_POOL_SIZE`, `RUNTIME_POSTGRES_CONTROL_POOL_SIZE`, and
  `RUNTIME_POSTGRES_LISTENER_LIMIT` independently bound ordinary operations,
  execution-control probes/transitions, and PostgreSQL job listeners in each
  Runtime process. Store and SessionLog clones share these budgets.
- `RUNTIME_POSTGRES_CHECKOUT_TIMEOUT_MS` bounds only waiting for a pool slot.
  `RUNTIME_POSTGRES_CONNECT_TIMEOUT_MS` bounds only establishing a new database
  connection. Neither value is a statement or end-to-end request deadline.
- `REDIS_MAXMEMORY` bounds transient live state; Redis eviction does not delete
  PostgreSQL facts.

Invalid or unsafe budgets fail instead of falling back to an unbounded value.

### Execution admission and request capacity

| Variable | Default | Scope |
| --- | ---: | --- |
| `EXECUTION_GLOBAL_LIMIT` | 8 | Leased/running hosted AgentRun lifecycle steps across worker and Runtime replicas |
| `EXECUTION_TENANT_LIMIT` | 4 | The same execution leases within one Workspace |
| `EXECUTION_GLOBAL_QUEUE_LIMIT` | 128 | Queued and running AgentRuns across API replicas |
| `EXECUTION_TENANT_QUEUE_LIMIT` | 32 | The same nonterminal obligations within one Workspace |
| `EXECUTION_QUEUE_WAIT_SECONDS` | 300 | Maximum age before first execution; expiry requests semantic cancellation |
| `RUNTIME_HTTP_REQUEST_LIMIT` | 24 | Ordinary handlers per Runtime process, including long executions |
| `RUNTIME_HTTP_LISTENER_LIMIT` | 8 | Job wait handlers per Runtime process |
| `RUNTIME_HTTP_CONTROL_LIMIT` | 4 | Cancellation, heartbeat, and job-status handlers per Runtime process |
| `RUNTIME_HTTP_REQUEST_TIMEOUT_SECONDS` | 30 | Absolute handler response deadline for short ordinary RPCs |
| `RUNTIME_HTTP_CONTROL_TIMEOUT_SECONDS` | 5 | Absolute control handler response deadline; also used by API/worker control clients |
| `WORK_RETURN_AUDIT_INTERVAL_SECONDS` | required | Deployment-selected delay between shared work-return repair pages; a whole pass spans all retained source pages |

All replicas sharing one database must use the same execution/admission limits.
Run admission retains its slot while Runtime capacity is unavailable or a Run
waits for input. Moving from queued to running, replay, restart and retry do not
release the obligation; a terminal state does. At capacity, new Run identities
are refused while an accepted operation can still replay. Completed work returns
retain their acceptance and pending carrier and bind a coordinator Run once
capacity becomes available, without creating another receipt or attempt.
All API replicas must use this revision for the expanded admission count to
apply consistently. These limits do not bound unbound Agent inputs or the
independent material-processing backlog.

The runtime job lease is the execution permit: yield and terminal transitions
release it; expired leases remain counted until lifecycle reconciliation.
`WORKER_SLOT_COUNT` remains a local worker-thread limit. The execution limits do
not count suspended sandbox containers, knowledge-processing jobs, or a parent's
internal subagents; they are not a total-container or total-process memory budget.
Parent logical waits release the lifecycle lease, avoiding a parent/child permit
deadlock. A single-Workspace perf run reaches the tenant limit before the global
limit; change its isolated environment explicitly when measuring another capacity.

Queue expiry is checked under the same AgentRun row lock as the first running
transition, and by the bounded lifecycle reconciler. Logical waits and recovery
backoff are excluded. Expired work cannot start user execution; committing its
cancelled terminal record still requires a lifecycle step and may occur later.
An unavailable Runtime leaves expiry pending and blocks that first transition.

Short/control deadlines include request-body reading and waiting for the blocking
handler after headers are parsed. AgentRun steps and knowledge processing keep
their lease/cancellation lifetime; job waits retain 20-second waits and a 25-second
HTTP deadline. Database checkout/connect and statement/lock waits receive the
remaining request budget. This is not a universal SQL or transaction execution
deadline: a blocking operation already underway may finish after the HTTP timeout.
It retains its HTTP permit and connection until it exits; a timeout is not evidence
of rollback and never authorizes replay of uncertain work.

The current initial database definitions include `execution_job_tenants` and the
API's `agent_run_initial_queue` partial index. Schema numbers stay at v1; existing
pre-release databases are not migrated or backfilled automatically. Isolated
verification must initialize the current schema, not reuse an incompatible one.

## Execution and processing

`SANDBOX_MEMORY_BYTES`, `SANDBOX_CPU_MILLI`, `SANDBOX_PIDS_LIMIT`, and
`SANDBOX_DATA_TMPFS_BYTES` define the authorized AgentRun profile.
Compose and the material Worker default `OCI_RUNTIME` to `runsc`. Install and
register gVisor on the Docker host before using that profile. Hosts without
gVisor, including local Docker Desktop test environments, must explicitly set
`OCI_RUNTIME=runc`. Runtime selection accepts exactly `runsc` or `runc`; an empty,
invalid or unregistered selection fails rather than falling back. Existing
private `.env` values are not replaced when the default changes.

`RUNTIME_EXECUTION_RECOVERY_MAX_ATTEMPTS` defaults to 5 replacement preparation
attempts per AgentRun; the initial execution is excluded. Each attempt is recorded
under the current lifecycle lease before preparing a replacement. A failed
preparation or process restart does not refund it. Retry delays are 1, 2, 4, 8,
and 16 seconds with deterministic ±20% jitter; the worker yields until the Runtime's
`retryAtMs` deadline. Only checkpoint-covered execution can enter this path.
Unknown tool side effects are not retried.

Compose owns the processor image reference through one YAML anchor shared by
the build service and material Worker. The general execution image is shared
between its build service and Runtime.
`KNOWLEDGE_PROCESSOR_IMAGE` is not a Compose `.env` input. Both images are required
services; build them before starting their respective execution services.

`KNOWLEDGE_PROCESSOR_DEVICE` is exactly `cpu` or `gpu:0`. One value configures
both the build and material Worker (as MATERIAL_PROCESSOR_DEVICE). The Dockerfile derives the installation extra from
that device (`cpu` or `gpu`); do not supply a separate `PROCESSOR_EXTRA`.
The shared `local` image tag deliberately does not encode `gpu:0`. Rebuild after
a device change. GPU deployment additionally requires compatible host hardware
and drivers, and a Compose override granting exactly one visible GPU to the
processor spec service. Configuration checks alone are not a GPU execution
certification.
The Docker release gate compares actual built/runtime image IDs and the image's
embedded device, so an existing stale image cannot satisfy the check.

## Docker Engine transport

Runtime container management, exec and archives use one shared local Docker Engine client and
negotiates the API version at initialization. `DOCKER_HOST`, when supplied, must
name a `unix://` socket or `npipe://` named pipe. Otherwise the platform default
local endpoint is used. Nonempty `DOCKER_CONTEXT` and remote TCP endpoints fail
explicitly; there is no CLI fallback. Initialization failure requires
a Runtime restart after the endpoint is repaired.

Queries have a 5-second deadline; create/start/remove requests have a 30-second
deadline. At most two create requests are issued concurrently per Runtime
process, including time spent waiting for that local permit. These are per-call
limits, not a deadline for the entire preparation sequence or a fleet-wide quota.
An expired request can still have taken effect in the daemon and is inspected
before its result is accepted.

Processor batch containers use Engine wait/logs, retaining the existing 64 KiB
per stdout/stderr channel, specification timeout and processing timeout. Ordinary
exec drains stdout/stderr independently and retains 8 MiB per channel, with raw
byte counts and an explicit truncation diagnostic. Hook diagnostics retain their
64 KiB limit; helpers enforce their existing output limits. MCP keeps the shared
4 MiB raw-line budget. Snapshot and archive payloads are streamed with bounded
buffers, not collected into a whole-payload allocation.

Exec create/start have bounded API handshakes. Once attached, command deadlines
and cancellation remain the host's responsibility; stream closure is not proof
that the remote process stopped. Synchronous helper reads/writes have a 30-minute
idle ceiling; MCP does not inherit that ceiling as a session lifetime. Archive
upload has a 30-second request deadline and download reads a 30-second idle
deadline. Processor download additionally enforces maxOutputBytes plus the
existing 64 MiB manifest allowance and bounded tar overhead. Unknown write or
exec outcomes are not automatically replayed.

The Runtime image no longer contains Docker CLI. Debian production uses the
mounted local Unix socket and must pass the daemon gate on its deployed Engine
version. Host-side Compose and diagnostic scripts may still use Docker CLI.
No precreated pool is enabled.

## Email

Password-reset mail is disabled unless `PASSWORD_RESET_ENABLED=1` and the
separate mail-sender service has valid SMTP configuration. The API process does
not silently send mail through an unconfigured backend.

## Office preview

DOCX, XLSX and PPTX previews use the existing document-processing worker and
LibreOffice installed in the document-processor image. No separate Office
viewer service or Office-specific environment variable is required. Keep the
document processor specification active and run the material worker with access
to the same object storage as the API. See
[Office preview operations](../operations/OfficePreview.md).

## Garbage collection

Cleanup failures use finite attempt limits and persistent retry delays. Set
`GC_MAX_CLEANUP_ATTEMPTS`, `GC_RETRY_BASE_SECONDS`, and `GC_RETRY_MAX_SECONDS`
through the deployment environment. Defaults, quarantine handling and migration
recovery are documented in [Garbage collection](../operations/GarbageCollection.md).

## Production review

Runtime's `DOCKER_CREATE_CONCURRENCY` limits simultaneous Docker container-create
calls (integer 1..16, default 2); invalid values fail startup. This does not change
worker slots, start/remove concurrency or execution limits. The production
Compose configuration retains the default. Isolated perf experiments use
`PERF_DOCKER_CREATE_CONCURRENCY` in `perf/.state/test.env` through the perf overlay.

Before deployment, set `DJANGO_DEBUG=0`, use explicit allowed hosts and web
origin, rotate all example values, verify HTTPS termination, and render the
complete Compose configuration with:

```powershell
docker compose --env-file .env -f docker-compose.yml -f docker-compose.prod.yml config
```
