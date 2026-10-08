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

Workspace owns Agent Memory behavior and storage. Public Core receives generic
execution file operations and mutation facts; it does not interpret the private
memory namespace.

This is a private Markdown directory protocol, not a background model maintaining
memory. The active model uses ordinary read/edit/write tools through
`plastic-memories://self/`, with MEMORY.md as an index and topics/*.md as detail.
Storage is scoped by user and Agent, across Sessions. The existing filesystem
lock and guarded write coordinate concurrent execution instances of that scope.
The helper consumes `WriteFile.observedFileHash` as its expected current version;
ordinary workspace files do not enforce this observation. The internal protocol
rename requires coordinated Core and Workspace updates, with no old-field alias.

## Private Agent messages

The API binds one private, user-owned Agent instance to one newly created
coordination Session through `AgentCoordinationSession`. This is independent of
shared Agent definitions. Authenticated creation locks the existing Agent and
membership boundary, commits the new Session and unique binding together, and
returns the same binding on retry. It cannot select or convert an existing work
Session. Coordination Sessions are excluded from the ordinary workspace Session
list, including its Agent filter; ordinary transcript, browser rendering, live
stream and Final behavior remain unchanged.

Only Runs in that bound Session receive the hosted `coordinationSessionId`,
which Rust requires to equal the signed authorization's Session identity.
Runtime then registers the first-party `workspace.agent_messages` provider and
its `send_message` contract. Ordinary Runs do not receive this tool. The model
supplies a complete body and opaque resource associations, with no recipient
field. The recipient is always the executing private Agent's own history.
Empty Plugin activation works; no extension or credential inheritance is needed.

Each dispatch calls the API with the trusted Run, authorization digest and call
identity. The API verifies the signed binding, original current membership,
active user/Agent/Session, managed definition availability and current delegated
authority when present. Session associations use existing current read policies.
File associations must be SessionAssetLink inputs in this Run's signed asset
authorization and pass the existing current source ACL, generation, captured
identity and storage checks. Owning a Library file or adding an attachment after
Run acceptance does not add it to that authorization. Associations grant no new
access; later resource reads still require their existing current policies.

Successful validation authorizes only that in-flight message call. Revocation
committed after validation does not retrospectively retract that call's
authorization; it may finish persistence. Validation does not bypass cancellation,
the current lifecycle lease fence or storage failures. A new call after revocation
must pass current authorization again and is rejected when authority is absent.
Every history read independently checks current source authority. Reading an
associated Session or file separately checks that resource's current authority;
retained messages, opaque refs and old Run authorization confer no continuing
access. The history may retain an association whose resource is no longer readable.

Validation performs no message write. The provider's validation response and
Core's internal receipt are not delivery facts. Delivery takes effect only when
the existing fenced Session append commits the successful tool result. The API
rebuilds messages from active committed SessionEvents: the result and preceding
call must have the same Session, Run, turn and call identity, with the first-party
provider and exact contract digest. The full body and refs come from the call's
normalized input, never a summary or validation response. The committed result's
stable event ID identifies the message; lost-ack replay and reopened storage do
not duplicate it. Failed, rolled-back, stale-fence, foreign or tombstoned records
do not produce messages. `ContinueTurn` preserves the call/result pair for later
model requests. The Run still ends with its existing single Final, which does
not create another Agent message.

The first-release initial schema includes the unique coordination relation.
Initialization creates no Agents, Sessions, Runs, messages or grants. Recovery
must retain matching bindings and histories; it must not infer bindings from
titles or fabricate message facts.
There is no second message fact table, Inbox, wake mechanism or Agent UI here.

## Private work admission from committed requests

The API-only work materializer consumes an explicitly identified, active committed
`dispatch_work` call/success pair in a bound coordination Session. Its first-party
provider identity and exact consumer contract digest must match. The source Run,
turn and tool call identify one business request, independently of duplicate
successful events or the later admission receipt. A provider cannot materialize
work in its source append transaction. The native coordination Runtime registers
the tool contract and notifies the materializer after committed success. The
existing worker source scan repairs missed admission notifications. These paths
admit child work; coordinator wake remains outside this boundary.

Current original membership, signed Run identity and coordination authority are
checked again at admission. This consumer supports native private Agents only;
managed definitions and app-delegated sources are rejected. It creates one fresh
ordinary work Session and queued Run for the same user, Workspace and private
Agent. Session, Run, new authorization, independent hosted acceptance receipt and
`AgentWorkSession` origin relation commit together. The objective remains complete.
Only explicitly selected, currently authorized Library/SourceObject inputs get
new links; parent input sets, workspace snapshots and credentials are not copied.
The new input retains the validated source generation and digest; changes after
validation cannot upgrade its signed identity and still fail current input reads.
Artifact inputs are rejected because existing access binds them to their source
Session. Session refs remain source associations, confer no grant and are not
inserted into the child's model context by this consumer.

Replay checks current source and accepted work resource authority, then returns
the original receipt even after child execution failure or model unavailability.
It does not start another Run or repeat scheduling. A lost relation index can be
rebuilt from the original committed source and receipt, preserving the first
successful source event identity. Missing/tombstoned source facts do not authorize
reconstruction; an unavailable accepted work resource is not replaced. The
receipt digests bind the original success identity so a later duplicate cannot
replace an erased origin. New admissions retain current model selection rules;
receipt replay does not repeat model availability or queue admission.
Existing lifecycle scheduling begins only after admission commit; its existing pending
reconciliation handles scheduling failure. Accepted does not mean processed.
Different source call identities may admit separate work Sessions for one Agent.
No new consumption retry, progress policy, stop cascade or cross-Session waiter
behavior is introduced.

The first-release initial schema includes this relation and index. Initialization
infers no work origin from text. Recovery must retain matching source history,
relations and receipts; do not replace accepted resources or infer origins from
Session titles.

## Delivered private work returns

The API additionally owns immutable delivered work-return facts in
`AgentWorkReturn`, keyed to the retained original work binding and child Run.
Canonical Core terminal records, Runtime's validated pre-admission cancellation
receipt and lifecycle operational failures remain distinct sources. Job success
does not classify work success. A lifecycle fault notice survives a later genuine
Session terminal, which has its own identity. Delivery retains original signed
identities as audit facts; read-only query rechecks current coordination and work
authority, including the original membership references. Rejoining cannot revive
those references. Child output is an untrusted Session reference, never an elevated
instruction. Source rewrite does not reverse an already committed admission.

The terminal projection's best-effort delivery attempt follows its transaction.
An independent worker discovers read-only, frozen pages of retained work bindings
without Runtime HTTP, then attempts individual deliveries within a soft budget.
Errors and lost responses advance attempted sources; the unattempted tail keeps
the same upper bound, and later passes retry earlier failures. This repairs the
terminal-commit-to-notice gap even after Job outbox publication. The
existing lifecycle lease, Core ledger and Session active-Run lock retain execution
ownership. The delivery scan neither admits coordinator Runs nor consumes notices.
First automatic admission is a separate notice-ledger pass in the same worker
control thread, with its own budget and cursor. Handling still requires explicit
committed confirmation; ordinary Final, ACK and admission are insufficient. Future
Agent Read requires actual loop input access and its timestamp, distinct from handling.

The first-release initial schema includes the delivery relation. Initialization
creates no delivered facts. Recovery must retain matching work, return facts,
signed authorizations and accepted receipts.

Explicit first consumption uses one immutable `AgentWorkConsumeAttempt` per notice,
with one-to-one links to its coordinator Run and existing hosted operation receipt.
Its input binding records the accepted Core native input and Run/Turn/Session/
authorization identities. The API holds the ordinary Session admission lock,
rechecks native ownership and original membership/current read authority, and
commits attempt, Run, authorization and receipt atomically. A busy coordinator
spends no first opportunity. Profile/input creation and lifecycle scheduling run
outside the acceptance transaction. Existing record-reference lifecycle jobs and
reconciliation retain scheduling ownership; no second scheduler or signing
scheme is introduced. Runtime checks the persisted binding before writing a
canonical `host_event_input`, and replay preserves Core's input origin. A later
notice admits only itself and does not carry an older failed attempt.

The first-release initial schema includes this relation and permits
`consumeWorkReturn` in the receipt result constraint. Every accepted attempt has
a non-null delivery sequence; the queue retains its explicit initial host owner.
Initialization infers no attempts or input ownership.

Automatic first admission scans committed `AgentWorkReturn` rows without a first
attempt. A pass freezes its upper notice ID, advances each attempted row even on
busy, denied or lost responses, and retains the unattempted tail for the next
round. A later full pass or restart revisits notices still lacking an attempt.
The API derives the stable operation ID; the worker forwards it to the existing
consumer, which rechecks authority and coordinator busy state under its original
locks. One coordination Session per Agent retains serial admission, while child
work Sessions retain independent execution. A persisted attempt excludes its
notice regardless of queued, running, failed, cancelled or completed Run state.
New notices do not retry old attempts, and scheduling failures use the same Run's
existing lifecycle reconciliation. There is no new table, queue, job kind or
handling state. Explicit retries, Read and handled confirmation remain unimplemented;
this boundary certifies first admission, not the complete handling lifecycle.

## Hosted Agent input preparation

`AgentInput` is one immutable API-owned fact carrier shared by pending delivery
and retained input history. Each scoped stable ID retains exact body text,
server ordering/time, the Session ledger waterline at acceptance and the original
membership identity. The API does not add
delivery, Read or handled tables. Existing Runtime supplement batches remain
the delivery mechanism to adapt after Core freezes its input uptake contract.
Runtime queue acknowledgement cannot erase the only retained body or establish
Read; Read must be derived from actual successful main model-request uptake.

The prepared acceptance/history endpoints do not yet dispatch to Runtime. Their
wire `read` remains null. Idle, busy and Final-race handling must eventually use
the same serialized Agent coordination chain. Inputs arriving while an already
generated `send_message` is being committed remain eligible for the next legal
safe point; they do not invalidate that generated output or create a parallel
coordinator. Ordinary text cannot release special waits.

Active work-return input additionally requires multiple immutable attempts to
refer to one coordinator Run. The current OneToOne relation and initial-input
rehydration must change together: selecting any attempt with `.first()` cannot
determine whether a HostEvent was initial or taken up while the Run was active.
The FK migration must preserve every notice, authorization, receipt and binding
byte. It must not be applied before that identity boundary is implemented and
tested. Handled confirmation remains
deferred until those tests establish the correct current input ownership.

The unified history is a projection over this carrier and verified Session
output facts, read in one SQL snapshot. Input acceptance and Runtime event
append both lock the same Session row; the saved waterline and input sequence
give an immutable order between output commits. The cursor carries the full
position and Session scope. Display timestamps and the two independent source
sequences cannot replace this ordering. No synthetic Core fact or duplicate
history table is introduced.

The owner chooses one existing ModelConfig and effective thinking mode in Agent
settings. Default/new Agents have no implicit selection. The setting governs
future coordinator Runs through their existing authorization snapshots; active
Runs retain their original selection even if settings change or are cleared.
Idle native work-return admission snapshots this policy after revalidating the
source proof and again after external profile resolution. Source and child
authorization/membership evidence remains independent. Accepted coordinator
rehydration verifies its own signed model/effort snapshot and does not compare it
with a later Agent setting. Generic messages cannot target the dedicated
coordination binding; checks under the Workspace creation/admission lock prevent
a binding race from opening another model or coordinator path. Ordinary work
Sessions retain their existing model behavior. This prepared wiring is untested;
ordinary typed and active native input still await the Core adapter.

## Shared Agent definitions

The API owns Workspace-scoped `AgentDefinition` drafts and immutable published
`AgentDefinitionVersion` records. Workspace owners and admins manage draft name,
description, instructions, avatar and `pluginNames`, publish complete versions, and select
availability `none`, `workspace` or `members`. The default is `none`; only active,
published, currently available definitions can create instances or new Runs.
Member grants reference current WorkspaceMembership identities, so leaving and
rejoining does not restore a previous grant.

Each user retains a private Agent, Sessions and Memory. A nullable, immutable
Agent definition reference and a unique `(workspace, owner, definition)` binding
provide one personal instance per definition. First use resolves this instance
transactionally. Sharing a definition grants administrators no access to another
user's Agent, Session, Run, files, transcript or Memory. Existing private Agents
retain null definition references and their current behavior.

Each new managed Run rechecks definition availability and resolves the current
published version in its acceptance transaction. It stores that version foreign
key together with the existing `agent_instructions` snapshot. A new publication
affects subsequent Runs, including those in an existing Session, without changing
the Session or Agent identity. Accepted Runs and receipt retries retain their
original version and instructions. Disabling a definition or removing availability
blocks new acceptance and subsequent managed MCP dispatch. It does not cancel an
accepted Run or reinterpret its history; ordinary ownership and membership checks
continue to apply.

Draft `pluginNames` select complete packages from Workspace-enabled Plugins. A
published version freezes the exact `pluginActivation`, including Skills, CLI,
MCP and Hook contributions and their digests. New Runs require that frozen
activation to remain available in the Workspace. Runtime uses it for actual
Skill sources, plugin mounts, CLI paths, tools and Hooks, not just tool visibility.
An empty selection remains valid; first-party built-in tools and authorized
materials remain available. Neither publication nor migration grants access to
global MCP credentials.

`McpBearerCredential` remains the encrypted credential store.
`AgentConnectorCredentialApproval` references an existing record and source
version; it does not copy the secret. The source record's `created_by` identifies
its custodian in the current data model. An approving superuser must also be that
creator. Each approval fixes Workspace, definition, plugin, server, declaration
resource path and resource digest. The file digest also binds the declared
endpoint URL. `AgentConnectorBinding` selects an approval for that exact scope.
Workspace administrators see only scoped approved references and cannot enumerate
or bind arbitrary global secrets. Different definitions may bind different
credentials for the same plugin. Missing, revoked or mismatched bindings never
fall back to another definition or the global store.

Source rotation invalidates approvals of the old source version and cached
connections. Using the new version requires a new explicit approval and binding.
Approval revocation is permanent. Approval foreign keys protect source records
from deletion; the management API returns a controlled 409 for a protected source,
including one referenced by a revoked approval. A shared service account gives
approved users the configured business capability; it does not assert their
individual downstream ACLs.

Every external MCP provider, including bearer HTTP, unauthenticated HTTP and
stdio, has a Runtime guard. Before each call the API checks current membership
identity, Run/Session/Agent state and ownership, managed definition availability,
Workspace plugin enablement, frozen resource identity, current approval, binding
and source version. Lazy connect fixes a binding fingerprint before receiving a
token. The connector returns a guarded real provider, so every dispatch checks
the fingerprint and current authority after initialization and connection-queue
waiting, including calls using the cached provider. Changed authority,
fingerprints or API failures reject the call
without executing or reconnecting the inner provider. Ordinary configuration
stays frozen while security revocation remains current. Delegated Runs also require
their current user-app grant at this dispatch boundary. SSE checks are separate
from provider invocation; neither check cancels all accepted execution.
The authorization decision precedes the actual provider invocation; it cannot
atomically fence a remote effect against revocation committed after that decision
or withdraw an external call already dispatched.

The private credential path requires persistent `Agent.definition_id = null` and
`AgentRun.definition_version_id = null`. A managed Agent missing its version is
rejected. The old bearer resolver is restricted to that private state; the new
adapter always uses strict connector authorization. Models and external tool
arguments cannot choose secret references. Public responses expose neither downstream
tokens nor internal authorization signatures. The management contracts support a scoped approval
picker and binding controls; a new real-secret management UI is not implemented.

## Delegated business applications

The API registers `BusinessApplication` records and user-owned `UserAppDelegation`
grants. Registration is platform-admin-only and defaults to pending. The existing
Settings layout lets a signed-in user explicitly select an active application,
Workspace, available definition or their own native Agent, operation scopes and
either a finite expiry or a grant without automatic expiry, then revoke the
grant later. The acting identity remains that user, with the application recorded
as the actor; applications cannot supply a different user identity or manage
definitions, Plugins, connector approvals or credentials.

A grant issues a random opaque bearer token once, with a no-store response. Only
its SHA-256 digest is persisted. The server fixes issuer and audience, validates
expiry and revocation, and intersects the scopes with the user's current membership
identity, target availability and resource ownership. Leaving and rejoining a
Workspace does not revive an old grant. Browser cookies keep their existing CSRF
checks; requests combining cookie identity and an Authorization header fail.

Definition-delegated requests use the existing Session, upload, transcript, citation,
artifact-download and cancellation routes. A delegated Run and hosted operation
receipt retain their server-assigned application and grant origin. Browser and
application submissions share the user-owned operation identity and digest, so
retries recover the same receipt and changed input still returns 409. Acceptance
and revocation serialize under the Workspace lock; current grants are checked
again before acceptance commits. Migration leaves old Run and receipt origins
null and does not invent applications, grants or credentials.

An open Session stream checks current user, membership, definition, ownership and,
when delegated, grant authority before each item and every five seconds while
waiting for an item. Authority checks have a five-second timeout and fail closed;
the stream ends within fifteen seconds of committed revocation. Its existing
`session.stream.item.v1` snapshots, revisions and committed cursors are unchanged.
No check promises to withdraw an external effect already dispatched.

Credentials have a version independent of the grant's immutable policy and Agent
identity. Owner-only rotation atomically replaces the token digest and increments
the version, appending audit metadata without secrets. Current requests and open
streams retain and recheck their authenticated version. A replaced credential
cannot pass delayed admission or later stream reads. Already accepted delegated
Runs retain their grant identity and are unaffected by credential rotation;
grant revocation continues to control their connector dispatch.

Native business application access targets an owner-maintained root Agent. A
trusted application backend authenticates external business users and supplies
their stable identities to the branch resolution endpoint. `BusinessAgentBranch`
uniquely maps application, root and exact business identity to a permanent private
Agent and its coordination Session. The tree is root configuration → isolated
business user Agent → coordination and real work Sessions → their histories,
snapshots, inputs and artifacts. Private memory is shared across Sessions only
within the branch, using the existing `(userId, agentId)` physical namespace.
No root or sibling history, memory or workspace snapshot is cloned.

Subsequent application requests carry the resolved branch identity in one exact
header. Authentication freezes the branch and credential version; admission and
resource reads revalidate both. All Agent/Session/file guards bind to the exact
private branch, never all descendants of the root. Model-side message/work
Session references also enforce this boundary, rather than treating the common
platform owner as sufficient authority. Signed material authorization and
explicit attachment links remain required; the tree confers no implicit global
Library access or automatic cross-Session file mounting.

The root is the single maintenance point for instructions and model policy.
New coordinator Runs snapshot its current configuration; existing Runs and child
work retain their accepted instruction snapshot. The owner can inspect a read-only
branch tree in settings, while ordinary Agent lists retain only maintained roots.
Native grants do not authorize ordinary Session submissions or configuration
writes. Each accepted input retains its application, grant and credential version
as immutable source facts. Coordinator Runs remain owner execution. The existing
input intake, actual Read evidence, native work dispatch, result return and
recovery contracts remain authoritative.
Revoking application access stops new requests; it does not discard accepted
owner tasks. Owner, membership and current source permissions still govern those
tasks. Closing a browser or rotating a credential does not replace the Agent or
its coordination Session. Configuration remains in the owner's maintenance UI.

The forward `0002_persistent_app_delegations` migration retains existing finite
deadlines and definition targets. It adds credential version 1 without inventing
audit operations or application origins for existing inputs. Only explicit new
consent creates a persistent or native-target grant. The forward browser credential
migration transfers old signed records with their original deadlines into one
nullable-deadline authority. The business branch migration creates an empty
identity mapping; it does not infer branches from old histories or copy customer
data. Explicit new resolution creates each branch. Browser login, application
grant lifetime, branch identity and Run execution lifetime are separate facts;
elapsed time or credential rotation does not discard an Agent tree.

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

`WORKER_SLOT_COUNT` sets concurrent jobs per worker process. It defaults to `8`
and accepts integers from `1` through `16`; invalid values fail at startup.
Compose forwards the setting from the deployment environment. Recreate the
worker container after changing it.

Slots are shared by lifecycle, knowledge-processing, and no-op jobs. This is
neither a per-tenant quota nor a fairness guarantee. Replicas multiply the total
slot budget; terminal dispatch and reconciliation remain separate control loops.
Size concurrency against sandbox memory, host CPU, and database capacity. The
configuration ceiling is not a claim that a host can sustain sixteen jobs.

The example deployment sets each execution sandbox's resource ceilings to
4 CPU cores (`SANDBOX_CPU_MILLI=4000`) and 8 GiB of memory
(`SANDBOX_MEMORY_BYTES=8589934592`). These are per-sandbox limits, not reserved
resources or a shared budget for the deployment.

For capacity comparisons, hold the revision, workload, replica count, and
historical-data baseline fixed while varying slots. Compare completed throughput,
queue age/depth, failures, and host/database resources; fast submission alone
does not establish sustainable capacity.

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
occurs at runtime safe points, not on a timer; concurrent workspace activity defers
collection. Terminal hosted teardown cancels remaining native children bound to
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
verification. API consumers share the six-field run binding (run, workspace,
session, user, agent and model configuration), while keeping endpoint errors and
resource-specific checks. The existing endpoint scopes are:

| API consumer | Digest | Signature | Six-field binding | Additional checks retained |
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

`gc_deleted_resources` also collects local workspace bytes of Sessions with
`status=deleted` and a non-null `purgedAt`. Both the purge time and the file mtime
must precede its cutoff, which defaults to 30 days. It enumerates outside the
transaction, then rechecks the locked owner before unlinking each exact key.
Only canonical Session snapshot generations, checkpoint keys of AgentRuns owned
by that Session, and this uploader's temporary-name format are eligible. It
rejects symlinks/reparse points and reports unknown Run directories as blocked.
Active Sessions, restorable trash, unrelated names and directory entries remain.
This collector neither reads nor changes Runtime checkpoint tables. It reclaims
purged-owner payloads, not active-owner orphans or all filesystem metadata.
Dry-run reports keys without deletion; missing keys are idempotent, and other
failures are reported and make the command fail for retry.

The first deployment enabling this collector must replace every API writer and
drain requests served by the old upload implementation before starting the new
GC worker. An old writer can otherwise create a final name after purge. No schema
migration is required; existing canonical objects of purged Sessions are eligible
under the same cutoff. Remote object storage needs a separate version-aware
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
`min(memoryBytes, dataTmpfsBytes)` from the existing execution authorization.
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
cleanup of already-purged Sessions and expired Agents. No ordinary read performs
cleanup and no time-based eviction of accessible history is introduced.
