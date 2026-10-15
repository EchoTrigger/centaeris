# Retained final-tree acceptance

This is an execution checklist and evidence contract, not an observed pass.
The `a168010` continuity record remains historical evidence. Acceptance needs
the final candidate's source tree, API/Worker code, Runtime binary and tools
image; complete remote CI for the exact published head is a separate condition.

[The executable owned-fixture harness](../../../scripts/workspace/retained_acceptance/README.md)
prepares fresh synthetic business inputs, exercises A/B, Runtime restart, 100
uses/releases, same-Run history, stale controls and maintenance reconciliation.
It preserves the foreign-managed-instance guard and reports remaining fault and
capacity tests explicitly. The harness alone is tooling; its saved commands,
source and binary identities, and physical observations establish the evidence.

## Fixture ownership and prerequisites

Task operations use `environment.task.v2`. Version 1 supervisors are rejected
before takeover or task mutation because their operation keys cannot safely
replay the new controller-epoch protocol. Drain the old instance with matching
old consumers before replacing the tools image; do not adopt its private journal.

Whole-instance maintenance uses `POST /internal/environments/retire`, the
ordinary internal token and a distinct `ENVIRONMENT_MAINTENANCE_TOKEN` of at
least 32 bytes in `x-environment-maintenance-token`. It is disabled by default.
Supply `schema: runtime.environment.retire.v1`, stable `operationId`, exact
`environmentId`, `instanceId`, `hostId`, the inspected `expectedControllerEpoch`
and `acknowledgeTmpfsLoss: true`. The token goes only to the Runtime and the
independent maintainer. Keep it out of user Agent and Worker credentials.
Persist the original request privately and retry that exact body after a lost
response. `pending` retains the admission barrier and resource debit. Only
confirmed `destroyed` releases the debit and permits terminal Run cleanup from
the durable destruction fact; immutable allocation and release history remain.
Unknown outcomes require reconciliation, never a SQL history deletion or prune.

Use an explicitly owned disposable Linux Docker host, dedicated PostgreSQL
database, private model fixture, isolated API/Worker/Runtime processes, bounded
tmpfs and delegated cgroup subtree. Record the exact database identity, host,
container IDs, image digest, slice and test-operation IDs before testing. A
deterministic model may request tools; terminal facts, process execution,
cleanup/release receipts and resource holdings must come from real services.
Credentials stay in local environment/configuration and out of the report.

Read-only prerequisites on the chosen Linux fixture:

```sh
git rev-parse HEAD
git status --porcelain
docker info --format '{{.CgroupDriver}} {{.CgroupVersion}} {{json .Runtimes}}'
stat -fc %T /sys/fs/cgroup
sha256sum /absolute/path/to/runtime /absolute/path/to/execution_agent
docker image inspect --format '{{.Id}} {{json .RepoDigests}}' "$TEST_TOOLS_IMAGE"
```

Require `systemd`, cgroup version `2`, `cgroup2fs` and the configured runc
runtime. Require the inspected instance PID's cgroup below the dedicated slice,
with tasks below the instance and aggregate memory/CPU/PID limits intact.
Never substitute the host cgroup root or a production instance. An unavailable
daemon, unverified cgroup delegation or unknown database ownership stops physical
testing; record the limitation without weakening admission checks.

Focused ordinary Worker checks from the repository root:

```sh
(cd packages/worker && python -m unittest test_retained_lifecycle test_worker)
```

Focused Linux task tests, only after the fixture owner supplies its exact
delegated disposable subtree:

```sh
cargo test -p hosted_execution task_service::linux::tests -- --list
CENTAERIS_TASK_TEST_CGROUP_ROOT="$OWNED_DELEGATED_TASK_SUBTREE" \
  cargo test -p hosted_execution task_service::linux::tests -- --ignored --test-threads=1
```

List actual test identities alongside command output. PostgreSQL selection and
database ownership use `scripts/workspace/runtime_outbox_gate.py`; record its
explicit expected and executed identities. A passing total cannot replace an
omitted ignored test. Do not run the previously stopped full local API suite.


The instance-private SQLite task index is control/replay state, not Session
history or the resource ledger. Import old JSON state under the existing
exclusive lock, verify fence/task/request/pending identities, retain the original
journal and switch only after the import commits. Never keep JSON and SQLite as
two writable authorities. Existing active old-version instances retain compatible
consumers and images until drained or explicitly retired; replacing/restarting
the task supervisor does not constitute process adoption. A rollback must read
the new index or use a verified conversion with no active instances. Files on
private tmpfs do not create a host-power-loss recovery guarantee.

## Physical lifecycle sequence

1. Start Run A through the API and Worker. Request one environment task that
   writes a monotonic marker, plus a workspace file with known bytes. Capture
   Session/environment/instance/container/task identities and file SHA-256.
2. Let A reach authoritative terminal state and commit cleanup/release receipts.
   Verify its Run tasks and descendants are empty, the environment task remains
   alive and exactly one resident holding remains.
3. Restart the exact Runtime process and record its old/new PID or container
   start identity and binary digest. Submit B through the same real chain. Verify
   the same container ID, instance ID, environment task ID and file hash.
4. Probe query/start/input/terminate with A's stale fence. Save each rejection
   and confirm the environment task survives using B's current fence. Distinguish
   outer Runtime authorization evidence from direct task-service fence evidence.
5. Complete at least 100 sequential uses/releases on this same live instance.
   Retain the background task. Record active/terminal tasks, output bytes,
   indexed replay receipts, whole control/output directory bytes and task-domain
   count before/after each retention boundary. Every Run must complete; a
   cumulative-history rejection does not satisfy this sequence. Separately,
   complete at least 1000 ordinary commands/internal file-operation combinations
   within one Run, with automatic AGENTS.md reads enabled and the environment
   background task alive. Record actual command/helper identities, a real
   controller restart during that Run, active tasks <=32, hot records <=64 and
   bounded storage/output. The sample count is not a new product limit.
6. Exercise more than 256 short inputs and near-control-storage admission.
   Confirm input rejection cannot prevent terminate/EndRun or poison existing
   tasks. Replay old starts and inputs before/after retirement, lose one ACK,
   restart the controller and retry: no duplicate spawn or input side effect.
   Include same-key/different-request rejection, cold-record Query and explicit
   output gaps; retain the last recoverable output until its safe release fact.
   A lost spawn/input acknowledgement remains unknown rather than re-executing.
7. Start a task whose descendant double-forks and calls setsid, alongside an
   unrelated task. Terminate the first and require its cgroup to be empty while
   the second remains alive. Process-group disappearance alone is insufficient.
8. Invoke the internal maintenance entry using a stable operation ID and exact
   environment/instance identity. Declare loss of unarchived tmpfs first. Race
   new Run admission; test revoked execution user with independently authorized
   maintenance. Lose a deletion response and retry the same operation. Require
   exact container/task-domain absence and one resource reconciliation receipt.
   Make Docker observation unavailable in a separate case: pending and resource
   holding must remain until positive absence is observable. Preserve history.

The maintenance command must be copied from the final implemented interface,
with actual owned fixture identifiers. Do not replace it with Docker prune,
direct SQL deletion or a reuse of the old instance ID.

## Report required before accepting the candidate

Save raw command transcripts and machine-readable observations outside tracked
source when they contain local deployment identities. The reviewable report must
include these fields:

| Evidence | Required contents |
| --- | --- |
| Source | Before/after HEAD, final tree digest, dirty patch digest if applicable |
| Executables | Runtime and execution-agent SHA-256, tools image digest, build commands |
| Fixture | Owned database, dedicated slice, exact containers, resource limits, runtime configuration |
| Lifecycle | A/B/100-use Run IDs, cleanup/release IDs, same physical identities, file hashes, runtime restart |
| Control/history | Stale rejection results, replay/ACK observations, retained sizes, descendant cleanup and unrelated survivor |
| Destruction | Operation ID, authority identity class, admission rejection, missing container/domain proof, reconciliation receipt, once-only debit release |
| Tests | Expected/executed ignored test names; failed, skipped and unrun names with reasons |
| Remote CI | Exact head, complete workflow run ID and required check conclusions |

Record physical-chain and CI conclusions separately. If either is unavailable,
the limited merge conditions remain unmet. A mock pass is component evidence
only. Candidate tar+zstd has no production capture/restore claim, and the broader
persistent-volume design remains deferred after this checklist passes.
