# Benchmark execution through Session environments

Harbor uses the same Session environment as ordinary Workspace execution. An
immutable `runtime.environment_profile.v1` selects the image digest,
`workspaceRoot`, numeric `user`, `network` and resource declarations. A task image
and working directory are configuration values in that profile. Core owns Agent
execution and tool semantics; the host supervisor owns physical processes.

The API binds the profile before the first Run authorization or explicit
provision operation. Ordinary Sessions freeze their profile when first used.
The current `workspace.agent_run_authorization.v1` signs the complete profile and
its digest. Start, step and teardown each use their current v1 contract, with
controller authority required for both `perRun` and `retained` lifetimes.

## Host and resources

The native Linux supervisor attests the Docker container, runc state, init
incarnation, OCI security settings and cgroup membership before launching work.
Commands, filesystem transfers, hooks and MCP use its common task protocol.
The reviewed static helper is launched from a sealed descriptor. Workloads have
no host control socket, journal, Docker socket or writable host cgroup mount.
See [supervisor installation](../../../packages/hosted_execution/deploy/README.md).

Docker enforces CPU, memory and PID limits. `workspaceBytes` declares the workspace
capacity used by admission and bounds expanded file, input and recovery data.
`storageBytes` preserves the independent task storage declaration; the current host
does not use it for admission or enforce it as a writable-layer quota. The common
profile preserves the image's writable filesystem without mounting a workspace tmpfs. Missing workspace directories are initialized for the profile's
user; existing directories retain the image's contents and permissions.
`disabled` selects Docker `none`; `publicInternet` selects `bridge` and the
configured execution proxy. GPU and multi-container task topologies remain
outside this adapter's supported task inputs.

## Setup, Agent execution and verification

1. Create a Session and bind its environment profile through the independently
   authorized maintenance API.
2. Acquire a signed provision operation. Runtime reserves capacity, creates the
   instance, registers it with the host supervisor and persists an immutable
   provision receipt before the API releases the controller.
3. Run setup with a maintenance operation in that instance.
4. Submit the original instruction through the public Session/message API.
   This is the trial's AgentRun. The Worker acquires the same environment
   controller and invokes the common Runtime lifecycle.
5. After the Agent's terminal and cleanup facts are confirmed, acquire a
   maintenance operation for the native verifier and collect its artifacts.
6. Retire the exact instance and confirm physical absence and resource release.

Provision, setup and verification have typed maintenance ownership. They create
no placeholder AgentRun and require no setup-ready flag to unlock the model.
Environment observation addresses `sessionId` and `environmentId`; it reports
allocation and physical facts independently of an AgentRun.

Command retries reuse the original operation ID and request. A lost
acknowledgement triggers observation of that operation, not another dispatch.
Leader exit, output EOF and an empty task domain are separate facts. Environment
tasks can outlive a Run in a retained instance. Cancellation and teardown finish
the Run's own task scope before verification takes the controller.

Filesystem transfers use a bounded length-prefixed metadata header followed by
raw bytes. The receiver checks the declared length and SHA-256 before publishing
a file or result. Large verifier artifacts do not expand into JSON byte arrays.
See the [controller guide](../../../scripts/harbor/workspace-benchmark.md) for
task selection, image locking and native verifier evidence.

## Runtime backends

The native path currently attests runc. The existing runsc default remains
unchanged while its adapter is connected to this same contract. A runc pass
establishes that backend's behavior; runsc acceptance is recorded independently.

## Upgrade and acceptance

API migrations `0023` through `0025` preserve historical signed grants, reconstruct
known persisted profile facts and add typed provision ownership and immutable
maintenance grant digests. Runtime schema 13 preserves allocation history while
adding maintenance allocation owners and provision receipts. Current execution
readers implement only the current contract; historical reconstruction belongs
to those explicit migrations.

Upgrade API, Worker, Runtime and the native supervisor together after draining
instances owned by the previous implementation. A full Harbor pass requires a
native task through setup, the real AgentRun, verifier and confirmed retirement.
The focused protocol and disposable-host tests cover the individual boundaries;
the batch controller records the integrated result separately.
