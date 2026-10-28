# Native environment supervisor

Build `environment_host_supervisor` for the Linux host. Install that binary and
the reviewed, static `execution_agent` helper as root-owned files without group
or other write permission. These examples are installation inputs, not evidence
that a host has passed physical acceptance.

Install the service unit and an operator-owned copy of the strict JSON example.
The config and every ancestor must be root-owned and protected against writes
by other users. Control, output and socket directories must be distinct private
directories (mode `0700`); the service creates missing directories with that
mode and refuses to change permissions on an existing shared directory.

Set `ociRuntime` to `runc` or `runsc`, and `runtimePath`, `runtimeRoot` and
`dockerSocket` to the exact binary, state root and
daemon that own the workload containers. Record the binary version and digest.
Operator configs using `runcPath`/`runcRoot` must be updated explicitly; those
field names are no longer accepted. The task wire protocol is unchanged.

The runsc adapter requires the matching sealed helper and gVisor's
`--in-sandbox-cgroup=v2` runtime argument. Workload capabilities must be empty
and the workload cgroup mount read-only. A sealed runner uses a different UID
for trusted setup, restores the profile identity and capabilities before exec,
and keeps its reaper outside the task cgroup. Commands retain the environment's
PID namespace; cancelling one task uses its gVisor cgroup rather than killing
the whole sandbox. Host-owned journals and output remain outside the workload.
An upstream interface reference is runc v1.3.3 `exec --process --cgroup`; that
reference does not establish the installed binary's provenance. The supervisor
cross-checks Docker identity, runc state, init PID/starttime, bundle/security
configuration, separate PID/mount namespaces and cgroup v2 membership. Missing
capabilities and changed targets fail explicitly; no execution backend fallback
is selected.

Host control commands for different containers can execute concurrently. A
short process-wide registry protects child creation, wait ownership and adopted
child collection; it does not cover command completion. Controls for the same
OCI runtime/root/container retain a shared lifecycle mutex. Failed command
handles transfer their remaining wait responsibility to a cleanup worker.

Resource observations use one 30-second server budget from connection acceptance,
including admission, command waits and response writes. At most two full resource
walks execute concurrently; overload returns `host_probe_busy`. A disconnected
response reader cancels read-only probes, while a client that only closes its
request-writing side may still receive a response. Individual command execution
and output drain remain bounded by five seconds or the shorter probe budget.
Client timeouts may be shorter, and unavailable observations remain unknown.

Inspection uses the same budget while attesting or waiting for a loaded manager.
After a supervisor restart, entering manager recovery is also budgeted, but once
recovery starts it retains its task reconciliation responsibility across client
disconnect. The budget does not cancel accepted task starts, termination, helper
operations or retirement. The resource-walk limit is not a reserved control lane.

Connection admission has separate limits of eight header readers and eight
dispatched operations. A header reader authenticates the peer and validates one
JSON request header, bounded by 128 KiB and an absolute two-second deadline from
connection acceptance. Only a fully parsed, authenticated, valid request can
receive the top-level `host_busy` error when all execution slots are occupied.
That response proves this request was not dispatched: no task handler, helper
input spooling or helper execution has begun. An execution slot remains held
through response transfer. If all header-reader slots are occupied, an unparsed
connection is closed without a busy receipt; its transport outcome remains
unknown. Malformed headers, disconnects and timeouts are not busy receipts.

Helper uploads use an explicit `helperInputReady` response before binary input.
The Runtime checks its `operationId`, `inputLength` and `inputDigest` against the
original request before reading or sending input bytes. A pre-upload `host_busy`
therefore cannot consume the upload. Readiness confirms upload admission, not
helper execution or completion; later transport failures remain unconfirmed.

Mount only `/run/centaeris-host` into the trusted Runtime service and set
`ENVIRONMENT_HOST_SOCKET=/run/centaeris-host/control.sock`. Workloads receive no
control socket, journal, output directory, host PID namespace, Docker socket or
writable host cgroup mount. The Runtime needs no added privilege or host cgroup
mount. The supervisor runs natively with the host authority required by runc.

Use one container profile for image, workload user, cwd, environment, resource
limits, filesystem and network configuration. Set Docker's `User` to the actual
workload identity. Workload PID 1 may run `execution_agent environment-keeper`;
it only keeps the workload alive and reaps its descendants. All workload task
processes inherit the registered OCI process security and workload identity.
The host creates exact task sub-cgroups before invoking runc; leader exit does
not kill background descendants or establish domain cleanup.

Keep the workload init cwd at `/` and include exactly one
`CENTAERIS_WORKSPACE_ROOT=<profile directory>` and
`CENTAERIS_WORKSPACE_USER=<numeric uid>:<numeric gid>` in its registered OCI
environment. The user must match the OCI process identity. These host-attested
values select the helper workspace; task environment additions do not alter it.

Enable fixed helper capabilities by setting `helperPath` to the installed,
root-owned static `execution_agent`. The example leaves the path unset, so
helper dispatch fails explicitly until configured. A dynamically linked ELF is
refused because its interpreter would be resolved inside the workload.
For an x86_64 GNU Linux host, one build input is
`RUSTFLAGS=-Ctarget-feature=+crt-static cargo build --locked --release --target
x86_64-unknown-linux-gnu -p hosted_execution --bin execution_agent`; verify the
result's ELF program headers and source revision. The host checks the managed
path and file permissions, reads the binary once, and calculates its digest
internally for execution and replay identity. No manually supplied digest is
required. It seals those exact bytes in a memfd and passes that descriptor to
runc, so a root workload cannot replace its executable through a writable pathname.
Install or update `environment_host_supervisor` and `execution_agent` from the
same version. After an update, restart the supervisor and Runtime together.
The running supervisor continues using its sealed copy until restarted.

For runsc on x86_64, build the helper as a static musl executable instead:

```sh
RUSTFLAGS=-Ctarget-feature=+crt-static cargo rustc --locked --release \
  --target x86_64-unknown-linux-musl -p hosted_execution \
  --bin execution_agent -- -C relocation-model=static
```

Verify that `readelf -l` reports no `INTERP` segment before installation. The
GNU static PIE helper can abort during glibc startup when runsc imports its
sealed descriptor as an anonymous executable. The musl target alone is not
sufficient if the linker still emits a dynamic PIE. The supervisor itself may
use the host GNU target. The host seals the helper and reopens its descriptor
read-only before import; runsc requires a read-only host descriptor even when
the underlying memfd already has write seals.

Helper requests select a fixed kind and carry the owner fence, operation ID,
scope, byte length and SHA-256 before a bounded binary body. The host buffers
and verifies the complete input before execution. Input and artifact contents
are bounded at 64 MiB; snapshot streams at 1 GiB; aggregate uploads at 2 GiB.
Output spools remain outside the workload for receipt replay, with a 2 GiB
per-instance retained capacity. `prepareWorkspace` runs after fenced takeover
and before other helpers: it creates missing directories with the profile
identity while preserving existing image directories and content. Filesystem
and process-quiesce helpers use the profile user. Other fixed helpers use only
the root capabilities required by those reviewed entries; they retain the
registered seccomp, LSM and no-new-privileges policy.

The service's host cgroup is separate from every workload cgroup. Stopping this
unit stops its control processes; workload retirement is an explicit operation.
After a supervisor failure, persisted tasks remain uncertain until observed or
cleaned. Restart does not adopt stdin, reconstruct missing exit status, or replay
accepted work. Runtime controller restart can reconnect to a living supervisor.

Before enabling this path, use a disposable daemon and resource subtree to
verify root isolation, immutable target rejection, leader exit and output EOF,
background-domain retention, owner-scoped stop, lost acknowledgement, restart
reconciliation, and confirmed resource release. Existing in-container journals
require the explicit persisted-data migration with original evidence retained;
moving files alone is not a live process/pipe handoff.
