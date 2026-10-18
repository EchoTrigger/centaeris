# Benchmark execution profile

The internal `benchmark_task_v1` capability runs a single Harbor task through the
normal Workspace API, Worker and Core loop. It requires an explicitly bound
`runtime.benchmark_execution_profile.v1` and a signed
`workspace.agent_run_authorization.v3`. Its network value must match the shared
[environment network snapshot](EnvironmentNetwork.md). Historical v1/v2
authorizations retain their original bytes and execution policy.

The profile binds the original immutable image ID, working directory, numeric
user, network policy and declared resources before the Session admits its first
Run. It is immutable and permits one Run per Session. A new trial creates a fresh
Session and instance; it cannot reuse another task's writable filesystem.

## Supported host

The opt-in host requires Linux/amd64, runc and the validated systemd/cgroup v2
configuration. It preserves the task's writable image filesystem and explicit
public-internet or disabled-network choice. The task receives no API credential,
Docker socket, maintenance token, private Agent-memory volume or control journal.
A read-only, digest-verified static execution helper provides ordinary filesystem
operations. Core continues to own model requests, tool contracts and receipts.

CPU, memory and PID limits are enforced by Docker. `storageBytes` records the
task's declaration; this Docker profile does not enforce a writable-layer disk
quota. `dataTmpfsBytes` is an accounting reservation, not a mounted workspace
tmpfs. The benchmark controller checks available disk capacity before image
preparation and retains cached-image evidence separately from live allocations.

Supported tasks use one Linux container and a verifier in that same container.
Compose sidecars, GPU/TPU, plugin hooks, MCP, external mounts and custom injected
task environments require separate support and are rejected before model work.
The normal retained `environment_task` service is not installed in this profile.
Shell-created background processes can remain in the task container through
verification after normal completion; uncertain command dispatch is not
automatically replayed.

## Setup, verification and cleanup

The controller binds the profile through an independently authorized maintenance
endpoint, then submits the original Harbor instruction through the public
Session/message API with a stable operation ID. Runtime allocates the exact
instance and yields `benchmark_setup_required` before entering the Core loop.
The Worker preserves that Run and releases its execution slot until the explicit
retry deadline. Harbor performs setup against the recorded instance; a durable
setup-complete fact unlocks model execution.

After the Agent reaches a committed terminal state, Harbor runs its native
verifier and collects artifacts in the same container. A terminal Run does not
release its resident allocation. The independent maintainer retires the exact
instance after verification; physical absence and the zero debit must both be
observed before the controller releases admission. Unknown Docker outcomes stay
pending. Maintenance exec does not revive an expired model execution grant.

On timeout or cancellation, Runtime quiesces the one-Run container's processes,
including detached descendants, while retaining its keeper PID 1 and filesystem.
The helper first verifies that PID 1 is the same immutable benchmark keeper;
it refuses the host or an unrelated PID namespace. The keeper reaps adopted
children. Agent cancellation waits for the original Run's committed terminal
fact and an independently confirmed quiescence receipt before verifier commands
or file transfer are admitted. An uncertain handoff blocks verification but
does not block independently authorized retirement. This does not increase the
Agent deadline or turn a timeout into successful execution. Verification, when
available, evaluates only the original task's existing output in that instance.

Task-file transfer uses bounded tar payloads and rejects links, devices, sparse
entries, traversal and conflicting paths. Transport loss during exec is an
unknown outcome, not permission to dispatch it again. See the
[controller guide](../../../scripts/harbor/workspace-benchmark.md) for limits,
image locks, native verifier evidence and the one-task/pilot/full-run sequence.

## Upgrade and exit

Install migrations `0020_benchmark_execution_profile` and `0021_environment_network`
with matching v3 readers before admitting new work. Forward migration preserves existing v1/v2
authorization and history. Reversal is rejected after benchmark facts exist;
empty fields are not a compatibility substitute for those facts.

To exit, stop new benchmark admission, preserve existing consumers, finish or
explicitly cancel current Runs, collect verification artifacts and retire every
owned instance. Confirm physical absence and resource reconciliation, then stop
the isolated consumers. Retain the database and native evidence. Destroying a
task container discards its writable filesystem; this profile supplies no file
archive or cold restoration. Switching the Worker's process configuration to
`perRun` does not clean up existing retained instances.

The port-9000 viewer only reads a bounded public projection. It has no execution
or maintenance credentials and cannot start, retry, cancel or clean up trials.
Missing provider cache counters or host memory observations remain unknown.
