# Agent Run API: design and review boundaries

Review date: **2026-11-06**. Baseline: Draft PR
[#55](https://github.com/EchoTrigger/centaeris/pull/55), commit
[`aaacd11498b4c85f09faa79118516d69adc93f5b`](https://github.com/EchoTrigger/centaeris/tree/aaacd11498b4c85f09faa79118516d69adc93f5b).

This document distinguishes observed v1, prepared corrections, and proposed
changes for subsequent commits on the same PR.
**The proposed result API is not implemented.** Existing callers must follow
the [v1 transport reference](../reference/API.md#agent-run-api).
Acceptance criteria do not claim that deployment or migration gates have passed.

## Decision and intended use

Agent Run API execution accepts one bounded task under an explicitly scoped
credential. It creates a private temporary Session and runs the existing
Core/Runtime lifecycle. It does not introduce a second execution engine,
external Session management, or an interactive human-approval workflow.

The central invariant is: one request identity admits at most one Run within
its credential scope; observing that Run does not own its execution lifetime.
Execution, result delivery, cancellation, and physical cleanup have separate
outcomes. A successful HTTP response cannot stand for all four.

V1 provides one original output stream and later receipt lookup. That is useful
for controlled backend integrations that persist the original response and can
tolerate irrecoverable output loss. It is insufficient for a promised durable
document-delivery or OA workflow unless the caller provides its own durable
receiver and handles that remaining failure mode. A temporary execution
environment does not require temporary, unrecoverable business results.

## First integration journey

| Stage | Required behavior |
| --- | --- |
| Create a credential | Ask for a name and available Workspace model. Plugins and materials default to empty scopes. Display the new secret at creation and let its issuer copy it again from the list. |
| Submit a task | Provide one runnable backend example. Save endpoint, credential reference/fingerprint, operation ID, and exact request body before sending. Require no optional material or plugin setup. |
| Receive results | Save the receipt and Run ID immediately. Persist text as received; publish files only after integrity checks. Confirm both Run and delivery outcomes. |
| Handle uncertainty | Query the original operation using the same credential. If deliberately resubmitting, preserve the original ID and body. Never manufacture a new identity merely because a response was lost. |

The console owns credential configuration. Optional plugin and material
directories load when the optional-permissions area is first expanded; their
loading, empty, failure, and retry states belong inside their respective sections. An unavailable
optional directory must not prevent creation with an empty scope. Switching
Workspaces must discard stale selections and late responses.

This isolation also applies on the server. An empty plugin scope must produce
an empty activation without reading or validating unrelated Workspace plugins.
The prepared correction adds that early return in
[`plugin_activation_for_run`](../../../packages/api/app_core/agent_run_api_credentials.py).
Selected nonempty scopes still undergo the existing current-availability checks;
this review does not claim to eliminate all catalog coupling for selected plugins.

If credential creation times out, the issuer can check the list and copy the
secret from the resulting row. A credential display name can be shared by
multiple credentials.

The local console, guide, generated-reference, and sample-client corrections
preserve the HTTP wire contract. The sample now distinguishes a receipt from
delivered output, offers read-only `--status --operation-id`, rejects unexpected
response media types, and preserves later valid files after an `artifactFailed`
event. Local integrity or sequence errors terminate consumption.

## Ownership and authority

| Owner | Responsibility and boundary |
| --- | --- |
| Credential | Binds the issuing user and membership, one Workspace, one model configuration, and explicit plugin/material scopes. It is not the provider's model secret. |
| API admission | Validates current authority, binds the request fingerprint, and atomically creates the operation, private Session, Run, and signed authorization. |
| Runtime and lifecycle Job | Own execution, deadline enforcement, cancellation reconciliation, and environment release. Transport teardown cannot substitute for these facts. |
| Response connection | Projects the original output and owns only its delivery lease. Closing it releases observation and delivery resources. |
| Cleanup collector | Erases temporary execution content after terminal execution, physical release evidence, Runtime purge, and delivery closure/expiry permit it. |
| Caller | Persists its request identity and received results; distinguishes unknown observations from terminal execution facts. |

Agent Run API authorization uses the v3 execution policy and `perRun` allocation.
Personal-library access and human questions are disabled. Material IDs must be
ready, granted files from the credential's own Workspace and explicit allowlist.
A call may select a subset of that list; an empty request list grants nothing
implicitly. Plugin permissions must not expand when an unrelated installation
is added to the Workspace. A Run carries the signed activation selected for it.

The credential allowlist contains plugin names, not package-version pins. A
later Run resolves those names against the current Workspace activation; an
update to an allowed package can therefore change that later Run's capabilities.
The existing nonempty-scope path still reads the full Workspace catalog and
silently omits allowlisted names that are no longer available. Selected-package
isolation and a stable error for missing selected dependencies need a separate
contract decision; the empty-scope correction does not promise either behavior.

The public bearer endpoints reject mixed browser-session authentication. Every
new external request checks the credential's current authority, including
revocation, active Workspace/user, sponsoring admin membership, and model
availability. Operation lookup and cancellation are scoped to that credential,
not merely to the Workspace or possession of a Run ID.

Revocation blocks subsequent external lookup and cancel calls as well as new
submissions. Live delivery and lifecycle reconciliation also recheck authority;
revocation is not a synchronous rollback of an in-flight tool action. Routine
rotation should settle old operations and save their output first. Emergency
revocation may intentionally sacrifice that recovery path.

A replacement credential cannot automatically inspect the old credential's
operations. Administrator receipt recovery or a stable integration principal
across key rotation would need explicit authentication, authorization, audit,
and retention design. Neither is an existing v1 capability.

## V1 acceptance and recovery contract

The implementation anchors are
[`agent_run_api_runs.py`](../../../packages/api/app_core/agent_run_api_runs.py),
[`http/agent_run_api.py`](../../../packages/api/app_core/http/agent_run_api.py), and
[`agent_run_api_protocol.py`](../../../packages/api/app_core/agent_run_api_protocol.py).

### Response selection

| Condition when POST is processed | Successful response |
| --- | --- |
| No operation has been accepted for this credential and key; this request accepts it | `200 text/event-stream`, with `runAccepted` as its first data event |
| The operation already exists and the request fingerprint matches | `200 application/json`, containing the current minimal operation receipt |
| The operation exists but the fingerprint differs | `409 operation_conflict` |

The distinction is **first acceptance versus an already accepted operation**,
not first network attempt versus retry. A retransmission can be the first
request to reach acceptance and therefore still return SSE. Concurrent matching
requests converge on one Run, with one original stream and receipt responses
for the others. `Accept` does not choose a mode. Clients inspect `Content-Type`
before parsing; changing only `200` to another status would not recover lost
output or remove this parsing requirement.

The successful SSE response does not mean the Run completed. `outputText`
contains committed assistant text after terminal execution, split into ordered
chunks; a failed or cancelled Run may have only its last partial response.
It is not a live model-token stream. `runFinished` separately
reports `runStatus` and `deliveryStatus`. EOF without that event leaves delivery
unconfirmed even if some text or verified files were received.

### Identity, fingerprint, and expiry

The required key is `<Unix-seconds>.<lowercase UUIDv4>`. Its timestamp is immutable
across retries. V1 rejects timestamps more than five minutes ahead and rejects
keys at or beyond 24 hours after their embedded timestamp. That expiration is
checked before looking up an operation, so a stale key returns `410` even after
its receipt has been collected. This prevents a forgotten old identity from
silently becoming a new Run after GC.

The fingerprint uses the trimmed, nonempty prompt and validated request fields;
material IDs must already be sorted and unique. Callers should preserve the
exact request body rather than depend on normalization. Reusing a key with a
different credential is a different scope and can create another Run. Run
admission deduplication does not guarantee exactly-once external plugin effects.
Tool-intent recovery and any downstream idempotency remain separate contracts.

**Current edge case:** a never-used key close to its 24-hour expiry can be
accepted although its lookup/cancel window ends before the new Run's ten-minute
deadline. The final admission check only verifies that the key has not already
expired. Generate a key immediately before first use; do not preallocate old
keys as a queue. A follow-up should reject new admissions lacking a sufficient
remaining observation window, while preserving lookup of already accepted
operations. That is a validation-policy change requiring contract tests and a
documented migration; it is not silently fixed by changing the timestamp.

### Uncertain observations

Timeout, disconnect, proxy error, and a missing terminal event are observations
of the caller. `unknown` is not a new Run state. A prompt `404` lookup can race
with admission still in progress and is not proof that a new key is safe.
Retain the original identity. An intentional same-key resubmission can resolve
admission but cannot reattach to an already accepted operation's original stream.

The sample's submit mode exits `0` only after verified successful delivery,
`2` for a JSON receipt without output, and `1` for a call/delivery failure.
Its status mode performs one GET and exits `0` for a successful receipt lookup;
the caller must still examine `runStatus`. There is no automatic retry or
automatic replacement of the key.

The quickstart persists a credential fingerprint with the original request and
rejects a changed secret before its recovery lookup. It does not persist the
secret in the request file or silently transfer the operation to a new key.

## Execution, delivery, and cleanup lifetimes

| Lifetime | Observed v1 rule |
| --- | --- |
| Caller wait | A transport setting; expiry does not prove cancellation or failed execution. |
| Run | Ten minutes from acceptance, including queue time. |
| Original output delivery | At most 60 seconds after terminal execution; disconnect closes delivery. |
| Receipt/key validity | 24 hours from the embedded key timestamp, not from terminal execution. |
| Temporary content | Collected only after terminal execution, settled physical cleanup, Runtime purge, and closed/expired delivery. |
| Result recovery | No result GET, stream reattachment, or output replay is promised. |

`expiresAt` describes the operation identity window, not a download deadline.
`cleanupStatus=completed` reports reclamation, not that the caller received its
results. Conversely, `runStatus=completed` can coexist with failed delivery or
pending cleanup. The retained minimal receipt remains useful after content has
been removed; physical-cleanup provenance has its own Runtime responsibility.

Cancel is an explicit empty-body POST. `202` with `cancelStatus=requested`
acknowledges a request; query the operation to establish a terminal state.
A `200` terminal receipt can report completion because completion won the race.
Cancellation, revocation, and environment destruction do not undo external
effects already performed by plugins.

## Design only: keep POST SSE and add bounded terminal-result reads

**Yes: the existing POST can remain unchanged while one additional read resource
provides bounded terminal results.** A new execution engine, JSON-only admission,
live-token replay, and stream reattachment are not prerequisites. This patch
adds no route, persisted result schema, response field, or retention promise.
The result retention duration and new fields are deliberately undecided.

### Caller compatibility and discovery

Existing clients keep the current contract: first acceptance returns `200` SSE;
an already accepted operation with the same credential, key, and input returns
`200` JSON. The original `GET` still returns only a receipt, and all current
status codes, events, key validation, and `expiresAt` semantics remain unchanged.
Clients that only consume the original stream require no protocol migration.
Clients that opt into recovery add a result read and bounded polling for result
publication; they must still persist the original credential reference,
operation ID, and request before POST. No new key or re-execution is necessary.

The new resource must be discoverable using the original operation ID under the
issuing credential. Requiring only a Run ID learned from `runAccepted` would
leave a caller that lost the first response unable to recover. The exact route,
representation, pending/expired/error statuses, and expiry metadata are a
follow-up contract decision. A terminal outcome and a fully published result
are separate facts: completed execution must not expose a successful manifest
whose bytes are not ready. Failed or cancelled Runs may have a partial terminal
answer and valid artifacts; the representation must preserve that outcome.
Each read has its own transfer outcome. A later successful transfer does not
rewrite the terminal Run or pretend that its original SSE delivery succeeded.

One additional API route is sufficient only if its representation can deliver
the bounded file bytes as well as text and metadata, for example a terminal
result stream or a downloadable bundle. A JSON manifest alone does not restore
lost files. If the chosen representation uses URLs, each download is another
authorized read boundary with its own expiry and revocation rules, even if
existing storage infrastructure serves it. Choose the representation before
claiming that one result GET completes file recovery. This proposal does not
add an SSE cursor, `Last-Event-ID` support, or incremental token replay.

### Receipt expiry and retained ownership

V1's 24-hour key window is measured from the embedded timestamp; it is not a
terminal-result retention period. Do not repurpose its `expiresAt` or relax the
old POST/GET expiry guard as part of an additive endpoint. A result promise
that can outlive that window needs its own retained credential/operation/Run
mapping, lookup policy, and garbage-collection owner. Otherwise a result may
still exist but become undiscoverable, especially after a near-expiry admission.
The new read may validate the operation ID's syntax without treating the old
POST admission cutoff as its result expiry. That policy applies only to the new
resource; an expired original key still cannot create another Run.

Choose the retention duration, its terminal/publication time anchor, and how
callers learn the deadline together. Reads must not silently extend that
deadline. Existing Runs whose output was already collected cannot acquire a
recoverable snapshot retroactively. Deployment must distinguish them from new
retained results without promising a backfill that cannot be performed.

### Publication and cleanup

The lifecycle worker, rather than the original response connection, must own
result publication. Otherwise the disconnect that needs recovery also prevents
the snapshot from being produced. Persist the terminal text and artifact
manifest, verify the immutable bytes, and transfer or pin their ownership
before the execution collector can remove them. Publish readiness atomically
only when that transfer is complete; recovery after a crash must finish or
explicitly fail partial publication. A broken file must not hide other verified
files or turn the terminal Run outcome into fabricated success.
Execution-payload deletion must wait for that bounded handoff to finish or
explicitly fail; disconnect alone cannot authorize deletion of an unpublished
promised snapshot. Physical environment release can still proceed independently.

The execution container, private transcript, and temporary delivery lease may
then follow their existing cleanup lifecycle. A result owner retains only the
promised terminal snapshot and blobs; it must not keep the whole environment
alive. Expiry removes result references and releases blob ownership without
deleting shared material or another Run's retained files. A bounded read lease
can protect an already authorized transfer from concurrent GC without allowing
new reads after expiry. Decide its duration and revocation behavior explicitly.

Keep per-Run artifact budgets, bound total retained text by UTF-8 bytes, and
account for aggregate Workspace capacity and collection lag. Chunk size alone
does not bound total text. Quota/backpressure policy must prevent silent early
eviction of a promised result. No quota values, new error codes, or retention
fields are selected in this patch.

### Revocation and authority

New reads must check the current secret, revocation, active Workspace/user,
sponsoring membership, and original credential ownership. A different
credential in the same Workspace, including a replacement key, inherits no
access. Revocation blocks new result and file reads; it cannot erase bytes
already delivered or undo completed external side effects. It need not require
immediate physical deletion: authorization denial and storage expiry are
separate events. Administrative recovery after rotation is a separate audited
feature, not an implicit privilege of the new endpoint.

The current v1 authentication also rejects a disabled model, including on
receipt lookup and cancellation. This patch keeps that behavior. For the new
read resource, decide explicitly whether model availability is an execution
prerequisite only; allowing an otherwise authorized caller to read an existing
result after model retirement is reasonable, but cannot be achieved by silently
weakening the old endpoints. Signed bearer URLs can survive credential
revocation until their own expiry. Strict immediate revocation requires an
authorization check at download time, or an equally explicit and tested URL
revocation mechanism. Long-lived public URLs are not a substitute.

Before implementation, decide the route/representation, retention duration and
anchor, new fields and statuses, text/capacity limits, read leases, model-retirement
policy, and behavior for pre-deployment Runs. Contract tests must cover lost
acceptance, disconnect, publication/GC races, credential revocation, expiry,
same-Workspace foreign credentials, and files verified after retrieval.

## Artifact integrity boundary

Current publication and client limits are **four artifacts, 8 MiB per artifact,
16 MiB total raw bytes**. Base64 transport does not increase the permitted raw
byte budget. Limits are enforced at publication as well as checked by the sample.

Each artifact is staged separately. Its ID must identify an active transfer;
chunk indices start at zero and remain contiguous. Completion must repeat the
started filename, content type, byte size, and SHA-256 exactly and match actual
bytes and chunk count. Only then can the client atomically publish the file
without overwriting caller data. Duplicate completion, completion after failure,
missing completion, truncated bytes, or a bad digest cannot produce a valid file.

An `artifactFailed` discards only that partial file and permits later artifacts
to arrive. Verified files remain available even when overall delivery fails.
Authenticated access, content integrity, and availability after disconnect are
different properties. A future signed download URL would additionally require
an explicit expiry and revocation policy; a checksum does not grant access.

## Upgrade boundary: migration 0046

[`0046_workspace_owned_resources`](../../../packages/api/app_core/migrations/0046_workspace_owned_resources.py)
preserves provider/model identities for historical references while retiring
shared configuration. It disables shared current models/providers, revokes
Agent Run API credentials bound to those models, deletes shared provider credentials
and MCP keys, and preserves Workspace-local configuration. Historical identity
preservation must not authorize new execution with a retired secret.

Existing plugin enablements without Workspace installations acquire installation
metadata from the legacy catalog; existing installations are kept. This does
not migrate the legacy package bytes into immutable storage. Keep the matching
legacy directories mounted and unchanged while historical bindings reference
them. A missing package must fail the upgrade, not resolve to unrelated content
with the same name. Workspace plugin bindings may still require replacement
Workspace MCP credentials after shared keys are deleted.

The baseline supplied a no-op reverse for a destructive data migration. The
prepared correction makes reversal fail explicitly: deleted secrets cannot be
reconstructed by unapplying a migration. Deployment recovery requires the
matching pre-upgrade database and storage backup, not merely old application
code. See [resource administration](../operations/WorkspaceResourceAdministration.md).

A release test using an actual database at migration `0045`, historical Runs,
legacy plugin bindings, and both shared and local credentials remains required.
It must migrate forward and exercise historical reads, current model selection,
plugin resolution, and credential isolation. Calling the migration function
with today's model registry, or a unit test of irreversible reversal, does not
prove historical-schema compatibility. This document makes no database-gate
pass claim.
The separate [PostgreSQL upgrade acceptance checklist](../eval/AgentRunAPIUpgradeAcceptance.md)
records the historical-state test results and remaining deployment checks.

## Documentation ownership and migration costs

The console owns creation, scope selection, and one-time secret display. Its
first-call example and the quickstart use one shared example generator, without
adding a quickstart shortcut to the console. The quickstart owns the walkthrough
and original-operation recovery instructions. The artifact guide owns file handling.
The generated reference and HTTP OpenAPI describe the same exact fields, required
headers, media types, and response conditions. Generation derives payload schemas
from the implementation and must pass its stale-output check; contract tests
compare the bundled reference with the actual Ninja OpenAPI. This document owns design decisions, lifecycle
boundaries, compatibility, and outstanding acceptance gates.

Avoid separate competing examples in each surface. In particular, do not call
every retry JSON, every `200` success, or every `completed` receipt a delivered
result. Changes to public behavior require updating the schema source,
generation script/output, runnable client examples, and behavioral contract
tests in one reviewable change.

| Change | Caller or operator cost | Status |
| --- | --- | --- |
| Sample submit returns exit `2` for a receipt | Automation must distinguish receipt-only from delivered output; status lookup success remains `0`. | Prepared locally; no HTTP change |
| OpenAPI, generated headers and response-condition documentation | Callers gain an accurate parser contract; existing requests remain valid. Regenerated SDKs may expose response types that were previously unspecified. | Prepared locally; no wire change |
| Keep POST SSE; add terminal-result reading | Existing SSE clients keep their current parsing. Recovery-aware clients add result reads and preserve the original operation identity; generated SDKs gain an optional operation. | Design only; no route or fields added |
| Key format and v1 expiry policy | No caller migration in this patch. Any later change needs separate versioning/validation and deduplication review; never reinterpret an expired key as new work. | Unchanged |
| Retained result resources | Operators add durable snapshots, capacity policy, credential-bound reads, and independent GC; results already deleted cannot be backfilled. Retention duration and new fields remain undecided. | Design only |
| Shared secret retirement | Operators supply Workspace replacements and retain matching historical plugin bytes; rollback requires coordinated restoration. | Existing migration, corrected irreversible declaration |

## Acceptance matrix

| Boundary | Fault or scenario | Required observable result |
| --- | --- | --- |
| Basic credential form | Optional directory fails while collapsed | No eager request or optional error blocks name/model creation. |
| Workspace switching | Old optional request resolves after switching | No stale selection, secret, or directory state crosses Workspaces. |
| Empty backend scope | Unrelated Workspace catalog is broken | Empty activation succeeds without consulting that catalog. |
| Acceptance | Concurrent identical submissions | One Run; response type follows actual acceptance ownership. |
| Request identity | Same credential/key with changed input | Conflict; no second Run. Original key/body survive transport failure. |
| Clock and GC | Future skew, expired key, receipt already collected | Documented validation/expiry response; no stale-key recreation. |
| Near expiry | Unused key accepted just before expiry | Reproduce v1's short observation window; follow-up proves the chosen replacement rule. |
| Lost response | Disconnect before/after `runAccepted`, delayed commit, lost terminal frame | Original operation resolves execution facts; no new-key retry or implied output replay. |
| Client media types | SSE, queued/failed/completed JSON receipt, unexpected HTML | Correct parser; receipt is not result success; unsupported type fails. |
| Artifact transfer | Failure followed by valid file, changed completion metadata, duplicate end | Valid files survive; partial/tampered files are withheld; overall failure remains visible. |
| Authority | Other credential/Workspace, revoked sponsor/key/model | No cross-scope read/cancel; no replacement-key inheritance. |
| Cancellation | Completion races accepted cancellation | Queryable terminal fact; requested does not imply cancelled. |
| Cleanup | Delivery closes while Run or physical release is pending | Cleanup waits for required facts; receipt and shared data remain correct. |
| Upgrade | Real `0045` database and matching plugin storage | History remains readable, shared secrets are unusable, local credentials remain isolated. |
| Proposed retention | Restart or GC during result publication and expiry | No ready manifest with missing blobs; no early eviction; eventual bounded reclamation. |

### Verification of the prepared corrections

With the locked Python 3.12.10, Node 22.21.0 and pnpm 12.10.1 dependencies:

| Executed check | Result |
| --- | --- |
| Sample client: receipt-only exit, lookup, identity reuse, media types and artifact verification | 10 tests passed |
| Agent Run API delivery and artifact budgets | 8 tests passed |
| Actual Ninja OpenAPI vs bundled reference: response media/payloads, request bodies/headers and Bearer security | 3 tests passed, without a database |
| Empty plugin scope and irreversible migration guard | 5 tests passed; regressions failed on the baseline |
| Credential component: lazy requests, isolated retries and Workspace switching | 5 tests passed |
| English/Chinese key and interpolation parity | 2 tests passed |
| Web typecheck, scoped frontend lint, generated-reference freshness, whitespace | Passed |
| Actual Bash examples against a local HTTP fixture | Six scenarios passed: first SSE, repeated-copy rejection, original-ID GET, changed credential rejection, lost response, and HTTP error propagation |

The bundled reference's payload schemas, event registry, paths, methods, status
codes and response media types were compared with the baseline and remain
unchanged. Its metadata now describes required authorization and response
conditions. The HTTP OpenAPI gains declarations for the existing responses;
that schema correction does not change the actual response bodies or transport.

Database API tests discovered 14 tests but executed none because the configured
PostgreSQL connection was refused. No real-model end-to-end request or full PR CI
gate was run here. Focused checks do not substitute for the historical-state
database upgrade or end-to-end authority/cleanup acceptance.

## Lessons from mature implementations

**Dify** was inspected at
[`a83ce881a1285fa2ca95a34d615ee7e38b0f69d7`](https://github.com/langgenius/dify/tree/a83ce881a1285fa2ca95a34d615ee7e38b0f69d7).
Its [App key service](https://github.com/langgenius/dify/blob/a83ce881a1285fa2ca95a34d615ee7e38b0f69d7/api/services/app/api_key_service.py)
separates configured application scope from key issuance. Its
[workflow controller](https://github.com/langgenius/dify/blob/a83ce881a1285fa2ca95a34d615ee7e38b0f69d7/api/controllers/service_api/app/workflow.py)
lets the caller select JSON or streaming explicitly and provides scoped
persistent-result lookup. Its
[event endpoint](https://github.com/langgenius/dify/blob/a83ce881a1285fa2ca95a34d615ee7e38b0f69d7/api/controllers/service_api/app/workflow_events.py)
can rebuild node-state snapshots; that is not proof of exact token replay.
Its workflow stop handler passes no end-user owner constraint and reports a
generic success response. That behavior should not be copied as Centaeris's
authorization or cancellation-confirmation contract.

**LangGraph SDK**, inspected at
[`5847733bcb186c26b32da5cc822eaf8a89deffeb`](https://github.com/langchain-ai/langgraph/blob/5847733bcb186c26b32da5cc822eaf8a89deffeb/libs/sdk-py/langgraph_sdk/_async/runs.py),
separately exposes disconnect policy, resumable streaming, and retention of a
stateless run's thread. Its ordinary join-stream documentation also warns about
unbuffered prior output. The useful lesson is explicit lifecycle separation,
not that every reconnection guarantees replay.

**Stripe's [idempotency contract](https://docs.stripe.com/api/idempotent_requests)**
illustrates opaque client keys, request-parameter matching, and a finite
retention boundary. Its v1 permits a new request after a pruned key is reused.
That differs from Centaeris's timestamp-based stale rejection. Borrow the
explicit contract and tests, not an unsupported claim of permanent or
tool-level exactly-once execution.

These are architectural comparisons; no third-party code is copied.
