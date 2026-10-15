# Owned retained acceptance fixture

These executable tools adapt the historical deterministic model and actual
API/Worker/Runtime/Docker observer for the final repaired candidate. Passing the
physical-chain gate requires saved observations from the final tree.
`prepare.py` and `run.py` both reject
any pre-existing managed sandbox, including foreign deployments: an isolated
database cannot reconcile another stack's host-wide managed inventory.

Do not weaken that guard, stop old workloads, share Docker data roots or treat
the existing private fixture as an empty disposable host. Use a separately
authorized disposable daemon/VM with Docker systemd cgroup v2, runc and cached
base images. The tools never perform global cleanup or automatically tear down
services. `run.py --acknowledge-tmpfs-loss` explicitly permits maintenance
destruction of the exact instance that this new fixture creates.

Supply all six image references explicitly; `prepare.py` resolves each to a
locally available immutable Docker image ID and records the requested reference
and ID. These are dependency/base images, not a claim that their baked source
matches this candidate. API/Worker source is mounted from the clean candidate;
Runtime and task-service binaries are replaced by the matching Linux build.
Provide the API dependency target containing
`backports.zstd==1.8.0` if the cached API environment lacks it. Never bind a
different source revision beneath an old Runtime binary.

Example preparation on the isolated Linux fixture, with explicit source/binary
paths supplied by its owner:

```sh
REVISION=$(git -C "$SOURCE" rev-parse HEAD)
test -z "$(git -C "$SOURCE" status --porcelain)"
TOOLS="$SOURCE/scripts/workspace/retained_acceptance"
python3 "$TOOLS/build.py" --source "$SOURCE" --target-dir "$OWNED_BUILD_TARGET" \
  --output "$OWNED_BUILD_MANIFEST"
BINS="$OWNED_BUILD_TARGET/debug"
FIXTURE="/data/retained-acceptance-$REVISION"
TLS="/data/retained-acceptance-tls-$REVISION"
umask 077
mkdir "$TLS"
openssl req -x509 -newkey rsa:2048 -nodes -days 2 \
  -keyout "$TLS/model.key" -out "$TLS/model.crt" \
  -subj /CN=model -addext subjectAltName=DNS:model
cat /etc/ssl/certs/ca-certificates.crt "$TLS/model.crt" > "$TLS/combined-ca.pem"
python3 "$TOOLS/prepare.py" --source "$SOURCE" --revision "$REVISION" \
  --bins "$BINS" --model-script "$TOOLS/model.py" --output "$FIXTURE" \
  --build-manifest "$OWNED_BUILD_MANIFEST" \
  --api-image "$API_IMAGE" --worker-image "$WORKER_IMAGE" --runtime-image "$RUNTIME_IMAGE" \
  --general-image "$GENERAL_IMAGE" --postgres-image "$POSTGRES_IMAGE" --redis-image "$REDIS_IMAGE" \
  --python-deps "$API_DEPS" --tls-dir "$TLS" --api-port 18081 --runtime-port 19002
docker compose -f "$FIXTURE/compose.json" config --quiet
PROJECT=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["project"])' "$FIXTURE/manifest.json")
DOCKER_BUILDKIT=0 docker build --network none -t "$PROJECT-general:fixed" "$FIXTURE/general-image"
docker compose -f "$FIXTURE/compose.json" up -d --wait postgres redis
docker compose -f "$FIXTURE/compose.json" run --rm -T api /app/.venv/bin/python manage.py migrate --noinput < /dev/null
docker compose -f "$FIXTURE/compose.json" run --rm -T catalog-init < /dev/null
docker compose -f "$FIXTURE/compose.json" run --rm -T api /app/.venv/bin/python manage.py shell < "$TOOLS/seed.py"
docker compose -f "$FIXTURE/compose.json" up -d model api runtime worker
API_CONTAINER=$(docker compose -f "$FIXTURE/compose.json" ps -q api)
docker cp "$API_CONTAINER:/var/lib/centaeris-storage/acceptance-seed.json" "$FIXTURE/seed.json"
chmod 600 "$FIXTURE/seed.json"
python3 "$TOOLS/run.py" --fixture "$FIXTURE" --acknowledge-tmpfs-loss < /dev/null
```

Do not display the generated `synthetic.env`, `seed.json`, private TLS key or
expanded Compose configuration. The new seed uses current public Django models,
fresh synthetic credentials and business inputs only; it does not manufacture
Run, execution, task, terminal or receipt facts. Service startup and TLS readiness
must succeed before running the driver. Diagnostics remain in the owned fixture.

The default driver starts A, records its real release, restarts the exact project
Runtime, then runs B and 98 additional real use/release cycles. All 100 Runs must
complete. One cycle completes 70 ordinary commands within one Run, exceeding the
old cumulative-history boundary. A capacity failure is a failure of this gate,
not an accepted substitute for completed work. The observer joins each actual
Core tool call to its unique successful result; requested model counts and model
logs cannot substitute for that evidence.

Run the separate long-work gate against another fresh owned fixture:

```sh
python3 "$TOOLS/run.py" --fixture "$LONG_RUN_FIXTURE" --long-run --acknowledge-tmpfs-loss < /dev/null
```

This starts A's environment background task, writes an actual AGENTS.md and
workspace file, restarts Runtime, and requests 1000 distinct ordinary commands
within the next single Run. Automatic AGENTS.md observation remains enabled.
The gate requires 1000 unique authoritative call/result pairs with the expected
actual command output; 1000 is a regression sample, not a product limit. Its
completion and release counts are separate from the default 100-use sequence.
The driver saves live bounded hot/active task, index and control/output byte
observations using the root-only read-only `environment-task-store-inspect`
command. It restarts Runtime after at least 100 actual completions while the Run
is still active, requires a newer execute fence while that same Run is running,
and rejects the old
fence's Start/Input/Terminate/EndRun before confirming the background survives.
A later cleanup epoch or completed Run cannot substitute for this mid-Run reclaim.
Each command appends a unique marker before returning output; a bounded read
must contain exactly the ordered 1000 markers, proving no duplicated side effect.
Actual Core model-request observation manifests must bind the exact AGENTS.md
content. Metadata from the actual model HTTP requests must independently cover
all 1000 command indices and match the physical file digest. Neither model
metadata nor command intent substitutes for authoritative tool-result evidence.
Live measurements enforce the 8 MiB control-directory and 72 MiB output-directory
budgets as well as active/hot and instance memory/PID bounds. A proven identity,
resource or invariant failure aborts immediately. Only an explicit typed
not-ready observation (queued admission or positively observed fence movement
between read-only snapshots) may retry; an arbitrary command failure cannot
later disappear behind a successful sample.
The evidence keeps at most the latest 256 full live samples and independently
aggregates the count and metric maxima across every successful observation.
`long-live-samples.json` labels retained samples versus total observations;
summary maxima cover observations outside the retained window too. Raw commands
append in sequence to `commands.jsonl`, avoiding repeated rewriting of earlier
observations. Each output stream is limited to 1 MiB in saved evidence; an
oversized response saves its size/digest and bounded prefix, then fails the gate
instead of silently dropping required proof.
Additional indexed Run tasks are recorded as uncategorized; the index does not
store command bodies, so these counts are not falsely labelled helper identities.
A terminal success without these saved observations does not pass the gate.

Each cycle verifies retained task/container/instance/file identity and one
resident holding. The old controller probes list, terminate, start and input.
Mutation probes use the actual task snapshot's original operation epoch,
canonical epoch-prefixed operation IDs and execute purpose, and require exact
`task_controller_fenced`. This task-service relay evidence does not certify
outer signed HTTP authority. Private storage observations cover the whole
control directory (including SQLite transaction files) and output directory;
they do not infer capacity from the old JSON journal alone.

Maintenance uses its independent token and a stable exact request; it deliberately
discards the first acknowledged response and retries, verifies container absence,
one destroyed receipt, zero resident rows and preserved allocation/release history.
This caller ACK discard is not a transport-fault injection claim.

The script explicitly reports unrun tests: more than 256 inputs, near-storage
capacity, concurrent admission/destruction, revoked-user maintenance, unobservable
Docker, and descendant isolation. Run those isolated component/fault gates
separately. Its limited sequence success does not satisfy the full acceptance
checklist by itself, and it never supplies remote CI evidence.

Preparation safety tests:

```sh
python3 -m unittest discover -s "$TOOLS" -p 'test_*.py'
```

If the selected host contains foreign managed sandboxes, the inventory guard
blocks a new real-chain fixture. Do not import a second daemon's image store
without separately verified capacity and ownership. Existing deployed stacks
and sandboxes remain untouched. Record this as an unrun gate instead of weakening
production admission checks.

The pinned offline tools-image build uses `DOCKER_BUILDKIT=0` because its FROM
reference is a locally inspected immutable image ID. BuildKit may interpret
that ID as a registry name; this command records the actual accepted builder
configuration rather than claiming a BuildKit build. It makes no network fetch.

The task index is private control state, not user history or a durable working
volume. Upgrade only while holding the service's exclusive control lock; import
an old journal transactionally, validate identities/fences/pending operations,
retain its original file and switch to one writable authority only after success.
Keep compatible old consumers for old active instances; do not restart their
supervisor to simulate process adoption. Drain or explicitly destroy them with
tmpfs-loss acknowledgement before using the new tools image. Rollback must retain
new-index readability or perform a verified conversion with no active instance;
never run an old binary that silently ignores newer operation receipts.

No physical success is asserted by this README or its Python helper tests. Record
fresh source/binary/image hashes, exact commands, successful and unrun identities,
and raw observations for the final candidate. Remote CI for that exact head is a
separate result. No local full API discovery or production changes are required.
