# Testing

Package license scope is checked with `python -B scripts/test_product_licensing.py`:
parsed TOML/JSON manifests must match `LICENSING.md`, and Apache packages must
not depend on AGPL workspace packages. This check requires no Cargo installation
or dependency cache and runs in both Node and Rust gates. After Rust dependencies
are prepared, `python -B scripts/test_product_licensing.py --crate-payloads`
also checks that independent crate payloads retain license and README files,
including the model catalog's original logo notices.
`node --test scripts/product-licenses.test.mjs` checks distribution license
assembly and rejects missing or changed notices. Desktop and TUI packaging use
the same assembly and validation helper; container builds copy the same license
texts and scope statement. A public release still requires the complete release
gate and inspection of its final artifacts.

## Test quality

Reuse existing behavioral coverage before adding tests. Add a focused regression
when a reported defect or new behavior lacks coverage; refactors, extractions and
component splits do not automatically require new characterization tests.
Consolidate overlapping cases around their observable contract.

During implementation, run directly affected tests and necessary compile or type
checks. Reuse passing results while their relevant inputs remain unchanged. A
commit, handoff or change of agent does not require another test run. The lead
coordinates integrated validation at a stable candidate; full suites, query-loop
gates and end-to-end acceptance run when affected semantics or the actual CI and
release requirements call for them. Generic skill checklists do not expand the
agreed scope. Sub-agents receive a bounded validation scope and report existing
results for reuse instead of repeating each other's checks.

The hosted Python CI runs support and cross-language checks separately from three
API test shards. Shards discover the complete API suite, assign whole test classes
by case count, and preserve Django's execution order. Each job owns its database
and temporary storage. All shards remain required; unsharded local execution
continues to run the complete suite. Skipped tests and discovery/execution
mismatches still fail the gate. Fresh-schema and migration-drift checks run once
in the first API shard. Each support gate reports its elapsed time.

Frontend visual design uses manual appearance acceptance rather than TDD.
Implement and iterate on layout, icons, spacing, color and typography directly.
Focused behavioral tests still protect parsing, validation, requests, state
transitions, streaming, recovery, accessibility interactions and content
preservation. Do not turn visual preferences into pixel or class-name assertions.

Pure declarative configuration and catalog-data updates do not require new tests
or a red/green TDD cycle when implementation semantics stay unchanged. This
includes model IDs, provider endpoints and protocol selections, context/output
limits, reasoning presets, and capability flags. Verify authoritative values,
validate the format/schema, and run directly affected existing checks. Do not
copy mutable catalog facts into a second provider or model inventory in tests.
Consumer tests should verify that the catalog is correctly projected and applied.
Parsing, validation, request construction, routing logic, and persistence changes
still require focused behavioral tests.

Remove source-name and formatting checks that do not protect an observable
contract. An empty implementation passing a check, or a harmless comment or
formatting change failing it, is evidence that the check is insufficient.
Preserve real security, protocol, and behavioral coverage rather than replacing
one source-pattern checklist with another. Package managers and compilers own
their configuration syntax; tests should exercise the resulting behavior.

Library note conditional commits, immutable content reads, migration/rollback,
history capacity and browser draft recovery are covered in
[LibraryNotes.md](LibraryNotes.md). PostgreSQL tests observe real commit and
independent-connection contention; Chromium acceptance exercises actual IndexedDB
across reload. Failure injection must preserve previously acknowledged content
and must not let an old response claim newer edits are saved.

Storage contract consolidation and the file-backed asynchronous SQLite driver
validation are described in [StorageExecution.md](StorageExecution.md). The narrow
Job result and recovery contract is in [JobStorage.md](JobStorage.md). Its driver
tests run in the ordinary `centaeris-runtime-sqlite` suite; the separately compiled
`panic=abort` executable checks abrupt database-worker exit without relying on
unwind cleanup. Both paths use the same adapter transaction implementations.

Run failure presentation uses shared input/output cases in
`tests/fixtures/run-error-display.json`. Web validates the complete mapping and
localized English copy against the local presentation catalog; Desktop tests
actual rendered live and historical notices, replay deduplication, ordinary
notice preservation and referenced-content retry. TUI tests rendered terminal
notices, history replacement, plain-text wrapping, multi-page UTF-8 reads and
stale/invalid reference rejection. Start/completion and existing tool/result
coverage remain independent. Error UI tests assert visible content and behavior,
not component or CSS names.

Tests protect observable behavior and documented contracts, not the spelling of
the current implementation. A refactor that preserves behavior should not need
test changes merely because an internal function, type, variable, CSS class, or
file was renamed, moved, or split.

Removing an implementation is not a reason to add its names to a source
blacklist. For example, replacing a history reader should be protected by the
actual request sequence, pagination, reconnect, and failure behavior, rather
than by searching source files for the former reader's name. Likewise, removing
a UI component should be verified through the resulting interactions and
visible output, not by requiring its old file to remain absent forever.

Negative assertions remain appropriate for real contracts: unauthorized requests
must fail, credentials must not appear in output, unsupported protocol fields
must be rejected, and Core must not depend on a storage adapter. State the
contract being protected. Prefer dependency graphs, parsed structure, generated
registries, and public protocol results over source text matching. A comment
mentioning a prohibited dependency must not be mistaken for an actual import.
Static checks supplement behavioral coverage; they do not establish that a
feature works merely because certain source text is absent.

Web and Desktop message-scroll tests execute both controllers against the same
anchoring, reserved-space, manual-reading, return-to-latest and reset contract.
Host animation adapters may differ. Before replacing the former file-equality
assertion, an ignored-pause negative control was verified to fail for both hosts.

Choose UI scenarios from observable risks such as wrapping, clipping, keyboard
navigation, zoom, localization, and layout transitions. Viewport dimensions are
test inputs, not correctness criteria. Assert useful relationships such as
visibility, alignment, and containment rather than incidental class names or
exact pixel values unless those values are an explicit product contract.

Before replacing a brittle test, identify equivalent existing coverage or add
the smallest missing behavioral test. Retain security and public-contract checks
when consolidating tests; a separate mutation campaign or new validation
framework is not required to remove an incidental assertion. Do not
implement this policy as another blacklist of source patterns in tests; enforce
it through review and evidence.

After closing a PostgreSQL client, observe backend and advisory-lock removal
with a bounded deadline: client close does not synchronize `pg_stat_activity`.
Continue asserting both counts reach zero, and use a retained-connection
negative control to prove that a real leak still fails.

Keep unrelated outbound calls outside controlled timeout fixtures. The real
Core/HTTP admission-response tests isolate return-job scheduling, whose real
HTTP/store recovery has separate coverage; a closed port's platform-dependent
connection delay must not determine whether the response-loss window is reached.
Upload alias tests retain their no-refund assertion on both platforms: Unix uses
a dangling symlink, and Windows uses a dangling directory junction without
requiring the symlink privilege. Neither case skips the resource-safety check.

HTTP transport refactors reuse coverage of actual request dispatch, rejection
order, response envelopes/headers and cancellation/timeout resource ownership;
add characterization tests only for observable behavior not already covered.
Protocol-function and storage tests remain necessary but do not substitute for
the complete HTTP path. For hosted Runtime endpoints, use the protected
behaviors and coverage gaps in [Runtime HTTP](../workspace/reference/RuntimeHTTP.md).

For Django schema refactors, compare the generated OpenAPI and both validation
and serialization schemas before and after, in addition to request tests.
`test_schema_wire_contract.py` covers nested camelCase fields, strict types,
exceptional aliases and unset/null/false distinctions. `test_http_error_contract.py`
exercises shared-path method routing and public error statuses through Django
and Ninja. `test_runtime_http_client.py` uses a loopback HTTP server to check
request bytes, operation-specific errors and absence of automatic retries.
Schema parity does not replace authorization, persistence or cancellation tests.
`test_http_query_contract.py` protects scalar query multiplicity, required fields,
framework validation precedence, authorization order and repeatable transcript
sequences through real Django requests. Keep both valid and invalid repeated
values: Ninja may reject a value before the route checks multiplicity.
`test_transcript_runtime_client.py` exercises generated block samples through the
page response boundary, rejects malformed nested types and separately protects
integer representation/range and UTF-8 byte limits beyond schema validation.
For loopback HTTP/HTTPS fixtures, isolate inherited proxy settings in the test
process. `NO_PROXY` alone may not prevent HTTP clients from initializing optional
proxy transports; do not change machine-wide proxy settings to run these tests.

Keep domain effects under their existing real store/provider tests; add only the
smallest observation seam needed for controlled blocking or write detection.

Hosted model gateway tests exercise real SDK requests with synthetic HTTP
responses: credentials work without an application quota binding, upstream
429/503 metadata remains available, and subsequent requests are neither retried
nor delayed by application-wide provider cooldown. The migration test preserves
encrypted credentials, model configuration and application capacity records.
Real HTTPS/ASGI tests retain coverage for ORM connection return, caller-owned
transactions and stream cancellation. See the
[hosted model gateway boundary](../workspace/architecture/HostedModelAdmission.md).

## Web presentation delivery

[Web presentation delivery](../workspace/architecture/WebPresentation.md) defines
command acceptance, execution, applied SSE cursors, navigation snapshots and
owner read acknowledgements. Its fault matrix distinguishes local synthetic
coverage from production/WAF acceptance. Use an isolated synthetic PostgreSQL
instance configured through `TEST_POSTGRES_*`, never an application database.

```sh
uv run --frozen --package api python packages/api/manage.py test \
  app_core.test_presentation_stream app_core.test_presentation_authority \
  app_core.test_session_attention \
  app_core.test_session_attention_migration app_core.test_schema_wire_contract \
  app_core.test_workspace_session_list_queries app_core.test_session_projection_concurrency \
  --settings=api.test_settings --noinput
pnpm --filter web exec node --test \
  tests/unit/workspace-presentation-stream.test.mjs \
  tests/unit/api-deadlines.test.ts tests/unit/operation-receipts.test.mjs \
  tests/unit/session-read-state.test.ts
```

The notification migration reconstructs durable watermarks while preserving the
ambiguous legacy unread boolean as an explicit manual choice. Migration tests
must protect existing facts, read history and the forward-only reversal guard.
Concurrent HTTP ACKs and repeated terminal projection must not regress read state.
The ordinary Session-list query-growth test also protects batched discovery from
introducing one database query per Session.

The manual-unread storage rename is covered by
`test_session_attention_characterization` and
`test_session_manual_unread_migration`. Preserve the old field in historical
migration fixtures; assert stored choices, read watermarks, revisions, recency
and source facts survive the forward rename, and that writes to the renamed
column still invalidate navigation. Automatic read ACK and explicit mark-read
commands retain different effects. Compare generated OpenAPI plus validation and
serialization schemas across this storage-only change.

Removing a display invalidation axis must retain fresh authorization while an
SSE producer is blocked and before prepared payloads are emitted. The migration
coverage seeds legacy global versions and pending rows and verifies that only
those derived rows are removed while useful scoped invalidations still work.

Session listing and navigation-only SSE authorization share a fully filtered,
unevaluated query scope. Characterization coverage preserves empty-list access,
active-user and definition visibility checks, delegation credential versions,
membership replacement, native branch bindings, public rejection statuses and
request grant/branch refreshes. Cost and side-effect tests observe actual reads
and writes: repeated authorization must not materialize navigation rows or
advance attention projections. Actual navigation reads still refresh and return
the complete allowed snapshot. These checks establish behavior and removed work,
not an unmeasured latency or capacity claim.

For changes limited to this authorization/list boundary, run:

```sh
uv run --frozen --package api python packages/api/manage.py test \
  app_core.test_presentation_authority app_core.test_workspace_session_list_queries \
  app_core.test_http_query_contract app_core.test_http_error_contract \
  --settings=api.test_settings --noinput
```

## Session workspace authority and stopped-instance preservation

Use the isolated synthetic PostgreSQL configuration through `TEST_POSTGRES_*`:

```sh
uv run --frozen --package api python packages/api/manage.py test \
  app_core.test_session_workspace_authority \
  app_core.test_session_workspace_authority_migration \
  app_core.test_session_workspace_retention \
  app_core.test_session_workspace_preservation \
  app_core.test_workspace_snapshot_publication \
  app_core.test_workspace_snapshot_publication_migration \
  app_core.test_workspace_snapshot_reachability app_core.test_workspace_snapshot_roots \
  app_core.test_workspace_snapshot_download_roots app_core.test_gc_failure_control \
  app_core.test_workspace_snapshot_download_encoding \
  app_core.test_execution_publication_fencing app_core.test_environment_lifecycle \
  --settings=api.test_settings --noinput
```

The migration distinguishes a committed baseline from missing legacy evidence;
new Sessions explicitly start with an empty baseline. Unknown legacy state must
not produce signed empty-workspace authority. The idle-time test commits real
snapshot bytes, advances the fixture by 35 days, runs GC, and admits a continuation
of the original Session while preserving its history, attachment identities,
membership, profile and prior signature.

Preservation tests exercise terminal cleanup authority, exact allocation and
original grant bindings, lease replacement during streaming, empty captures,
atomic immutable receipts, lost acknowledgements and newer-head CAS conflicts.
Legacy archive fixtures also verify that nonempty and explicit-empty frame
publication update the head encoding atomically and preserve the old archive.
These tests use synthetic physical records. Physical stopped-state inspection, bounded
export without starting the guest, actual restore into a replacement instance,
and deployed original-Session continuation remain separate acceptance gates.

Reachability fixtures preserve heads, protected frozen baselines and selected
Runtime roots while reclaiming old generations and unselected checkpoint bytes.
They check unknown baselines, witness changes, grace, dry-run, an old open stream
with a valid lease, outstanding preservation ownership and metadata retirement.
Publication tests cover GC followed by legitimate identical-content reuse,
rejection of the older upload/GC token, retry rearming, and a second successful
collection. Download tests reject a historical checkpoint which Core no longer
selects. Encoding tests require the exact `X-Workspace-Snapshot-Encoding` header
from the locked Session head or immutable frame-stage contract, preserving stored
bytes, SHA-256 and size. Runtime's separate gates validate Core root selection and the shared
SQL witness; these API fixtures isolate that dependency and exercise its fences.

## Focused Core gate

Rust implementation changes run at least:

```powershell
cargo test --locked -p centaeris-core query_loop
```

Add the smallest focused test owned by the changed contract or adapter. Core
tests runtime semantics; adapter tests cover storage, transport, and platform
integration.
Configuration-only changes, including updates to existing Rust test expectations,
use the configuration validation scope above and do not trigger this broader gate.

Hosted Redis live-stream changes also run the ignored `transient_stream` Redis
tests against a disposable loopback Redis service with `MEMORY USAGE`, a 32 MiB
`maxmemory` limit, `maxmemory-policy noeviction`, and persistence disabled:

```powershell
$env:TEST_REASONING_REDIS_URL = 'redis://127.0.0.1:<disposable-port>/0'
cargo test --locked -p runtime_server transient_stream::tests::redis_ -- --ignored --nocapture --test-threads=1
```

Never point this fixture at a deployed Redis service. The tests protect exact
wake retention, latest complete text/reasoning recovery, revision identity,
expiry and sealing, and two active streams whose retained memory remains
proportional to their latest snapshots. The shared
`tests/fixtures/agent-run-transient-signals.json` corpus checks Rust producer/API
decoder parity. API `test_transient_stream.py` and the existing Session stream,
reasoning, and authority suites protect SSE snapshots, reconnect, committed
cursors and durable terminal reconciliation during Redis failure. These bounded
regressions do not establish a production concurrency limit or benchmark result.

## Full local gate

```sh
python3 scripts/ci.py Source --frontend-tests
```

On Windows use `python`. Windows distribution acceptance additionally runs
`pwsh -File scripts/ci.ps1 Release`.

The gate checks formatting, the Rust workspace, Clippy with warnings denied,
focused Core and SQLite integration, all Rust tests and Desktop/UI source checks.
Packaging and packaged-application smoke checks remain in the Windows release stage.

Shared frontend primitive changes also run `pnpm --filter web run test:components`.
These mounted React tests protect controlled model/effort choices, focus,
dismissal, confirmation and drafts. Web retains its own page and transcript
interaction policy. With `CHROME_BIN` pointing to a local Chromium executable,
`pnpm --filter web run test:chat-ui` checks production styles, floating-control
placement, transcript folding and reading position in the browser. The shared
package has its own `typecheck` script, run by both frontend product gates.

## Test data

Tests must use temporary roots and synthetic credentials. They must not read a
developer's real `~/.centaeris`, production data, private extension content, or
customer files. A test result is evidence for the exact tested source tree; it
is not a source artifact and should not be committed.

## Clean-clone gate

Before release, run the same gate from a clean clone of the exact candidate
revision. A passing dirty working tree does not prove that ignored files,
adjacent repositories, or previously built binaries are unnecessary.

Work-return publication changes protect three behavior boundaries: history-independent normal job traffic; shared audit leasing, restart cursors and stale-owner rejection; and omitted/unknown schedule repair without losing source records. The immutable previous publisher characterization and history-count RED establish the replaced behavior; worker/API tests cover deadlines, partial progress, cancellation and transport outcomes. No source-name absence checks enforce this change.
