# Short-window throughput observations

`WORKSPACE_PERF_OBSERVATIONS=1` enables `workspace.perf.v1` JSON lines on stderr
in worker and Runtime. It is off by default and enabled only in `compose.perf.yml`.
No concurrency, backoff, queue or timeout settings change. Duration uses monotonic
clocks; `atUnixMs` is only for correlating windows. Logging failures must not fail
jobs. Observations contain no request bodies, credentials or raw error messages.

Worker emits RPC timings, HTTP failure status, separate `runtime_busy` and
`execution_claim_busy` labels, claim outcome, actual backoff duration and slot-held
duration, with slot/job identity where available. `slotHeld` starts after claim
returns and ends when the lifecycle handler returns; claim RPC is separate.
An `ok` timing means the function returned normally, not that the AgentRun succeeded.
Heartbeat threads are not part of the slot thread's context; use lifecycle RPCs
for the critical path, not a sum of all RPCs.

Runtime emits bounded route/lane admission/rejection and in-flight snapshots.
The release guard follows actual blocking work even after an HTTP deadline.
Route `other` deliberately groups unrelated/dynamic routes. Docker records
permit wait, daemon create/start/remove, ensure-created, sandbox ensure and
teardown. These are nested spans: never sum them as independent work. Dropped
async Docker operations emit `interrupted`, which does not imply rollback.

Collect structured logs after a window (including its drain), using that
experiment's immutable container IDs:

```powershell
python perf/harness/collect_observations.py --experiment-id OBSERVATION_ID --since UTC_START --until UTC_END
```

In a second process, after the experiment manifest exists, sample read-only DB
wait and lock counts at 10-second intervals:

```powershell
python perf/harness/collect_observations.py --experiment-id OBSERVATION_ID --sample-db 300
```

Both modes refuse to overwrite evidence files. The collector only copies the
structured observation schema; raw application logs are not exported. Empty logs
fail collection. DB sampling records no query text or credentials and cannot
measure try-lock misses (those require worker/runtime observations).

Before comparing throughput, deploy the current API/worker/Runtime images, apply
the pre-admission cancellation migration, verify cancellation and smoke, verify
both sources emit observations, then keep slots and limits fixed. A 120/min
baseline and 250/min for 180 seconds are enough for an initial diagnostic window.
Drain between runs; do not infer production capacity or improvement from the old
uncontrolled Windows/VM runs. Instrumentation overhead is part of this new baseline.

## Docker create concurrency experiment

Runtime accepts `DOCKER_CREATE_CONCURRENCY` (1..16, default 2). It is validated
at startup and controls only simultaneous Docker create calls; start/remove and
the two Docker I/O runtime threads are unchanged. The perf overlay maps
`PERF_DOCKER_CREATE_CONCURRENCY` from the isolated env file into this setting.
Do not change the root deployment env or assume an inherited shell variable
passes through the controller's environment allowlist.

For a 2 → 4 → 2 comparison, build one Runtime image, drain before each change,
update only that key in `perf/.state/test.env`, then use `Stack.compose` to
recreate only runtime (`up -d --no-deps --force-recreate runtime`). Confirm
container configuration and health, verify other container identities and the
Runtime image digest remain unchanged, and apply the same warmup every round.
Use 250/min for 180 seconds per round and the existing observation collectors.
Compare completions during arrival, queue peak/drain, mean slotHeld and nested
Docker spans. Reduced permit waiting alone is not an improvement if daemon
latency rises or throughput remains unchanged. Restore 2 and preserve evidence.
