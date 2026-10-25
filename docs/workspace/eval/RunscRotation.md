# Native runsc retained-memory rotation

This gate follows the public API → Worker → Runtime → existing Core tools →
native host supervisor → Docker/runsc path. It uses two retained environments
and one running-environment slot. Pausing keeps the original container,
process memory and files. Resident count, memory, CPU, PID and storage budgets
remain charged; `hostMemoryReleased=false` describes this boundary.

The native Runtime settings are:

```dotenv
OCI_RUNTIME=runsc
RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS=1
RUNTIME_RUNSC_IDLE_PAUSE_MS=3000
```

The idle grace is a test value. Choose an operational value from actual request
gaps. The fixture deliberately uses two Worker/execution leases so demand can
reach the independent running-environment gate. This does not change the
application's normal Worker or execution-lease limits.

## Build the exact candidate

Use Linux x86_64 with the repository's pinned Rust toolchain, the matching musl
standard library, `musl-tools` and `readelf`. Use new output directories outside
the checkout; source must be clean and committed.

```sh
python3 "$SOURCE/scripts/workspace/runsc_acceptance/build.py" \
  --source "$SOURCE" --target-dir "$OWNED_TARGET" \
  --runsc-rotation --artifact-directory "$NEW_ROOT/bins" \
  --output "$NEW_ROOT/build-final.json"
```

The builder stages GNU `runtime_server`/`environment_host_supervisor` and a
static musl `execution_agent`. It rejects an ELF interpreter, hashes the exact
artifacts, and leaves Cargo's GNU helper artifact intact. The launcher's ELF
check uses program headers and does not require host `readelf`.

## Run an owned fixture

Use a newly created root-owned mode-0700 disk-backed directory named
`/var/tmp/centaeris-runsc-full-<unique>` with the artifacts above. Supply explicit
cached image references; no image pull or application deployment is performed.

```sh
unshare --net -- python3 "$SOURCE/scripts/workspace/runsc_acceptance/launch.py" \
  --root "$NEW_ROOT" --source "$SOURCE" --execute \
  --api-image "$CACHED_API" --worker-image "$CACHED_WORKER" \
  --runtime-image "$CACHED_RUNTIME" --general-image "$CACHED_GENERAL_TOOLS" \
  --postgres-image "$CACHED_POSTGRES" --redis-image "$CACHED_REDIS"
```

For a Windows-hosted Git worktree accessed through WSL, also pass its exact
Linux `--git-dir` path. The launcher requires a fresh uplink-free network
namespace. Its bridge publishes only the fixture's loopback API/Runtime ports
inside that namespace. Shared Docker is used only for cached image inspection
and streaming `image save`; its process incarnation is pinned during transfer.

The standalone raw probe also requires a private network namespace:

```sh
unshare --net -- python3 "$SOURCE/scripts/workspace/runsc_power_probe.py" \
  --image "$CACHED_PYTHON" --root "$NEW_RAW_ROOT" --pids-limit 512
```

The default 512 PID budget matches the application profile. The old reduced
64-PID raw fixture hit its own controller limit during ptrace startup. Each raw
pause/resume and product observation records scoped `pids.current`, `pids.max`
and `pids.events`; exhaustion fails acceptance. Paused observation uses only
Docker inspect, read-only SQL and the exact container's cgroup files.

The fixture starts a separate containerd with explicit owned socket, root and
state. Docker receives that endpoint and unique containerd/plugin namespaces.
Actual process arguments and the connected endpoint are checked. Before any
private process starts, its network namespace must differ from init and the
shared Docker daemon; the observed namespace identities are recorded. A separate
protected uid-0 host supervisor uses the OCI root observed from the fixture's
own raw sandbox. Runtime receives only its control socket directory and the
same-path static helper, never another daemon's supervisor.

The registered runsc platform is preserved. Installed `runsc flags` must
advertise the private panic/runtime logging options. Logs remain under the
owned protected root; captured diagnostic heads/tails are bounded and suppress
credential-related lines. No global runtime configuration or clock is changed.

## Required evidence

Before starting product Runs, the launcher performs **12 raw pause/resume
cycles** with one original container, host PID, guest PID, process-start time,
in-memory nonce, fresh challenge proofs and actual file hashes. The exact
runtime version, arguments, namespace and observed OCI root are recorded.
A stopped/replaced process or failed raw probe prevents product admission.

The product gate then requires **12 completed main Runs**, alternating A/B:

- The first Run in each environment starts its persistent memory oracle.
  Later Runs only query that original process. File hashes are reread from the
  actual file for each challenge, not cached in the oracle's memory.
- Every idle state has matching physical Docker `Paused=true` and durable
  `paused` facts. Running reservations reach zero while resident charges remain.
- The sixth main Run is admitted once while the fifth holds the only running
  slot; the original demand completes after capacity becomes available.
- Two additional cancellation Runs prove queued demand executes no command and
  cancelling the holder leaves both original environments available.
- The exact fixture Runtime is restarted with both containers paused. Later
  product Runs prove the same in-memory oracles survived.
- Product retirement confirms exact container absence, released resident
  holdings and preserved execution history for both environments.

While paused, observations use read-only SQL and Docker inspect, never task
RPCs into frozen workloads. PostgreSQL `SUM(bigint)` values are normalized to
exact integers; fractional values are rejected and large values are never
converted through floating point.

`results/observations.json` contains collected facts. `summary.json` is written
only after all checks pass. Successful retirement allows only the owned
services/processes to stop. Data stays in the explicit validation root for
review and subsequent operator disposal. On failure, `owner.json` records the
remaining responsibility and private processes are preserved; an empty list,
timeout, daemon restart or missing response never establishes physical absence.

The smaller primitive can also run independently:

```sh
unshare --net -- python3 "$SOURCE/scripts/workspace/runsc_power_probe.py" \
  --image "$CACHED_PYTHON_IMAGE" --root "$NEW_RAW_ROOT" --pids-limit 512
```

Its twelve-cycle result is a prerequisite check, not product acceptance.
Unit tests use fake observations to verify the collector and isolation guards;
they do not establish that a host passed either physical gate.
