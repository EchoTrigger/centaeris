# Sandbox residency admission

Execution leases limit active work, but waiting containers can retain processes,
memory and workspaces after releasing those leases. Resident admission separately
charges every managed sandbox, including lawful waits, against finite global,
Workspace and Docker-host limits for count, memory, CPU, process count and tmpfs
workspace space. Native subagents sharing an execution host do not reserve a
second container. Independent `dispatch_work` Runs may wait until earlier Runs
finish and release their containers.

Capacity is reserved durably before physical creation. Unknown creation outcomes
remain charged until inventory reconciliation establishes their state. Restarting
the service or switching Docker daemons does not erase tenant/global holdings.
Only confirmed physical absence releases the corresponding reservation. An
unavailable or untrusted inventory blocks admission rather than assuming zero
occupancy. The Docker request applies the declared memory, CPU, PID and tmpfs
limits; admission does not replace those actual enforcement limits.

Capacity shortage returns `execution_resident_capacity_wait` and uses the existing
recoverable lifecycle wait. This includes a signed profile that cannot fit the
current host or configured ceilings; operators must provision capacity or review
the deployment configuration. Waiting alone consumes neither a crash reclaim nor
an execution recovery attempt. A recovery attempt is committed after admission
and before actual replacement creation; real creation failures still count.
Ordinary-pool capacity one remains supported by reusing the admission connection
for that fenced source commit and catching up its projection after returning it.

Removal validates and locks the current lifecycle owner through the bounded
Docker mutation, absence confirmation and reservation release. Concurrent lease
reclaim skips that locked row until removal finishes. An owner already lost at
entry cannot remove or refund the sandbox. A failed or uncertain physical removal
retains its charge. This transaction boundary does not certify absolute external
mutation fencing during a database/network partition or actual power-loss safety.

Configure the finite ceilings and optional dynamic host headroom using
[Configuration](../reference/Configuration.md). Use consistent global/tenant
values on replicas sharing the database. Size the host and sandbox profiles for
the intended concurrency; admission limits are fuses, not a throughput promise.
This change keeps legal waits resident. It adds no automatic unload protocol,
task-duration limit, model usage ledger or account spending guarantee.

Checkpoint recovery validates and holds the immutable restore source before
retiring the exact ended execution. Its `remove_stopped` operation refuses a
running container; cancellation and terminal teardown keep their existing
deletion policy. A lost deletion reply or unavailable inventory keeps the charge
until the current owner confirms physical absence. Replacement admission then
uses that released capacity. The physical restore checks and pins the current
lifecycle owner, so an owner already lost cannot write the held checkpoint.

A committed recovery attempt can leave a created setup container behind if its
worker exits before `execution_started`, including after restoring the files.
On the next claim, the host replays Core records and fetches the committed
checkpoint source again. Under the current lifecycle owner's row lock it replays
those records once more and requires the same pending attempt, latest unused
checkpoint and no active execution. Only that exact unstarted setup can be
force-discarded; lawful waits and executions already started from the checkpoint
cannot enter this path. The next attempt is admitted after Docker absence is
confirmed and its old charge released, using the existing attempt budget.

Repeating a committed discard before the next attempt is reserved is safe: a
missing holding skips physical deletion, while inventory must still confirm the
exact container is absent. A reservation with no known container ID and no
inspectable physical object retains its unknown creation charge. Unavailable
inventory also retains the charge.

The ignored `real_docker_checkpoint_recovery_releases_only_confirmed_old_residency_and_restores_one_slot`
test covers retained PID and charge, ambiguous deletion, confirmed release and
actual file restoration under the shipped single-sandbox Workspace profile.
It also covers a simulated worker interruption after restore but before `execution_started`,
then a new owner rebuilding its store, replaying durable Core records and
fetching the source again. The shared Main preparation path repeats the pending
discard, creates and restores the next execution, commits its started fact and
replays that fact to verify the active execution and used checkpoint. A stale
owner's first restore into an empty workspace is rejected.
It requires `CENTAERIS_RECOVERY_DOCKER_TEST=isolated`, a dedicated database named
`centaeris_recovery_docker_test`, explicitly selected local execution-agent image,
and dedicated plugin/memory volumes with the `centaeris-recovery-test-` prefix.
Supply the dedicated endpoint through `CENTAERIS_RECOVERY_DOCKER_POSTGRES_URL`,
the image through `CENTAERIS_RECOVERY_DOCKER_IMAGE_DIGEST`, and the two volumes
through `PLUGIN_VOLUME_NAME` and `AGENT_MEMORY_VOLUME_NAME`; select `OCI_RUNTIME`.
The memory volume must contain its matching `.centaeris-recovery-test-volume`
marker. The test rejects colliding test identities and only removes its own
containers; it is not a capacity or saturation certification.

```sh
cargo test --locked -p runtime_server real_docker_checkpoint_recovery_releases_only_confirmed_old_residency_and_restores_one_slot -- --ignored --nocapture --test-threads=1
```

## Upgrade and rollback

The PostgreSQL forward migration upgrades Runtime schema 6 to 7, preserving the
lease counters, backoff, checkpoints and prior migration history. The new resident
table records declared occupancy; startup reconciles existing managed containers
before accepting new physical creations. Unknown or malformed schemas fail.

Stop writers and take the consistent backup described in [Data](Data.md) before
upgrading. An older binary does not understand schema 7, so source revert alone
does not downgrade the data. Prefer a forward corrective release preserving the
ledger, or a consistent stopped-writer recovery. Never clear the resident table
while managed containers still exist to make capacity appear available.
