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

## Workspace Agents and independent Sessions

Ordinary Agent configuration is shared within a Workspace. Owners and
administrators publish its instructions, default model and thinking mode, avatar
and Plugin activation through the existing Agent settings. Members use the same
published definition. Ordinary conversations can select a model and thinking mode
for the next submitted Run; each accepted Run freezes its selected configuration.

`0035_workspace_shared_agents` adds the shared configuration and Workspace default binding.
For an existing Workspace, an administrator explicitly promotes the chosen
existing Agent through its settings. Explicitly binding additional legacy
personal identities uses the promotion API's `bindAgentIds` list. Migration and
bootstrap do not guess which legacy
configuration should become the default. A Workspace without a selected default
requires an administrator to select it before ordinary execution.

Sessions retain their own history and files when an Agent is deleted. Their
historical Agent identity and old Run authorizations remain unchanged. New Runs
use the Workspace default if the former Agent configuration is unavailable.
Deleting the selected default restores the built-in appearance and instructions,
retaining the selected default's model configuration. Session deletion and
retention, rather than parent Agent deletion, govern file reclamation.
`0036_workspace_navigation_changes` wakes the Workspace Session list across Agent
identities. `0037_merge_workspace_resources_and_agents` joins this released branch
with `0036_workspace_file_objects` without renaming applied migrations. Both
upgrade paths retain the original Agent, Session and accepted Run identities.
The resource-contract migration still requires application writers and Workers
to be stopped and environment responsibilities drained before upgrading.

## External OAuth sign-in

`SSO_PROVIDERS_JSON` is a server-side JSON array; `[]` disables external sign-in.
Each provider requires `providerId`, `displayName`, `clientId`, `clientSecret`,
`authorizeUrl`, `tokenUrl`, `userinfoUrl` and `subjectField`. Optional fields are
`scope` (default empty), `displayNameField` (default `name`) and
`tokenEndpointAuthMethod` (`client_secret_basic`, default, or `client_secret_post`).
Profile field paths may use dots for nested JSON objects. The subject must be the
provider's stable user identifier; email does not automatically link accounts.

Register the exact callback `${WEB_ORIGIN}/api/sso/<providerId>/callback` with the
provider. This integration uses Authlib's authorization code flow with state and
PKCE S256. The provider must support PKCE and return JSON user information using
a bearer token. OAuth does not define a universal userinfo endpoint or field map;
configure those from the provider's documentation. OIDC discovery and ID-token
validation are not implemented; `openid` scope is rejected.

Keep provider secrets in deployment configuration outside Git and image layers.
Providers and `WEB_ORIGIN` require HTTPS. `SSO_ALLOW_INSECURE_HTTP=1` is permitted
only with `DJANGO_DEBUG=1` for an isolated local test deployment.

An existing user signs in locally, opens **Security → External sign-in**, verifies
the current password, and authorizes their external identity. The same local User
and workspace membership are used on subsequent external sign-ins. Unlinked
identities cannot create accounts. Workspace invitations and roles remain under
existing membership management. Unlinking requires a usable local password and
invalidates sessions using that identity; local sessions remain available.

Treat `providerId` as a permanent identity namespace. Changing client ID, endpoint
URLs or subject mapping invalidates existing associations and their external
sessions; sign in locally, unlink, then link again. Display labels and secret
rotation do not change the identity namespace. Back up the database before
applying the additive external-identity migration.

The standalone `scripts/workspace/oauth_test_provider.py` supplies synthetic
Alice/Bob identities for local acceptance. Its fixed credentials and account
selector are test fixtures. Keep it on an isolated test host.

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
  `api-init`, material processing, GC, or mail processes. Runtime has its own
  connection budgets. Provider admission belongs to the model gateway.
  Before increasing API workers or replicas, budget
  `API_WORKERS × API replicas × API_POSTGRES_POOL_MAX_SIZE` together with the
  Runtime pools, other database clients, and operational reserve
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
| `AGENT_RUN_EXECUTION_MODE` | `perRun` | Worker physical lifetime: destroy the original instance after confirmed Run cleanup; `retained` explicitly keeps it for reuse |
| `EXECUTION_GLOBAL_LIMIT` | 8 | Leased/running hosted AgentRun lifecycle steps across worker and Runtime replicas |
| `EXECUTION_GLOBAL_QUEUE_LIMIT` | 128 | Queued and running AgentRuns across API replicas |
| `EXECUTION_TENANT_QUEUE_LIMIT` | 32 | The same nonterminal obligations within one Workspace |
| `EXECUTION_QUEUE_WAIT_SECONDS` | 300 | Maximum age before first execution; expiry requests semantic cancellation |
| `RUNTIME_HTTP_REQUEST_LIMIT` | 24 | Ordinary handlers per Runtime process, including long executions |
| `RUNTIME_HTTP_LISTENER_LIMIT` | 8 | Job wait handlers per Runtime process |
| `RUNTIME_HTTP_CONTROL_LIMIT` | 4 | Cancellation, heartbeat, and job-status handlers per Runtime process |
| `RUNTIME_HTTP_REQUEST_TIMEOUT_SECONDS` | 30 | Absolute handler response deadline for short ordinary RPCs |
| `RUNTIME_HTTP_CONTROL_TIMEOUT_SECONDS` | 5 | Absolute control handler response deadline; also used by API/worker control clients |
| `WORK_RETURN_AUDIT_INTERVAL_SECONDS` | required | Deployment-selected delay between shared work-return repair pages; a whole pass spans all retained source pages |

`perRun` keeps the original instance through retries and lease takeover of the same Run.
Published artifacts remain in artifact storage; the destroyed instance provides no
cross-Run process or workspace continuity. Changing this process setting does not
retire existing instances: follow the [coordinated exit procedure](../eval/EnvironmentTaskService.md#supported-configuration-and-coordinated-exit)
before switching a retained cohort.

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
`WORKER_SLOT_COUNT` remains a local worker-thread limit (default 8). It describes
one deployment's ordinary Job handling capacity, not tools within an environment.
Execution admission has no per-Workspace quota: one Workspace can use all eight
global permits. Its persisted Workspace binding remains an identity fact. The
execution limit does not count suspended environments, knowledge-processing jobs,
or a parent's internal subagents; it is not a memory budget or a limit on all
nonterminal AgentRuns. Parent logical waits release the lifecycle lease.

Hosted Runtime explicitly selects tool parallelism 64; Core's generic default
remains 16. A parent and its hosted child runtimes share their tool coordinator.
This capacity bounds eligible parallel tools; tools requiring serial execution,
including the default bash/write/edit policies, still share one serial permit.
The current hosted subagent batch budget is three across a Runtime process,
not three per Session; that separate scheduling boundary remains to be revised.

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

### API memory observations

Each ASGI process emits an `api_memory_sample` every 30 seconds. It includes
the deployment revision, RSS and peak RSS, cgroup memory, glibc allocated/free
arena bytes, and HTTP/stream lifetime counters. Up to 16 active and 16 recently
completed request identities are sampled, with authorized route and Run bindings.
Request bodies, credentials and query strings are not recorded. Unsupported
allocator or cgroup metrics are reported as unavailable rather than zero.

Compose limits only the API's glibc arenas through `API_MALLOC_ARENA_MAX=8`.
The mmap and trim thresholds retain glibc's adaptive behavior. This bounds arena
proliferation; it does not guarantee return of all free heap pages to the host.
A local concurrent model-request comparison rejected fixed mmap thresholds
because they increased request latency. No periodic collection, heap trimming
or automatic restart is added.

Worker wait and slot-failure diagnostics use `workspace.worker.diagnostic.v1`
without enabling performance tracing. A confirmed Job yield records its Job ID,
lane, control stage, transition reason, and safe HTTP error category/status/route.
An unconfirmed yield does not produce a waiting record. Exception text,
credentials and lease owners are excluded. Runtime classifies the exact
`cleanup execution environment lost` failure as
`cleanup_execution_environment_lost`.

API and Worker Docker logs rotate at 20 MiB per file, retaining five files per container.
Recreate the API container to apply allocator environment and logging changes;
restarting an existing container does not apply new Compose configuration.

## Execution and processing

### Resident sandbox capacity

Resident limits count containers while executing, paused, or waiting for work.
They bound retained resources independently of execution leases and worker slots;
yielding an execution lease does not free its resident sandbox capacity.

| Variable | Default | Scope |
| --- | ---: | --- |
| `RESIDENT_GLOBAL_COUNT` | 8 | Resident sandbox count across Runtime hosts sharing the database |
| `RESIDENT_GLOBAL_MEMORY_BYTES` | 12884901888 | Global admission memory budget (12 GiB) |
| `RESIDENT_GLOBAL_CPU_MILLI` | 8000 | Global CPU ceiling (8 cores); shared admission checks each request |
| `RESIDENT_GLOBAL_PIDS` | 8192 | Global process budget |
| `RESIDENT_GLOBAL_WORKSPACE_BYTES` | 34359738368 | Global workspace byte budget (32 GiB) |
| `RESIDENT_TENANT_COUNT` | 8 | Resident sandbox count within one Workspace |
| `RESIDENT_TENANT_MEMORY_BYTES` | 12884901888 | Workspace admission memory budget (12 GiB) |
| `RESIDENT_TENANT_CPU_MILLI` | 4000 | Workspace CPU ceiling (4 cores); shared admission checks each request |
| `RESIDENT_TENANT_PIDS` | 2048 | Workspace process budget |
| `RESIDENT_TENANT_WORKSPACE_BYTES` | 34359738368 | Workspace byte budget (32 GiB) |
| `RESIDENT_STARTUP_MEMORY_BYTES` | 268435456 | Host memory allowance for a new preparation (256 MiB), capped by its cgroup ceiling |
| `RESIDENT_HOST_MEMORY_HEADROOM_BYTES` | Greater of 20% of Docker host RAM or 512 MiB | Memory reserved for host services |
| `RESIDENT_HOST_CPU_HEADROOM_MILLI` | 100 per Docker host CPU core | CPU reserved for host services |

These are Compose deployment defaults for the accepted eight-environment,
12-GiB admission profile. Runtime also derives host capacity from Docker and
retains host headroom; eight is a count ceiling, not guaranteed physical capacity.
The shared policy charges fresh observed memory plus swap and PID usage for known
residents. New preparations need host headroom plus `RESIDENT_STARTUP_MEMORY_BYTES`, not
headroom plus the full cgroup ceiling. Aggregate global/Workspace declaration
checks remain unchanged. Pending or unknown creations keep their full declared charge; unknown
workspace usage keeps the declared workspace byte budget. CPU remains a
per-environment ceiling and is not summed as a reservation. Headroom that leaves
no capacity fails rather than allowing overbooking.

Compose supplies the ten finite global and Workspace defaults and passes explicit
overrides to Runtime. All Runtime replicas sharing one database must use the same
global and Workspace limits. Leave host headroom variables unset to use the
Docker host-derived defaults; Compose renders unresolved pass-through entries as
null and omits them from the container environment. Every explicit resident limit
or headroom override must be a positive integer. Empty values, zero, negative
values, and non-integers reach Runtime and
fail validation instead of silently selecting a default.

Compose and `.env.example` set `RUNTIME_NATIVE_SHARED_ADMISSION=1` for the
matching Docker/runsc HostSupervisor. Direct Runtime launches still require an
explicit shared-admission switch and resource budgets; an unset switch retains
declaration-based admission. Capacity planning includes reservations and
containers awaiting confirmed removal, not only active execution leases. A
known capacity wait preserves the original Run/Job without consuming failed
retry budget. The dense-memory acceptance uses an explicit 8-GiB Workspace
override; it does not change this 12-GiB deployment default.

### Sandbox execution profile

`SANDBOX_MEMORY_BYTES`, `SANDBOX_CPU_MILLI`, `SANDBOX_PIDS_LIMIT`, and
`SANDBOX_WORKSPACE_BYTES` define the authorized AgentRun profile.
`workspaceBytes` is a workspace data budget for admission, inputs and expanded
snapshots; it does not select a tmpfs or impose a live OverlayFS disk quota.
The deployment example sets new environments to 4 GiB of memory and 2000 CPU
milli (two cores). These are upper bounds. Existing signed environment profiles
keep their original resource limits.

Before upgrading the resource contract, stop application writes and drain Runs,
cleanup Jobs, maintenance operations and resident instances using the existing
lifecycle. Then stop workers and run the forward migration with
`SANDBOX_WORKSPACE_BYTES` configured. Unresolved creation dispatches block the
migration. It creates a new current Environment binding with `workspaceBytes`;
old Environments, Run bindings, signed grants and committed workspace descriptors
remain unchanged. Start the matching API/Runtime version together after migration.
Compose and the material Worker default `OCI_RUNTIME` to `runsc`. Install and
register gVisor on the Docker host before using that profile. Hosts without
gVisor, including local Docker Desktop test environments, must explicitly set
`OCI_RUNTIME=runc`, `RUNTIME_NATIVE_SHARED_ADMISSION=0`, and explicitly empty
values for both runsc scheduling variables. Runtime selection accepts exactly `runsc` or `runc`; an empty,
invalid or unregistered selection fails rather than falling back. Existing
private `.env` values are not replaced when the default changes.

The Docker `runsc` registration must explicitly include exactly one
`--overlay2=none` runtime argument. This makes guest workspace writes reach the
Docker OverlayFS layer used by stopped-instance preservation. Missing, duplicate,
or conflicting overlay arguments fail Runtime startup. New instances carry a
fixed workspace-backing marker; existing containers are not relabeled. This
runsc setting does not apply to direct `runc` execution.
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
calls (integer 1..16, default 4); invalid values fail startup. This does not change
worker slots, start/remove concurrency or execution limits. The production
Compose configuration supplies 4 and passes explicit overrides through. Isolated
perf experiments use
`PERF_DOCKER_CREATE_CONCURRENCY` in `perf/.state/test.env` through the perf overlay.

Before deployment, set `DJANGO_DEBUG=0`, use explicit allowed hosts and web
origin, rotate all example values, verify HTTPS termination, and render the
complete Compose configuration with:

```powershell
docker compose --env-file .env -f docker-compose.yml -f docker-compose.prod.yml config
```


### Native retained-memory scheduling

Compose and `.env.example` enable automatic idle pause and demand resume with
`RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS=8` and `RUNTIME_RUNSC_IDLE_PAUSE_MS=3000`,
matching the accepted eight-environment profile. Explicitly empty both values in
Compose to disable it; direct Runtime launches can leave both unset. The
running-environment limit applies per Docker host. Idle eligibility requires
released controller authority and no queued demand. An active Run is not paused
merely because it is waiting for a model response. `perRun` still removes its
instance after terminal cleanup rather than retaining it between Runs. Pausing preserves the original
container, memory and files, and every resident memory/PID/storage/count debit.
Only an exact observed pause releases its running slot. Unknown outcomes remain
charged; restart reconciles persisted power state before admission starts.

Execution permits remain independent: `EXECUTION_GLOBAL_LIMIT=8` counts only
execute lifecycle leases; there is no per-Workspace execution quota.
`WORKER_SLOT_COUNT=8` limits local ordinary worker threads. Hosted tool concurrency
is 64 while Core's existing default remains 16. Resident tenant limits are separate
resource budgets. Control and cleanup do not consume execute permits.

The native activation guard preserves released provider history and refuses
activation while old physical, power, cleanup or creator-job responsibilities
remain unsettled. It never interprets a timeout or missing inventory as removal.
Historical API migrations and snapshot references must be preserved when upgrading
previously deployed databases; see the native migration acceptance gate.
