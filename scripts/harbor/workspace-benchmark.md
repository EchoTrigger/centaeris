# Workspace Terminal-Bench controller

This adapter runs Harbor 0.21.0 native `Trial.create()` / `Trial.run()` lifecycles.
Each trial submits its canonical Harbor instruction through Workspace's normal
public Session/message API. Worker and Runtime own model/tool execution. Harbor
setup, verification and artifact transfer use the same recorded physical instance.

## Execution order

1. Select validation for the affected behavior and bind the candidate's source
   and physical artifacts. The launcher verifies the supplied checks and logs;
   it does not prescribe six suites or require every check to rerun for a new
   HEAD. Reused evidence retains its actual source commit. The operator must
   establish that its covered code and dependencies are unchanged; it is not
   fresh-candidate execution evidence. Repository-required checks still apply.
   The private bootstrap supplies `physical_gate`; false prevents admission.
2. Freeze the approved task selection from dataset commit
   `7131e4375048a0e408a8fb404b5f499d726b695b`. Every tracked selected file is checked
   against its Git blob, including instruction, Dockerfile, tests and metadata.
   Dirty, untracked, ignored, linked or mode-mismatched source is rejected.
3. Run `kv-store-grpc` as a separate `preflight-*` batch. Its original instruction
   requires package download, generated application files and a background server
   on port 5328. Require a real verifier
   result, actual tool effects, artifact download, background continuity, confirmed
   a verifier reward of one, physical absence and a zero resident debit. These service observations must be
   implemented by the private bootstrap; absent checks block the pilot. A valid
   reward of zero is a model result, not proof of an adapter malfunction, but
   does not satisfy the successful one-task gate.
   Record application-file hashes, exact instance identities and the live server
   observation after verification and before retirement; do not add task hints or
   synthesize missing outputs.
4. Pass that exact preflight report to `run_pilot()` to run the plan's one through
   twenty distinct tasks once each. Root then inspects the actual logs and evidence.
   Escalation through pilot review still requires twenty distinct completed task
   results; a shorter pilot only establishes readiness for its selected task set.
   Under the maintainer's conditional authorization, Root may write the candidate-bound
   review artifact and call `run_full()` for the configured full task set and
   attempt count. A single-pass full plan contains 89 tasks × 1 attempt.
   Normal model rewards of zero are reported separately from infrastructure or
   severe anomalies. Any anomaly blocks escalation and requires a targeted
   proposal for maintainer judgment; the runner does not repair or retry it.

An explicit maintainer instruction may authorize a direct pilot of one through
twenty distinct tasks, each attempted once.
Record it as `centaeris.workspaceBenchmarkPilotApproval.v1` with
`authorized: true`, `admission: "directPilot"`, the exact `candidateDigest`,
`sourceDigest`, and the exact ordered selected `taskIds`. Supply that artifact with
`--stage pilot --approval <path>` instead of a preflight report. This preserves
failed preflight evidence and does not claim that preflight passed. Physical,
database, source, resource, verification and cleanup gates still apply; the
full-run authorization remains separate. A continuation requires a new plan and
approval bound to only its selected tasks; it does not replay earlier tasks or
combine earlier batch results into a full-review certificate.

An explicit maintainer instruction may instead authorize a direct 89-task single
pass, without claiming that preflight or pilot passed. Use the pinned task list
from `full-tasks.json`, a plan with `attempts: 1`, and
`centaeris.workspaceBenchmarkFullApproval.v1` containing `authorized: true`,
`admission: "directFull"`, exact `candidateDigest`, `sourceDigest`, ordered 89
`taskIds`, and integer `attempts: 1`. Supply it with
`--stage full --approval <path>` and omit `--pilot-review`. The runner records the
approval in `batch-result.json`. This permission covers exactly 89 trials;
it does not authorize five attempts or remove any physical/source/resource gate.

The reviewed-pilot path uses `require_full_review()` and never infers log quality from pass
counts. Static plans keep `fullRunAuthorized: false`; the later explicit review
artifact authorizes that entrypoint. Its reviewer should identify the actual
review, for example `Codex review under maintainer conditional authorization`.
Product and adapter must match the pilot exactly; full task/config bindings are
recorded separately. Full attempts run in deterministic attempt-major task order,
each with a fresh Session and instance.

Every batch receives a fresh directory. Existing batches are never resumed,
overwritten or used as permission to repeat model work. Uncertain calls retain
their original operation identity and pending cleanup allocation.

Agent timeout remains an `AgentTimeoutError`. Cancellation first waits for that
same Run's terminal fact, then confirms process quiescence through the existing
maintenance exec port. The helper stops all other processes in the owned
benchmark PID namespace, including escaped descendants, and preserves its
keeper and filesystem for the verifier. Until that receipt is confirmed,
Environment exec/upload/download operations are blocked. Retirement remains
available and releases admission only after physical absence and zero debit.
No tool or benchmark attempt is resubmitted by this handoff.

The focused Docker handoff regression is opt-in and uses two disposable,
network-disabled 128 MiB containers with a supplied original Python task image
and static helper. It checks detached-child cleanup, original-file preservation,
verification in the same live container, unrelated-container survival and exact
probe removal; it does not run or score a dataset task:

```bash
export CENTAERIS_BENCHMARK_CANCELLATION_TEST=1
export CENTAERIS_BENCHMARK_CANCELLATION_IMAGE=sha256:<cached-image-digest>
export CENTAERIS_BENCHMARK_CANCELLATION_HELPER=/absolute/path/to/execution_agent
python3 -B scripts/workspace/retained_acceptance/test_benchmark_cancellation.py -v
```

## Private controller ports

`BenchmarkRunner` accepts authenticated `client_factory`, independently authorized
`provisioner`, `Admission`, image preparer and resolved `model_ref` in memory.
The public client uses ordinary account cookies and CSRF. The provisioner uses
separate internal and maintenance credentials. Supply these through a private
bootstrap outside the job tree; do not put their values or objects in Harbor JSON.

Harbor's import-path environment class resolves a one-use opaque
`controllerBinding` reference in a process-private registry. It checks the trial
and task-directory binding before injecting ports. Native configuration, lock and
result files contain that reference, not the ports or their credentials.

Model settings come from the plan's one `model` descriptor: `apiBase`,
`credentialEnv`, `id`, `thinking` (a mode or null), `contextTokens`, and
`outputTokens`. This adapter uses the existing `openai-completions` protocol.
The committed plan is an example; copy it into private controller storage and
select the provider endpoint, model, credential variable and budgets there.
Use an HTTPS endpoint without URL credentials, query parameters or fragments.
The output reservation must be positive and smaller than the total context.

The seed process receives the same descriptor and creates its ModelConfig from
those values. Native Harbor model labels, Agent thinking mode and setup thinking
mode also use that plan. The private seed records the descriptor and its canonical
SHA-256; launch refuses a plan/seed mismatch before authentication or admission.
The full descriptor remains part of the candidate configuration digest. The private
bootstrap must also verify the actual Workspace ModelConfig and Runtime budgets
before the physical gate returns true. Credentials are read from the named variable
only by private operator code and streamed to the one-shot seed process, never
injected into task environments. A configuration label alone does not prove effective
limits.

An explicit provider output-limit finish is carried through the API and Runtime
as `incomplete_output_token_limit`. Core owns the continuation policy: preserve
stream-observed partial text, reject unfinished tool calls without executing them, and
continue in a new turn up to its existing recovery limit. A normal stop with no
text and no tool calls is a separate `empty_final_response` failure handled by
the existing bounded provider retry. Reaching the configured token count alone
is not evidence of either finish condition.

## Images and resource lifecycle

### Download egress

Image pulls use the Docker daemon's proxy, independently of the API and task
container proxies. Changing a shell's `HTTPS_PROXY` or an execution-host default
does not change an already running daemon. On a host with a local proxy listener,
merge `http-proxy` and `https-proxy` under the existing `proxies` object in
`/etc/docker/daemon.json`; both can use an HTTP endpoint such as
`http://127.0.0.1:7890`. Preserve the existing direct destinations, data root,
runtimes and registry settings. Keep any proxy credentials in host-private files.

Validate the merged file with `dockerd --validate --config-file <path>`. Applying
these proxy changes requires a Docker restart, so first drain benchmark trials
and record the other running services for the approved maintenance window.
Check the effective daemon proxy and restored service health afterward, then
pull the original failed image and record its immutable digest. A Registry v2
endpoint returning its normal authentication challenge verifies connectivity;
it does not prove that every image layer can download. See the
[Docker daemon proxy documentation](https://docs.docker.com/engine/daemon/proxy/).

The API's model-provider proxy is independent of task networking. Configure
`EXECUTION_HTTP_PROXY` and `EXECUTION_HTTPS_PROXY` on Runtime for the execution
host's public-internet containers, and `EXECUTION_NO_PROXY` for direct destinations
(default `localhost,127.0.0.1,::1`). Only HTTP(S) proxy endpoints are supported.
Runtime installs uppercase and lowercase proxy variables as container defaults,
shared by model commands and Harbor verifier execs. Explicit per-exec values keep
their usual override behavior. Offline profiles receive no proxy variables and
keep Docker networking disabled. These defaults apply to newly created instances.

The endpoint must be reachable from the task's Docker network; a proxy listening
only on the API's Compose gateway or on host loopback is insufficient. Keep node,
subscription and model-provider credentials on the host/control plane. If proxy
authentication is needed, use a separate credential scoped to this execution
egress. Validate downloads from the original task image and run its unchanged
verifier; configuring the API or Docker daemon alone does not validate task
downloads. Programs that ignore proxy environment variables need their own
documented configuration and are not covered by these defaults.

Acquire admission before image download/build and hold it through verification,
artifact collection and confirmed cleanup. At most two trials run concurrently;
an 8 GiB or larger task runs exclusively. The host's actual available capacity and
control-plane reserve must be supplied to `Admission`. Recheck disk headroom
before selecting an attempt and after image preparation because cached images
persist after task-container cleanup. A live disk shortage leaves the task
pending, so a peer can finish and release its reservation before the next
admission. If only one trial fits, execution becomes serial. A post-download
shortage keeps the same trial identity, attempt and prepared image; it does not
repeat the pull or create a Workspace Run. If no running trial can release
capacity, remaining attempts are reported as `unrun: diskCapacityUnavailable`
with `freeBytes` and `requiredBytes`, and the batch is blocked. These are host
capacity observations, not model failures or verifier rewards.

`DockerImagePreparer` uses an explicit Linux Unix-socket Docker endpoint. Existing
original prebuilt images are preferred; a missing one is pulled. Source-only
tasks use Harbor's content-addressed build helper with the original Dockerfile
and context. Its inherited `DOCKER_HOST` must exactly match the configured socket.
No helper is installed into or layered onto the task image by this controller.

Supply one shared public `image_lock_path` to the image preparer for preflight,
pilot and full batches. First resolution records the immutable image ID,
repository digest(s), source checksum, working directory and effective/original
user. Later attempts inspect that ID rather than resolving a mutable tag again.
If the cached image is missing, pull a recorded repository digest or fail; do not
fall back to the original mutable tag. Pilot review binds its complete image
inventory and full admission checks that all overlapping tasks retain those
exact locks. Newly encountered full-only tasks are locked on first preparation
and reused for their five attempts.

Each batch retains `image-locks.json`, per-trial image/profile evidence and the
inventory hash in its result. This image evidence is a separate provenance layer
from the source/config candidate hash; an immutable source commit does not make
a Docker tag immutable.

Inspect immutable image ID, original working directory, original image USER and
the task's optional agent user. Named users are resolved from `/etc/passwd` and
`/etc/group` using a short-lived, stopped metadata container; no image command is
executed. The exact metadata container is removed afterward. An unresolved user
is unsupported; the runner does not substitute root.

Profiles preserve declared task CPU, memory, storage, effective user, working
directory and network. Benchmark host settings additionally use a PID limit of
4096 and a 64 MiB data accounting reservation. The latter is a Runtime ledger
reservation, not a mounted task tmpfs. `storageBytes` records the task declaration;
this adapter does not claim that native Docker provides a hard disk quota.

Current supported tasks are single-step Linux/amd64 with a shared verifier, no
Compose sidecars, GPU/TPU, MCP, custom bind mounts, environment injection or network
allowlists. Unsupported combinations fail before model admission. Harbor's three
standard log-directory hints are copied, not mounted from the controller host.

## Setup and exit

The environment creates a fresh Session, binds the immutable profile and submits
the task with a stable operation ID. Runtime must attest `waitingSetup` before
Harbor mutates setup files. Harbor's own canary removal is reused for instruction
preprocessing. Agent start adopts that original Run receipt, activates readiness
and observes committed events; it never submits a second prompt.

After verification, the preflight service checks run before destruction so they
can inspect actual task state. Observation failures do not block exact cleanup.
The maintenance endpoint destroys only the recorded instance. Both physical
absence and resource reconciliation are independently observed before admission
is released. Unknown outcomes remain pending and keep their capacity reserved.
A failed trial does not stop subsequent trials. After an unconfirmed cleanup,
later trials may use genuinely available capacity; tasks that cannot start once
all running trials finish are listed as unrun with a retained-capacity reason.
The batch remains blocked for review when any infrastructure or cleanup issue
occurred, even if all other trials completed; it never automatically starts the
full benchmark.

Exec and transfers are never automatically retried after transport loss.
Upload/download use raw `application/x-tar` bodies, bounded to 32 MiB archive
and expanded data and 10,000 members. Upload metadata is strict JSON in
`X-Benchmark-Transfer`, bounded to 8 KiB; download metadata remains a JSON request.
The controller streams temporary tar files in 64 KiB blocks; Runtime retains one
bounded binary archive for validation and Docker dispatch, without base64 copies.
The shared limits are in `benchmark-transport.json`, with Runtime parity tests.
Before any task image or Agent starts, every selected task's original verifier
input is packed through these same limits. Unsupported inputs block the batch.
Links,
devices, sparse entries, traversal, duplicate entries and file/parent collisions
are rejected. Larger artifacts fail explicitly instead of being clipped.

Transfer failures retain stable public codes for missing artifact paths, archive
limits, rejected ownership/physical identity, unavailable dependencies and unknown
upload outcomes. Diagnostics never include Docker error bodies or private paths.
Trial summaries retain independent `agentOutcome`, `verifierOutcome` and
`cleanupOutcome` facts. Harbor's original first exception remains intact; a later
verifier failure is still reported as infrastructure failure after Agent timeout.
Pilot review validates these canonical Outcome objects and their agreement with
the final verifier and cleanup state. An Agent timeout alone does not invalidate
a completed verifier reward when cleanup confirms absence and zero resource debit.
An earlier cleanup error may remain recorded after confirmed absence.
Verifier outcomes use exact `notStarted`, `running`, `completed`, `failed` and
`unknown` statuses. A complete controller hook observation without verifier entry
establishes `notStarted`; its start hook projects `running`. Final `completed`
requires a valid native reward, including zero; an observed verifier error is
`failed`. Entry without a reward or phase error ends as `unknown`, not a guessed
failure or a zero score. The terminal monitor fact uses the same verifier outcome.
Agent failure and confirmed cleanup remain independent of these verifier facts.

Historical `completed`, `failed` and `unknown` outcomes retain their exact meaning
when reviewed; no status aliases or inferred scores are accepted. Reconstruction
from a native result may establish `notStarted` only when the complete trial and
Agent timing explicitly record no verifier phase. Missing phase fields or a null
result alone remain `unknown`. Reading or reviewing evidence never rewrites the
original result files. Every outcome except a completed verifier with a valid
reward still blocks pilot readiness and the reviewed full-run gate.

## Monitoring and evidence

Each batch creates its own read-only projection directory. `batch_started` exposes
`projectionDir` before image/model admission so the launcher can rebind only the
monitor service. The normal viewer uses port 9000 over the local SSH tunnel.
Telemetry queues are bounded and fail open; a stale monitor cannot cancel work.

Agent completion and cleanup completion do not imply a verifier reward. Final
trial status combines the native Harbor result and confirmed cleanup. Retain
native config/lock/result files, source and image bindings, verifier output,
service-check evidence, Workspace identities and the compact batch result.
Only one result for every exact selected task counts toward that pilot's review;
missing, duplicate or additional results block readiness. A full-run review still
requires twenty distinct completed pilot results. The service preflight remains
a separate experiment.

Local contract tests use fake services and are not physical acceptance evidence.
The real one-task preflight and pilot must remain unstarted until the private
bootstrap's physical gates succeed.
