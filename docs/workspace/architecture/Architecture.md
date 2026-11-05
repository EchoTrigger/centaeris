# Workspace architecture

## Ownership

The external Runtime Framework owns Session, model, tool, continuation, and
runtime-event semantics. Workspace composes that Runtime with hosted identity,
authorization, storage, execution, and browser delivery. It does not redefine
Core behavior.

| Component | Responsibility |
| --- | --- |
| `packages/api` | Django identity, membership, workspace ACL, files, credentials, Plugin installation, AgentRun authorization, and durable product jobs |
| `packages/runtime_server` | Core composition, PostgreSQL RuntimeStore adapter, Redis live projection, and Docker execution binding |
| `packages/worker` | Bounded claim, lease, retry, and wake loop for product jobs |
| `packages/hosted_execution` | Fixed helper included in the AgentRun execution image |
| `packages/document_processor` | Office, PDF, and image inspection plus bounded canonical representations |
| `packages/web` | Browser product consuming REST and SSE |
| `skills/system` | First-party built-in behavior, separate from installable Plugin inventory |

Runtime HTTP uses axum and Hyper; explicit routes call concrete operations through
one shared admission/body reader and blocking-task boundary. The
[Runtime HTTP inventory](../reference/RuntimeHTTP.md) records endpoint ownership,
framework-reuse opportunities and the behavior protected during transport
refactors. HTTP framework changes leave Django's product ownership and Core's
runtime semantics at their existing boundaries.

Django Runtime clients share `app_core/runtime_http.py` for JSON request
construction, token headers and response reading/closure. Each operation retains
its timeout, HTTP error interpretation and response binding checks in
`runtime_client.py` or `runtime_job_client.py`. The transport does not retry calls
or translate uncertain writes into successful outcomes. Worker keeps its own
dependency-free transport and durable scheduling policy.

Transcript response structure comes from the generated Rust JSON schemas.
Python checks that structure before applying identity bindings, ordered waterlines,
nonblank identifiers, exact integer representations and UTF-8 byte budgets.
JSON Schema's `integer` also accepts integral floats, and a `uint32` format label
alone does not enforce its upper bound; those transport guards remain explicit.
Avoid maintaining a second field/type registry in Python after schema validation.

Product response serializers retain computed role/access fields, failure
redaction and explicit timestamp formatting.
Replacing these with ORM attribute serialization requires equal wire output and
query counts, including the current `+00:00` timestamp spelling. Framework use
should remove maintenance work rather than move it into additional resolvers.

Workspace owns private Memory behavior and storage. Public Core receives generic
execution file operations and mutation facts; it does not interpret the private
memory namespace.

This is a private Markdown directory protocol, not a background model maintaining
memory. The active model uses ordinary read/edit/write tools through
`plastic-memories://self/`, with MEMORY.md as an index and topics/*.md as detail.
Storage is scoped by user and Workspace, across that user's Sessions in the
Workspace. The existing filesystem lock and guarded write coordinate concurrent execution instances of that scope.
The helper consumes `WriteFile.observedFileHash` as its expected current version;
ordinary workspace files do not enforce this observation. The internal protocol
rename requires coordinated Core and Workspace updates, with no old-field alias.

## Workspace and Session identity

Session is the user-owned work object within a Workspace. Its transcript,
files, environment binding, active Run and durable operation receipts retain
the Session identity. The browser opens ordinary conversations at
`/w/{workspaceId}/sessions/{sessionId}`; `/w/{workspaceId}/app` starts a new
conversation. Workspace layout owns the browser title and favicon: the title
uses the Workspace name and the icon uses its avatar, or its name's first
character when no avatar is set, across Session and settings navigation.
Initial HTML restores a matching Workspace identity from tab-scoped session
storage before application loading; authenticated Workspace data remains
authoritative. Logout clears this presentation cache.

Each message submission explicitly selects a currently available
Workspace model through `modelConfigRef` and an optional `thinkingMode`.
The browser can retain the user's model preference, but does not choose the
first catalog entry as a default.

The former Agent configuration, definition, private message, coordination,
dispatch/return and business-branch application surfaces are retired. Agent
configuration and Agent-only history are not promoted into Session configuration
or ordinary Session history. Ordinary Sessions keep their own transcript and
file identities. The technical AgentRun name continues to identify one Core
execution in a Session; it is not a separate user-facing Agent object.

The old application grants and `/api/v1/chats` transport are also retired.
Existing browser Session endpoints remain governed by user ownership and
Workspace membership. This change does not supply a replacement delegated
Session API, external-user identity mapping, embedded browser entry point, or
implicit access to an entire Library. See [business application access](../reference/BusinessAPI.md)
for the retained registration boundary.

## Request flow

1. Django authenticates the user and checks workspace membership and resource
   access.
2. The API commits the caller's operation receipt with the Session/AgentRun and
   immutable AgentRun authorization containing the exact workspace, model,
   execution profile, files, and Plugin activation. A currently authorized
   retry returns that acceptance identity without creating another Run.
3. The worker claims the durable job and asks Runtime Server to start or resume
   the AgentRun.
4. Runtime Server validates the authorization and composes Core with the
   PostgreSQL store, model adapter, Plugin resources, and one execution binding.
5. Core drives model and tool continuation. Hosted execution and document
   processing remain adapters behind current contracts.
6. Durable events are committed to PostgreSQL. Redis carries bounded live state
   for connected browsers. The API exposes one ordered logical stream.

The command receiver, connected browser, execution owner, and ExecutionHost have
different responsibilities. A receipt says that a command was accepted; it is
not the current Run status or a transcript projection watermark. Django owns
hosted admission and receipt persistence; Core continues to own execution and
terminal runtime semantics. Browser recovery queries the receipt before
resubmitting the same command identity after an uncertain response.

## Worker concurrency

Worker dispatches each claimed Job independently. Runtime Job leases establish
ownership and fencing, not a fixed concurrency quota. Advance and cleanup have
independent dispatchers so long model calls do not occupy their execution threads.
Terminal publication and reconciliation remain separate control loops.

Model request concurrency and rate limits belong to the configured upstream
gateway or provider. Docker environments retain their explicit CPU, memory and
workspace budgets and the host's physical resource admission.

The example deployment gives each environment 2 CPU cores and 4 GiB of memory.
Existing signed environment profiles retain their original resource declarations.
For comparisons, hold the source revision, workload, replica count, physical
resource budgets and historical-data baseline fixed. Measure completion,
waiting, failures and host/database resource use together.

## Durable and live truth

PostgreSQL stores durable product and Runtime facts. Django application tables
and the Runtime schema have separate owners. Runtime schema v1 rejects unknown
or drifting identities.

Redis is not a job broker or history store. Live overlay generations are
discarded when a corresponding durable event establishes a supersession
barrier. Redis expiry or cleanup failure must not create or delete durable
history. The browser consumes API projections and never reads Redis directly.

## New user turn admission

The web transcript keeps its last readable snapshot when live updates fail.
Optional tool-detail projection cannot block canonical transcript patches.
Stream recovery reads a validated tail and active-run cursor before replacing
the view, then reconnects with bounded backoff; it never replays tool execution.
After repeated failures, only the current conversation offers a reconnect action.
Errors retain their original cause in developer diagnostics rather than becoming
a generic page-wide failure. Invalid transport identities still fail validation.

A new user turn is admitted only after Core has closed any unpaired tool call at
the tail of the session history. Hosts persist the accepted input (prompt,
attachments, identities), read the execution evidence, and ask Core for a
read-only closure plan; the plan carries the expected session head and the
evidence each closure was computed against.

The committing host writes the recovery facts and the new run's first batch in
one transaction, under the current lifecycle lease, with per-row attribution:

- `tool_call_closure` is a session-level recovery record (`session_level = true`,
  `agent_run_sequence = NULL`, not projected to the per-run stream). It
  references the original call's owning AgentRun without reopening it.
- ordinary records stay `session_level = false` with a positive run sequence.

A failure, cancellation, or interruption before admission writes nothing to the
model history; it stays in the AgentRun control plane. Re-delivery of an already
committed admission returns the stored receipt. A plan whose expected head no
longer matches an uncommitted batch is rejected.

Release order for a storage or transcript block-encoding change: schema first,
then readers, then the closure writer. A rollback must not hand a store that
contains closures to a reader that does not understand them.

## Files and processing

PostgreSQL stores file identities, ownership, grants, lifecycle, and processing
state. Original bytes and derived representations live in the configured
storage root. A database row is not a second copy of file contents.

Office, PDF, and image processing is lazy and version-bound. Long documents are
processed incrementally without a fixed page-count ceiling while retaining
pixel, output-size, timeout, and memory limits.

Historical transcript output must be bound to committed bytes. Current tool
spill events record a mutable workspace path and byte range, without a content
hash or retained snapshot identity. The API therefore reports their complete
output as unavailable instead of reading the Session's current snapshot. This
also applies before a later run: another tool can overwrite a path in the same
execution. Complete inline output remains readable; the committed event and its
preview are retained without rewriting history.
A future full-output capture must establish immutable identity at capture time,
with explicit retention and authorization; copying a file on first historical
read cannot reconstruct that evidence. Material evidence receipts remain a
separate existing source of immutable read/search results.

## Execution

Each AgentRun receives one frozen execution profile and temporary container.
Runtime resolves the configured image to an immutable Docker identity before a
run is authorized. The configured OCI runtime, mounts, work directory, process,
memory, CPU, PID, and network policy are Host facts. Core sees only the
`ExecutionHost` contract.

Runtime controls Docker through the host socket and is therefore a privileged
infrastructure component even when individual AgentRun containers drop
capabilities.

## Deployment trust boundary

Only Runtime and the dedicated material Worker mount the Docker socket.
That socket makes both services part of the host's trusted infrastructure; container
mount separation does not isolate other container secrets from a compromised
Docker controller. The deployment contract gate protects against accidentally
adding socket access to an API, lifecycle Worker or another service.

API owns credential storage, authorization decisions and credential release.
This does not mean it is the only process holding secrets: the shared API
environment also supplies the signing/encryption keys and database credentials
to api-init, gc, mail-sender and the material Worker. Runtime shares the HMAC authorization key and
receives authorized MCP bearer tokens for HTTP connections. Worker receives the
internal API token, not the HMAC key through Compose. Narrowing those inherited
credentials is a separate design and test task, not a guarantee of the current
layout.

The API service drops all Linux capabilities and enables no-new-privileges.
It still runs as the image's default user and retains access to its mounted data
and configured credentials. This limits process privileges; it does not defend
all data against API compromise. The production override removes the API host
port and exposes the Web service on loopback for a reverse proxy.

The Docker gate verifies actual process capabilities, fresh-volume startup,
upload storage and Plugin lifecycle writes, then replaces the API container and
checks persistence. Only Runtime inspection of a synthetic Plugin is mocked in
that probe; filesystem operations, catalog validation and database locking run
normally. Runtime `main()` already resolves the general image with
the Docker Engine image-inspect API before binding its listener. The material
Worker separately inspects its processor image and runs an isolated specification
check before claiming platform tasks.
Compose starts Runtime directly; it has no shell/CLI image preflight. Direct Runtime startup with
a missing image fails before listening; this does not depend on the Compose
entrypoint. No duplicate entrypoint check is needed.

## Plugins

Superusers install a validated package directory through a bounded ZIP carrier.
Installation, workspace enablement, credential resolution, and AgentRun
activation are separate steps. A run freezes exact package identities and
digests; package changes do not mutate a running request.

Plugin Skills, CLI paths, MCP tools, and Hooks reuse Core's existing composition
and execution paths. They cannot own a second Agent loop or bypass workspace
authorization. An empty installed catalog remains a valid startup state.

## Indexed waiting relationships

Core derives one `RuntimeJobWaiter` per tool-call wait from the validated
checkpoint contract. PostgreSQL and SQLite persist these rows in the same
transaction as the checkpoint. A cascading checkpoint foreign key removes them
on consumption or session deletion. No hosted adapter reinterprets model output
to reconstruct waiting semantics.

The source-job index bounds notification lookup to related waiters. The primary
key `(checkpoint_id, tool_call_id)` supports global and within-checkpoint
continuation without loading checkpoint payloads. Both request paths use bounded
pages and yield cursors; pending delivery is acknowledged only after its final
page. Reconciliation is proportional to current waiting relationships, not
historical completed notifications. A full pass can span multiple control ticks.

The waiter tables originate in runtime store schema v1. Schema v2 adds the
transcript read model, and v3 adds an immutable wait handoff attachment through
explicit migrations. Unknown versions or table shapes fail validation.

## Recovery from a committed runtime Job wait

After Core returns a runtime Job wait and Runtime commits its accepted call and
result records, the live owned execution may publish a recovery boundary before
returning `waiting`. Publication requires an empty in-flight call set, a known
stable workspace generation and a fresh snapshot witness from that host. A
content-identical workspace object may be reused; an earlier host witness may not.

Runtime stores Core's complete returned Session snapshot and exact wait checkpoint
as an immutable attachment to a new recovery checkpoint. Its `waitcp:` identity
hashes that attachment, and the canonical checkpoint reference hashes the recovery
payload. The boundary retains the real originating model request identity without
issuing another model request. Attachment, recovery checkpoint and Session
reference commit together under the original lifecycle lease and Session lock.
The transaction checks the source position, current Core wait state and accepted
call/result and source Job bindings. A replay returns the same committed boundary.
Core permits its derived terminal-child result projection to evolve after sealing,
after checking the referenced durable Jobs' ownership, kind, terminal status and
result references. All other Session state and the wait checkpoint remain exact
replay invariants. Hosts do not interpret Core's private metadata.

After owned execution loss, the existing recovery reservation and checkpoint-used
guards still apply. Restoration loads the bound immutable Core snapshot, including
its opaque pending batch, and Core's existing wait resolver consumes the completed
source evidence. Core reconstructs the derived child-result projection from durable
Jobs and retains verified cached summaries. Recovery and projection writers use
atomic compare-and-save; a concurrent projection write is merged, while unrelated
state advancement during recovery retry is rejected. Ordinary turn snapshot writes
still use their existing store operation; the projection remains a rebuildable
cache, and wait completion reads durable Jobs and scoped result objects.
Restoration does not reconstruct the pending batch from tool receipts or substitute
a later mutable snapshot for it. A missing or altered attachment fails closed. Old recovery
checkpoints retain their existing behavior; migration cannot infer a missing wait
boundary for an already lost execution.

An active Execution does not select a recovery checkpoint. Checkpoint collection
occurs at runtime safe points, not on a timer. A process-local gate shared by
runners for the same container tracks helpers, commands and hooks for their full
dispatch lifetime. Recovery collection skips a busy gate; new dispatch waits
while collection owns it. An idle gate alone is insufficient: a non-destructive
process inventory also defers recovery collection while agent-owned background
processes remain. Normal recovery collection does not terminate those processes.
This gate does not establish exclusion between separate Runtime replicas; the
existing execution ownership and lease fences remain necessary.

Final workspace publication returns Pending when foreground host activity is
still active, preserving the prepared completion for a later attempt. Final
publication and explicit restoration retain their own cleanup semantics; a
normal recovery snapshot must not be treated as terminal cleanup. Collection
validates one complete frame while spooling its exact bytes to a private anonymous
temporary file, bounded by the sandbox workspace capacity plus manifest overhead.
Recovery upload reads this immutable artifact after releasing the activity gate
and overlaps the next model request. Before the next tool call, completion, or
model request, Runtime joins any outstanding upload and publishes a separate
checkpoint reference only when the captured host/activity evidence remains valid.
The reference uses its actual committed Session sequence and precedes the next
tool call; an uploaded object alone never advances the recovery frontier.
Final workspace publication still retains exclusion through its synchronous
commit. Each execution retains at most one pending recovery artifact; temporary
files close on success, failure and teardown. There is no aggregate spool byte
budget or hard maximum pause yet. Slow uploads may still delay the next tool at
the publication fence. Opt-in observations distinguish capture, upload, gate hold,
dispatch wait and the recovery upload fence.

Terminal hosted teardown cancels remaining native children bound to
that exact parent AgentRun across its turns before removing its context and
sandbox. Cleanup failure leaves the parent terminal fact intact and returns a
retryable dependency failure so the worker retries teardown without rerunning the
model. Already-completed historical teardown Jobs need separate reconciliation.

## Safe replacement of a lost execution


At the next real model-request safe point, Runtime may replace a lost sandbox
only when the latest in-process checkpoint is followed by exactly one closed
bash call and a committed unsuccessful, non-executed receipt. Host evidence must
prove failure before process dispatch, owned missing/stopped container state,
and a quiesced workspace snapshot whose activity epoch has not changed. Open or
parallel calls, external tools, successful receipts, semantic facts, unknown
generation, identity mismatch, and uncertain dispatch prevent replacement.

User commands, hooks, snapshot restoration, and mutating helpers invalidate the
host witness before dispatch. MCP command builders conservatively disable this
automatic recovery path because their later process spawn is not owned by the
snapshot boundary. Witnesses are never transferred between host instances.

The advanced checkpoint covers the already committed failed receipt. Its
reference and the old Execution's `lost` end record commit in one transaction
under the lifecycle lease. Runtime then stops before sending the next model
request and yields to the worker's durable retry deadline. A replacement loads
the advanced model state; it never dispatches the recorded call again.
Preparation attempts are reserved durably and share the configured five-attempt
budget across restarts. Uncertain outcomes remain failures, not replay requests.

The supported worker dispatches one step per claimed attempt. Recovery returns
before the worker yields that lease; yield clears its owner, and the next claim
gets a new owner even on the same worker. A lost step response schedules a job
retry rather than resending the step with the old owner. Thus replacement in
this flow crosses a lease boundary while retaining the Run and frozen workspace
generation. Session workspace publication checks that lease before and after
upload, and Session terminal/checkpoint appends check it inside their transaction.
Execution workspace staging alone does not publish a checkpoint or advance the
Session workspace.

This guarantee depends on the supported dispatch flow. The internal step endpoint
does not independently enforce one invocation per lease. A custom caller,
automatically retrying proxy, or future recovery within one lease requires a new
admission/fencing review; the current contract does not certify those flows.

## Hosted execution capacity

Hosted admission is owned by the API; Workspace is its tenant boundary. A
transaction-level PostgreSQL admission lock protects counts and insertion across
API replicas. Only initial queued rows enter the partial-index count. Runtime
stores immutable job-to-tenant bindings alongside scheduled lifecycle jobs, and
serializes capacity-check plus lease acquisition across replicas. Generic Core
job-store semantics are unchanged. Notifications remain hints: a listener checks
both due time and capacity, and periodic reconciliation repairs expired leases.

Runtime HTTP handlers have separate ordinary, listener and control semaphores.
The cancellation handler receives a store view backed entirely by the shared
control connection pool; no preflight read borrows ordinary capacity. Absolute
response deadlines propagate to connection checkout/connect and statement waits.
A timed-out blocking handler retains its permit until actual completion, so
timeouts cannot multiply in-flight work or imply rollback of uncertain writes.

## Docker management boundary

`runtime_server::docker_engine` owns the shared local socket transport for Engine
info, image/container inspection, filtered listing, create, start and removal.
The execution host still owns authorization, container identity, resource and
mount policy, workspace sentinels and recovery decisions. Processor batch
start/exit/output collection also uses this transport. Attached exec carries
commands, hooks, MCP stdio, filesystem helpers, snapshots and the persistent
generation RPC. The processor uses archive upload/download instead of file-copy
subprocesses. No production Runtime Docker CLI branch remains.

Creation uses a stable name within an attempt. Both successful and uncertain
responses are followed by inspection of the returned immutable ID or that same
name. Adoption requires the requested image, labels and every supplied security,
resource and mount field to match. The SDK request is checked for field loss
before transmission. An uncertain create is never blindly repeated by the
transport. Start checks state and never restarts an exited container. Removal
requires an owned immutable ID and confirms absence; only an Engine 404 counts
as absence. This transport does not retry user commands or change Runtime
checkpoint semantics.

Exec requests validate the expected run/execution labels and bind a container
ID before creating an exec ID. Argument vectors, environment, user and working
directory are sent as structured fields with TTY and privileged mode disabled.
Each exec is created/started once. Attached stdout and stderr use bounded byte
pipes; input and output advance concurrently. EOF requires a confirmed exec exit
code. A stream error or unconfirmed exit remains an unknown outcome, not success
or permission to replay. Confirmed sandbox removal wins over attach/inspect
errors when reporting cancellation. Closing an MCP/RPC attachment alone does not
claim remote cancellation; the execution host retains teardown responsibility.

MCP raw-line framing and size enforcement remain in the public MCP adapter's
generic bounded byte-stream transport. Workspace supplies Engine streams and
owns their lifecycle. Snapshot frame length, digest and generation checks are
unchanged. Processor archives accept only the declared regular output files,
reject links/traversal/duplicates, enforce byte budgets and validate transport
completion before outputs can be committed.

## Source dependency

Rust packages resolve the public Runtime through exact Git revision dependencies.
Compose builds fetch the same locked source without an adjacent checkout. Pin,
manifest, lockfile and example image-label revisions are checked together; npm
and Python remain local to this repository.

## Hosted authorization verification

The API owns current membership and resource access. Rust validates the signed
startup envelope at its own service boundary. Neither boundary trusts a digest
supplied by a caller without computing it from the strict authorization payload.
The two implementations retain shared signature vectors and rejection cases.

Within one verification, canonical payload hashing is reused for signature
verification. API consumers share the five-field run binding (run, workspace,
session, user and model configuration), while keeping endpoint errors and
resource-specific checks. The existing endpoint scopes are:

| API consumer | Digest | Signature | Five-field binding | Additional checks retained |
| --- | --- | --- | --- | --- |
| Runtime scheduling/start payload | Yes | Receiver verifies | Yes | Current membership, thinking mode |
| Deferred input | Yes | Yes | Yes | Current membership, declared input, live owner/generation/hash and blob availability |
| Material access | Yes | MCP credential wrapper verifies | Wrapper/input resolver verifies | Current membership, processing specification and representation |
| Platform MCP credential | Yes | Yes | Yes | Token scope and lifetime |
| Model proxy | Yes | No | Yes | Current membership, model/thinking mode and output budgets |
| Artifact publication | Yes | No | Existing publication/resource binding | Current membership, scope, publication identity and bytes |
| Workspace snapshot commit | Yes | Yes | Yes | Job lease, active session and compare-and-swap generation |

This consolidation does not add signature requirements to existing API endpoints
or treat their service authentication as proof of resource access. Snapshot
commit still checks the locked lease and generation before and after object
storage I/O. File-mutation event persistence and Memory coordination remain
separate responsibilities.

Workspace snapshot uploads use bounded streaming into a request-owned temporary
file on the storage filesystem. Directory and temporary-file creation occur
under the active Session lock; payload copying, manifest/hash validation and
flush/fsync occur outside the transaction. Final publication locks Session,
AgentRun and the lifecycle job in that order, then rechecks the current lease,
signed authorization and active owner. A read-only lease probe locates the
Session; it does not authorize publication. Session commit also checks its
baseline or exact replay. Execution checkpoint staging uses the same final
owner/lease fence without advancing the Session or publishing a Runtime
checkpoint.

Only fully validated bytes are linked atomically to the content-addressed key,
without overwriting an existing file. Concurrent identical uploads verify the
winner outside row locks, then recheck authority before reuse. Failed cleanup
removes only the request's temporary file, never the shared key: an expired
uploader must not delete a replacement owner's published snapshot. This uses
the same local filesystem/atomic-link requirement as immutable evidence storage.
An interrupted process can leave a temporary or staged object; that alone grants
no committed Session or recovery-checkpoint reference.

Both snapshot download routes acquire bounded stream capacity, then recheck
authorization and open the exact local file while holding its Session lock.
Streaming and closing occur outside the transaction. Purge waits for the open,
not for the full transfer. On POSIX an already opened descriptor survives unlink;
on systems that reject unlink of an open file, GC reports a retryable failure.
Cancellation during opening, unused responses and response-construction failures
close the handle and release capacity. Later requests against a deleted Session
remain unavailable; they never select another Session or replacement file.

A private Session baseline-authority marker distinguishes a new, known-empty
workspace from an old descriptor with missing evidence. Forward migration marks
only previously committed generations as known. An unknown baseline cannot
authorize a new empty instance or resolve as `empty`; neither Run counts nor
current environment defaults reconstruct missing history. Explicit Session
workspace commits, including empty commits, record authoritative baseline state.

`runtime.session_workspace.preserve.v1` accepts the existing framed snapshot
stream under a current terminal cleanup controller and lifecycle lease. Runtime
separately proves that the exact allocation is stopped and exports its readable
workspace without starting the guest. The API revalidates the original signed
grant, exact allocation and resident identity, profile, active owner and cleanup
authority before and after streaming. Its baseline must equal the cleanup Run's
signed baseline, and publication uses the ordinary Session generation CAS and
snapshot paths. It cannot rebase an old root onto a newer committed head.

The head advance and immutable preservation receipt commit in one durable
transaction. The receipt binds the Run and originating grant, exact physical
target, baseline, committed descriptor and encoding; its creator lease and epoch are audit
facts. A later valid cleanup lease can replay the same receipt. A replay after
a newer head advance returns the original proof without changing that head.
The receipt does not grant guest execution or provide an additional archive.
Physical removal must independently revalidate current cleanup authority and
the exact stopped target against the durable proof.

New nonempty Session commits, execution checkpoints and stopped-instance
preservation publish `fileManifestV1`: canonical `workspace.snapshot.v1` manifest
JSON whose file bodies are immutable, Session-scoped SHA-256 content objects.
A snapshot stores the complete file listing; unchanged bodies are shared across
snapshots, and removed paths are absent from the next manifest. Restoration
requires one manifest and its content objects rather than a chain of deltas.
Empty workspaces retain the explicit `frameV1` no-object representation.

Small content objects may share a Session-scoped immutable physical pack. A pack
contains 2–32 files of at most 16 KiB each and at most 512 KiB in total; a lone
small file or a larger file remains a loose object. File identities in manifests
stay the SHA256 of their individual bytes. The file index records its pack and
byte offset, and the API exposes the same bounded file stream to Runtime. This
reduces physical publication/fsync count without changing the public manifest,
file hashes, file transport schemas, or helper frame protocol. A transport batch
may contain multiple bounded physical packs; streaming transfer does not load
the entire batch into memory.

`snapshotSha256` and `snapshotSizeBytes` identify the exact stored manifest bytes;
`expandedSizeBytes` sums file bodies across paths, including repeated content.
The referenced unique-object bytes and PostgreSQL indexes are reported separately
when measuring storage. `workspaceBytes` limits expanded workspace data plus the
existing input and restore allowance, regardless of deduplication or compression.

The helper continues to produce and consume framed snapshot streams. Runtime
retains the validated capture source while uploading only missing file objects,
then publishes the manifest under the existing Session generation CAS. Current
Job/lease reservations protect both reused and new objects before publication.
The publication transaction records indexed manifest-to-file references without
reading or hashing those immutable file bodies again. The reservation does not
grant execution authority; every upload/publication uses its existing authority
boundary, including the distinct terminal-cleanup path.

Successful downloads return `X-Workspace-Snapshot-Encoding` as `fileManifestV1`,
`frameV1` or `tarZstdV1`. New manifest bytes retain their stored identity while
Runtime reconstructs a separate private helper frame from authorized content.
Existing complete framed or tar/zstd archives retain their original bytes,
encoding, signatures and reader paths. Tar/zstd remains a complete archive codec;
it is not the backing store for new file-level incremental snapshots.

Reachability collection expands retained manifest roots through indexed file
references and current canonical file reservations. It does not open manifests
or read file bodies to compute reachability. Root/witness and ownership checks
run once per bounded collection batch; exact publication incarnations remain
individually fenced. A recent manifest also retains its referenced content for
the grace period. Expired reservations and collected manifest edges are retired
as scheduling metadata once their responsibility ends.

For candidate packs, GC expands only their bounded member index, finds members
referenced by the actual retained roots, and maps those members back to physical
storage. Current lease reservations and grace-period logical registrations also
protect their packs. A live logical file registry row or a historical snapshot
edge does not independently root a pack. Any reachable member retains the whole
pack; it is reclaimed only when no member remains reachable, through the same
owner/witness and exact publication-token fences. The collector does not compact
partly live packs or delete their virtual file names one by one. Measurements
therefore count physical pack bytes, including any retained dead members, rather
than treating logical deduplication alone as reclaimed disk space.

`gc_deleted_resources` also collects local workspace bytes of Sessions with
`status=deleted` and a non-null `purgedAt`. Both the purge time and the file mtime
must precede its cutoff, which defaults to 30 days. It enumerates outside the
transaction, then rechecks the locked owner before unlinking each exact key.
Only canonical Session snapshot generations, indexed loose file objects and
file packs, checkpoint keys of AgentRuns owned by that Session, and this
uploader's temporary-name format are eligible. It
rejects symlinks/reparse points and reports unknown Run directories as blocked.
Unrelated names and directory entries remain. Retained active Sessions and
restorable trash use a separate reachability pass with the same cutoff grace.
Its roots are the current Session head, frozen baselines of Runtime-protected
canonical Runs, Core-selected recovery/wait checkpoint objects, and exact
preservation receipts while physical or canonical cleanup responsibility remains.
Settled historical grants and receipts do not keep every older payload alive.
An unknown legacy baseline blocks this retained-owner pass.

Runtime selects recovery roots through `runtime.workspace_snapshot.roots.v1`.
The API requests this manifest outside owner locks. Runtime and API use the same
packaged SQL witness to compare Session high-water and a SHA256 fingerprint of
authoritative relational state. The API then locks the Session, pins canonical Job ownership, checks the witness again, and recomputes current API
roots and publication ownership. A changed witness aborts that plan. The API
does not reproduce Core's checkpoint-selection policy. Historical recovery
downloads likewise require a currently selected root and a current witness.

Each upload durably registers its temporary and final keys before streaming.
A current valid canonical Job lease protects these bytes regardless of age,
including the interval before a checkpoint reference commits. Unregistered
legacy keys also retain live ownership protection. After grace and loss of
ownership, GC records a collected publication incarnation under the owner lock
before unlinking. Final publication checks its captured token, including replay
and immutable-name collisions, so a late upload cannot undo that decision.

A fresh authenticated upload with a valid signed baseline may reintroduce the
same content at the same key with a new random publication token. It rearms
cleanup retry state for that incarnation; older upload and GC tokens remain
invalid. Retry claims and completion updates compare the token, and exact unlink
rechecks it under the owner lock. Expired upload rows and absent collected
temporary-object metadata retire after grace; current references do not need to
retain every historical upload request.

The collector reads Runtime facts only for witness and liveness checks; Core owns
the recovery root selection, and GC does not modify Runtime checkpoint history.
Dry-run reports keys without deletion; missing keys are idempotent, and other
failures are reported and make the command fail for retry.

Enabling reachability collection requires the publication-registry migration,
the matching Runtime root/witness implementation, replacement of every API
writer, and draining requests served by the old upload implementation before
starting the new GC worker. Existing unregistered canonical objects acquire an
incarnation fence when selected. Remote object storage needs a separate version-aware
collector and cannot use this local-file implementation.

Input batch resolvers live for one request. They reuse only a verified snapshot
of the signed authorization facts, invalidating it when payload, digest,
signature, key or expected digest changes. Membership, run identity, source access,
version and storage existence are checked on each input, including after an
earlier item succeeded. No authorization cache survives into another tool call.

### Hosted transcript text captures

The Docker host observes successful create-only UTF-8 writes without parsing
Core's private spill-file format. A committed tool result selects the exact
path and byte range from this Execution's observed buffer. Repeated creation of
the same path is ambiguous and cannot produce a capture. Recovery into another
Execution or a process restart does not reconstruct candidates from mutable files.

After the existing fenced event commit, an optional background publisher stores
an event-owned `TranscriptOutputCapture` and 64 KiB `TranscriptOutputChunk` rows
in one PostgreSQL transaction. It checks the exact committed event payload and
the preceding execution-start fact, never overwrites another capture, and locks
the Session only for final publication. Deletion during chunk I/O causes rollback.
This follows the existing hosted result-snapshot model; it adds no Core port,
event schema, object-store dependency, or binary transcript transport.

Candidate retention and the pending publisher each use a byte budget of
`min(memoryBytes, workspaceBytes)` from the existing execution authorization.
Accounting includes retained identity/record metadata; these are operational
archive bounds, not a limit on tool success or total Session history. One
background writer drains each Execution's bounded queue. Each publication has a
five-second pool-request deadline and a five-second PostgreSQL transaction
timeout, with no automatic retries. Candidate allocation, UTF-8 checking and
copying still have CPU/memory cost on the host; asynchronous database work is
not zero-cost. The implementation has no disk spool or restart recovery queue.

Missing candidates, conflicting identities, exhausted budgets and storage
failures leave the historical full text explicitly unavailable. They do not
change Core's tool result or cause tool replay. Publication can lag the event;
an early content request may need to be retried. Captures retain the existing
per-request authorization and do not create public object URLs. Session deletion
purges chunks transactionally and retains tombstones; trash expiration repairs
cleanup of already-purged Sessions. No ordinary read performs
cleanup and no time-based eviction of accessible history is introduced.
