# Owned retained acceptance fixture

These executable tools adapt the historical deterministic model and actual
API/Worker/Runtime/Docker observer for the final repaired candidate. They have
not passed the final physical-chain gate. `prepare.py` and `run.py` both reject
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
docker build -t "$PROJECT-general:fixed" "$FIXTURE/general-image"
docker compose -f "$FIXTURE/compose.json" up -d --wait postgres redis
docker compose -f "$FIXTURE/compose.json" run --rm api /app/.venv/bin/python manage.py migrate --noinput
docker compose -f "$FIXTURE/compose.json" run --rm -T catalog-init
docker compose -f "$FIXTURE/compose.json" run --rm -T api /app/.venv/bin/python manage.py shell < "$TOOLS/seed.py"
docker compose -f "$FIXTURE/compose.json" up -d model api runtime worker
API_CONTAINER=$(docker compose -f "$FIXTURE/compose.json" ps -q api)
docker cp "$API_CONTAINER:/var/lib/centaeris-storage/acceptance-seed.json" "$FIXTURE/seed.json"
chmod 600 "$FIXTURE/seed.json"
python3 "$TOOLS/run.py" --fixture "$FIXTURE" --acknowledge-tmpfs-loss
```

Do not display the generated `synthetic.env`, `seed.json`, private TLS key or
expanded Compose configuration. The new seed uses current public Django models,
fresh synthetic credentials and business inputs only; it does not manufacture
Run, execution, task, terminal or receipt facts. Service startup and TLS readiness
must succeed before running the driver. Diagnostics remain in the owned fixture.

The driver starts A, records its real release, restarts the exact project Runtime,
then runs B and 98 additional real use/release cycles. One cycle attempts 70
ordinary commands within a Run; explicit `task_history_capacity` is an accepted
recoverable limit after prior successful completions. The driver requires the
Run to release and subsequent Runs to execute successfully; cumulative ordinary
completed commands across the 100 cycles exceed 64. Each cycle verifies retained task/container/
instance/file identity and one resident holding; observations include bounded
history sizes. The old controller probes list, terminate, start and input.
Mutation probes use the actual task snapshot's original operation epoch,
canonical epoch-prefixed operation IDs and execute purpose, and require exact
`task_controller_fenced`. This task-service relay evidence does not certify
outer signed HTTP authority.
Maintenance uses its independent token and a stable exact request; it deliberately
discards the first acknowledged response and retries, verifies container absence,
one destroyed receipt, zero resident rows and preserved allocation/release history.
This caller ACK discard is not a transport-fault injection claim.

The script explicitly reports unrun tests: more than 256 inputs, near-journal
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
