# Environment lifecycle reclamation

Callers submit Agent invocations; the platform owns queuing, admission, environment reuse and reclamation. Reliable resource facts are the prerequisite for automated placement and elastic capacity supply.

Queued cancellation and physical resource settlement must continue independently of ordinary execution capacity. Complete that lifecycle before making retained environments the default for ordinary Sessions. Execution limits remain eight globally, four per Workspace and eight slots per Worker process.

## Observed gaps and expected behavior

| Boundary | Current evidence | Required behavior |
| --- | --- | --- |
| Workspace execution capacity exhausted | The deployed baseline could not claim a cancelled queued Job while four owners remained active. The control-lease implementation below now claims it independently. | An independent bounded control path commits the cancellation without waiting for an unrelated execution slot or granting execution authority. |
| Global execution capacity exhausted | The deployed baseline could not claim a cancelled Job while eight owners across two Workspaces remained active. The same independent control path now handles it. | The same control progress must hold independently of the global execution limit. |
| Resident capacity becomes available again | In the isolated 8 GiB boundary test, released capacity was followed by a live waiting workload after 29.65 to 30.08 seconds. Worker capacity waits use the default 30-second lifecycle recheck. | Durable capacity release should make eligible capacity waiters due promptly, while retaining admission checks, single ownership and periodic recovery. |
| Failure before physical creation | The initial regression left a reservation charged after a fresh pre-create callback failed. The undispatched settlement below now closes this case. | Preserve allocation history and atomically settle the proven unmaterialized attempt, its holding and subsequent obligations. |
| Cleanup proof lost at the host boundary | A controlled host callback removes its own synchronous attempt but returns only a string failure. The ledger retains the reservation. | Carry typed physical evidence to durable settlement. A string error or an empty inventory cannot authorize release. |
| Historical reserved allocations | Earlier records may have no attempt evidence or container ID. | Recover a durable investigation obligation; never infer non-creation from age, lease expiry or an empty observation. |
| Workspace storage budget | The current container request carries the data budget label but no explicit Docker tmpfs or storage option. | Establish the actual writable backing and enforce the declared byte/inode contract at the physical boundary. |

The database regressions are in
[`resource_lifecycle_tests.rs`](../../packages/runtime_server/src/postgres_store/integration_tests/resource_lifecycle_tests.rs).
The two capacity regressions check ordinary exclusion, control readiness/claim, persisted purpose and unchanged execution owners. Separate Runtime HTTP tests cover terminal history, receipt replay and completion gates; the service-chain measurement below covers a never-admitted queued Run. Physical cleanup remains a separate acceptance boundary.

Existing unknown-create regressions continue to require charged resources and adoption of the exact late instance after reopening. Any settlement fix must preserve those protections. A known failure and an unknown physical outcome must never share an unconditional release path.

## Undispatched allocation settlement

A fresh AgentRun allocation commits its holding and one creation attempt before the pre-create callback. The attempt's immutable allocation reference binds instance, host, origin grant and profile. A callback failure may close only this fresh, still-prepared attempt. Under current controller and lease authority, recording `notDispatched` and removing the exact reserved holding commit together. Allocation history remains intact. The same failure while reusing a resident changes neither the original holding nor its physical instance.

Successful preparation commits `dispatched` before entering the host callback, then reacquires authority pins across the physical operation. Failures after this conservative boundary retain their charge, including a crash before the actual send or an error string from a callback that privately removed its object. A possible dispatch cannot be rewritten as non-dispatch.

Cleanup recognizes an unmaterialized allocation by durable evidence rather than treating it as `NeverAllocated` or environment loss. Its receipt retains the planned instance identity without claiming physical deletion. Both lifetime policies replay that receipt after reopening. Observation reports `notMaterialized`, and a later Run can obtain a fresh allocation without recreating the old execution.

Runtime schema migration 14 adds the dispatch journal without inventing evidence for historical reservations. Historical unknowns remain charged and cannot dispatch again through ordinary AgentRun preparation. No-Run provision dispatch, typed post-create removal evidence and interrupted-attempt investigation remain follow-up boundaries. The bounded cancellation path is described below.

## Workspace creation is a separate boundary

A bare Docker create/start probe does not perform the host's workspace preparation. The native `prepare-workspace` helper creates a missing workspace for the profile's numeric user and preserves existing image content and permissions. Absence of `/mnt/data` immediately after bare container creation is therefore not, by itself, an established product defect.

Acceptance must exercise the real preparation and task path before judging workspace availability. It must then identify the workspace mount/backing, enforce its configured limits, verify file visibility across supported lifecycle operations, and distinguish workspace data from temporary directories. A write to `/tmp` cannot certify the `/mnt/data` contract.

The opt-in `export_owned_runsc_storage_probe_request` test exports a request from the production container request builder for an explicitly owned physical probe. Probe drivers, private configuration and artifacts remain outside the source checkout. A separate Docker daemon must also run in a separate Linux network namespace; a different socket/data root alone does not isolate Docker's host network management.

## Resource limits, commitments and observations

Environment resource limits, admission commitments and observed consumption are separate facts. The resource limit bounds what an isolated environment may consume. Its admission commitment describes what the scheduler promises and charges. Observation describes a particular host/container at a particular time; it does not modify the promise or establish physical absence.

Core's `ResidentResourceClaim` binds immutable `limits` and `reservation` vectors. Resident admission adds the reservation, while preserving the original limits for host enforcement. Both vectors describe one sandbox. Memory commitment is positive and cannot exceed the memory limit; CPU commitment may be zero for best-effort service and cannot exceed the CPU limit. This first contract keeps PID and workspace-space ceilings fully reserved. It does not introduce a storage-overcommit policy or configure a cgroup. Invalid bounds and arithmetic overflow fail explicitly.

Both Runtime allocation entry points, AgentRun and no-Run provision, explicitly construct fully reserved claims. Their existing profile signatures, Docker limits, inventory comparison, PostgreSQL resource columns, historical debit and default behavior retain their current meaning. Partial CPU/memory claims are supported and tested by Core; hosted shared admission is not enabled by this change. A low observed RSS cannot reduce or erase an existing holding.

Before hosted callers may use partial commitments, persistence must separately retain the original limits, the immutable admission commitment and its policy revision/digest for each allocation. Reuse, reconciliation, settlement and no-Run provision must all read the same commitment. Existing rows require a tested forward migration that preserves their full original debit and signed authorization bytes. Changing a process configuration must not reinterpret an existing allocation. Unknown creation/deletion and releasing resources retain their commitment and investigation responsibility.

Shared admission also needs a current node-pressure gate and accounting for in-flight placements. Summing commitments alone cannot guarantee simultaneous use of every environment's maximum. Keep independent control capacity, a bounded aggregate workload pool and host headroom; specify what happens when workloads burst together before enabling overcommit. Actual node supply and placement remain platform responsibilities. The current 8/4/8 Agent execution settings are separate from physical sandbox density.

The observation contract must identify the host and exact container/instance plus the sample time and interval. Use cgroup `memory.current`, memory pressure/events, CPU usage/throttling, PIDs and storage usage. In gVisor, application memory is largely accounted as shmem/file, so host `anon` or guest RSS alone is insufficient. Do not add shmem to file when one is included in the other. Observe individual cgroups and the aggregate pool; control-plane and runtime/daemon overhead outside that pool require their own capacity. No observation grants execution or deletion authority.

Linux CPU weights/quotas, demand paging and reclaim provide a mechanism to share a host while keeping separate address spaces and private writable state. `memory.high` causes reclaim/throttling rather than reserving memory, and aggressive settings can delay ordinary operations. Anonymous private data cannot be discarded merely because it is idle. Retained cold suspension/restoration requires its own state-preservation contract. Firecracker remains a candidate backend if measured isolation, compatibility or resource-efficiency requirements warrant it; a backend change does not replace this admission and settlement contract.

The admission regression first failed while the algorithm charged the ceiling: three contract/characterization tests passed and the shared-commitment case failed. Charging the validated reservation made all four pass. Final validation passed the portable Rust source gate, Core's 729 unit tests and focused 129-test query-loop gate, plus warning-denied Core/hosted Runtime Clippy. The native hosted Runtime suite passed 468 tests with 204 opt-in tests ignored. The first native hosted run inherited a proxy that broke 15 loopback fixtures; process-local proxy exclusion restored all 468 passes. This change adds no persistent format; PostgreSQL physical/shared-admission acceptance is not established by these results.

## Bounded lifecycle control leases

Runtime schema 15 adds a persistent `execute/control` purpose and, for control leases, an `advance/cleanup` stage to the existing lifecycle Job. Missing historical purposes become `execute`; existing signed grants, owners and job history are preserved. The purpose and stage cannot change while the same lease owner remains current. Upgrade API, Worker and Runtime together after draining incompatible live consumers, following [Session environment upgrades](SessionEnvironments.md#persistence-and-upgrades).

Ordinary ready/claim excludes Jobs with a digest-bound durable cancellation or terminal-control intent. Control ready and claim share the same server predicate: queued, due, no current owner, matching durable intent and the appropriate authoritative terminal state. Requesting `control` alone grants nothing. Execute capacity stays global 8 and Workspace 4, with eight ordinary Worker threads. Independent control threads and global control-lease budgets provide two advance and two cleanup slots. Reclaim exhaustion retains a pending control Job for investigation/retry rather than deleting its intent.

The control API reads the original signed Run authorization without acquiring an Environment controller. Runtime restores Core Session state and uses Core's termination operations. A never-admitted Run with no allocation or owned controller receives a durable `noEffectsForRun` fact containing its no-operation cleanup and release receipt identities. It does not acquire another Run's controller. A Run with prior Core history or an allocation cannot use that shortcut: it yields the same Job to the cleanup stage and follows existing cleanup/release authority.

Server boundaries reject control authority at ordinary start, API execute-controller/start resolution, Runtime step, model requests, ordinary record append and host execution-controller validation. An old signed execute grant does not override a current control lease. `complete` verifies an authoritative terminal plus matching cleanup/release facts, including on successful reply replay. Missing or altered no-effects evidence and unresolved physical cleanup are rejected.

Short Runtime control HTTP/DB budgets remain four/two. Slow teardown uses a separate HTTP limit and DB pool, both defaulting to two (`RUNTIME_HTTP_CLEANUP_LIMIT`, `RUNTIME_POSTGRES_CLEANUP_POOL_SIZE`). It retains the existing authority pins across physical cleanup. This separation prevents slow teardown from consuming the short-control pools; it does not establish fixed completion times under database, host or shared-executor failure.

This boundary repairs queued cancellation under ordinary execution saturation. Active-owner failure handoff, historical terminal/dead-letter reconciliation, HTTP/DB combined saturation, physical cleanup under pressure, no-Run provision recovery, storage quotas and target-host acceptance still require their own tests. Retained is not yet the default.

## Runtime HTTP cancellation measurements

A small WSL experiment used the deployed Runtime image at `b389f6d29411423d494d78d0b94fe4d68fdc54e0` (`sha256:993a7e156a545ac8317342ff92ea2469246db545e51c7e7da248d2881b4e636c`) and its schema 13 against a fresh PostgreSQL 18 container. The measured cancellation handler, ready query and claim capacity logic are unchanged in candidate `b67e04396e9d9613ba864fb2902fb8685f9d76ae`; this remains a deployed-build baseline, not final-candidate acceptance.

The driver issued real authenticated HTTP schedule/claim/start/cancel/wait/heartbeat/yield requests. Synthetic Session/Run rows supplied the database boundary; no API admission, Worker thread, model or sandbox execution was exercised. Execution limits remained global 8 and Workspace 4. The private Runtime had 512 MiB/1 CPU and PostgreSQL 1 GiB/1 CPU; these are local diagnostic timings, not production throughput estimates.

Each condition ran three times. Full-capacity cases retained their original four/eight running lease owners and heartbeats for a 1.5-second observation window, polling every 500 ms. Global saturation used two Workspaces with four owners each and a target in an empty third Workspace.

| Condition | Cancel HTTP acknowledgement, ms (three samples) | Target before releasing a blocker | Yield one blocker through HTTP, then claim target, ms |
| --- | --- | --- | --- |
| Idle | 12.74, 6.25, 6.21 | Ready and claimed 26.17, 11.87, 11.94 ms after cancellation request began | Not applicable |
| Workspace full | 6.19, 6.24, 6.50 | All four observations per trial timed out for ready and claimed zero; target remained queued through at least 1.5 seconds | 6.26, 6.21, 6.59 |
| Global full | 6.12, 6.55, 6.65 | All four observations per trial timed out for ready and claimed zero; target remained queued through at least 1.5 seconds | 6.46, 6.21, 6.25 |

Every target had exactly one durable cancellation-intent event. Targets had no authoritative terminal event during this experiment; even the idle result proves claimability only. Physical allocations and resident holdings remained zero. A read-only Docker proxy prevented host mutations, owned containers/network were removed, and the existing application containers retained their exact IDs, image IDs and start times.

The first demonstrated obstruction is execution-capacity eligibility at ready/claim: cancellation is acknowledged promptly but cannot obtain processing while ordinary execution is full. Releasing one unrelated execution owner removes that obstruction immediately. The control-lease implementation above and service-chain follow-up below address that obstruction. This experiment does not measure physical cleanup or Job completion latency.

## Service-chain queued-cancellation measurements

The repaired build was tested from committed source `8e7b36ed58a44a2811ac4cc139b17a8050edd115`, using Runtime image `sha256:5873344755da0a3922241e10ba9aa2cb2ae9162a23c08bbd011beaede25cf280` and Runtime binary SHA-256 `496b63be3f1194e2a7ba2aba25fa9a1b1bb41b930f60f676a6f196c1ce5780c4`. API and Worker used that same committed source over the existing dependency images in a separate owned stack. A documentation-only follow-up does not change those executable sources.

This experiment exercised public authenticated Session/Run admission and cancellation, real Worker ordinary and control threads, original signed authorization, API terminal projection, Runtime receipts, PostgreSQL and Job completion. Ordinary workers were held immediately before Runtime step dispatch, after real claim/start/controller resolution; their normal heartbeat loops remained active. These were execution-entry barriers, not eight physical 8 GiB sandboxes. There were no model calls, physical task executions or sandbox allocations. A GET-only Docker socket proxy rejected physical mutations.

WSL had eight virtual CPUs and 16 GiB RAM. The private stack bounded Runtime and Worker to 512 MiB/1 CPU each and API and PostgreSQL to 1 GiB/1 CPU each. Per-sandbox declarations remained 8 GiB, four CPUs, 4 GiB data budget and 512 PIDs; this test did not establish their physical enforcement. Both execution limits and ordinary Worker count remained 8/4/8. Each saturated condition ran three times with a distinct target Session.

| Condition | Cancel HTTP acknowledgement, ms | Request start to observed Job completion, ms | Ordinary execution held through completion |
| --- | --- | --- | --- |
| Workspace full | 25.78, 25.44, 28.84 | 399.10, 397.61, 431.18 | Four owners in the target Workspace |
| Global and Worker full | 26.48, 25.20, 22.86 | 395.52, 411.24, 416.02 | Eight owners in two Workspaces; target in an empty third Workspace |

All six targets had one no-effects receipt, a cancelled Run with committed pre-admission cancellation and a succeeded lifecycle Job whose final lease purpose was `control`. Their Environment epoch stayed zero with no controller owner; no Core execution records, physical allocations or resident holdings appeared. The original four/eight owners stayed unchanged, the same ordinary Worker slots remained held, and no extra ordinary execution was admitted. Completion timestamps include database polling and client overhead; they are diagnostic observations, not latency guarantees or percentile estimates. This experiment includes more services than the earlier Runtime-only baseline, so acknowledgement timings are not a direct speed comparison.

The useful comparison is dependency: the deployed baseline remained unclaimable throughout the 1.5-second observation until an unrelated owner yielded. The repaired service chain completed while every blocker stayed held. Control workers currently poll every 250 ms; their idle request overhead and contention at larger Worker counts have not been benchmarked. HTTP/DB saturation, slow physical teardown and actual sandbox cleanup remain separate measurements.

The probe issued zero Docker mutation requests. All owned probe containers and network were removed, and the existing application's container IDs, image IDs and start times were unchanged. Private fixture drivers and synthetic secrets remained outside the source checkout.

## WSL physical cancellation and retained residency measurements

Committed source `91c7e4280787940c99439d25900c2bdc51b1f2e5` was exercised through public API admission/cancellation, Worker, Runtime, PostgreSQL, the native host supervisor and real runsc tasks. A local deterministic model supplied fixed tool calls without an external model service. Execution settings stayed 8/4/8. Each case below ran three times; only one sandbox was resident at a time, so these are physical lifecycle measurements rather than eight-sandbox saturation acceptance.

The WSL host had eight virtual CPUs, 16 GiB RAM, kernel `6.18.54.1-microsoft-standard-WSL2` and runsc `release-20261005.0` with `--platform=ptrace --in-sandbox-cgroup=v2`. The fixture used its own containerd process/socket/namespaces, Docker daemon/data root, runsc root, and Linux network and mount namespaces. A raw owned runsc container verified the private runtime root before application admission. Current source was built into Runtime image `sha256:54e899b2f03eaad4388a1686ee1325eb1a47a0d4cc9298a8a3052b7a0dec3d06` and sandbox image `sha256:f34c0f8a7b300d8e7646ebed834b325ec546a54ea3c253cb02376fe2f7f4cc7c`. The Runtime binary digest matches the service-chain build above; the static musl `execution_agent` digest was `1010e74091070431900913fe5ce1b6a013f294f76e793d405f4d81c4d532961d` and supervisor digest was `1b5d6da26ce8e116a91ff9041e7aca9f703f87c4e06e6eb605390754691b1e29`.

Before cancellation, a real foreground tool had two live guest PIDs: a parent ignoring SIGTERM and a child in a separate process session, also ignoring SIGTERM. Retained cases additionally started an Environment-scoped task that wrote a heartbeat file. The probes observed physical files, guest process presence, native task receipts and database facts; they did not infer cleanup merely from an HTTP acknowledgement.

| Case | Cancel acknowledgement, ms | Cancellation request to observed Job completion, ms | Required result observed in every trial |
| --- | --- | --- | --- |
| Active perRun | 162.27, 49.71, 116.48 | 1690.66, 2390.47, 1684.88 | Cancelled Run, one terminal/cleanup/release fact each, exact container absent, holding zero; a new Session subsequently executed successfully. |
| Active retained | 102.51, 211.53, 62.18 | 1803.79, 2108.93, 2181.69 | Run task cleanup confirmed by exact task-ID query and both guest PIDs absent; same instance/container, unchanged file hash, Environment tasks and one holding preserved. |
| New Session waiting behind retained residency | Included in completion interval | 897.80, 912.50, 432.96 | Waiting Run cancelled and Job completed; incumbent container and resident holding stayed intact. |

For perRun, the exact container was first observed absent at 1690.65, 1608.28 and 1684.87 ms. For retained, Run task cleanup was first observed at 628.11, 782.42 and 703.19 ms; cleanup/release receipts appeared at 1713.32, 1982.76 and 2010.63 ms. Each active cancellation finished under its original execute owner; it did not require or test failed-owner handoff. Serial database, Docker and supervisor observations add overhead and cannot order close physical/database transitions precisely. These three-sample observations are not latency guarantees.

All retained rounds reused one Environment and container. Following each cancellation, another Run in the same Session completed successfully. Native task-ID queries confirmed completed Run cleanup while prior Environment task IDs remained active; an additional live file observation confirmed all three Environment heartbeat files continued growing. This is same-instance continuity, not restart or cold-restore acceptance.

The resident budget for the Workspace was 8 GiB/four CPUs, equal to one sandbox declaration. After same-Session reuse, the retained sandbox's host cgroup sampled 28.98, 36.96 and 44.41 MiB and approximately 0.0365, 0.0704 and 0.0734 CPU cores over one-second intervals, while the ledger still reserved 8 GiB/four CPUs. Those rounds accumulated one, two and three Environment tasks and included observer overhead; the samples do not establish a production resource profile. The configured cgroup limits were `memory.max=8589934592`, `cpu.max=400000 100000` and `pids.max=512`. Quota configuration is not an exhaustion test, and the 4 GiB data label was not validated as a physical storage limit.

A different Session in the same Workspace repeatedly yielded with `execution_resident_capacity_wait`, created no container or holding, and remained waiting through 3115.84, 3172.19 and 3230.45 ms observation windows. The Workspace reservation budget was exhausted; the 16 GiB host and its headroom could also constrain a second 8 GiB allocation, so this experiment does not isolate the sole limiting domain. Cancelling the waiting Run continued to work without freeing the incumbent environment.

The next resource-efficiency question is concrete: how should protected retained state and its execution reservation be admitted and supplied when observed use is small but promised capacity remains unavailable to other Sessions? This evidence does not authorize refunding a resident holding while its resource promise remains enforceable. Compare explicit resource profiles, capacity supply and a separately specified retirement/restoration policy before changing that promise.

A secondary observation needs its own timing breakdown: after perRun cancellation, the next Session's first successful Bash result took 6516, 6508 and 6527 ms from admission request start, with Job completion at 12554.40, 12686.67 and 13137.87 ms. Retained follow-up Runs took 3979, 5154 and 6271 ms to their first successful tool result and 9232.55, 11748.04 and 13936.10 ms to Job completion. Their first tool was a task-list request followed by Bash, so the two groups are not a controlled reuse speed comparison. Admission, claim, preparation, tool execution and finalization need separate timestamps before assigning the delay to storage or scheduling.

Fixture bring-up failures are excluded from the successful trial counts: a GNU static helper failed during memfd execution and was replaced by the musl build; stale task-protocol and output-read parameters were corrected in the private observer. An early daemon-isolation attempt coincided with changes to application start times, whose cause was not established. After isolation was strengthened with an independent containerd and forced private runsc root, both reported physical groups retained the existing application's exact container IDs, images and start times. Owned test containers and daemons were removed after the measurements; operator teardown is not product retirement acceptance.

These results establish the exercised WSL cancellation, Run-scope preservation and resident-capacity waiting boundaries. Physical saturation, crash/reply-loss recovery, explicit Environment retirement, byte/inode enforcement, Debian acceptance and the default lifetime migration remain open.

## WSL bounded sandbox density experiment

The density target is 100 separate runsc sandboxes, each holding and repeatedly reading 100 MiB of private, actually touched anonymous memory. This is a physical adapter experiment, independent of API/Worker AgentRun admission and the unchanged 8/4/8 execution limits. It does not certify 100 concurrent AgentRuns or enable hosted overcommit. The sandbox image and ptrace runtime match the physical experiment above; the isolated diagnostic Docker daemon uses the cgroupfs driver rather than the deployment's systemd driver.

A dedicated parent cgroup bounds workload memory to 12 GiB, CPU bandwidth to four cores and PIDs to 16,384, with swap disabled. Each child retains an 8 GiB/four-CPU/512-PID ceiling. The driver advances toward 100 only while keeping test-pool and host headroom. Docker, containerd, runsc state, network/mount namespaces and all status directories are private to the fixture. Daemon/controller memory outside the workload parent is not included in the table.

Each guest touches all 25,600 pages, retains the mapping, periodically scans it and verifies a private canary. The observer checks host-side heartbeat-file modification times; guest and host monotonic clocks have different origins and are not compared. The workload and sentinel checks establish the exercised private-state behavior, not a general security-isolation acceptance.

| Live sandboxes at ramp sample | Payload memory, MiB | Aggregate cgroup memory, MiB | Excess over payload per sandbox, MiB | Host cgroup PIDs |
| --- | --- | --- | --- | --- |
| 1 | 100 | 122.09 | 22.09 | 39 |
| 5 | 500 | 604.63 | 20.93 | 193 |
| 10 | 1000 | 1208.13 | 20.81 | 392 |
| 20 | 2000 | 2414.45 | 20.72 | 780 |
| 40 | 4000 | 4826.45 | 20.66 | 1584 |

All sampled working sets and canaries were live. At the 40-sandbox sample, the parent reported no OOM, no limit hits and zero recorded memory-pressure stall time; the host reported 9490.66 MiB available. The observed excess includes runtime and charged kernel/cache memory, not just application heap. This demonstrates that the nominal child ceilings are not physically preallocated.

The WSL distribution then stopped and the probe exited without reaching its steady hold or cancellation measurements. A subsequent check found Debian stopped; after starting it for diagnosis, kernel uptime was 12.35 seconds, versus 278.12 seconds at the last sample. A later reproduction and host diagnostics identified kernel panics coincident with Windows system-commit exhaustion, as recorded below. Separate host Docker/containerd stop/start activity was also observed during fixture calibration. The interrupted ramp is not treated as a measured Linux resource ceiling. All owned orphaned temporary roots from the interrupted fixtures were removed after checking ownership and absence of live runtime processes.

At that point, the 100-sandbox target, steady-state CPU overhead, removal latency under peak density and Linux resource ceiling remained unmeasured. Windows commit exhaustion interrupted the ramp before these could be established. The controlled post-reboot continuation below resolves the target-density and removal observations; it does not seek the destructive host ceiling. The small earlier mechanism pilot separately observed a 512 MiB touch/release move pool memory from 92.64 to 605.40 and back to 92.09 MiB. Its aggressive `memory.high` configuration later caused an in-sandbox observer request to exceed eight seconds; that pilot did not finish its pressure/teardown acceptance.

### Windows commit boundary found during the resumed measurement

The resumed attempt again reached 40 live 100 MiB working sets, with 4825.39 MiB in the workload parent, 1597 host PIDs, zero cgroup OOM kills and 9491.63 MiB of guest-host available memory. The WSL VM then terminated. Windows' automatically retained panic reports for 12:51:46 and 13:18:45 both record `Bank 0: b200000080060001`, `Processor Context Corrupt` and `Kernel panic - not syncing: Fatal local machine check`. These are direct crash artifacts, rather than an inference from container restarts.

Windows System event 2004 from `Microsoft-Windows-Resource-Exhaustion-Detector` occurred at both exact crash times. At 12:51:46, system commit was 42,551,042,048 of 42,614,681,600 bytes; at 13:18:45 it was 42,516,000,768 of the same limit. Remaining commit was approximately 61 and 94 MiB. Guest free RAM therefore did not establish host admission headroom. Host commit exhaustion is demonstrated; the precise mechanism by which this condition becomes a guest machine check is not established, and this evidence does not diagnose defective physical hardware or a gVisor density ceiling.

The host has approximately 32 GiB physical RAM, automatic pagefile management disabled, and a C: pagefile configured for 4096 to 8192 MiB. Its active commit limit changes as that file grows. No Windows paging configuration was changed during diagnosis. Microsoft's [pagefile explanation](https://learn.microsoft.com/en-us/troubleshoot/windows-client/performance/introduction-to-the-page-file) distinguishes available physical memory from system commit. A reporter in [WSL issue 13258](https://github.com/microsoft/WSL/issues/13258) reported recovery from the same machine-check signature after restoring system-managed paging; that report is supporting context, not proof that a particular setting fixes this host.

The private driver now samples Windows committed bytes, commit limit and available physical memory alongside cgroup usage. New sandboxes stop when Windows commit headroom falls below 2 GiB or physical availability below 2 GiB; the hold drains early below 1 GiB commit headroom. Missing or stale host observations stop the probe rather than being treated as capacity. Calibration exposed a cross-filesystem status-file rename gap and freshness failures. The reader now retries missing or incomplete samples for a bounded interval and reads the sample timestamp rather than cached file metadata. Isolated checks accept a fresh sample despite old file metadata and reject stale or missing samples. Before the reboot, live calibration had not established a reliable monitor: one attempt held two sandboxes for 20 seconds before an observer failure; later attempts stopped before payload admission on freshness checks. These are probe failures, not successful density acceptance. All owned daemons stopped during cleanup; one attempt also observed a changed start time for the existing Runtime container, so unchanged-application acceptance is not claimed for that attempt. The cause of that restart is unestablished.

The maintainer deferred the reviewed pagefile proposal to control variables, rebooted Windows, and granted an exclusive test window. No paging settings were applied. The continuation uses the same workload and runtime, with the original C: pagefile configured for 4096 to 8192 MiB.

### Post-reboot 100-sandbox result

Windows booted at 17:12:47 on 2026-10-21 (Asia/Taipei). Before WSL startup, available physical memory was 21,442 MiB and system commit was 14,908,243,968 of 38,319,714,304 bytes. After WSL and the private fixture started, the first density sample retained 16.58 GiB Windows commit headroom. Debian still had eight virtual CPUs and approximately 16 GiB RAM; the image, runsc platform, child limits and 12 GiB workload parent were unchanged.

A no-load reproduction found why the prior Windows sampler stopped: PowerShell converted the null backup-path argument of `File.Replace` to an empty path, causing the second write to fail. An isolated test reproduced that error and verified an explicitly typed null. The actual writer and reader then passed eight cross-boundary observations with seven distinct timestamps and a maximum sample age of 1152 ms. The subsequent density run used this repaired observation channel; the host headroom thresholds were unchanged.

| Live sandboxes | Payload memory, MiB | Aggregate cgroup memory, MiB | Host cgroup tasks (`pids.current`) | WSL available memory, MiB | Windows commit headroom, GiB |
| --- | --- | --- | --- | --- | --- |
| 40 | 4000 | 4827.41 | 1602 | 9506.05 | 11.82 |
| 60 | 6000 | 7242.73 | 2414 | 7017.32 | 9.29 |
| 80 | 8000 | 9653.42 | 3247 | 4515.19 | 6.89 |
| 100 | 10000 | 12071.77 | 4075 | 2100.11 | 5.39 |

All 100 sandboxes then held for 60.57 seconds. At the last hold sample, all canaries were valid, every guest had completed at least 31 touch/scan cycles, workload memory was 12,078.97 MiB (11.80 GiB), `pids.current` was 4174, Windows commit headroom was 5.24 GiB and WSL available memory was 2093.02 MiB. The parent recorded no OOM, memory-limit hit or memory-pressure stall. Average CPU charged to that parent during the hold was 0.119 cores. This deterministic memory workload does not measure CPU-intensive agent throughput; daemon/controller costs outside the workload parent remain excluded.

The touched payload totaled 10,000 MiB (9.77 GiB); the observed excess was 2078.97 MiB, or approximately 20.79 MiB per sandbox. Each sandbox still had an 8 GiB/four-CPU ceiling, whose sums were 800 GiB/400 CPUs. This demonstrates shared physical use beneath independent hard ceilings. It does not promise that every child can reach its ceiling simultaneously, or enable shared admission in the hosted ledger, which still uses fully reserved claims.

Five exact containers were forcibly removed while 100, 99, 98, 97 and 96 remained before each removal. Docker removal returned in 520.81, 531.06, 529.39, 532.85 and 537.70 ms; exact IDs were absent, and the other 95 guests retained valid, fresh canaries. These are host deletion observations, not public AgentRun cancellation or database settlement latency. The probe reached its configured target without hitting a headroom guard, so 100 is a demonstrated density point rather than the measured maximum. The WSL boot identity remained unchanged, all owned daemons and temporary resources were removed, and the existing application's IDs, images and start times were unchanged.

## Resident-capacity boundary and automatic queue progression

The post-reboot continuation then exercised the same frozen physical build (`91c7e4280787940c99439d25900c2bdc51b1f2e5`) through authenticated public API admission/cancellation, the actual Worker, Runtime, PostgreSQL, native supervisor and runsc. API/Worker executable sources are unchanged between that revision and the current candidate. The fixture changed only its private global resident-memory budget from the example's 32 GiB to 8 GiB. Workspace resident memory remained 8 GiB; child limits remained 8 GiB/four CPUs/512 PIDs, and execution capacity remained 8/4/8. This intentionally exercises a logical resident-admission boundary with physical headroom, rather than driving Windows into exhaustion. These queue trials use the two-process lifecycle workload from the physical cancellation fixture, not the 100 MiB density payload; they are a separate scheduling experiment, not a direct density or speed comparison.

Each of three trials started a physical perRun workload A in one Workspace. Its two guest processes ignored SIGTERM and its holding charged the complete 8 GiB budget. Two new Sessions B and C were admitted in a different, empty Workspace, isolating the global memory budget from the first Workspace's limit. Both yielded with `execution_resident_capacity_wait` and reported no container or resident holding. The observer maintained this state for at least three seconds after observing both capacity waits; total observation windows were 5098.81, 5121.74 and 5215.76 ms. Exactly one resident holding existed throughout that interval.

C was cancelled while A remained live. Every C ended with one committed pre-admission cancellation, no Core execution records, one complete cleanup receipt, one controller-release receipt and a succeeded lifecycle Job. A's container, instance, full holding and two guest processes remained intact; B stayed waiting. The initial observer incorrectly required a Core terminal event for this pre-admission path and stopped despite successful completion. That calibration attempt is excluded; the final observer checks exactly one authoritative terminal source and the required cleanup/release facts.

A was then cancelled through the public endpoint, invoking actual perRun deletion and settlement. Without any manual wake or configuration change, B acquired the released capacity, created a distinct physical instance and reached the same live workload. B was subsequently cancelled and settled. All nine Runs across the three trials completed their lifecycle Jobs; all six physically executed Runs had one Core terminal, cleanup and release fact each. Every trial ended with zero resident holdings and zero managed sandbox containers.

| Trial | C cancel acknowledgement, ms | C cancel request to observed Job completion, ms | A cancel request to holding release observed, ms | A cancel request to B physical workload observed, ms |
| --- | --- | --- | --- | --- |
| 1 | 26.86 | 885.10 | 1309.50 | 29971.97 |
| 2 | 26.42 | 895.52 | 1993.10 | 30080.45 |
| 3 | 30.79 | 921.93 | 2138.38 | 29647.99 |

These intervals include sequential polling and preparation; they are not scheduler-only latency or percentile estimates. Nevertheless, the long interval after an early holding release exposes a concrete utilization gap. Worker maps `execution_resident_capacity_wait` to a yield whose deadline is the current time plus `AGENT_RUN_LIFECYCLE_RECHECK_MS`, default 30,000 ms. Ordinary claim requires `run_at_ms` to be due. The physical cleanup transaction removes the proven holding and commits its receipt, but does not advance those future waiter deadlines. A readiness notification alone cannot override that predicate. This code path explains a timer-induced delay; the experiment did not independently time every preparation stage.

The next repair should durably connect eligible resident-capacity waiters to capacity-release progress, with bounded selection and ordinary revalidation. A release or wake does not grant execution authority, reduce an existing promise, create a second owner or erase Unknown responsibility. Keep periodic recheck as recovery when notifications are lost. A focused PostgreSQL regression should demonstrate a waiter scheduled in the future becoming eligible after the matching durable release; the same physical experiment should then measure release-to-next-workload again with unchanged 8/4/8 settings. This repair is not implemented by the measurement.

All owned fixture containers, daemons and temporary resources were removed. The existing application's IDs, image IDs and start times were unchanged. This experiment does not validate retained Environment retirement: a retained Run ending can legitimately keep its holding and therefore cannot be treated as a capacity release.

A later health-check invocation observed a different WSL boot identity after the fixtures had exited. No new retained kernel-panic report or Windows event 2004 existed since the Windows reboot. The cause of that later VM lifecycle transition was not established; continuous development-stack uptime beyond the measurement windows is not certified by these tests.

## Capacity release scheduling contract

The capacity-wake candidate adds Runtime schema 16 with an explicit forward migration from 15. Before attempting resident admission, the current execute owner durably registers the resource-release generation it observed. A transaction which actually deletes a resident holding advances a singleton generation and notifies `runtime_job_ready_v1`. The notification carries no permission: readiness and ordinary claim compare durable generations, then retain the existing global/Workspace execution limits, lease fencing, reclaim embargo and control exclusion. The waiting Job remains the same Job.

The release transaction does constant-size signaling work and does not scan or lock waiting Jobs. A release preceding yield remains visible afterward; missed notifications and Runtime restart do not erase eligibility. A new claim consumes the registration and records the release/claim timestamps; a non-capacity yield clears it. A subsequent failed admission registers the latest generation and waits for another release or the existing periodic fallback. An Unknown physical outcome does not remove its holding and supplies no release signal. Protected retained environments ending a Run keep their holding and supply no release signal either.

PostgreSQL [LISTEN](https://www.postgresql.org/docs/current/sql-listen.html) requires committing the subscription before inspecting current state; the existing listener follows this order. [NOTIFY](https://www.postgresql.org/docs/current/sql-notify.html) is delivered after commit and serves as a hint to inspect durable state. The generation and holding update share one transaction, so a rollback exposes neither a capacity change nor a committed signal. Old queued Jobs without a registered attempt retain their existing fallback deadline until a new attempt records one. Deploy matching schema-aware Runtime binaries; this migration does not establish mixed-version rolling compatibility.

## Lifecycle measurement method

Matklad's [Benchmark In Milliseconds](https://matklad.github.io/2026/10/05/benchmark-milliseconds.html) recommends adjusting a microbenchmark's input size until one run takes roughly 300 ms, supporting quick repeated comparisons and intuition. That duration is a benchmark-method choice, not a resource-release, scheduler or end-to-end latency objective. Product timings retain their actual duration.

The repaired physical comparison keeps execution capacity at 8/4/8, sandbox ceilings at 8 GiB/four CPUs/512 PIDs, and the queue fixture's 8 GiB resident boundary. Measure release recorded to next claim recorded separately from cancel request to the next actual workload. Also record admission through final lifecycle Job completion for a fixed local-model tool workload, with existing Session events, Runtime startup profiles, cleanup/release facts and a live private Docker event stream. Client totals use a monotonic clock; recorded facts use the common WSL wall clock and are not exact database commit acknowledgements. Polling adds observation delay. Three repetitions establish directional evidence, not latency percentiles or a production SLO. Cached-image new instances and same-instance retained reuse are distinct conditions; this experiment does not claim cold remote image download or page-cache eviction.

## Capacity wake and full-call observations

The repaired Runtime was built from `70d98b0d144e8bd16ef8b65ebbff26e7bdc472db`. To control variables, the supervisor, static execution helper and sandbox image remained at the previously measured `91c7e4280787940c99439d25900c2bdc51b1f2e5` baseline; their protocol did not change. The Runtime image was `sha256:5500bb5272c6efda20e8c02f2a1ebb34a97f5b1b7e0b409d716ec13cb154e359`, with Runtime binary SHA-256 `979a661c4adcfc34ca89b5e9a37d1067752439e665cefc77986f750aa7a2ac04`. Supervisor and helper SHA-256 were respectively `1b5d6da26ce8e116a91ff9041e7aca9f703f87c4e06e6eb605390754691b1e29` and `1010e74091070431900913fe5ce1b6a013f294f76e793d405f4d81c4d532961d`. API and Worker production sources were unchanged from that baseline. Execution remained 8/4/8; no Windows pagefile setting changed.

The same three A/B/C capacity trials passed with a private 8 GiB global resident budget. C cancellation preserved A and left B waiting. Removing A enabled B without manual intervention; all nine lifecycle Jobs settled, and every trial ended with zero holdings and managed containers. Each physically executed Run had exactly one terminal, cleanup and release fact. The new claim event recorded these intervals:

| Trial | Release recorded to B claim recorded, ms | C cancellation to Job completion observed, ms | A cancellation flow invocation to B workload observed, ms |
| --- | --- | --- | --- |
| 1 | 29 | 971.99 | 9923.72 |
| 2 | 26 | 2101.89 | 9795.27 |
| 3 | 22 | 950.66 | 10547.52 |

The durable eligibility change removes the timer-induced wait in these trials. The final column improves on the baseline's 29.6-30.1 seconds, but includes sequential pre-cancel verification, database/container observations and guest-file/task queries. It is not release-to-start latency. It must not be replaced by the 22-29 ms claim interval either. A subsequent diagnostic uses Docker event timestamps and the workload's own leader/child file modification times to separate physical progress from observation overhead.

One additional instrumented trial retained the same frozen binaries and limits. Its release-record-to-claim interval was 87 ms. Docker's event stream recorded container creation 338.78 ms after release and start at 565.55 ms. The first model-request record was at 3400 ms, the first tool-call record at 6169 ms, and both workload leader/child marker files existed by their own filesystem timestamps at 6682.95 ms. The sequential observer reported 9660.42 ms from cancellation-flow invocation to discovering the live workload. Thus observation overhead matters, but the remaining delay also includes real time after container start. All three Runs settled, holdings/managed containers returned to zero, and the existing application was unchanged. This diagnostic is one additional sample, not a new percentile estimate; all four observed claim intervals were 22/26/29/87 ms.

A separate six-call experiment used the same deterministic local model and a single successful `printf` tool before normal completion. The global resident budget returned to the example's 32 GiB; Workspace resident capacity stayed 8 GiB. Each perRun call used a new Session; the retained calls shared one Session and one verified physical container. Images were cached. The first retained call created that container, and the next two reused it.

| Call | Admission HTTP, ms | First successful tool recorded, ms | Run terminal recorded, ms | Lifecycle complete recorded, ms | Lifecycle complete observed, ms |
| --- | --- | --- | --- | --- | --- |
| New perRun 1 | 106.86 | 7550.06 | 11421.06 | 13776.06 | 13792.00 |
| New perRun 2 | 58.60 | 6770.39 | 10560.39 | 12926.39 | 13177.83 |
| New perRun 3 | 50.64 | 6621.44 | 10560.44 | 12914.44 | 13200.90 |
| First retained | 56.49 | 4005.75 | 5265.75 | 6338.75 | 6784.02 |
| Same-instance retained 1 | 40.32 | 3714.59 | 4875.59 | 5956.59 | 6298.63 |
| Same-instance retained 2 | 37.12 | 3757.03 | 4937.03 | 6039.03 | 6248.23 |

All columns after admission are cumulative from request initiation, not additive phase durations. Recorded facts use the shared WSL wall clock; the last column is a client monotonic total ending at observation. Each Run had one terminal and complete cleanup/release receipt pair. PerRun calls removed their exact container and holding. Retained calls kept the same container and holding; their completion is Run completion, not Environment retirement.

The three perRun startup profiles reported sandbox ensure of 1550/1593/1500 ms and total profiled startup of 1600/1642/1543 ms. First retained ensure was 1509 ms; same-instance ensure was still 1252/1249 ms, with startup totals 1324/1348 ms. New containers' Docker create-to-start intervals were approximately 187-211 ms. Even same-instance calls therefore spend time in preparation beyond container creation.

PerRun's first model-request record preceded its first tool-call record by 3404/2737/2718 ms; the corresponding retained gaps were 94/97/109 ms. PerRun terminal-to-cleanup-record gaps were 2300/2318/2307 ms, versus 1027/1036/1056 ms for retained. These are measured regions, not proof of which function dominates them. Trace the pre-dispatch/checkpoint and terminal cleanup boundaries before choosing the next optimization. The experiment has not established that remote image paging or a different isolation runtime would remove these intervals.

These small samples provide optimization intuition, not percentiles or a production latency commitment. They exclude external model/provider latency, cold image downloads, CPU-heavy agent throughput and storage-quota acceptance. Fixture cleanup of the retained container does not validate the product's Environment retirement contract. Both experiments removed their owned daemons/resources and verified the existing application's container IDs, image IDs and start times were unchanged.

### Sandbox preparation timing observations

A paired WSL experiment used Runtime and host supervisor source `d0f23f68501c6105fcc01756782651d5e4d69e71`, first with `WORKSPACE_PERF_OBSERVATIONS=0`, then with `1`. Each private stack ran three new perRun Sessions, one new retained Session and two subsequent calls in that retained Session. The local model, cached images, workload and 8/4/8 execution limits were unchanged. All twelve calls completed with exactly one terminal, cleanup and release fact each. PerRun removed its container and holding; retained kept its verified instance until fixture cleanup. Both private daemons were stopped and the existing application's container identities, image identities and start times were unchanged.

Both passes used Runtime image `sha256:118711071fb08b9171c5649c320d7e38d26a0c845857717de1cf776186a3a9dc`. Runtime binary SHA-256 was `e21815deafec172b24188f69306498358c25baae5608f0158c690df9567bc294`; supervisor SHA-256 was `054260874ad1f9e37160f5570e09591d5dc2748d536a6550944805811b8b8098`. The static helper and sandbox image stayed at the earlier `91c7e4280787940c99439d25900c2bdc51b1f2e5` baseline, with the helper hash recorded above. The supervisor directly invoked `/usr/bin/runsc` with its private runtime root. The fixture's Python wrapper was confined to Docker's OCI runtime invocation and was not on the supervisor's state/query/helper command path.

Runtime observations now expose the sandbox-preparation boundary, controller takeover, workspace preparation helper, input inventory and helper start/confirmation/output-copy spans. Existing numeric timing records gain `startedAtMs` for alignment. The supervisor's opt-in records contain fixed phase labels, numeric wall-clock start and monotonic duration, outcome and process ID; they contain no command, path, request body or error text. Disabled observations emit nothing. Unavailable diagnostic output does not change the operation result. No authorization, locking, resource claim, lifetime default or polling interval changed.

The enabled pass produced these preparation durations, in milliseconds. Columns after total are successive outer steps; the final column is the total less those measured steps, including admission, database work and surrounding checks.

| Call | Total | Container ensure | Controller takeover | Workspace preparation helper | Input inventory | Other preparation |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| New perRun 1 | 1546.70 | 344.21 | 188.39 | 414.99 | 530.46 | 68.64 |
| New perRun 2 | 1508.82 | 309.31 | 173.64 | 419.28 | 540.14 | 66.46 |
| New perRun 3 | 1657.99 | 303.89 | 315.21 | 412.92 | 552.57 | 73.39 |
| First retained | 1566.46 | 338.52 | 179.32 | 437.71 | 539.03 | 71.89 |
| Same-instance retained 1 | 1282.29 | 1.76 | 264.12 | 424.81 | 532.59 | 59.01 |
| Same-instance retained 2 | 1291.63 | 1.94 | 263.15 | 419.11 | 549.65 | 57.78 |

With observations disabled, new perRun preparation was 1554/1536/1507 ms, first retained 1521 ms and retained reuse 1258/1315 ms. Enabled and disabled passes remain in the same approximate range; this small sequential comparison does not establish zero instrumentation overhead or a percentile bound. The enabled perRun outlier coincided with the larger takeover span. The experiment has not identified the exclusive cause of that individual outlier.

The supervisor recorded the following activity inside the two same-instance preparation windows. Counts exclude Runtime's own Docker API calls. These are nested or potentially concurrent spans: their sums must not be added together as an exclusive wall-clock breakdown.

| Supervisor activity | Count in each reuse | Summed duration, reuse 1 / reuse 2, ms |
| --- | ---: | ---: |
| Target attestation | 12 | 447.36 / 423.84 |
| Docker inspect subprocess | 12 | 245.70 / 245.25 |
| runsc state subprocess | 26 | 269.70 / 269.27 |
| OCI identity verification | 14 | 158.88 / 158.23 |
| runsc task-domain read-control | 7 | 72.46 / 72.36 |
| Observed global-command-lock acquisition | 36 | 62.60 / 55.59 |
| Two helper launch spans | 2 | 112.89 / 112.95 |
| Helper ready handshake, inside launch | 2 | 50.89 / 50.92 |

Container reuse therefore reduces container ensure to about 2 ms, while workspace preparation and input inventory still account for approximately 958/969 ms of the 1282/1292 ms preparation window. The launch spans are not pure gVisor startup measurements: they include identity verification and waiting. Likewise, workspace-helper duration includes transport and completion confirmation, not just guest directory work. These observations support reducing repeated host attestations and small-query round trips before attributing the whole delay to runsc. The existing child-command observer polls every 10 ms; inspect/state durations cluster near 20/10 ms, so the measurements include that observation granularity. The next change should preserve bounded command completion, immutable target identity, current controller/lease checks and physical cleanup evidence.

Validation: the numeric-output contract first failed when enabled observations emitted no record, then passed. The final three Linux diagnostic tests cover opt-out, result/error preservation, bounded output fields and unavailable diagnostic output. The broader Linux hosted suite passed 136 tests with 20 opt-in cases excluded before the diagnostic-output guard; the final guard was checked by the focused tests. Native Runtime passed 468 tests with 211 opt-in cases excluded, including its seven existing observation tests. The Rust source gate, 129 Core query-loop tests, native warning-denied Runtime/hosted Clippy, formatting and whitespace checks passed. The minimal Linux builder lacked Runtime test fixtures and the Clippy component, so its attempted Runtime test/Clippy commands are not acceptance results; native checks and the actual private WSL calls are reported separately. No PostgreSQL schema or scheduling logic changed in this timing work. This does not extend storage-quota, Environment retirement, target Debian 13 or retained-default acceptance.

### Target-bound host requests

Runtime and supervisor source `5193163fe8fab60051ecee6a82b2e94cea14d93a` put the exact immutable launch target in each task/helper request. The supervisor resolves and attests that target in the same request, removing Runtime's separate registration round trip. Each subsequent request still performs fresh attestation; the manager pin, retirement barrier, current controller checks and physical launch/output confirmations remain. Missing targets and cross-instance envelopes are rejected. Runtime and supervisor must be upgraded together; old task/helper bodies are rejected. Persisted registration and task-journal formats are unchanged.

The first measured candidate used image `sha256:d26a20e41482d514e94457a9e0481a3990dd129968fc45d7d681997cb45ac719`, Runtime SHA-256 `d1e578e9e562a8d153681fdbc865b2643e08274da443dabab6742550ea9a7a18` and supervisor SHA-256 `a69c08cdf97ed69597cdfa911f846e1f2d3e4e67127bea4e39d54a5e9c26d997`. The helper, sandbox image, local model and 8/4/8 limits were unchanged from the preceding timing comparison. All six calls had exactly one terminal, cleanup and release fact, with perRun removal and retained instance continuity. The subsequent native acceptance attempt stopped at its missing `strip` test dependency; that initial attempt is not counted as a native acceptance pass.

The paired observations below are individual calls, in milliseconds. The final column is the durable Job-completion timestamp relative to admission request start; client observation adds its own sampling delay.

| Call | Baseline preparation | Candidate preparation | Baseline first tool | Candidate first tool | Baseline Job complete | Candidate Job complete |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| New perRun 1 | 1546.70 | 1461.08 | 7566.30 | 7214.81 | 13934.30 | 13574.81 |
| New perRun 2 | 1508.82 | 1493.27 | 6850.79 | 6690.02 | 13289.79 | 12722.02 |
| New perRun 3 | 1657.99 | 1546.66 | 6936.91 | 6675.95 | 13337.91 | 12768.95 |
| First retained | 1566.46 | 1438.93 | 4157.13 | 3926.48 | 6486.13 | 6247.48 |
| Same-instance retained 1 | 1282.29 | 1277.27 | 3844.25 | 3714.79 | 6164.25 | 5954.79 |
| Same-instance retained 2 | 1291.63 | 1246.81 | 3868.32 | 3715.63 | 6180.32 | 8301.63 |

Inside retained preparation, Docker-inspect and attestation counts fell from 12 to 9, and runsc-state queries from 26 to 23. Removing five front registration requests did not eliminate five attestations overall: task requests increased from three to five. Both preparation helpers use completion queries, so earlier entry into those loops can consume some of the saved round trips. The traces establish the changed counts; they do not establish an exclusive causal duration for each additional poll. Helper completion/output confirmation remains a candidate for a separately tested optimization.

Preparation improved modestly in this sample; retained reuse remained about 1.25 seconds. The last retained call had a slower interval after its first successful tool and must remain in the result. Its unchanged first-tool timing and preparation do not support blaming the preparation change for the whole outlier, nor do they prove the change unrelated. These small sequential samples establish fewer observed queries and functional continuity, not a percentile improvement or a startup SLA.

The final isolated run passed all 23 native host checks against the same supervisor binary, using acceptance script revision `326e0a1e696f74fc6150c0b2348f231e18713df7`. It covered target-bound takeover without registration, forged container/profile/image/Environment/instance targets, current-controller fencing, typed helper input/output binding, large binary I/O, replay, scope preservation and supervisor restart with outstanding cleanup. The fixture used static BusyBox from `busybox:1.37.0-musl`, image manifest `sha256:5cec3fc171c87218698e85a52af7087de727372aae264a787b8112901a5b0092`, executable SHA-256 `2c92c1289f3ab36f8b43fb68d3b56b0ec32290f2d6f6bf4ef44df6bd7edc02ad`. A real strip executable and its libraries came from the pinned Rust builder through the explicit `--strip` test option. Earlier missing-tool and dynamic-BusyBox fixture failures were resolved before counting acceptance; no assertion was skipped.

That run also exercised one full resident-capacity queue trial with observations disabled. Global resident memory was 8 GiB, equal to one sandbox reservation; execution limits remained 8/4/8. Both queued Runs waited without allocations for a stable observed full window of 5336 ms. Cancelling one queued Run preserved the incumbent's two live guest processes: cancellation HTTP took 28.42 ms and complete was observed at 965.53 ms after the cancel request. After the incumbent released its resources, the remaining Run was claimed 20 ms after durable release, its container was created at 312.72 ms and started at 495.74 ms; the fixed long-running probe reached its workload file marker at 6232.40 ms. Claim, container start and effective workload remain distinct measurements. Both physical Run cancellations settled, and final holdings and managed containers were zero. This was resident-budget saturation, not simultaneous saturation of all execution, Worker, HTTP and DB capacity.

Local validation passed the red/green target-required and one-request transport regressions, 15 native protocol tests, six native Runtime transport tests, the Rust source gate including 129 Core query-loop tests, native warning-denied hosted/Runtime Clippy, and Runtime's 469-test native ordinary suite. Linux passed the full 138-test hosted ordinary suite, the focused 28-test host-supervisor suite and Runtime's 470-test ordinary suite. Their 20 hosted and 211 Runtime opt-in cases were excluded from those ordinary counts. The Linux Clippy component download timed out at its 45-second bound, so Linux Clippy is not reported as passed. Formatting, Python syntax/CLI and whitespace checks passed. All private fixtures stopped their owned daemons, removed owned resources and verified the existing application's container IDs, image IDs and start times were unchanged. No storage-quota, Environment retirement, target Debian host or default-retained acceptance is added by this request optimization.

### Validation of the capacity wake candidate

The focused baseline first failed the future-deadline and release-before-yield regressions while preserving the Unknown-removal guard. The final complete PostgreSQL gate passed all 178 discovered identities, including all seven new capacity-wake cases and the required manifest. Coverage includes release/yield races, reopen without relying on notifications, unknown and rolled-back removal, full execution/Workspace capacity, reclaim embargo, conversion to question waiting, schema-15-to-16 preservation and drift rejection. Legacy scheduling/terminal fixtures now supply the canonical lifecycle binding and required settlement facts; production completion guards were not weakened. Terminal append still exercises all three business terminal kinds, stale-owner rejection, transaction rollback, waiter consumption, terminal outbox publication, acknowledgement and reopen.

The Rust source gate passed, including 129 Core query-loop tests and the full local-product Rust suite. Runtime's ordinary suite passed 468 tests with 211 opt-in tests excluded from that ordinary result; the PostgreSQL gate above executes its selected opt-in cases separately. Warning-denied Runtime Clippy, formatting, whitespace and all 11 PostgreSQL gate-driver checks passed. These results establish this repair and its measured WSL paths, not the remaining storage, no-Run creation, retirement, target-host or default-retained acceptance requirements.

## Verified control boundary

The repaired source passed 115 individually selected tests on fresh owned PostgreSQL databases: all 113 required identities plus both retained Unknown-create reservation/adoption protections. Each identity executed exactly once. These tests cover ordinary/control readiness parity and exclusion, independent bounded stages, unchanged owners, current-purpose host and execution rejection, no-effects reply replay, missing/foreign receipt rejection, prior-history cancellation, cleanup-required completion, purpose immutability, reclaim exhaustion and schema-14-to-15 preservation/drift rejection.

The final Runtime unit suite passed 468 tests; 204 opt-in tests were ignored by that ordinary run and are not counted as passed there. Core `query_loop` passed 129 tests. Worker discovery executed all 101 tests with no skips, including full ordinary-slot occupancy, terminal handoff and cleanup failure retry. The affected API PostgreSQL suite passed 45 tests. All 37 rendered Compose contract tests, 11 PostgreSQL gate-driver tests, warning-denied Runtime Clippy, formatting and whitespace checks passed. This is focused repair validation plus the service-chain experiment above, not completion of the full hosted release or physical-host acceptance gate.

## Reclamation completion conditions

- Preserve one authoritative lifecycle and one current lease owner. A control lease cannot run the model, allocate a sandbox or invoke ordinary execution.
- Keep cancellation intent, authoritative Run terminal state, physical cleanup and resource release as distinct durable facts. Measure their delays separately.
- Persist creation attempts and settlement responsibility before physical side effects. Settle each allocation at most once with explicit physical evidence and fencing.
- Recover pending work after notification loss, worker/runtime restart, lost replies and dependency outages. Slow physical cleanup must not exhaust short control handling capacity.
- Apply Run-scoped cleanup without deleting a retained Environment, another owner's tasks or files that its policy preserves.
- Exercise both AgentRun-owned creation and provision without a Run, plus stale owner/epoch/instance responses and PostgreSQL forward migrations.
- Verify environment retirement and host-private retirement independently, with precise identity, confirmed physical absence and durable receipts.

## Default lifetime change

The maintainer-approved target is `retained` for ordinary Sessions after reclamation acceptance passes. Keep execution limits at eight/four/eight. Do not change previously signed grants or silently reinterpret existing persistent data. Document and test the forward upgrade policy before switching the Worker default and API-visible lifetime consistently.

The switch is gated on the following evidence:

- Full API/Worker/Runtime/PostgreSQL/host cancellation, cleanup, release and completion while execution capacity remains occupied.
- Definitive creation failure settles; unknown and historical uncertain outcomes keep their budget and responsibility until evidence permits release.
- Retained reuse preserves the expected files and Environment tasks; explicit retirement reclaims the exact instance and its resource vector.
- Real workspace preparation and physical storage limits match the public contract.
- The same repaired build passes the target Linux acceptance. WSL adapter probes and database fixtures are reported separately from that result.

## Initial validation

The first isolated PostgreSQL run after restoring the test environment executed eight named tests: five passed and three failed at the intended assertions. The four pre-existing cancellation/capacity/unknown-create baselines passed. The three failures are the two saturated-capacity cancellation cases and the pre-create reservation settlement case above. The additional string-failure characterization passed and preserves conservative accounting.

The focused Core query-loop gate passed all 129 selected tests. Rust test compilation, request export, formatting and whitespace checks passed. That initial candidate changed neither production behavior nor lifetime defaults; the settlement implementation and its later validation are recorded below. Full terminal/control-lane behavior, historical migration, actual workspace quota enforcement and target-host acceptance remain open.

## Verified settlement boundary

The implemented undispatched-allocation boundary passed 108 named tests against owned disposable PostgreSQL databases: all 106 required identities plus the two existing unknown-create reservation/adoption protections. Each selected identity executed exactly once in its database. Coverage includes settlement/release rollback, stale-controller rejection, resident reuse, both lifetime policies, cleanup replay after reopening, later-Run creation and the v13-to-v14 historical-unknown migration.

The final Runtime unit suite passed 467 tests with 199 opt-in tests ignored. Core `query_loop` passed 129 tests. Warning-denied Runtime Clippy, formatting, whitespace, all 11 gate-driver unit tests and discovery of the 106 required identities in the final binary passed. Local HTTP fixtures required a process-local loopback proxy exclusion; no application network configuration changed.

At that settlement-only revision, the two saturated-capacity cancellation regressions still failed at `ready=false, claimed=0` while the original four/eight execution owners remained running. The later control implementation supersedes those results; its evidence is recorded separately. No-Run attempt unification, physical storage enforcement, target Linux physical acceptance and the default lifetime transition remain open.
