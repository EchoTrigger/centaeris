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

Set `runcPath`, `runcRoot` and `dockerSocket` to the exact binary, state root and
daemon that own the workload containers. Record the binary version and digest.
An upstream interface reference is runc v1.3.3 `exec --process --cgroup`; that
reference does not establish the installed binary's provenance. The supervisor
cross-checks Docker identity, runc state, init PID/starttime, bundle/security
configuration, separate PID/mount namespaces and cgroup v2 membership. Missing
capabilities and changed targets fail explicitly; no execution backend fallback
is selected.

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
