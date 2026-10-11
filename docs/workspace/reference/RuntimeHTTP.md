# Runtime HTTP

This document inventories the HTTP surface owned by `packages/runtime_server`
and the behavior that a transport refactor must preserve. It describes the
current implementation. Framework reuse does not establish a performance gain.

Django owns product identity, workspace access, admission receipts, files and
credentials. Runtime HTTP adapts those accepted operations to Core and the
hosted stores. Core owns runtime semantics and does not depend on axum.
Browser REST, SSE and Django-owned `/internal` routes remain in [API.md](API.md).
An `/internal` prefix alone does not identify which service owns an endpoint.

## Code navigation

| Concern | Current source |
| --- | --- |
| Connection serving, bounded request reading and response headers | [main.rs](../../../packages/runtime_server/src/main.rs): `serve_http`, `read_axum_request`, `http_response` |
| Explicit routes, authentication, admission and blocking-task ownership | [http.rs](../../../packages/runtime_server/src/http.rs) |
| AgentRun HTTP operations | [agent_runs.rs](../../../packages/runtime_server/src/http/agent_runs.rs) |
| Catalog, package inspection and host-input HTTP operations | [catalogs.rs](../../../packages/runtime_server/src/http/catalogs.rs) |
| HTTP transport characterization | [runtime_http_tests.rs](../../../packages/runtime_server/src/runtime_http_tests.rs) |
| Request capacity and deadlines | [request_capacity.rs](../../../packages/runtime_server/src/request_capacity.rs) |
| Job and outbox operations, wire types and validation | [job_protocol.rs](../../../packages/runtime_server/src/job_protocol.rs) |
| Transcript reads and projection checks | [transcript_protocol.rs](../../../packages/runtime_server/src/transcript_protocol.rs) and [wire.rs](../../../packages/runtime_server/src/transcript_protocol/wire.rs) |
| AgentRun request types and signed-start validation | [contract.rs](../../../packages/runtime_server/src/contract.rs) and [agent_run_authorization.rs](../../../packages/runtime_server/src/agent_run_authorization.rs) |
| Bounded timing labels and request accounting | [observations.rs](../../../packages/runtime_server/src/observations.rs) |
| Django callers | [runtime_client.py](../../../packages/api/app_core/runtime_client.py) and [runtime_job_client.py](../../../packages/api/app_core/runtime_job_client.py) |
| Worker callers, terminal dispatch and waiter reconciliation | [worker.py](../../../packages/worker/worker.py) |

The service uses axum with Hyper HTTP/1. Its Router registers each operation
explicitly. Supported operations and fallback share admission, bounded body
reading and synchronous execution on `spawn_blocking`. The original
method, URI and `HeaderMap` are retained, and bounded body bytes are moved into
the request without a second body copy. Header text and duplicate checks still
precede dispatch. Responses use axum's `Response` and `IntoResponse` directly.
Routes call the concrete protocol operation; job and transcript modules do not
dispatch method/path pairs again. The wire types and validators are authoritative;
this inventory does not duplicate their field lists.

## Endpoint inventory

There are 29 method/path entries, including the parameterized job read. All
require `X-Internal-Token` when reaching a supported operation. Admission and
body/header validation occur before that token check. Successful responses are
JSON; the status column below describes success, not every failure outcome.

### AgentRun lifecycle

Requests use the corresponding types in `contract.rs`. Signed AgentRun
identity, lifecycle binding and current storage fences remain domain checks.

| Method | Path | Responsibility | Success |
| --- | --- | --- | --- |
| POST | `/agent-runs/step` | Validate the lifecycle lease and execute or resume the Run; return its disposition and terminal/waiting information | 200 |
| POST | `/agent-runs/cancel` | Persist a scoped cancellation request or return the existing terminal state; acceptance does not establish a new terminal fact | 200 |
| POST | `/internal/agent-runs/supplement` | Admit or replay a supplement in the existing bounded Run queue; return its identity and queue revision | 202 |
| POST | `/agent-runs/teardown` | Require a valid lifecycle lease and terminal Run, close supplements, settle child jobs and remove the sandbox/context | 200 |

### Jobs

Request types, schema identities, worker-kind restrictions and transition
validation are in `job_protocol.rs`. Worker kinds come from the shared
[worker_jobs.json](../../../packages/worker/worker_jobs.json) contract.

| Method | Path | Responsibility | Success |
| --- | --- | --- | --- |
| POST | `/internal/jobs/schedule` | Schedule or replay an allowed worker job with its durable identity, payload reference and applicable tenant binding | 200 |
| POST | `/internal/jobs/claim` | Claim due allowed worker jobs under leases and shared execution capacity | 200 |
| POST | `/internal/jobs/wait` | Wait for eligible worker jobs or the bounded wait deadline; return readiness and the next due time | 200 |
| POST | `/internal/jobs/start` | Start a claimed job under its lease owner | 200 |
| POST | `/internal/jobs/heartbeat` | Renew a job lease under its owner | 200 |
| POST | `/internal/jobs/yield` | Requeue the same job with its run time and transition reason under the current lease | 200 |
| POST | `/internal/jobs/complete` | Complete a leased job, enforcing its output contract and equivalent completed replay | 200 |
| POST | `/internal/jobs/fail` | Apply retry policy or atomically dead-letter and fail the job | 200 |
| POST | `/internal/jobs/reconcile` | Reclaim expired leases; do not replay acknowledged terminal notifications | 200 |
| GET | `/internal/jobs/{jobId}` | Read one validated job identity and its durable state | 200 |

The GET implementation derives its identity with
`trim_start_matches("/internal/jobs/")` and validates the result; it is not a
one-segment axum `Path` extractor. Invalid identities return 400 `job_id_invalid`.
Characterize repeated-prefix targets as well as ordinary malformed suffixes.
For example, GET on `/internal/jobs/claim` reads the identity `claim`, while POST
on that path is the claim operation.

### Terminal outbox and waiter recovery

These routes use the durable outbox and waiter relationship index. Publishing
an acknowledgement, waking execution and scanning for missed wakes are distinct
operations. Their exact request types remain in `job_protocol.rs`.

| Method | Path | Responsibility | Success |
| --- | --- | --- | --- |
| POST | `/internal/job-outbox/pending` | Read a bounded page of pending terminal notifications | 200 |
| POST | `/internal/job-outbox/published` | Acknowledge one notification under its generation | 200 |
| POST | `/internal/job-outbox/wake-waiter` | Process a bounded page of relationships for one terminal source job | 200 |
| POST | `/internal/job-outbox/reconcile-waiters` | Scan bounded durable relationship pages for late or missed wakes | 200 |

### Catalog and package inspection

These operations use `main.rs` wire types and the existing model, plugin,
skill, MCP and hook implementations. Discovery consumes an activation snapshot;
it does not itself install a plugin or execute its tools.

| Method | Path | Responsibility | Success |
| --- | --- | --- | --- |
| GET | `/internal/model-catalog` | Return the Rust-owned model catalog envelope | 200 |
| GET | `/internal/execution-profile` | Return the configured immutable execution image identity and capability | 200 |
| POST | `/skills/catalog` | Build the skill catalog for the supplied activation snapshot | 200 |
| POST | `/skills/detail` | Return one skill's metadata and content from that snapshot | 200 |
| POST | `/mcp/catalog` | Build the MCP catalog for the supplied activation snapshot | 200 |
| POST | `/hooks/catalog` | Build the lifecycle-hook catalog for the supplied activation snapshot | 200 |
| POST | `/internal/plugins/inspect` | Inspect a validated upload staging path and return the parsed plugin package | 200 |

### Transcript and host input

Transcript requests use Core-derived types or `transcript_protocol/wire.rs`.
The host-input request uses `HostEventInputRequest` in `contract.rs`.

| Method | Path | Responsibility | Success |
| --- | --- | --- | --- |
| POST | `/internal/transcript/page` | Read a bounded page against the validated projection generation and source waterline | 200 |
| POST | `/internal/transcript/patches` | Read committed patches within the requested generation and waterline range | 200 |
| POST | `/internal/transcript/content` | Read a validated, session-scoped event content range | 200 |
| POST | `/internal/host-event-input` | Validate and construct a canonical Core host input; this endpoint does not append it or execute a Run | 200 |

## Behavior to preserve

### Admission, deadlines and connection ownership

Capacity is per Runtime process. Deployment inputs and defaults remain owned by
[Configuration.md](Configuration.md); the route distinctions are:

| Requests | Capacity lane | Handler response deadline |
| --- | --- | --- |
| POST cancel, POST heartbeat, every GET path beginning `/internal/jobs/` | Reserved control | Configured control deadline; permit acquisition uses the same absolute deadline |
| POST `/internal/jobs/wait` | Separate listener | 25 seconds; the validated job wait itself is at most 20 seconds |
| POST `/agent-runs/step` | Ordinary | No short handler deadline; execution uses its lease and cancellation protocol |
| Other requests, including unknown paths | Ordinary | Configured ordinary deadline |

Ordinary and listener requests acquire permits without waiting; saturation
returns 503 `runtime_busy`. Control requests may wait within their deadline.
The handler deadline begins before permit acquisition and body reading, not
when the business function starts. Body reading additionally has a 30-second
timeout and a 1 MiB limit, including for GET and unknown routes.

The blocking closure owns the request permit, observation lifetime and
PostgreSQL request deadline. An HTTP timeout or dropped connection does not
cancel already-started blocking work, refund its capacity, or prove rollback.
Control requests also use `execution_control_view()` so the reserved database
path extends through cancellation preflight reads. A blanket Tower timeout or
concurrency layer does not replace these ownership rules.

The custom Hyper connection loop bounds connections at 128, limits headers to
64 and the header buffer to 64 KiB, applies a 10-second header-read timeout and
a 30-second stalled-write timeout, and disables keep-alive. Preserve these
controls alongside explicit axum routes; changing the serving helper is
a separate transport change.

### Request rejection and authentication order

- Routing preserves complete-target rejection, while capacity and observation
  labels use `uri().path()`. Adding a query string to an otherwise supported
  fixed path prevents dispatch. A job GET query becomes part of its suffix and
  fails identity validation. axum normally matches only the path; do not
  silently expand the accepted request targets or decode previously rejected
  percent-encoded job identities.
- The body reader rejects non-text header values and duplicate values for any
  header name, including differently cased names. It currently maps body-read
  errors to 413 `request_body_too_large` and body timeouts to 408
  `request_body_timeout`. Direct use of `HeaderMap` must retain these checks.
- JSON parsing currently ignores Content-Type and uses the existing serde
  types, exact schema checks and per-operation validation errors. axum's
  default `Json` extractor requires a JSON Content-Type and can return 422 for
  a structurally invalid JSON value. Replacing parsing with that extractor
  would change behavior; keep bounded bytes plus existing validation until
  equivalent rejection handling is demonstrated.
- Body/header validation precedes authentication. Job protocol authentication
  covers every target starting with `/internal/jobs` or `/internal/job-outbox`,
  including unknown endpoints and unsupported methods. Its missing/empty token
  configuration returns 500 `internal_token_unavailable`; a mismatched request
  token returns 401 `unauthorized` before route rejection.
- Outside those prefixes, unknown paths and unsupported methods return 404
  plain-text `not_found` before token lookup. Supported non-job routes check
  the token afterward; unavailable token configuration propagates to the
  outer 500 `internal_error` response. The non-job check does not have the job
  protocol's explicit empty-token configuration guard. This difference is
  recorded for characterization, not endorsed as a new security policy.
- After successful job-prefix authentication, unmatched operations return 404
  JSON `job_protocol_not_found`; GET suffix validation can instead return 400.
  Method routing uses the shared fallback instead of axum's default 405 or
  `Allow` header, and HEAD cannot invoke a GET operation. The Router matrix
  covers unknown-prefix, trailing-slash, malformed suffix and HEAD cases.

Changing one of these rejection rules requires a separate behavioral change
with focused evidence. A routing cleanup must not silently normalize it.

### Responses, diagnostics and domain effects

`http_response` sets Content-Type and `Connection: close`, and adds
`Retry-After: 5` for 429 and 503. Preserve error envelopes, operation-specific
error strings, successful statuses and optional-field presence; equivalent
JSON object key ordering is not a contract. Unexpected handler failures become
500 `internal_error`; AgentRun step failure responses retain their structured
failure class, retryability and transition reason.

`observations.rs` emits opt-in `workspace.perf.v1` timing records with a bounded
route-label whitelist; other paths are `other`. It retains admitted/rejected
and actual release accounting. `MatchedPath` or `TraceLayer` must not silently
replace that event format, widen its labels or introduce request bodies,
credentials or unsanitized paths into observations.

Core validation, signed authorization, current lease fences, job idempotency,
terminal facts, supplement identity, projection generation, durable waiter
recovery, storage admission and Docker residency are application behavior.
HTTP extractors and middleware do not own or replace them. This review includes
shared worker kinds, worker pending admission and residency-aware teardown from
the current workspace; they remain protected behavior during routing cleanup.

## Framework reuse and remaining boundaries

| Area | Existing capability to use | Smallest useful change and boundary |
| --- | --- | --- |
| Repeated method/path dispatch | axum `Router`, explicit handlers and method routing | Implemented: register each supported operation once and call its business function directly; retain the documented fallback and rejection behavior |
| Owned method/path/header copies | axum `Request<Bytes>` and `HeaderMap` | Implemented: retain native request parts and the bounded body; validate headers without cloning names and values into another map |
| Custom response structure and conversion | axum `Response` and `IntoResponse` | Implemented: construct the framework response once with the existing headers; error JSON uses the infallible serialization of an already constructed `serde_json::Value` |
| Repeated internal-token checks | Shared admission boundary | Implemented: one token comparison retains prefix coverage, rejection order and current configuration-error distinctions |
| Repeated lane predicates and observation lane selection | Route-local policy plus the existing admission function | Give supported routes their lane policy once; keep unknown-route admission and actual blocking-task permit ownership explicit |
| Hand-maintained route labels | axum `MatchedPath`, if equivalent | Reuse only when mapped to the existing bounded labels and tested observation contract; no observability redesign is required for routing cleanup |

Catalogs, transcript reads, jobs/outbox and AgentRun lifecycle operations share
one concrete admission/body/blocking boundary. Keep their domain functions and avoid a
new generic endpoint framework, route-registration DSL, universal validation
trait, or one abstraction/file for every operation. Split by cohesive protocol
only where that makes the call path shorter and easier to inspect.

Accept simplification by the final call path and total production code removed,
including new wrappers and metadata. Moving branches into another file or
adding a second route registry does not reduce the maintenance burden. No
database-driver migration, scheduler rewrite or Django replacement is implied.

Framework behavior was checked against the locally resolved axum source;
upstream navigation: [Router](https://docs.rs/axum/latest/axum/struct.Router.html),
[Json](https://docs.rs/axum/latest/axum/struct.Json.html),
[GET method routing](https://docs.rs/axum/latest/axum/routing/method_routing/fn.get.html)
and [Tokio blocking-task ownership](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).
Use the version pinned by `Cargo.toml` and `Cargo.lock` when implementing changes.

## Verification and maintenance

Transport characterization covers response status/body/header parity, escaped
error text, structured step failures, a real Hyper error response, the exact
body limit, body-before-header rejection order, duplicate/non-text headers,
header name normalization, Content-Type independence and retained query targets.
These assertions ran green before replacing the custom request/response types.
The Router characterization reads all 29 method/path entries from this document
and exercises the production Router with configured, missing and empty server
tokens in isolated processes. It covers method/query/fallback rejection and
prohibits database checkout for rejected requests. A controllably blocked
operation verifies that timeout and disconnect retain capacity until completion.
These tests ran green before and after replacing dispatch. PostgreSQL outbox,
worker-pending and transcript HTTP fixtures also use the production Router;
their durable effects still require the dedicated PostgreSQL gate. Router
rejection coverage does not establish every successful domain transition.
Existing coverage also includes incomplete-header timeouts, request-lane
isolation, exact wire-type validation and catalog envelopes.

Before a transport refactor, add the smallest missing observable coverage:

1. Actual Router/HTTP requests covering the inventory, methods, unknown paths,
   job-prefix fallbacks, suffixes, trailing slashes, query strings, percent
   encoding and HEAD. Compare status, envelope and relevant headers.
2. Valid/missing/wrong token cases, isolated unavailable/empty configuration,
   duplicate/non-text headers, Content-Type variants, malformed JSON, unknown
   fields and unsupported schemas. Assert rejection order with compound-invalid
   requests and assert that rejected requests perform no domain writes.
3. Saturated execution and listener work while cancellation/heartbeat/job reads
   remain available through their reserved HTTP and database paths. With a
   controllably blocked operation, prove timeout and disconnect retain its
   capacity until the operation actually exits and do not assert rollback.
4. Existing real store/provider tests for signed Run binding, lease reclaim,
   equivalent replay, terminal teardown, supplement admission, projection
   invalidation and outbox/waiter recovery. Do not replace durable-effect tests
   with mock HTTP successes.

Use the smallest dependency-isolation seam only where an operation must be
blocked or observed. Keep characterization green before and after each protected
boundary; do not assert source names, removed symbols or a required file layout.
Every Rust change runs the focused Core `query_loop` gate and the applicable
[Core](../../eval/ReleaseGate.md) and [Workspace](../eval/ReleaseGate.md) gates.
Record local verification separately from CI and deployment.

Maintain endpoint purpose and transport behavior here, product semantics in
`API.md` and `Architecture.md`, limits in `Configuration.md`, and test policy in
[Testing.md](../../development/Testing.md). A method/path, purpose, rejection,
lane or deadline change updates its owning document in the same change. Exact
wire fields remain derived from their existing Rust types/generated contracts.
The Router characterization sends the documented method/path entries through
the registered operations, rather than maintaining another executable registry
or testing source-text spelling. Add new entries and their observable success
and rejection coverage together; this table is not an automated route export.
