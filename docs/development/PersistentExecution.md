# Persistent execution and versioned working volumes

## Status and delivery contract

This document contains the broader persistent execution design. PR #36 delivers
only an internal opt-in, single-host continuity boundary: the same still-live
instance may be used by successive Runs under a verified runc, systemd and cgroup
v2 configuration. `perRun` remains the default. The original persistent working
volume design is not complete when this limited candidate passes acceptance.

Required acceptance for this PR: Run A starts an environment task and writes
files; A completes with immutable cleanup and release receipts; Runtime restarts;
Run B obtains a new fence and uses the same container, instance, task and bytes.
Stale controllers fail. Within one Run, at least 1000 sequential ordinary
commands with automatic AGENTS.md file observations must complete while the same
environment task remains live; cumulative task history must not prevent admission.
The 32 active-task and 64 hot-record budgets stay distinct from indexed replay
facts and output retention, and real resource pressure remains bounded. Across
at least 100 separate completed uses/releases, authorized maintenance stops the exact instance,
confirms physical absence and reconciles its resource holding exactly once.

The Core long-loop gate also requires committed turns to release obsolete full
Session snapshots. Accumulated Run results retain compact tool and checkpoint
identities plus one final state, preserving exact completion, wait and child
result semantics. A same-process structural regression covers 100, 1000 and
10000 commits without a restart. The real 1000-command gate keeps the original
512 MiB Runtime limit, exercises actual model-context compaction and records
process and cgroup memory separately from the execution instance. A restart's
lower memory use does not establish release of pre-restart objects.

This result-lifetime change does not complete Session hot-state separation.
The current Session may still contain full history; repeated full-snapshot
loads/saves and context-selection copies remain measurable costs. Follow-up
work must first prove lossless historical coverage and bounded reads, then
separate execution state and model context, and finally validate versioned
migration and recovery. Only that work can establish ordinary-turn cost bounded
by the current working set rather than the entire historical transcript.

Candidate components only: tar+zstd encoding, validation and bridge support.
They are not enabled for production capture/upload or cold restoration. All
retained online file protection is currently disabled, including at terminal
Run completion. `/mnt/data` remains a bounded tmpfs; files survive only while
that instance lives. Destroying it can lose every unarchived working file.

Deferred work: persistent disk working volumes, consistent online checkpoints,
production archive upload/restoration, restic, CAS/Nydus, deduplication, lazy
restore, runsc retained, Firecracker, process-memory recovery, multiple hosts and
new UI. The filesystem design and its acceptance requirements below describe
this future work; they are not additional delivery claims for this PR.

No new browser status interface, host identifiers, arbitrary host migration,
lazy restoration, environment forks, custom filesystem or general backup engine
is included. File restoration does not promise process-memory restoration.

## Authority and identity

- Core remains authoritative for Run, execution and tool-result semantics.
- The API owns Session environment identity, membership and business authorization.
- Runtime owns physical allocation, controller fencing and resource reconciliation.
- A task service in the instance owns registered processes and output independently
  of a controller connection. Transport reconnection must not restart a command.
- A working volume has a stable identity and a single permitted physical writer.
  Replacing an instance does not silently reuse an old instance identity.
- The existing resident resource ledger remains authoritative for resource usage;
  allocation history is not a second heartbeat, lease or liveness ledger.

The original signed v1 Run authorization stays unchanged. Persistent execution
requires an explicitly versioned environment binding and strict validation; it
must not reinterpret the existing perRun authorization as permission to reuse.
Protocol types, ownership of migrations and production call sites are reviewed
together before parallel implementation changes shared interfaces.

## Execution and control

One foreground Run may hold an environment at a time. Registered environment
tasks may remain active across Runs, so a single foreground owner does not imply
that files are quiescent. Run-scoped tasks end with their Run. Ordinary shell
backgrounding does not automatically establish durable task registration.

Controller takeover advances a monotonically fenced authority. Launch, signal,
input, file mutation, restore and destroy must reject obsolete control at their
actual execution boundaries. Losing control does not terminate a legitimately
started environment task. Revocation and explicit cancellation are separate
actions with their own scope and result.

Lease expiry alone does not prove a former process or writer stopped. Unknown
instance state retains its reservation and prevents creation of a second writer.
Recovery needs positive evidence of the prior instance's loss or fenced storage.
The initial supported continuity domain is a still-live instance on one host.

A Run rejected before Core admission has no Session model history. Runtime must
commit a separate `runtime.agent_run.pre_admission_failure.v1` control receipt
under the current lifecycle lease before returning terminal failure. Its stable
Run identity, original authorization digest and first failure time survive
retries. API projection and cleanup verify that receipt; a `failed` presentation
status alone grants no cleanup authority. The receipt prevents later admission
or execution, but does not prove physical absence or release resident resources.
The existing exact-instance cleanup and immutable release receipts still apply.

## Working files and protected inputs

Online reads and writes use a mature Linux filesystem and its page cache, not
per-file PostgreSQL requests. PostgreSQL holds identities, authorizations,
operations and checkpoint roots. Rust coordinates capture, restore and bounded
I/O; it does not duplicate all file contents into an application cache.

Working files, protected Library inputs and reconstructible caches have explicit
mount and retention boundaries. Protected inputs are not silently copied into
an unrestricted archived input tree. Restore rechecks current authorization and
reconstructs controlled input mounts. Existing bytes already read by a process
cannot be recalled by unlinking a path; strong revocation may require stopping
the affected execution scope.

Persistent volume capacity must be enforced by the supported host backend.
Replacing bounded tmpfs with an unbounded directory is prohibited. The backend
must report its quota and consistency prerequisites, fail when they are absent,
and account for retained volumes separately from resident memory. Host filesystem
and quota selection requires the implementation audit and isolated Linux test.

## Capture, archive and publication

Capture produces a receipt identifying the environment, instance, working volume,
source generation, consistency method and covered filesystem scopes. Read-only
mounting alone does not establish a consistent point in time. Existing active
writer checks remain until a tested replacement covers the same boundary.

The deployed snapshot representation is the uncompressed
`workspace.snapshot.v1` manifest-and-content frame. Tar plus zstd is the selected
candidate representation, not an existing production codec. Introducing it
requires explicit encoding identification and tested reads of retained v1
snapshots; changing the writer does not migrate previously published objects.

Implement bounded streaming tar plus zstd
from a supported consistent view through tar encoding and zstd compression,
avoiding an intermediate complete uncompressed archive. Compressed staging may
still be necessary for destination retries and verification and must be measured
and bounded. This remains full-content capture, not incremental capture; the
streaming path requires implementation and comparative acceptance evidence.
Reuse the online working volume across Runs and avoid unnecessary captures before
adding cross-checkpoint deduplication.

Restic measurements remain experimental evidence only. Remove the candidate
adapter, its tests and implementation-specific documentation from the active
candidate; do not wire it into the production save path.
Reconsider a deduplicating repository only with representative retained-version
workloads showing benefits after capture, restore, cache, reclamation and
operational costs. Existing synthetic byte savings alone do not select a backend.
Do not build a custom chunk store or backup engine as an alternative.

Product code owns checkpoint authority and retention roots. Archive credentials
are configuration references, never command-line secrets, tool-visible fields or
captured output. Storage scope must preserve tenant isolation and all I/O must
have explicit resource bounds.

Publication follows these steps:

1. Commit a stable operation identity and expected environment/control generation.
2. Acquire a supported consistent view, archive and verify outside a long database
   transaction. Apply explicit concurrency and temporary-space bounds.
3. In a short transaction, recheck authority and expected generation, then publish
   the verified archive reference. Never publish missing or partial content.
4. Preserve uncertain operations for reconciliation. Retry the original operation
   instead of pretending the database and external archive commit atomically.

Failed publication may leave an unreferenced archive snapshot. Its creation is
tracked until reconciliation or safe reclamation; it is not a visible checkpoint.

## Restore and reclamation

Restore pins a fixed checkpoint, checks current authorization/configuration and
materializes a fresh bounded working volume. Only after validation may the
controller publish the new usable binding. An interrupted restore must not
overwrite the last usable volume or release its protection prematurely.

The first implementation restores eagerly. Validate the exact supported file,
directory, link and metadata contract; reject unsupported entries rather than
silently changing them. Process memory and open-descriptor restoration are outside
this filesystem receipt.

Retention roots include current and retained checkpoints and operations actively
capturing, publishing or restoring. A grace period alone is insufficient for GC.
Product root changes and archive-object deletion must coordinate so a
newly published or pinned checkpoint cannot be reclaimed. Start with dry-run,
bounded work and durable cleanup receipts. Do not delete allocation history merely
because resident resources were released.

## Task service

The minimum service surface is start, query/list, cursor-based output, input and
terminate. Task identity is stable; retries compare the complete request identity.
A crash between spawn and durable acknowledgement is an uncertain result requiring
reconciliation, not permission to spawn again.

Output is drained independently of browsers and controllers, retained under a
bounded policy and served with explicit cursor gaps. Logs are not unbounded rows
in the control database. Termination covers the registered task process scope,
including descendants; PID reuse cannot redirect a signal to another task.

Control files (authority, task identity and operation receipts) and task output
must use separate storage locations and independently enforced budgets. Output
rotation, exhaustion or cleanup must not consume the control-file reservation or
delete control records. Output retention is not control-state durability. This
separation is an acceptance requirement, not a claim about the current candidate.

The task supervisor's Linux tests cover kill-only cgroup v2 domains, including
descendants that start a new process session. Task cgroups must remain below the
charged Docker instance cgroup; aggregate memory, CPU and PID limits belong to
that instance. A standalone delegated test subtree does not establish this
deployment property. Retained admission must verify the actual hierarchy and
limits before accepting work, and teardown must confirm the subtree is empty
before releasing its resident reservation.

## Acceptance and measurement

Behavioral tests precede changes and record red/green evidence. Required boundaries
include stale control rejection, concurrent starts, lost acknowledgements,
controller restart, task output backpressure, descendant termination, bounded
tmpfs, safe task/receipt retirement, maintenance destruction, resource
reconciliation and forward migration of retained authoritative facts. File
capture/restore failures, revoked-input restoration and publication/GC races
remain acceptance requirements for the deferred filesystem delivery.

Real Linux acceptance must exercise the production API/Worker/Runtime/backend
chain. Fixture-only adapter tests do not establish isolation or process survival.
Run focused Core query_loop and all affected release gates. Do not restart the
previously stopped full local API suite; remote CI handles full API discovery.

Compare identical source state, metadata scope, consistency, retention and restore
oracles. Report scanned entries, application reads, temporary bytes, stored bytes,
network bytes, CPU, peak memory, capture time and restore time separately. Mark
unmeasured device I/O explicitly. Include cold/warm runs, source trees, incompressible
and compressed files, many small files and sparse files. Existing synthetic results
motivate implementation but are not production performance guarantees.

## Integration ownership

The primary reviewer owns protocol/schema integration, shared Runtime entry points,
Worker teardown semantics, deployment configuration, cross-component gates and
publication. Isolated implementation work covers storage/archive, lifecycle/control
and task supervision. Shared files require an explicit ownership handoff. A module
is not complete until its real production caller and failure recovery are exercised.
