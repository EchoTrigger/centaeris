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
`DOCKER_CREATE_CONCURRENCY=4` bounds create calls per Runtime process.

OverlayFS-backed Docker storage is required. Runtime refuses to start on `vfs`,
a missing backend, or any unsupported storage driver. Use the matching
HostSupervisor with cgroup v2, automatic idle pause, and demand resume enabled. The running limit
is 8 and idle grace is 3000 ms, matching the accepted eight-environment profile.
Pause eligibility requires released controller authority and no queued demand;
it does not pause an active Run merely because the model is waiting. Pausing
retains memory and all resident resource charges. Docker may report `overlayfs`
with the containerd image store or `overlay2` with the classic image store. Check the target daemon before rollout:

```sh
docker info --format '{{.Driver}} {{.CgroupVersion}}'
```

For runsc, the registered Docker runtime must also contain exactly one explicit
`--overlay2=none` argument in `runtimeArgs`. Keep its other required runtime
arguments. Runtime rejects missing or conflicting overlay arguments at startup.
gVisor's default `--overlay2=root:self` keeps guest writes in a separate overlay;
Docker's filesystem archive can then expose an empty or stale lower view instead
of those writes. See [gVisor filesystem configuration](https://gvisor.dev/docs/user_guide/filesystem/).

New environments record `centaeris.workspace_backing=dockerOverlayfs` under this
validated policy. Stopped workspace export requires that original creation marker.
A daemon configuration change does not prove the backing of an older container,
and must not be used to add the marker retroactively. Legacy recovery requires
an independently committed Session snapshot when its guest writes cannot be read.
Direct runc execution does not use gVisor overlay arguments.

Shared admission also requires host pressure stall information (PSI). Verify that
`/proc/pressure/memory` and `/proc/pressure/cpu` are readable before rollout. A
kernel built with `CONFIG_PSI_DEFAULT_DISABLED=y` requires `psi=1` in its boot
parameters and a host reboot before these files become available. Runtime cannot
start shared admission without the required host resource observations; enabling
cgroup v2 alone does not enable PSI.

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

For this native runsc revision, `api-init` applies the forward Django migration
chain through `0034_workspace_snapshot_publication`. Migration
`0033_session_workspace_authority` distinguishes an unknown legacy file baseline
from a committed empty workspace; `0034` fences snapshot publication and GC. Run
the same migration chain against fresh and existing supported databases. Runtime performs its own
published-schema upgrade to store version 17 when the matching service starts.

Drain existing workers and complete their cleanup responsibilities before
changing execution mode or replacing the HostSupervisor. Preserve database and
file volumes, historical authorizations and signed environment profiles. Existing
bindings keep their original configuration; stopped instances retain their
file-protection and cleanup responsibilities throughout the upgrade.

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
