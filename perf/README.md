# Isolated performance harness

Run from the repository root. The only deployment controller is
`python perf/harness/control.py`. It pins project `centaeris-perf`, the base and
performance Compose files, and `perf/.state/test.env`. It does not read the root
private `.env`. Do not invoke Compose manually to change slots.

## Setup and smoke

Initialization uses `runsc`, requiring a Docker host with gVisor registered.
On hosts without it, explicitly initialize with
`python perf/harness/control.py init --oci-runtime runc`. The option applies only
to initialization; it does not rewrite an existing test environment or the root
private `.env`. A configured runtime failure never selects another runtime.

```powershell
python perf/harness/control.py init
bash perf/certs/generate.sh
python perf/harness/control.py check
python perf/harness/control.py build
python perf/harness/control.py up
python perf/harness/control.py smoke --experiment-id smoke-001
python perf/harness/control.py slots --slots 6
python perf/harness/control.py smoke --experiment-id smoke-002
```

For a short end-to-end harness check (not a capacity claim), use the pinned K6
builder and a bounded load:

```powershell
bash perf/k6/build.sh
python perf/harness/control.py load --experiment-id load-001 --rate 12 --seconds 15
python perf/harness/control.py sample --experiment-id sample-001 --seconds 30
```

`load` records accepted run IDs, final per-run timestamps/status, K6 logs, resource
samples and baseline history counts. All accepted IDs must match completed durable
runs exactly. A failed command, invalid sample, unsuccessful terminal or drain
timeout fails the experiment. The outcome is explicitly continuous-history; fixed
snapshot restoration is a separate operator preparation step, not an automatic
consequence of draining. The existing controller runs one stage per invocation.

Initialization refuses to replace existing credentials or certificates. API is
bound to localhost:18000; optional Web uses localhost:13000. The test project owns
separate networks and data volumes. Mock CA trust is added only by the performance
overlay. TLS validation stays enabled. Production image tags are not rebuilt by
the controller; the document processor image is an existing shared read-only
dependency and must be built using the normal project build before setup.

Fresh isolated CA files use `perf/certs/out/isolated/`, leaving any legacy certificate
files mounted by an existing development stack unchanged.

Slot changes require zero queued/running runs, replace only the worker using
`--no-deps`, and verify that all other container identities are unchanged. Run
`stop` only after the stack drains; it preserves test volumes. There is no automatic
volume deletion, password reset, cancellation, or replay of failed runs.

## Evidence and source boundary

Only source scripts, tests and configuration belong here. Credentials under
`.state/`, generated certificates under `certs/out/`, downloaded binaries under
`k6/bin/` and old results under `results/` are ignored. The entire `perf/` directory
is excluded from production Docker build contexts; the mock has its own context.

Evidence is written to the sibling `centaeris-perf-evidence/<experiment-id>/`;
experiment IDs cannot be reused. Manifests record revisions, dirty paths, container
identities and image digests without credentials. Original evidence snapshots are
retained outside the repository. Superseded deployment/sampling scripts are archived
there under `harness-migration-20260906` and are not supported entry points.

## Measurement discipline

Preflight checks container ownership, health, a verified TLS handshake from API to
mock, and complete resource samples. A sampling/command failure invalidates the
experiment; empty columns must never be treated as zero resource use.

An empty queue does not mean unchanged history. For a fixed-history comparison,
restore the same synthetic test-data snapshot before each stage and record its
identity. Continuous-history experiments must be labeled as such. Never attach
production volumes or reuse production accounts. Existing K6 scenarios are workload
sources; accepted requests and completed iterations are not completed AgentRuns.
Persist exact run IDs and verify their terminal states separately.

The controller's smoke requires a completed run. It does not establish throughput,
capacity, fairness, restart recovery, or historical-cost independence. Run failures
must remain visible; cancellation must not count as successful draining.

## Tests

### Hosted transcript capture acceptance

After the normal isolated build and startup, run one bounded synthetic capture
workload. Choose a new evidence directory outside the repository:

```powershell
python perf/harness/transcript_capture_validation.py --evidence D:/Projects/centaeris-storage-evidence/transcript-capture-unique-id
python perf/harness/control.py stop
```

The stack must be idle and pass the existing ownership, health and TLS preflight.
The workload generates 110,000 bytes of UTF-8 stdout, overwrites its spill file
with equal-length different bytes, then removes it. All three tools and the real
AgentRun must succeed; the SSE terminal and durable database state must agree.
The script reads the formatted historical result in pages of at most 64 KiB
(at most eight pages), checks the original text, then replaces only API/Runtime
services while idle and requires exactly the same archived bytes. It preserves
test data and never cancels a Run to obtain a passing result.

A child process has a 360-second deadline. Ordinary HTTP/SSE operations reuse the
bounded workload client; content requests have a ten-second deadline. Only the
explicit projection-not-ready response and capture-unavailable response permit
bounded readiness waits (20 and 15 seconds respectively). Other errors or invalid
content fail immediately. The external manifest and report retain source/image
identities, exact accepted/completed IDs, tool-result metadata, request statuses
and timings, byte/page counts and SHA-256; credentials and full output stay out
of those files. Existing evidence directories cannot be reused.

This check proves retention of a published capture after source mutation and
service replacement. It does not certify full-text delivery before publication,
production deployment, throughput, or recovery of uncaptured old results. Validate
the production Web build separately using the conversation checklist in
`docs/eval/FrontendManualAcceptance.md`, including deliberate multi-page expansion.

### Bounded API connection-pool validation

After the normal isolated setup/build, use the dedicated controller:

```powershell
python perf/harness/pool_validation.py build
python perf/harness/pool_validation.py up
python perf/harness/pool_validation.py run --experiment-id pool-validation-unique-id
python perf/harness/pool_validation.py stop
```

The existing document processor image must also be available under the isolated
`centaeris-perf-processor:local` tag. This controller uses the same ownership and
configuration validation as `control.py`, with the checked-in `compose.pool.yml`
overlay. It does not read the private root `.env`. It fixes one API process and
two worker slots; CPU/memory budgets are explicit in that overlay. Docker startup
may also start unrelated containers according to their own restart policies;
the experiment never changes those containers.

Two continuous-history stages use pool size 8 followed by explicit 0. Each admits
at most twelve paced mock runs over a 60-second arrival window, at most two
outstanding Runs, with three independent SSE observers per Run. Admission stops
on HTTP/SSE failure, unsuccessful Run, service exit/OOM, missing resource sample,
or sampled database connections reaching 80% of `max_connections`. A child process
has a 210-second hard deadline; admitted work is not cancelled or counted as
successful merely to drain it. Exact accepted IDs, successful observer terminals
and durable completed Runs must agree. If a failure leaves active work, preserve
the evidence and investigate before stopping the stack.

Only the API is replaced between stages. All other container identities are
checked, including worker and PostgreSQL. The test env file is restored after
the experiment; running API configuration remains the last measured stage until
stopped/replaced, so the restored file alone is not a rollback action.

`Pool.Dockerfile` adds a perf-only ASGI wrapper to the normal API image. It samples
the existing pool in the actual serving PID every second without creating a pool
or borrowing a connection. Its `CONNECTION_POOL_PROBE` records include numeric pool
statistics and typed errors correlated with HTTP 500s, excluding credentials,
request bodies and exception messages. No metrics endpoint or production API
code is added. Pool samples, PostgreSQL connection logs, request latency/status,
resource samples, image identities and exact Run outcomes stay in the external
evidence directory. Saturation, host-wide capacity and throughput improvement are
not certified by this bounded correctness/rollback check.

For the bounded historical-notification amplification probe, run
`python perf/harness/outbox_profile.py --experiment-id outbox-001`. It requires
an idle isolated stack with no active waiters, temporarily stops its worker,
replaces only `perf.outbox.history` synthetic fixtures at 0/3,641/36,410 rows,
and exercises real reconcile/pending/waiter endpoints. It resets PostgreSQL
statement statistics on this test server and records HTTP results, fixture
generation/pending counts and SQL work. Worker startup is restored afterward.
The largest acknowledged fixture remains in the test database. This probe does
not establish steady-state CPU or capacity; do not run it alongside another test.

For a bounded loaded comparison, run `python perf/harness/history_load.py
--experiment-id history-load-001`. It requires two worker slots and an idle stack,
then runs 30 arrivals/minute for 90 seconds at 0/3,641/36,410/0 acknowledged
synthetic notification rows. Each fixture change stops only the idle worker and
preserves all real runs. Real run history accumulates; the final zero-fixture
stage helps identify time drift but does not replace a restored-snapshot study.
Each stage saves database-clock timestamps, raw session counters, statement
statistics, resource samples, accepted IDs and completed run receipts. Counter
resets and invalid windows fail measurement. Windows include setup and drain;
they are not pure steady-state samples. After success the 36,410-row baseline is
restored. Do not run alongside another load or reset database statistics.

```powershell
python -m unittest discover -s perf/tests -v
```

These regressions also run in `scripts/ci.ps1`. Docker Compose rendering tests use
synthetic configuration; live smoke runs require the isolated deployment.

## Harness repair (2026-09-20)

- Smoke consumes the current replayable run SSE endpoint. Its child process has a
  hard 180-second lifetime; a busy nonterminal stream cannot extend that deadline.
  K6 terminal observers use the same endpoint with an overall request timeout.
- PostgreSQL statements travel on stdin, including large accepted-ID sets. `up`
  initializes `pg_stat_statements` in the isolated database.
- Sampling failures invalidate the experiment but do not terminate a running K6
  workload. K6, sampling, drain and accounting errors retain separate phase labels
  in `failure.json`; `outcome.json` is retained even for unsuccessful experiments.
- `requestedDurationSeconds` is the configured load duration;
  `observedWorkloadSeconds` is the controller's elapsed time when it first observes
  K6 exit (includes setup and sampling delay), not an exact active-load duration.
  Check K6 logs/summary for interrupted windows. Never relabel them as complete.
- `completionBuckets` counts successful completions by UTC completion minute.
  Creation cohorts are a different statistic. `completedLatencyMs` excludes failed,
  cancelled and unfinished runs; `statuses` reports them separately. Partial first
  and last minute buckets must not be compared with full minutes.
- Keep background VM activity and continuous-history growth in the report. They
  can affect medians and trends, not only maxima. No capacity/extrapolation claim
  is justified by these uncontrolled Windows runs alone.
