# Platform materials and MCP

Workspace owns material authorization, document processing, search, evidence
receipts and citation presentation. Core owns generic runtime and tool semantics.
Material scope combines signed attached inputs with an immutable per-Run
personal-library grant. Native Session submissions receive that grant; existing
accepted Runs keep their original attached-input scope. Every access still checks
current membership, ownership, lifecycle and content identity.

## Responsibilities and execution

Material processing follows API admission -> durable platform task -> Celery
delivery -> dedicated material Worker -> isolated document processor -> atomic
platform publication.

Model access follows Runtime's generic MCP client -> Workspace material MCP
tools. Runtime registers the first-party provider when the Run has attached
inputs or its accepted personal-library grant. Normal sandbox file reads,
image inputs and artifact publication use their respective execution contracts.

The API owns access decisions, task admission, material storage and database
records. After admission commits, the API sends the task identity through the
dedicated Redis broker. The material Worker claims the corresponding PostgreSQL
lease before processing. PostgreSQL records ownership, attempt limits and final
publication. A single Celery Beat scheduler asks the Worker to recover older
pending tasks, expired leases and abandoned processor containers. This also
recovers admissions that committed while the broker was unavailable. Runtime and
the lifecycle Worker do not orchestrate document processing.

Only Runtime and the material Worker receive the Docker socket in bundled
Compose. Both are trusted infrastructure components: Runtime controls generic
execution; the material Worker controls isolated document containers. API and
lifecycle Worker containers do not receive the socket. See
[Architecture](Architecture.md#deployment-trust-boundary) for credential and
host trust boundaries.

## MCP connection and authorization

The API serves stateless Streamable HTTP at `/internal/mcp` through the official
Python MCP SDK. This is an internal service-to-service endpoint. It rejects
browser Origin headers, duplicate Authorization headers and hosts outside the
exact `PLATFORM_MCP_ALLOWED_HOSTS` allowlist. Request bodies are limited to
64 KiB. Use a private network and protected transport; do not expose this
endpoint through the browser proxy.

The material Worker inspects its configured image, checks its processor
specification and registers the document processor in `MaterialProcessor`.
Runtime obtains that specification from `POST /internal/materials/processor`,
then obtains a five-minute run-scoped bearer credential from
`POST /internal/mcp/credential`. Both endpoints require the internal token.
Credential claims bind the run, authorization digest and processing specification.
The MCP endpoint accepts the scoped bearer credential, not the internal token.

Every MCP request verifies the signed run binding and current membership.
Material access additionally resolves current input permissions and source
generation. Credentials are not cached authorization decisions.
`MaterialAccessContext` is trusted internal context, never a model-supplied
credential. Request-local context is isolated and cleared after dispatch.

Runtime owns the reserved provider identity `workspace.materials`. It
initializes the MCP client and discovers the catalog once per Run. Each
invocation obtains a fresh credential bound to that catalog and attaches the
actual tool-call ID to its own request. A catalog change rejects dispatch,
including when the issuer and MCP endpoint serve different catalog revisions.
The original-file path uses the Host delivery operation after credential
issuance. A whole-invocation deadline and cancellation cover queued and active
content calls. An original-file transfer that has begun admission finishes its
bounded Host operation and confirmation before reporting cancellation. Ambiguous
calls require reconciliation under the original identity.
Credentials and trusted headers do not enter model arguments or
history. Credential and material responses use `Cache-Control: no-store`.

Exact HTTP fields and citation response schemas are documented in
[API](../reference/API.md).

## Material tools

The current catalog contains six tools. Names and model parameters use
`lower_snake_case`; public JSON fields use `camelCase`. Unknown arguments fail
validation.

| Tool | Behavior |
| --- | --- |
| `list_materials` | Discover authorized metadata with `scope` (`all`, `library`, `attached`), name `query`, `folder`, opaque `cursor` and `limit` up to 100. Default `all` means the Run's authorized set; it does not grant access. |
| `read_material` | Read one `input_ref` with an explicit `delivery`. `content` supports line `offset` and `limit` up to 2,000; `execution_file` installs the original for Python/Skills and rejects content pagination arguments. |
| `search_materials` | Search authorized content in `all` (default), `library`, or `attached` scope. Optional `input_refs` explicitly select at most 128 versions; an empty array selects nothing. `ranking` is `relevance` or `recent`, and `limit` is at most 20. `cursor` continues ranked hits; `processing_cursor` advances bounded material preparation. |
| `read_material_result` | Continue an immutable saved content result using `result_ref` and its opaque `cursor`. |
| `get_operation` | Read the status of an `operation_id` owned by this run. |
| `cancel_operation` | Detach this run's operation without stopping shared processing or deleting its result. |

Representation identities and processing bindings are computed by the platform,
not supplied by the model. A missing read representation yields `pending` with
a durable operation handle. Search can return indexed hits alongside incomplete
processing coverage. After completion, the caller repeats its read or search.
Read/search evidence contains bounded content, source identity and locators.
UTF-8 byte locations and evidence hashes are calculated by
`material_evidence.py`; locator ranges can include a line terminator excluded
from returned content and its hash.

The Runtime provider rejects results exceeding Core's inline output budget
instead of committing a clipped success with unusable evidence. Callers can
request smaller read/search windows.

## Personal Library content and search coverage

Library notes retain their original titles, including titles without a filename
extension. The document processor accepts exact `text/markdown` and `text/plain`
content types through its strict UTF-8 text reader. Other supported document
formats retain their existing processing routes. A processor image change alters
the processing specification digest, so new processing does not reuse an older
image's representation identity or rewrite the original material.

Implicit Library search is bounded by database queries over current authorized
owner, generation, hash and processing specification. Its scope is not the
128-version explicit selection or the protected execution-file inventory.
PostgreSQL 18 performs Unicode case folding and keyword ranking before fetching
the bounded hit window. Returned evidence is rebound to current permissions.

Unprocessed materials are admitted through the existing shared processing tasks
and Run-owned operation handles, at most 20 materials per preparation page.
Unsupported formats such as ZIP remain available as originals. Failed and
unsupported materials are reported separately and do not prevent indexed
materials from returning hits.

Search responses include `coverage`, `operations`, `processingFailures`,
`nextSearchCursor` and `processingCursor`. `coverage.complete` remains false while
any scoped material is unprocessed, failed, unsupported, or requires a refreshed
search window. An empty incomplete window is not proof that the Library has no
matching content. Ranked cursors retain their search time boundary; changed
sources or newly processed content set `refreshRequired`, requiring a new search
without the ranked cursor.

`save_result` preserves one immutable hit window, including its coverage and
preparation status. `read_material_result` only pages that window's content;
its cursor is not a ranked-search or material-preparation cursor. Every page
retains coverage and both search cursors within the model output budget.
Repeating a committed call reuses its saved window and rechecks current source
access. New calls can obtain refreshed search and processing status.

## Original files and Session continuity

Discovery reads metadata without opening originals or creating Session input
bindings. Unsupported document formats can still be discovered and fetched as
originals; discovery is not a promise of parsed content or searchable archive
members.

An original-file delivery first reserves its immutable identity and capacity in
a short API transaction, then Runtime downloads and validates its version,
digest and byte count. The existing Host helper installs a protected read-only
input and commits its inventory before Runtime confirms delivery. Only then may
the tool return `kind: execution_file`, an execution `path`, and source metadata.
That result creates no parsed-content receipt or citation.

The original-file limit is 64 MiB per file. Retained protected inputs, including
distinct attached versions, are bounded by the existing 128-input validation
contract and the execution workspace budget. This bound does not limit Library
discovery: metadata remains paginated over the authorized Library.

`SessionMaterialInput` records the Session's version and stable path, while
`RunMaterialAdmission` records the current admission and originating tool call.
New Runs recheck their own grant and current permissions before restoring those
inputs. A Library version already attached to the Session uses one canonical
reference and path across both origins. Accepted authorization payloads are not
rewritten. Migration 0042 gives existing Runs an explicit disabled Library grant.

Unconfirmed reservations are retained across errors until a fenced Host
reconciliation establishes that the exact input is present or absent. Inventory
inspection also handles the interrupted file-install/inventory-commit boundary.
The execution input writer serializes installation and revocation with that
inspection; snapshot collection excludes concurrent dispatch. Protected inputs
remain separate from ordinary workspace snapshots and are restored from their
authoritative Session identities.

Stopped-container capture reads the original instance's persisted input inventory
without starting the guest and excludes those exact paths, including revoked
entries. Ordinary files under `inputs/` remain part of the workspace snapshot.

## Durable processing and cancellation

`MaterialProcessingTask` is shared by representation identity.
`MaterialOperation` is a run-owned waiting handle; repeated admission reuses
the handle, while different runs receive separate handles. One run cannot query
or cancel another run's operation.

A task stores only its source identity, size and processing specification.
Admission requires fresh run authorization, but execution does not borrow a
waiter's credential. Cancelling one or all waiting operations leaves a live
platform material eligible for processing. A cancelled handle remains cancelled
after the shared task completes. Result access still requires current permission.

The Worker resolves current source lifecycle and verifies original bytes before
processing. Deletion, changed generation/hash/size, unavailable storage or an
invalid material state prevents publication.

Task claims use database time, a lease owner and a monotonically increasing
epoch. The Worker renews its 300-second lease while processing, with a
1,200-second processing deadline. Expired tasks can be reclaimed; stale owners
or epochs cannot renew or publish. Automatic execution is bounded to three
attempts, after which the task fails.

Processor containers use a non-root user, read-only root, no network, dropped
capabilities and CPU/memory/process limits. Every document gets anonymous
input/output volumes. Output archives reject unexpected paths, links,
duplicates and oversized content. Cleanup checks owned container identity;
expired processing containers, their volumes and aged owned specification
containers are reclaimed. See [Document processing](../operations/DocumentProcessing.md)
for processor support and configuration.

## Atomic publication and storage recovery

`platform_material_commit.py` owns the outer durable publication transaction.
It locks the task and source lifecycle, checks the lease, validates output and
commits the representation, segments, derived resources and task completion
together. It rechecks the lease after stream verification. Exact completed
replay verifies identity and bytes without creating duplicate representations.

`MaterialStagedObject` records storage intents before writing output.
Output keys include representation and content hashes. Recovery under the task
lock repairs partial unregistered writes or discards abandoned staging.
Registered or published objects are preserved even when the publication
acknowledgement was lost. Live processing leases exclude staging garbage
collection. Material deletion and publication serialize through source locks.

The shared Runtime/Session transaction is a separate boundary: its event and
runtime-state commits remain atomic. Material processing does not split those
commits into independent HTTP writes.

## Evidence receipts and citation presentation

For citable calls, Runtime supplies `X-Workspace-Tool-Call-Id` through trusted
connection context. The platform checks the committed call's run, session,
provider, tool name and exact normalized arguments before binding evidence.
The header alone grants no access.

A ready read/search with eligible evidence stores one immutable
`MaterialEvidenceReceipt` per committed call, including the exact response
and server-derived evidence. Identical replay reuses it; conflicting replay
fails. Pending/empty results and diagnostic calls without call context create
no receipts. Returned `receiptId` and `citationIds` do not by themselves prove
successful tool execution.

Citation projection requires a matching same-run, same-turn committed
`successWithOutput` result with complete, exact output. Missing, failed,
aborted, altered or clipped results cannot publish citations. An arbitrary MCP
result mentioning a receipt ID cannot forge that binding. Generic MCP `facts`
are not the platform's evidence-persistence mechanism.

Projection rebuilding is transactional and serialized on the AgentRun. It
combines validated material receipts, historical application-owned citation
records, and generic `source_associated` read records bound to admitted input
identities. Workspace applies citation eligibility and document locators and
derives stable citation IDs across restart. Historical citation rows retain
their complete original evidence while storage readers reconstruct generic
Core records. Purging the owning Session also deletes its receipts.

Workspace exposes `workspace.citations.v1` snapshots through the run citation
endpoint. A snapshot includes the run/session identity, `throughSequence` and
citation summaries. The transcript protocol does not embed that snapshot;
browser presentation requires a stable transcript-block binding. It must not
infer trusted citations from answer text. Snapshot and preview access recheck
current authorization; revoked access does not reveal the resource.

## Code and verification

API modules under `packages/workspace/api/app_core` own the implementation:
`platform_mcp.py` and `platform_mcp_auth.py` handle transport/authentication;
`material_access.py`, `material_reads.py` and `material_evidence.py` handle
authorized evidence; `material_operations.py`, `material_task_source.py`,
`material_leases.py` and `material_processor.py` handle processing;
`platform_material_commit.py`, `material_commit.py` and
`material_staging.py` handle publication; `material_receipts.py` and the
citation projection handle citations. Runtime's first-party integration is in
`packages/workspace/runtime/src/platform_materials.rs`.

Run the [release gate](../eval/ReleaseGate.md) for integration validation.
The audited API suite covers authorization, cancellation, leases, lifecycle
locks, storage recovery and successful-call binding.
`scripts/workspace/platform-mcp-client-gate.py` exercises real Rust/Python transport and
citation contracts against an isolated database.

The opt-in isolated Docker probes are
`scripts/workspace/platform-material-processing-gate.py`,
`scripts/workspace/platform-material-recovery-gate.py`,
and `scripts/workspace/platform-mcp-live-run.py`. They cover real processing,
kill/reclaim, model execution, receipts and restart/revocation. Browser preview
and interaction acceptance is manual.
Live model credentials are supplied through stdin, never source files or command
arguments. These probes are not ordinary CI model calls.

## Capability boundaries

The current server implements the six material tools above with owner-scoped
Library discovery and original-file delivery. It does not expose arbitrary host
paths or a vector knowledge-base service. Source authorization, processing,
search and evidence policy belong to Workspace. Additional material
backends using the same contracts are platform work; adding or changing tool
names also requires updating the Runtime's explicit catalog validation.

Public OAuth, negotiated MCP Tasks and transparent full-document pagination are
not implemented by this interface. Native/synthetic processor tests do not
certify GPU execution, OCR quality or performance improvements.
