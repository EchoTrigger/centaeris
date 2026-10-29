# Deployment

## Build inputs

The Workspace checkout and one compatible Runtime Framework revision are both
required. The current Compose build accepts Runtime source through the named
`centaeris` build context. A release pipeline must materialize an exact revision
and use it for both Cargo and Docker builds.

Required images include API, Runtime Server, worker, Web, general execution, and
document processor. PostgreSQL and Redis use pinned upstream image digests.
Execution and document processing are normal runtime dependencies.

## Local start

```powershell
Copy-Item .env.example .env
# Fill every blank secret.
pwsh -File scripts/workspace/start-local.ps1
```

The start script builds required images, runs initialization, and starts the
persistent stack. One-shot image checks confirm required binaries exist; they
do not remain as background services.

## Execution defaults

The deployment template uses `perRun`, Docker/runsc and measured shared admission
(`RUNTIME_NATIVE_SHARED_ADMISSION=1`). Worker slots and execute leases are both 8;
global and Workspace environment counts are 8, memory budgets are 12 GiB, and
workspace byte budgets are 32 GiB. Each environment retains its 2-CPU/4-GiB
ceiling. Host-derived memory headroom and conservative unknown-allocation charges
remain active, so eight concurrent environments are not guaranteed on every host.
`DOCKER_CREATE_CONCURRENCY=2` bounds create calls per Runtime process.

Use the matching HostSupervisor with cgroup v2 and an OverlayFS-backed Docker
store, with automatic idle pause and demand resume enabled. The running limit
is 8 and idle grace is 3000 ms, matching the accepted eight-environment profile.
Pause eligibility requires released controller authority and no queued demand;
it does not pause an active Run merely because the model is waiting. Pausing
retains memory and all resident resource charges. Docker may report `overlayfs`
with the containerd image store or `overlay2` with the classic image store. Check the target daemon before rollout:

```sh
docker info --format '{{.Driver}} {{.CgroupVersion}}'
```

The application does not select or migrate Docker storage. A host using `vfs`
needs a separately planned storage transition; do not change its storage driver
in place during an application rollout. Explicit runc deployments must disable
shared admission with `RUNTIME_NATIVE_SHARED_ADMISSION=0` and explicitly clear
both `RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS` and `RUNTIME_RUNSC_IDLE_PAUSE_MS`.

Existing private `.env` files are not overwritten by these defaults. Reconcile
the listed variables explicitly and preserve secrets and existing signed
environment profiles. `perRun` still destroys an instance after terminal cleanup;
enabling rotation does not turn it into cross-Run retained execution. The
dense-memory fixture separately overrides the Workspace memory budget to 8 GiB. See
[configuration](../reference/Configuration.md#resident-sandbox-capacity) for limits.

## Initialization

`api-init` runs before API replicas. It validates or creates the installed
Plugin catalog, applies Django migrations, and bootstraps the configured
administrator. Initialization failure blocks rollout and must not clear the
Plugin volume.

The first-release `app_core` schema is one `0001_initial` migration, including
the PostgreSQL tool-result lookup index. It initializes a fresh database; it
does not convert development migration histories or change existing volumes.
Choose and confirm a data retention strategy before replacing a development
database with a fresh first-release database.

The `gc` service runs the server-side lifecycle collector. Worker and Runtime
start only after their required dependencies become healthy.

## Production topology

Apply `docker-compose.prod.yml` with the base Compose file. It removes the API
host port and binds Web to host loopback. An external HTTPS reverse proxy is the
only public entry point. PostgreSQL, Redis, Runtime, worker, and test services
remain on internal networks.

The production override is a topology baseline, not a complete infrastructure
product. TLS certificates, host firewall, backups, monitoring, and secret
injection remain deployment responsibilities.

## Update

1. Back up PostgreSQL and persistent file volumes.
2. Build all images from one candidate source revision.
3. Render and inspect the final Compose configuration.
4. Run the release gate, including a fresh-database migration.
5. Stop new work, run initialization and migrations, then replace services.
6. Verify health, empty-Plugin operation, document processing, and one AgentRun.

Do not remove volumes as an update step. Rollback is valid only when old
services understand current data formats or a tested restore is performed.

## Stop

For an opt-in retained cohort, complete the
[retained exit procedure](../eval/EnvironmentTaskService.md#supported-configuration-and-coordinated-exit)
before stopping its matching API, Worker and Runtime consumers.

`docker compose down` stops containers and preserves named volumes. Adding
`--volumes` is destructive and is not part of routine stop or upgrade.
