# Guest OCI task broker

The provider owns the physical VM lifecycle. Inside that VM, the template init
owns `execution_agent guest-task-broker` in a service domain independent of SDK
commands and of the workload cgroup. `execution_agent guest-task-once` is a
root-only client: one newline-delimited Core `EnvironmentTaskEnvelope` on stdin,
one Core `EnvironmentTaskResponse` on stdout. The client does not start or detach
a daemon. Missing service fails closed; a lost response remains unknown. A local
client exit does not establish task exit, cleanup, or capacity release.

The broker retains the existing `NativeManager`, Journal, owner/fence rules,
output cursors and `EndOperation` semantics. It introduces no Agent loop and no
second task state machine. A first valid Core Takeover initializes its single
manager. Repeated clients use the same manager; a restart reopens the existing
journal and frozen target registration.

## Fixed template installation

These paths belong to the trusted VM template, not to a task request:

| Purpose | VM path |
| --- | --- |
| Original-image manifest | `/etc/centaeris-guest/workload.json` |
| Resident broker socket | `/run/centaeris-guest/task.sock` |
| Runtime state | `/run/centaeris-guest/runc` |
| OCI bundle and rootfs | `/var/lib/centaeris-guest/workload/{config.json,rootfs}` |
| Journal, output, launches, helper output | `/var/lib/centaeris-guest/{control,output,launch,helper-output}` |
| Trusted runtime and static helper | `/usr/local/libexec/centaeris/{runc,execution_agent}` |

Control directories are root-owned 0700, control files and the socket 0600.
Symlinks, untrusted owners, writable ancestors, and replacing an existing
instance/target registration are rejected. The original image filesystem lives
only inside the OCI workload mount namespace; no broker state or control socket
is mounted there.

The strict manifest schema is `environment.guest.workload.v1`. It contains
`imageDigest`, `specDigest`, `imageProcess` (`user` with resolved `uid`, `gid`,
`additionalGids`; original `cwd`; original `env`) and a separate `workspaceRoot`
inside the workload. Both digests use lowercase `sha256:` notation; `specDigest`
is computed over the parsed OCI JSON serialized by serde_json. `imageDigest` is
trusted build provenance, not a runtime hash of a mutable filesystem. The
builder must pin and materialize the exact source image and configuration.

The OCI process preserves the image user, cwd and environment, followed by the
trusted `CENTAERIS_WORKSPACE_ROOT` and `CENTAERIS_WORKSPACE_USER` values. For a root
image with WORKDIR `/app`, the process remains `0:0` in `/app`; it is never
substituted with `10001:10001` or `/mnt/data`. A task-root `workspaceRoot` of `/`
allows trusted filesystem operations on paths such as `/etc` inside that same
workload rootfs. It does not expose the VM filesystem. Setup, Agent commands,
verifier commands and file transfer must all use the same target.

## Required kernel and template capabilities

The template must pre-create a running, digest-frozen OCI target named
`workload` with a fixed cgroup path `/centaeris-guest/workload`. This is a VM-local
runc target, separate from the retained legacy host runsc route. The broker does
not create a Docker daemon or import arbitrary caller-selected OCI bundles.

New user, mount, PID, network, IPC, UTS and cgroup namespaces are mandatory.
Workload UID/GID zero must map to a non-system VM ID range (at least 65536), with
all original image IDs mapped. The rootfs must already have compatible ownership
or a separately verified idmapped mount. The broker rejects a symlink rootfs leaf
and requires its observed UID/GID to match mapped container root. Automated
coverage establishes static shifted ownership; idmapped mounts remain unverified.
The broker performs no recursive chown. At open
and before every request it checks actual namespace identities, ID maps, init
user/group IDs, rootfs identity, exact cgroup membership, runc state, PID start
time and frozen bundle identity. Existing task-domain checks additionally
require real writable cgroup v2 domains and `cgroup.kill`.

The reviewed profile keeps no-new-privileges, seccomp, ordinary container
capabilities, private standard mounts and a restricted device policy. A root
workload may retain `CAP_NET_BIND_SERVICE` for nginx/git-server. The private
network requires template-owned veth, routing, NAT and access restrictions that
prevent reaching outer VM control services. Missing network support must not
silently select a shared VM network. Namespace presence alone does not prove
internet access or correct firewall rules.

## Validation status

Automated tests exercise original-image process preservation, strict manifest
and profile rejection, actual rejection of the current namespace, mapped-user
checks, frozen registration, private socket/lease protection, separate Unix
clients, lost replies, journal reopening and rejection of Start without a real
process domain. These tests use no cloud SDK or physical VM.

Seven guest filesystem/socket fixtures require Linux root (the shifted-rootfs
fixture also requires CAP_CHOWN) and are explicitly ignored in ordinary test
runs. They require no CAP_SYS_ADMIN, namespace creation or delegated cgroups.
The guest substrate gate must actually run all 22 tests in an isolated Linux
root environment, without treating default skips as security acceptance:

```text
cargo test --locked -p hosted_execution --lib guest_task:: -- --include-ignored
```

A normal non-root run executes 15 tests and reports those seven root fixtures as
ignored. Both modes must pass before enabling the template. The root-fixture
gate still does not prove positive namespace/cgroup isolation.

Positive VM acceptance remains required: original-image installation and low
ports, internet and control-service isolation, workload root unable to access or
signal the broker, attach-before-exec in real per-task cgroups, descendant cleanup,
output recovery, broker/VM restart and lost-reply reconciliation. The template
and every setup/Agent/verifier/file path must pass that acceptance before the
new channel can be called benchmark-ready. This substrate alone does not route
Core Runs through the new channel or establish a successful benchmark trial.