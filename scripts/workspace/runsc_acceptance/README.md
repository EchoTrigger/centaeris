# Native runsc acceptance utilities

See [the gate contract](../../../docs/workspace/eval/RunscRotation.md).

- `build.py` builds exact clean source and stages the static musl helper.
- `daemon.py` owns separate Docker/containerd processes and the raw memory probe.
- `isolation.py` verifies actual transport/root/namespace ownership.
- `prepare.py` writes a private native-only fixture without starting services.
- `model.py` returns bounded actual tool intentions over the OpenAI chat contract.
- `admit.py` uses public login/CSRF/admission; it creates no execution facts.
- `run_status.py` observes bounded read-only authoritative status.
- `seed.py` creates only synthetic business inputs in an empty fixture database.
- `launch.py` coordinates the raw prerequisite and full product gate.

Credentials remain in the owned root or Docker volumes and are never printed.
No utility changes the current application stack, shared Docker resources, or
operator runtime configuration.

New owned daemons explicitly use `--overlay2=none` so guest workspace writes reach
the Docker writable layer. The launcher normalizes inherited guest-overlay flags
only in the fresh private daemon configuration and records its effective runtime
arguments. It does not change the shared daemon or historical ownership records.
Stopped archive acceptance must preserve guest-written contents, including a
non-empty file, rather than treating a readable image-layer directory as evidence.
For a shared eight-environment experiment, pass `--shared-workload --environment-count 8`.
Each environment keeps its 2-core, 4-GiB ceiling. The eight-environment fixture uses
12 GiB for both workspace and global measured-memory admission; the four-environment
baseline keeps its 8-GiB workspace budget. Storage budgets remain at least 32 GiB
and at least 4 GiB per environment. These are experiment parameters, not proof that
eight environments can simultaneously consume their full memory ceilings.

- `--model-wait-ms` adds 0–120000 ms of model-side wait before the fixed command.
- `--initial-stagger-ms` spaces initial environment admissions; the total initial
  stagger `(environment-count - 1) * initial-stagger-ms` cannot exceed 120000 ms.
- `--expected-working-environments` defaults to the full environment count. A sparse
  eight-environment experiment may explicitly require two working simultaneously;
  its report still records eight environments, threshold two, and the observed peak.

Model wait and initial stagger are recorded separately from Run latency. Every
planned environment still requires original-instance continuity, successful Run
terminal and cleanup facts, physical pause/resume, and positive product retirement.
The original single-slot rotation fixture keeps its existing parameters.

The durable timing report correlates successful tool results with an earlier tool
call in the same Run using exact `callId` and tool name. It reports execution-fact
and first successful result `createdAtMs`, plus `executionFactToFirstToolResultMs`.
These are timestamps carried by committed Session events, not measured database
commit instants, task process-start times, or resume-to-first-tool latency. Missing
or unconfirmed event shapes remain null; exact resume timing remains not observed.

## Workspace restore and reachability GC

The owned launcher can run one Session through three sequential `perRun` Runs:
write a unique binary sentinel; restore/read it in a fresh instance and change a
separate churn file; restart only the owned Runtime, collect the superseded head,
then restore/read the sentinel again in a third fresh instance. Every phase waits
for original physical cleanup, exact container absence and successful completion
of the original canonical Job with zero failed retries before continuing.

After the exact clean candidate has been built with `build.py`, use the existing
isolated Linux root network-namespace launcher, with the same six pinned image
arguments as the shared gate:

```sh
unshare --net python3 "$SOURCE/scripts/workspace/runsc_acceptance/launch.py" \
  --root "$OWNED_ROOT" --source "$SOURCE" --execute \
  --shared-workload --workspace-recovery --execution-mode perRun \
  --environment-count 2 --iterations 3 --work-units 1 --between-run-ms 0 \
  --k6-binary "$LINUX_K6" \
  --api-image "$API_IMAGE" --worker-image "$WORKER_IMAGE" \
  --runtime-image "$RUNTIME_IMAGE" --general-image "$GENERAL_IMAGE" \
  --postgres-image "$POSTGRES_IMAGE" --redis-image "$REDIS_IMAGE"
```

Two fixture slots are configured, but this correctness gate admits one Session
and three Runs; it makes no concurrency/overlap claim. The deterministic
`perf-workspace-recovery` model makes two actual tool requests per Run (`bash`,
`publish_artifact`) followed by a final response. Reader commands receive only
the sentinel digest and fail on absent/corrupt bytes. The existing public admit
driver creates each Run; existing k6 smoke/common helpers independently verify
the exact command, committed tool facts and downloaded artifact bytes.

GC uses a real one-second cutoff after a two-second grace, first dry-run and then
collection. It must actually remove the first committed head and preserve the
second, with no timestamp rewriting or manually created orphan. The full report
is `results/workspace-recovery-report.json` inside the owned root. Failure keeps
the fixture and resource responsibility available for diagnosis. No real model
credentials or external model bridge are used.
