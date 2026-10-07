# Platform materials and MCP

Workspace owns material authorization, document processing, search, evidence
receipts and citation presentation. Core owns generic runtime and tool semantics.
Material scope is the inputs explicitly attached and authorized for an AgentRun,
not implicit access to the user's entire library.

## Responsibilities and execution

Material processing follows Python API/admission -> durable platform task ->
dedicated Python material Worker -> isolated document processor -> atomic
platform publication.

Model access follows Runtime's generic MCP client -> Workspace material MCP
tools. Runtime registers the first-party provider when the run has declared
inputs. Normal sandbox file reads, image inputs and artifact publication use
their respective execution contracts.

The API owns access decisions, task admission, material storage and database
records. The material Worker uses the API image and runs
`python manage.py process_materials`. It claims tasks directly through the
platform database. Runtime and the lifecycle Worker do not orchestrate document
processing.

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

Runtime owns the reserved provider identity `workspace.materials`. Each
invocation obtains a fresh credential, opens a separate connection, checks the
frozen catalog and supplies the actual tool-call ID. Catalog collisions and
changes during a run fail closed. A whole-invocation timeout and cancellation
cover connection, discovery and execution; ambiguous calls are not automatically
replayed. Credentials and trusted headers do not enter model arguments or
history. Credential and material responses use `Cache-Control: no-store`.

Exact HTTP fields and citation response schemas are documented in
[API](../reference/API.md).

## Material tools

The current catalog contains five tools. Names and model parameters use
`lower_snake_case`; public JSON fields use `camelCase`. Unknown arguments fail
validation.

| Tool | Behavior |
| --- | --- |
| `list_materials` | List the run's authorized attached inputs. |
| `read_material` | Read one `input_ref`, with optional line `offset` and `limit` up to 2,000. |
| `search_materials` | Keyword search authorized inputs; optional `input_refs`, `ranking` (`relevance` or `recent`) and `limit` up to 20. |
| `get_operation` | Read the status of an `operation_id` owned by this run. |
| `cancel_operation` | Detach this run's operation without stopping shared processing or deleting its result. |

Representation identities and processing bindings are computed by the platform,
not supplied by the model. Missing representations yield `pending` with durable
operation handles. After completion, the caller repeats its read or search.
Read/search evidence contains bounded content, source identity and locators.
UTF-8 byte locations and evidence hashes are calculated by
`material_evidence.py`; locator ranges can include a line terminator excluded
from returned content and its hash.

The Runtime provider rejects results exceeding Core's inline output budget
instead of committing a clipped success with unusable evidence. Callers can
request smaller read/search windows.

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
combines validated receipts with supported `citation_recorded` events and
derives stable IDs across restart. Purging the owning Session also deletes its
receipts.

Workspace exposes `workspace.citations.v1` snapshots through the run citation
endpoint. A snapshot includes the run/session identity, `throughSequence` and
citation summaries. The transcript protocol does not embed that snapshot;
browser presentation requires a stable transcript-block binding. It must not
infer trusted citations from answer text. Snapshot and preview access recheck
current authorization; revoked access does not reveal the resource.

## Code and verification

API modules under `packages/api/app_core` own the implementation:
`platform_mcp.py` and `platform_mcp_auth.py` handle transport/authentication;
`material_access.py`, `material_reads.py` and `material_evidence.py` handle
authorized evidence; `material_operations.py`, `material_task_source.py`,
`material_leases.py` and `material_processor.py` handle processing;
`platform_material_commit.py`, `material_commit.py` and
`material_staging.py` handle publication; `material_receipts.py` and the
citation projection handle citations. Runtime's first-party integration is in
`packages/runtime_server/src/platform_materials.rs`.

Run the [release gate](../eval/ReleaseGate.md) for integration validation.
The audited API suite covers authorization, cancellation, leases, lifecycle
locks, storage recovery and successful-call binding.
`scripts/platform-mcp-client-gate.py` exercises real Rust/Python transport and
citation contracts against an isolated database.

The opt-in isolated Docker probes are
`scripts/platform-material-processing-gate.py`,
`scripts/platform-material-recovery-gate.py`,
and `scripts/platform-mcp-live-run.py`. They cover real processing,
kill/reclaim, model execution, receipts and restart/revocation. Browser preview
and interaction acceptance is manual.
Live model credentials are supplied through stdin, never source files or command
arguments. These probes are not ordinary CI model calls.

## Capability boundaries

The current server implements the five material tools above, not an unrestricted
library browser or vector knowledge-base service. Source authorization,
processing, search and evidence policy belong to Workspace. Additional material
backends using the same contracts are platform work; adding or changing tool
names also requires updating the Runtime's explicit catalog validation.

Public OAuth, negotiated MCP Tasks and transparent full-document pagination are
not implemented by this interface. Native/synthetic processor tests do not
certify GPU execution, OCR quality or performance improvements.
