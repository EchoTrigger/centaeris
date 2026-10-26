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

## Shared concurrent acceptance

The shared-admission candidate uses four independent environments and three
rolling Runs per Session. All four may execute at once. The earlier single-slot
gate below remains a separate continuity and fencing regression.

```dotenv
RUNTIME_NATIVE_SHARED_ADMISSION=1
RUNTIME_RUNSC_MAX_RUNNING_ENVIRONMENTS=4
RESIDENT_GLOBAL_COUNT=4
RESIDENT_TENANT_COUNT=4
WORKER_SLOT_COUNT=4
EXECUTION_GLOBAL_LIMIT=4
SANDBOX_MEMORY_BYTES=4294967296
SANDBOX_CPU_MILLI=2000
RESIDENT_TENANT_WORKSPACE_BYTES=34359738368
```

These are isolated acceptance values, not an instruction to change an existing
application. The normal idle timeout is an operator choice; the fixture uses a
short timeout and an explicit between-Run gap to exercise pause/resume quickly.
K6 records Run latency before this artificial gap and reports wall time separately.

CPU quotas are per-environment ceilings. Shared admission charges observed
cgroup memory plus swap and observed process counts. Unobserved residents,
pending creation and unknown disk usage retain their declared charges. A pause
retains memory. Disk budgets are independent of RAM; four default environments
need at least 16 GiB of disk budget while disk usage is unknown. Existing explicit
8-GiB workspace settings still limit that workspace and require a deliberate
configuration change.

The candidate requires the matching host supervisor and validates host identity,
container incarnation, boot identity and sample freshness. Missing or invalid
observations cannot become zero usage. Admission observations do not bound later
aggregate memory growth; a running-environment memory pool is a separate host
protection boundary.

Use the same owned launcher with `--shared-workload`, `--environment-count 4`,
`--iterations 3`, `--work-units 10000` and an explicit pinned Linux `--k6-binary`.
The deterministic model requests actual tool work: two CPU worker processes,
a file write and artifact publication. K6 independently downloads each artifact
and checks its sample-specific size and hash. A completed Run alone does not pass.

The shared launcher derives one bounded timing plan before starting the fixture.
`budgets` is recorded in the launch progress, shared report and K6 manifest/client
report. It does not change work units, environment overlap, rolling Run creation,
terminal deadlines or the per-item completion/pause/retirement checks.

For `E` environments, `I` Runs per environment, terminal deadline `T` seconds,
between-Run gap `G` milliseconds and initial stagger `S` milliseconds, the K6
scenario budget is `I * (T + 60 + 30) + ceil(((E - 1) * S + (I - 1) * G) / 1000)`.
Admission uses the [K6 HTTP default of 60 seconds](https://grafana.com/docs/k6/latest/javascript-api/k6-http/params/);
artifact downloads already use an explicit 30-second timeout. Model waiting and
physical work remain inside `T`. Setup covers the existing three login requests
at 60 seconds each; the scenario explicitly retains the existing 30-second
[graceful stop](https://grafana.com/docs/k6/latest/using-k6/scenarios/concepts/graceful-stop/).
The K6 process also reserves 25 seconds for initialization, summary and exit,
matching the existing terminate/kill/reader cleanup bounds.

The outer shared step preserves serial readbacks and adds their existing limits:
270 seconds for driver initialization (three 60-second commands and three
30-second HTTP requests), `E * I * 181` seconds for completed-Run readbacks,
60 seconds for durable timing, `E * 241` for final pause, and `E * 391` for
retirement. These bounds include the last observation beyond each polling
window: status command 60 seconds; pause SQL and inspect 120 seconds; retirement
HTTP and durable readback 90 seconds plus inventory/proof commands 120 seconds.
Each polling bound rounds up its existing 200-ms sleep. Observer stop/finally
retry adds 30 seconds, K6 failure cleanup adds 25, and outer capture adds 10.

For four environments and three Runs, `T=300`, `G=30000`, `S=0` gives a
1230-second scenario, 1465-second K6 process and 6560-second outer step. Eight
environments with the same settings and a 3000-ms stagger give 1251, 1486 and
11281 seconds respectively. The complete outer plan must fit four hours;
excessive combinations fail before fixture loading or workload admission.
These are worst-case ceilings, not intended runtimes. The first failed client,
product or physical acceptance check still stops and preserves responsibility.

Required evidence includes all of the following:

- Four environments with overlapping active tasks and positive CPU counter deltas,
  using unchanged cgroup/container identities; VU counts alone do not prove this.
- Twelve distinct product Runs, four original retained instances, committed Core
  terminal facts and complete per-Run cleanup receipts.
- Automatic pause/resume observations between rolling Runs, with unchanged
  physical identity. This does not by itself establish in-memory oracle continuity.
- Product retirement, confirmed physical absence and released residency holdings.
- Separate measurements for artificial idle gaps, admission, terminal/tool result,
  verified artifact download, cleanup and capacity release-to-claim where the
  durable capacity event exists. Unobserved stages remain explicitly unobserved.

After correctness passes, repeat the same fixed work at increasing concurrent
environment counts. Record host CPU/RAM/swap, throughput, P50/P95/P99 latency,
CPU/memory pressure and backpressure reasons. Larger matrices must explicitly
raise all relevant count/Worker/execution/running limits. Twenty default
4-GiB disk declarations require at least 80 GiB of disk budget when unmeasured;
CPU caps remain shared. A WSL result identifies WSL and is not a VMware measurement.

## Eight-environment capacity model

For an 8-CPU/16-GiB host, eight 2-CPU/4-GiB environments are a candidate
concurrency policy with 2x declared CPU and RAM overcommit, not a guaranteed
half-load operating point. Session count, resident environment count and active
tool work must be reported separately. More Sessions may wait for user/model
input without consuming exclusive CPUs; their retained memory is still charged.

For each aligned interval, derive core use as `delta(cpu.usage_usec) / interval_usec`
and measure the distribution of the sum across environments. Do not sum separate
per-environment P99 values. Report peak and aggregate P95/P99, burst correlation,
platform CPU use, and time above the usable host capacity. Measure memory and swap
independently, including idle residency and host processes outside sandbox cgroups.
Avoid subtracting the platform baseline twice from MemAvailable.

The [DSec report, section 4.3](https://arxiv.org/html/2609.22978v1#S4.SS3)
reports sparse average CPU usage relative to requested capacity; it also explains
why retained memory requires separate management. Its workload distribution is
motivation for measurements, not an assumed utilization ratio for this product.

Use a one-environment reference, four environments doing sustained fixed CPU work,
and eight environments alternating real tool work with external model wait.
Compare staggered work with synchronized bursts at identical total work. Add
separate resident-memory steps with low CPU. Label controlled model wait, idle
pause grace, resource waiting and useful work separately. Short exploratory runs
cannot establish a stable P99 or a universal safe oversubscription factor.

Temporary contention must retain the original Run and execution ownership and
resume when eligible. Capacity waiting is a normal control-plane state, not a
terminal failure. Accepted operations and unknown outcomes must never be replayed
to obtain this behavior; task admission, Input and physical creation have distinct
one-shot boundaries. Validate cancellation, restart, refresh/reconnect and eventual
progress under pressure in addition to reporting throughput and delay.

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

The product gate requires **six waves of two in-flight main Runs**, alternating
A/B, for **12 completed main Runs** with one running slot:

- The first Run in each environment starts its persistent memory oracle.
  Later Runs only query that original process. File hashes are reread from the
  actual file for each challenge, not cached in the oracle's memory.
- Every idle state has matching physical Docker `Paused=true` and durable
  `paused` facts. Running reservations reach zero while resident charges remain.
- Every wave starts A, observes a successful background-task response in the
  `running` state, then admits B while A holds the slot. The short holder task
  finishes and its cleanup is queried before the memory probe. B must have a
  durable queued yield with `runsc_environment_capacity_wait` and a registered
  generation waiter. Its original job must be claimed after a capacity release,
  before the recorded timer fallback; a paused container alone is insufficient.
- After wave two, two additional cancellation Runs prove queued demand executes
  no command and cancelling the holder leaves both original environments
  available. Actual holder start, cleanup receipts and API releases are required;
  these Runs are excluded from the 12 main Runs.
- After wave three, the exact fixture Runtime is restarted with both containers
  paused. Later
  product Runs prove the same in-memory oracles survived.
- After wave six, product retirement confirms exact container absence, released
  resident holdings and preserved execution history for both environments.
  Transient HTTP 503/504 or a matching pending 409 retain the same operation ID
  and use bounded readback of the exact durable request. An unmatched conflict
  fails; a timeout never refunds the resident charge.

While paused, observations use read-only SQL and Docker inspect, never task
RPCs into frozen workloads. PostgreSQL `SUM(bigint)` values are normalized to
exact integers; fractional values are rejected and large values are never
converted through floating point.

`results/observations.json` keeps accepted, completed and validated main-Run
counts separate. Each completed peer and its later validated observation are
saved immediately, so failure of the other peer cannot erase its result.
`summary.json` is written
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
