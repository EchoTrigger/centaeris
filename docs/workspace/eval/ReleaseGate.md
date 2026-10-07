# Workspace release gate

The prepared Agent input ledger is not an execution-intake or Read gate pass.
Before integrating this capability, require the following behavioral evidence
against the frozen Core contract:

- Same scoped ID/body replays one retained fact; conflicting content fails and
  concurrent distinct inputs receive one server order without dropped bodies.
- Queue ACK and restart preserve exact original text in the same carrier.
- Successful main-model uptake exposes exactly the actual input IDs and the
  committed fact reference/time; admission, claim, ACK and failed uptake do not
  establish Read. A batch may consume multiple inputs for one reply.
- Idle, active and Final-race paths hand off in one serialized coordination chain
  with no fork, dropped input or duplicate acceptance. Already generated
  `send_message` output may commit before newly arrived input is taken up.
- Ordinary text does not approve, answer or release a special wait.
- Active native work-return uptake preserves notice consumption exclusivity;
  multiple immutable attempts may bind one Run without replacing its original
  initial input. The FK migration preserves existing facts and refuses unsafe
  reversal once multiple notices use one Run.
- Current ACL and original execution membership are rechecked. Handled
  confirmation is added only after these input identity boundaries pass.
- Native `confirm_work_return` uses exactly `notice_id` and `attempt_id`; the
  accepted Run and original source identities are supplied by the server.
  Read-only validation and a committed call alone must remain unhandled.
  Require actual Core/provider/PostgreSQL evidence for split call/result commits,
  both native and user initial inputs, stable confirmation projection and
  `continueTurn`. Rejected stale leases, transaction rollback, forged success
  receipts and authority revoked after validation must leave no successful
  confirmation result. Original membership revocation/rejoin never revives it.
  Failed results, ordinary Final, `send_message`, ACK and Read are not confirmation.
  The extra commit guard applies only to this tool's successful result; retain
  other tools' behavior and admission/Session/job lock order. Existing
  `get_work_request` response carries notice, attempt and confirmation evidence
  without new query parameters, a new get tool, handled storage or scan thread.
- Input acceptance and every production output append share the same Session
  transaction lock. Immutable waterline anchors replay unchanged; one-snapshot
  pagination cannot skip a concurrent input when output commits. Full scoped
  cursor positions advance over ignored candidates without timestamp sorting.
- Unanchored legacy inputs and retained owner settings are not guessed or lost
  by migrations. New/default Agents require configuration; no model fallback
  is inferred from browser preferences, catalog order or recent Sessions.
- Owner settings apply to new coordinator Run authorization snapshots. Existing
  Runs retain their original model/effort when settings change or are cleared.
  Work-return initial-input validation must use the admitted Run snapshot, not
  require it to match a previous source Run's model.
- Changing the Agent setting cannot launder source signature, model/effort or
  original membership tampering. An old source model becoming unavailable does
  not override an otherwise valid new Agent coordinator policy.
- Generic messages cannot start/fork a dedicated coordinator, even with the
  same model. A binding created during profile resolution is rechecked before
  Run/receipt insertion. Ordinary work Sessions keep their explicit model path.

Run from the repository root. Any failure blocks release.

Artifact storage characterization is discovered by the API gate through
`test_artifact_storage_contract.py`; service-free probe guards are discovered
by the scripts gate through `test_artifact_storage_probe.py`. The real RustFS
experiment is opt-in: use the pinned image and standalone locked script in
[Artifact object storage](../architecture/ArtifactObjectStorage.md). It creates
and removes an isolated project and is not run implicitly by ordinary CI.
Passing that experiment does not certify production backend integration,
data migration, backup recovery or performance.

Hosted transcript capture acceptance is included in the API PostgreSQL gate.
`test_transcript_capture_contract.py` invokes the Rust capture writer against
the same migrated disposable database, requires its explicit success receipt,
and reads its rows through the authenticated Django content endpoint. It covers
uncommitted and wrong-Execution rejection, immutable/idempotent publication,
equal-length snapshot replacement, UTF-8 continuation, transaction rollback on
a failed chunk, deletion during blocked chunk I/O, and forward migration without
fabricated captures. Capture identity tests additionally cover missing/corrupt
chunks, membership revocation, immediate deletion and expired-Agent cleanup.
The Rust `transcript_capture` tests exercise Core's real large-result spill via
a synthetic host and verify that archive-budget exhaustion leaves tool success
unchanged. The ignored Rust Django-contract entry point is deliberately executed
by the API test; it must not be counted as covered merely by ordinary cargo tests.
These tests do not constitute a production-load or fresh-Docker deployment gate.

Fresh-Docker capture acceptance is an additional opt-in check on the owned
`centaeris-perf` stack: follow the hosted transcript capture instructions in
[the performance harness](../../../perf/README.md). The bounded workload completes
one real AgentRun, replaces and removes its spill file, reads the archived UTF-8
text through the authenticated API, replaces only idle API/Runtime services, and
requires byte-for-byte equality afterwards. Its external manifest and report
record image identities, exact Run identities, request outcomes and content
digests without credentials or full text. This is retention acceptance for a
successfully published capture, not a guarantee that asynchronous capture always
publishes before a process fails, nor a production capacity or deployment check.
Browser acceptance additionally uses a production Web build and the
[conversation checklist](FrontendManualAcceptance.md). Web contract tests validate
Core-generated block samples, including optional `presentation` facts, while
continuing to reject unknown fields.

Sandbox-loss recovery requires an isolated-stack behavioral gate before release:
verify a pre-dispatch loss resumes from the advanced checkpoint with a new tool
call ID; the original failed call remains exactly once. Verify the recovery
checkpoint and old Execution end commit together, stale leases write neither,
and preparation failures/restarts retain the five-attempt budget. Post-dispatch
uncertainty, parallel/external tools, changed snapshot activity, deferred MCP
spawn, and foreign container identity must not trigger automatic replacement.
When concurrent child activity invalidates snapshot collection, the parent must
continue to its next model request without a new recovery checkpoint or reusable
snapshot witness. The safe-point commit and tool ledger remain intact; collection
errors still fail and foreign host evidence stays rejected. Verify that the parent
can reach durable waiting before injecting sandbox loss.
Compilation alone does not satisfy these checks.

Waiter-index acceptance must cover both PostgreSQL and SQLite: atomic index
creation and rollback, cascading removal on checkpoint consumption, targeted
source lookup, and more than 256 relationships inside one checkpoint. Worker
coverage must show cursor preservation after transient failures and no outbox
acknowledgement while `next` is non-null. Under load, unrelated historical rows
must not increase a source notification's lookup work; measure bounded pages
separately from the complete reconciliation pass.

Terminal waiter acceptance must reproduce sandbox loss while waiting for a runtime
job, followed by source completion and an unrecoverable parent failure. The
terminal session append must atomically write the Core `abandoned` event and
consume the checkpoint and waiter index. Cover completed, failed and cancelled
owners, rejected stale leases, transaction rollback on conflicting events, and
idempotent repair through the cancellation API for an already-terminal owner.
A live owner must not be consumed. Direct test-database cleanup is fixture
management only and cannot count as lifecycle acceptance. Record zero remaining
waiters before any manual cleanup, with no replayed tools or fabricated results.

The portable `python scripts/workspace/ci.py` gate runs `scripts/workspace/runtime_outbox_gate.py` in the API dependency
environment. It uses only `TEST_POSTGRES_*`, creates a random disposable database,
checks non-empty Rust test discovery, runs the PostgreSQL outbox regressions, and
drops only that database. Coverage includes acknowledged-history stability,
unacknowledged delivery across restart, duplicate/stale acknowledgement, active
wakes across yield, and concurrent late-waiter recovery across checkpoint pages.
The same disposable-database gate runs the PostgreSQL shared waiter contract and
cross-replica execution-capacity test. It pins execution limits to 8 global and 4
per tenant for that fixture. The controller's database-isolation and non-empty
discovery guard tests run before creating the disposable database.

The Python gate uses package-wide `test*.py` discovery, not a hand-maintained
list of API test labels. `scripts/workspace/python_test_gate.py api` runs Django's full
discovery against PostgreSQL, including transactional/locking behavior. The
`worker` and `document_processor` modes discover their respective packages in
their existing dependency environments. New tests must follow unittest/Django
discovery conventions (including importable package directories). Each run
reports discovered/executed counts and fails on an empty suite, duplicate IDs,
discovery/execution mismatch, import errors, skips, or expected failures.
`scripts/workspace/python_test_gate.py gate` exercises the guard's failure paths.

For local API tests, start the dedicated test PostgreSQL service; defaults are
`localhost:55432`, database/user/password `centaeris`. Override only with
`TEST_POSTGRES_HOST`, `TEST_POSTGRES_PORT`, `TEST_POSTGRES_DB`,
`TEST_POSTGRES_USER`, and `TEST_POSTGRES_PASSWORD` for another test instance.
The role needs permission to create databases. The runner uses a random test
database and temporary storage, cleans them after the run, and points unmocked
Runtime/Redis calls at a closed loopback port. It does not use deployed database
settings. CI provisions its own PostgreSQL 18 service. SQLite migration/drift
checks and the independent Python-to-Rust authorization gate remain in place.

The local gate includes `python scripts/workspace/agent-run-authorization-gate.py`. It checks the shared authorization
fixture and boundary corpus in Python and Rust, then verifies Python-generated
synthetic signatures in Rust. It requires a non-empty artifact and a Rust
consumption receipt; consumer failures block the gate. Vector tests use no
services, real Plugin content, or developer keys. Resource-builder tests isolate
asset and Plugin lookup while retaining production construction and validation.

The local gate also runs `scripts/workspace/deployment-contract.test.py` against rendered
Compose configuration with synthetic inputs. It covers processor build/material-Worker
identity, device mapping, Runtime port propagation, volume-path agreement,
internal addresses, API security options, and Docker socket access restricted to Runtime and the material Worker.
It needs the Docker Compose CLI, but not a running deployment.

The Docker fresh-start gate additionally verifies that the material Worker and Runtime reference
the built processor and general image IDs respectively and that processor device metadata
matches. It checks the API's actual capability sets and no-new-privileges, writes
synthetic upload and Plugin data, replaces the API container, and verifies reads
and removal. For a bounded local API-only reproduction, run
`uv run --frozen --package api python scripts/workspace/deployment-api-smoke.py`.
This uses a unique Compose project, fresh volumes and synthetic secrets, and
removes its containers, volumes and temporary API image after testing. Do not run
the full Docker release script on a host containing an existing deployment; its
disposable-host guard remains mandatory.

The local gate retains Web unit and contract tests. Playwright/E2E and visual
snapshot tests are not part of the repository; browser layout, interaction,
authentication, membership, and direct-route acceptance are verified manually.
GitHub Actions passes `-SkipFrontendTests`, so its Web portion is limited to
dependency installation, lint, typecheck, and production build. Backend,
Runtime, protocol, security, deployment, and performance gates remain automated.
Use [FrontendManualAcceptance.md](FrontendManualAcceptance.md) for the retained
browser interaction, authorization-UI, and appearance checks.

Hosted command acceptance tests must cover a committed-but-lost response,
concurrent identical submissions, retries after completion, changed input under
the same operation identity, authorization revocation and deleted resources,
upload content identity, transaction rollback, and scheduling failure. The
forward migration preserves existing business rows without fabricating old
receipts. Browser tests retain the operation identity across uncertain responses
and reloads, and distinguish receipt recovery from downstream projection or
material-link failures.

1. `python scripts/workspace/ci.py`
2. `node scripts/workspace/performance-eval.mjs`; review the independent phase report in
   [PerformanceEvaluation.md](PerformanceEvaluation.md). The 4,095-observation
   storage-growth tests are intentionally excluded from the normal test suite;
   this command runs each one exactly once. The checked-in `Performance`
   workflow runs it for relevant pull requests and `main` changes, and supports
   an explicit manual run.
3. Populate a private `.env`, then run `docker compose config --quiet`.
4. On fresh Postgres, apply `0001_initial` and confirm the Workspace app starts.
   Verify the complete current model schema, foreign keys, checks, unique
   constraints and the PostgreSQL tool-result expression index. Credentials
   remain unconfigured until assigned an explicit quota domain.
5. Build Runtime, API, worker, web, and execution images from root Compose
   contexts; verify health with an empty extension volume.
6. `docker compose config` must resolve project `centaeris-workspace` and only
   `centaeris-workspace_*` named volumes.

Gates must not read production data, real Plugin content, or developer secrets.

Shared Agent definition acceptance covers administrator-only draft management,
immutable publication, default-denied and Workspace/member availability, and
foreign-member rejection. Member grants bind current membership identities;
leaving and rejoining must not restore a previous grant. Administrators must
remain unable to read another user's private Agent, Sessions, Runs, files,
transcripts or Memory through definition management.

Independent PostgreSQL transactions must prove concurrent first use creates one
private instance per Workspace/owner/definition, and publication or scope changes
race atomically with new Run acceptance. Rejected admission must roll back Run
and receipt creation. New Runs in the same Session capture the latest published
version and instructions without changing Session or Agent IDs. Old Runs and
accepted receipt retries retain their original version and instructions after
publication, disablement or scope removal; new unavailable Runs are rejected.
Cover tail rewrites, immutable instance binding, cross-Workspace version/member
rejection and attempts to edit managed configuration.

Managed definitions select complete Workspace-enabled plugins through draft
`pluginNames`; publication freezes exact `pluginActivation` including Skills, CLI,
MCP and Hooks. Verify unknown/disabled selection rejection, immutable publication,
old Run snapshot stability, changed package rejection, and empty selection without
loading external contributions. Private Agent activation remains supported.

Connector acceptance must prove different definitions can bind different existing
encrypted credentials for the same plugin. Custodian approvals require the
credential creator's superuser identity and fix Workspace, definition, plugin,
server, resource path/digest and source version. Verify Workspace administrators
see only exact-scope approvals and cannot enumerate global secrets or bind an
arbitrary secret reference. Missing, revoked and cross-scope bindings never inherit
global or another assistant's credentials. Source rotation requires a new explicit
approval; protected source deletion returns a controlled 409.

On every MCP dispatch, including bearer HTTP, unauthenticated HTTP and stdio,
verify current membership identity, owner/lifecycle, definition availability,
Workspace plugin enablement, frozen declaration resource, approval, binding and
source version. A cached provider must reject revocation, changed fingerprints,
invalid/missing response fields and API failure without executing or reconnecting.
Dispatch must reject a fingerprint changed after its connect check. Preserve the
inner provider's structured errors. Synthetic model arguments must not select
server/resource identities or secrets. Private compatibility must require durable
null Agent definition and null Run version; missing managed version data fails.
The old credential resolver must reject managed Agents.

Initialization/queue acceptance uses explicit synchronization barriers: after the connector
receives its token, hold initialization, prove a second call is pending inside
Core's lazy connection queue, withdraw synthetic authority, then release both.
The real provider must receive zero calls for approval revocation, binding
removal, membership revocation and source rotation. Cover both `execute` and
`execute_with_error_info`, without sleeps. The guard belongs on the connected
provider after the lazy wait, rather than outside the lazy provider. This does
not promise an atomic remote-effect fence against revocation after the final
authorization decision or cancellation of already dispatched external calls.
The local red receipt `test-results/assistant-connector-dispatch-window-red.log`
records two failing tests with two actual calls instead of zero. The corrected
focused receipt `test-results/assistant-connector-dispatch-window-green.log`
records 14 passing tests; the LiveServer interoperability entry is executed
explicitly by the API gate. The unchanged pinned Core receipt
`test-results/assistant-connector-dispatch-window-query-loop.log` records 29
passing `query_loop` tests.
The full original local gate receipt
`test-results/assistant-connector-dispatch-window-ci.log` exits 0: 651 API tests
discovered/executed with no skips or expected failures, explicit Rust/Django
connector interoperability, and 120 passing Web tests. The Runtime unit gate
passes 194 tests; its 38 existing ignored markers do not replace the explicit
LiveServer invocation.

Focused synthetic evidence is retained locally in
`test-results/assistant-connectors-api-red.log`,
`test-results/assistant-connectors-api-green.log`,
`test-results/assistant-connectors-rust-red.log` and
`test-results/assistant-connectors-rust-green.log`. The Runtime red case proves
the old resolver misses the new authorization endpoint; the targeted `mcp::tests::`
suite passes 12 tests. Require the final API receipt to pass connector, definition,
migration and model tests, then run the normal local release gate; a focused
receipt is not a substitute for that gate. Evidence uses synthetic fixtures only:
it does not certify real credential creation/import/rotation, external connector
calls, deployment, or a new credential UI. Delegated stream revocation has its
own acceptance tests below.

`test_assistant_connector_runtime.py` starts the real Django authorization endpoint
and explicitly executes the Rust `python_connector_interoperability` entry point.
It requires one executed Rust test and its success receipt: two calls reuse one
synthetic downstream provider; after exact test-approval revocation, neither the
cached provider nor a new connect may execute. The ignored Rust marker prevents
standalone execution without its owned test database; ordinary Rust discovery
alone does not cover it. The API gate must execute it without a skip. Its local
receipt is `test-results/assistant-connectors-interoperability.log`.

Core's focused `query_loop` is also run against the unchanged exact public pin,
with a separate build target. The local receipt records 29 passing tests in
`test-results/assistant-connectors-query-loop.log`; this does not certify a Core
release or packaged Desktop acceptance.

The final original local `scripts/workspace/ci.py` receipt is
`test-results/assistant-connectors-ci-final.log`: exit 0, 651 API tests discovered
and executed with no skips or expected failures, the explicit connector
interoperability receipt, and 120 passing Web tests. The initial LiveServer
serialized-rollback fixture failure and transient public-pin fetch reset are
retained separately; neither was bypassed in the final gate. This remains local
source acceptance, not fresh-Docker or production deployment acceptance.

The first-release initial schema must create no definitions, versions,
applications, user grants, connector approvals or copied secrets. Definition,
Run and hosted receipt origins are assigned only by their normal admission
paths; plugin selection and activation retain their empty defaults.
Fresh-schema, SQLite migration/drift and PostgreSQL constraints must
also pass. The fresh-Docker gate checks the new leaf only on a disposable host;
source tests do not certify that deployment acceptance has run.

API PostgreSQL pool changes additionally use the bounded connection-pool controller described
in `perf/README.md`. It records actual serving-process pool statistics, typed
request errors, three SSE observers per accepted Run, and exact durable terminal
accounting. Verify pool 8 followed by explicit 0 with only the API container
replaced. Keep the workload budget, stop conditions, image identities and raw
evidence outside the repository. A successful low-load run does not establish
throughput or explain an earlier unobserved timeout.

Execution replacement acceptance protects the supported worker's single-step
dispatch: recovery yields the current lease before another claim, and an unknown
step response does not resend that step under the same owner. The disposable
PostgreSQL gate includes yield/reclaim identity and fenced terminal appends.
API tests use independent PostgreSQL connections and barriers during snapshot
upload to cover lease expiry/replacement, changed baselines, terminal Runs, and
idempotent publication. Real filesystem tests must preserve a new owner's
published snapshot when an old upload resumes, and permit valid retries after
invalid or interrupted input without exposing partial canonical files.
Staging an execution snapshot alone is not checkpoint publication. These checks
do not certify custom same-lease step callers or a proxy that retries steps.

Snapshot lifecycle acceptance also covers purge during payload copying and before
final publication for both Session commits and execution checkpoint stages.
Late requests must fail without recreating a canonical key, including after GC
has removed an open upload temporary. Independent PostgreSQL transactions must
prove public Session deletion and upload do not deadlock, and download opens the
authorized file before purge can proceed. Consume an already opened response
after GC and verify complete bytes; a new download must fail. Run these filesystem
races on Linux as well as any supported test host; an open-file unlink failure
must be reported and a later pass must succeed after close.

Workspace GC tests must cover all generations of a purged Session, owned
checkpoint keys and uploader temporaries, while retaining active/restorable and
cross-owner resources, unknown path shapes and unrelated bytes. Verify both age
cutoffs, dry-run, exact-key retry after failures, missing storage roots, disappearing
temporaries and symlink/reparse rejection. Assert managed payload file count and
bytes reach zero; retained directories and unknown keys are outside that metric.
Authorized stream tests must also cover exhausted capacity, authorization failure,
opening cancellation, unused response close and constructor/header failure.
Before first deployment, drain the old API writers before enabling the collector;
these source gates do not verify that operator action or a production rollout.

Execution-capacity acceptance uses a fresh isolated database with the current
initial schema. Across independent API/Runtime/worker replicas, verify initial
queue limits (128 global, 32 per Workspace) and execution leases (8 global, 4 per
Workspace), concurrent submissions/claims, immutable tenant binding, and rollback
on rejected admission. Yield, terminal transitions and reconciled expiry must
release capacity; at saturation, job waits must not spin on unclaimable work.
Expired initial queues must request cancellation at the first-start gate; user
questions, runtime-job waits and recovery backoff must remain unaffected.

Fill ordinary HTTP and database capacity, then exercise cancellation preflight,
cancellation writes, heartbeat and job-status reads through control capacity.
Fill listeners independently. Check 503 plus Retry-After, body/handler deadlines,
and that timed-out blocking work retains its permit until exit. Long AgentRun
steps must not inherit the short-request deadline. Material processing runs in its
dedicated Worker with a bounded processing deadline, outside Runtime HTTP.
These are acceptance requirements, not a claim that an isolated run was executed.
Rust dependencies use local paths in the shared Cargo workspace and one locked
resolution. Core source identity is verified by scripts/workspace/core-source.mjs.
All image provenance is the full actual monorepo checkout SHA. External pins,
Core checkouts and local Cargo patches are not normal build inputs.

Historical receipts in this document predate source unification and retain their
original pin terminology. They are baseline evidence, not a migration gate pass.
Current validation uses the candidate tree and product gates from the shared root.

The checked-in CI workflow runs the selected source, unit, and Compose gates from a
clean checkout. Required status checks must be enabled on the public `main`
branch before external pull requests are accepted.

The root license, first-party Rust/npm/Python package metadata, README, and
contribution policy must consistently identify `AGPL-3.0-only`. Third-party and
brand-asset exceptions remain explicit. The README, contribution guide, and issue
template must consistently describe the temporary restriction on external works.
Pull request creation is limited to collaborators. Any future reopening of
external contributions requires the published contributor agreement and explicit
contributor acceptance described in the contribution guide.
Every distributed image or application must identify the AGPL license and the
complete corresponding source for its exact released revision. A modified
network-interactive deployment must offer that corresponding source to users
interacting with it remotely, as required by AGPL section 13.

Docker management transport acceptance requires both source/build checks and
an isolated daemon gate. Source checks cover request field preservation, sandbox
limits/mount isolation, processor CPU/GPU configuration, and structured missing
container errors. Production management, exec and archive operations must have
no Docker CLI branch, and the Runtime image must not copy a Docker CLI binary.
Check rendered Compose entrypoint/command as well as the image. With a missing
processor image, material Worker startup must fail through Engine inspection;
Runtime must not contain a processor preflight or processing route. No external entrypoint override may be used for acceptance.
Verify both explicit and omitted false for Mount.ReadOnly after create and start;
a required read-only mount must still reject omission/false, and mismatched
volume identity/subpath and missing security fields must still fail.

The isolated daemon gate must exercise root preparation/sentinel verification,
processor specification and document processing, nonzero batch exit/output,
resource/security/mount inspection, owned-ID cleanup, and a lost create/start
response. Unknown outcomes must not cause duplicate creation or process restart;
foreign containers and daemon failures must not be treated as missing containers.
The Debian/production-Engine gate also covers binary/interleaved exec output,
nonzero exit and missing exit confirmation, stdin EOF, 8 MiB output truncation,
confirmed cancellation versus unknown transport failure, and no command replay
after an exec-start response is lost. Verify MCP discovery/line limits/close,
persistent generation RPC reuse, snapshot upload/restore hashes, and processor
archive traversal/link/size rejection plus large-file streaming. Run these on
an isolated stack with the CLI-free Runtime image; host-side probes may use CLI.
Compilation alone does not satisfy this behavior gate. No throughput or latency
improvement is certified without a separately recorded measurement.

The local gate also checks document streaming beyond 1000 PDF pages/image
frames, UTF-8 locations, bounded incremental output, and API manifest validation.
These are synthetic/native-parser checks; they do not replace user acceptance
with real Office documents or real OCR model measurements.

## Private Agent message acceptance

Use synthetic identities, files and models with disposable PostgreSQL/storage.
The ordinary package-wide API gate discovers `test_agent_messages.py`,
`test_agent_coordination_migration.py` and `test_agent_message_runtime.py`.
Focused reproduction from the repository root is:

```sh
uv run --frozen --package api python packages/api/manage.py test app_core.test_agent_messages app_core.test_agent_coordination_migration app_core.test_agent_message_runtime --settings=api.test_settings --noinput
cargo test --locked -p runtime_server agent_message
```

The API runtime test explicitly invokes the ignored Rust Django-contract test
against its migrated disposable database. Ordinary cargo discovery alone does
not cover it. It runs the same-checkout Core AgentRuntime, production message provider,
production Session record builders and PostgreSQL Session append, then reads
the actual authenticated API projection. Require two complete committed messages,
three successful calls and one Final/RunCompleted through
send → ordinary tool → send → Final. Check full body/refs and prior call/result
pairs in every ModelClientRequest, no message before commit, rollback and stale
lease rejection, lost-ack replay and reopened storage without duplication, and
cached-provider rejection after file or membership revocation. No supplier API,
developer key or paid model is required.

The in-flight revocation contract permits a call validated before revocation to
finish persistence, subject to cancellation, lease fencing and storage success.
The deterministic API/Rust barrier test commits membership revocation after the
real validator returns and before the fenced Session result append. Require one
stable committed message through replay/reopen, rejection of later calls, current
history denial, and no revival of the old Run membership identity after rejoining.
This barrier covers membership revocation only; it does not establish in-flight
window coverage for all other revocation types.

Current-read tests must separately revoke associated Session Workspace membership and
file source access after message commit. Require the authorized coordination
history to retain the original body/opaque refs, while actual associated Session
reads and file downloads deny current access and later sends reject those refs.
Retained history and signed old inputs must never grant continuing resource access.

API coverage must also reject other owners, model-selected recipients, ordinary
Session send attempts, mismatched source event identities/provider/contract,
failed and tombstoned results, and owned files outside the Run's signed inputs.
Verify that both ordinary workspace list modes exclude the new coordination
Session while preserving existing work Sessions. Migration coverage must retain
the previous Agent/Session/Run/event/authorization records byte-for-byte without
creating bindings, and concurrent authenticated creation must produce one fresh
Session and binding. Normal Web unit tests protect existing work Session
rendering, streaming and Final behavior; this change adds no Agent UI.

Run `python scripts/workspace/ci.py` and the same-checkout Core focused `query_loop` suite as well
as the focused contract checks. Keep builds serial and record the exact source SHA and
toolchain. On Windows, use a temporary directory outside any Cargo workspace for
the exporter's standalone Cargo fixtures, which also declare independent
workspaces when TEMP is nested; loopback HTTP tests may require
`NO_PROXY=127.0.0.1,localhost` when the host has an unsupported SOCKS proxy.
Report unrelated fmt/clippy findings against the unchanged baseline separately;
focused acceptance does not establish a green full release gate. Provider live
experiments remain outside Git and default CI and cannot replace these tests.
This gate does not certify a Docker deployment, Agent Inbox/wake coordination,
shared credentials or new memory permissions.

## Committed private work materialization acceptance

Package-wide API discovery includes `test_agent_work.py` and
`test_agent_work_migration.py`. Focused reproduction uses disposable PostgreSQL,
synthetic native identities/files and mocked existing profile/scheduling I/O:

```sh
uv run --frozen --package api python packages/api/manage.py test app_core.test_agent_work app_core.test_agent_work_migration app_core.test_transcript_index_migration --settings=api.test_settings --noinput
```

Require source facts to be invisible from a second connection before commit and
reject in-transaction direct materialization. Matching active committed call and
success facts must create one same-user/Workspace/private-Agent work Session,
Run, new authorization, origin relation and independent admission receipt in one
transaction. Race two real HTTP materializations; require one admission and one
scheduling call. Replay, duplicate success and relation reconstruction retain the
original identities after failed child execution and profile/model unavailability.
Use two PostgreSQL connections and barriers to race source rewrite with admission.
An admission holding the source Session lock must commit its original binding
before rewrite can invalidate the successful pair. A rewrite holding that lock
must produce a retryable `503 agent_work_source_busy` with no child/receipt rows;
after its commit, invalidated call or success facts must reject with 409. Recheck
facts after taking the Session lock, including rewrite committed after an unlocked
source lookup. Include a new source event in the rewrite transaction to exercise
deferred Workspace foreign keys; do not permit a Session/Workspace lock cycle.
These controlled races reproduce Runtime's lock/tombstone SQL, not a full Core
rewrite execution.
The normal Runtime PostgreSQL gate separately runs
`postgres_fenced_rewrite_tombstones_completed_dispatch_request`: the same-checkout Core
planner rejects a running tail, accepts a terminal Final without file mutations,
and the real fenced append waits for the Session lock before tombstoning its
dispatch call/result. This does not combine the Runtime rewrite and API race
into one end-to-end execution.
Changed input with the same source identity conflicts; independent calls create
distinct work Sessions. Erasing the original success and losing the relation must
not reconstruct a different origin from a duplicate. Newly accepted work must
retain existing enabled/current model and explicit thinking-mode selection rules.
Deleted accepted work cannot be replaced. Admission
failure rolls back all new rows, while post-commit scheduling failure retains
acceptance for existing lifecycle reconciliation.

Reject untrusted provider/digest, failed source facts, ordinary Sessions, foreign
owner identities, unavailable membership and unknown Session refs. Verify only
explicit selected inputs enter the child authorization, recheck source input
generation after request commit, and reject source-Session Artifact links without
child/receipt writes. A generation change after successful validation must not
upgrade the child signed input, and later input reads must reject that change.
Available managed sources are explicitly unsupported in this
native consumer. The first-release schema creates no inferred work bindings.
The current fresh-Docker migration leaf is `0004_business_agent_branches`; a leaf assertion is not a
deployment result.

Dispatch validation additionally covers native private coordination success,
ordinary/managed/delegated source rejection, current membership and resource
revocation, signed-input generation and Artifact exclusion. Assert provider
validation alone creates no child Session/Run, work binding or operation receipt.
Use the real pinned Core loop and Runtime provider against the migrated API test
database: successful source commit continues to the next model request without
admission; an injected PostgreSQL source-result commit failure leaves no durable
success and creates no work. Require a non-empty Rust test and explicit receipt.
The hosted native coordination identity must match both coordination and signed
Session identities; ordinary Sessions must expose no dispatch tool.

Observe the actual pinned Core model request through the production contract and
provider registration functions using API-signed ordinary, managed, delegated
and native private coordination Runs. Require `dispatch_work` and `get_work_request` only in the native
case, and run the real provider/commit-boundary tests through those same functions.
Checking hosted identity fields or a manually assembled registry alone is not
production registration coverage. Compare the query descriptor digest across API
and Runtime, and exercise dispatch commit followed by a real Core query returning
pending without admission. Check uncommitted/call-only, pending and admitted
requests, cross-turn ambiguity, source rewrite, public child deletion/expiration,
permission revocation, duplicate/conflicting facts and exact receipt lookup without
binding repair. Assert no query business writes, profile request or scheduling.

Transcript tool-output lookup checks index availability using the real ORM
projection, including `eventId`, against two Sessions and 1,024 additional events.
A real transaction applies `SET LOCAL`, fresh `ANALYZE` statistics, and strict
partial-index/session/callId plan assertions. Roll back fixture rows, column
statistics and the local setting before checking content rejection and membership
revocation; `pg_class` row/page estimates are not guaranteed to roll back. This controlled
index path is not proof of the natural production plan or lookup latency.

Run `python scripts/workspace/ci.py` and the unchanged exact public Core `query_loop` suite.
Keep one build pipeline, D-drive target/temp on Windows, and record the final
source diff and tree fingerprint before and after validation. These query checks
do not establish child model completion or recovery.

The post-commit trigger bridge must use the production fenced append/trigger
helper with a real pinned Core loop and real API materializer. Hold materializer
HTTP until Core reaches its next request and Final; require current source
Session/Run row locks and the append mutex to be available during HTTP. A failed
source transaction or rejected lease fence sends zero requests. Successful
commit creates exactly one child/binding/receipt; duplicate wakes and a durable
admission with a lost response followed by replay retain one operation and one
scheduling call. HTTP errors, timeouts and full in-flight capacity leave the
source success and Final intact. A mixed receipt must inspect every result and
send only current native dispatch successes, using their committed event IDs.
Empty receipts, foreign identities and terminal/non-dispatch facts send nothing.
The trigger consumes only HTTP status and does not add a queue, retry or recovery
state. These trigger tests do not certify child model execution.

Recovery acceptance includes `test_agent_work_recovery.py`,
`test_agent_work_recovery_migration.py` and worker `test_work_recovery.py`.
Require the real Core capacity-skip fixture followed by the actual worker HTTP
client to create one admission; lost-response restart and two real recovery
workers must preserve one receipt/child/scheduling call. Admission followed by
schedule failure must be repaired by existing lifecycle reconciliation, without
another admission. Original-identity condition restoration can admit only after
current permission and input checks pass; member deletion/rejoin must retain the
old invalid membership reference and authorization. Cover rewrite, deleted child
with retained receipt, missing binding and exact receipt scope.

Exercise multiple pages, persistent first-source failure, frozen upper bounds
under new arrivals, a real late commit behind an already walked cursor, and soft
budget expiry retaining unprocessed tails. Page failures preserve position;
restart begins at the head. Slow scan HTTP must allow lifecycle and waiter calls
to finish on their independent control thread. Verify one scanner thread, shared
stop/join handling, no new calls after stop, and periodic waiting after failures.
These are soft-budget checks, not a strict total HTTP deadline guarantee.

Delivered work-return acceptance includes `test_agent_work_returns.py`,
`test_agent_work_return_runtime.py`, `test_agent_work_return_migration.py` and
worker `test_work_returns.py`. Require canonical pinned Core terminal constructors
through the production Runtime record adapters and a real current lifecycle lease
against API-owned work bindings. A stale fence must fail before terminal delivery.
Require Runtime's real durable cancellation request and fenced hosted receipt for
the history-free pre-admission cancellation case, and a real dead-letter Job for
the operational fault case. Publish each Job outbox before the real worker HTTP
repair; restart the publisher and retain identical notices without new Runs or
scheduling. The lifecycle read seam may expose the public Job shape from those
real stored rows; this fixture does not certify an external Runtime HTTP service
or child model generation.

Cover uncommitted terminal reads, two concurrent writers, duplicate/lost response,
condition restoration versus membership rejoin, deleted/foreign resources, source
rewrite, later manual child Runs, malformed terminal identities and late Core
terminal after operational fault. Assert PostgreSQL read-only queries and exact
camelCase transport fields. Delivery must not imply Read, handled or business
success. Exercise frozen bounded pages, individual failure advance, late commit
behind cursor, restart, separate control threads and stop/join.
`test_agent_work_return_progress.py` additionally requires real worker/API HTTP
with 101 accepted bindings: two first-page lifecycle Job socket reads delayed
beyond the unchanged five-second timeouts, and a later-page terminal committed
through pinned Core/Runtime fences. Assert attempted-source progress survives
timeouts, the unattempted tail remains reachable, later passes retry slow sources,
and the terminal is delivered. Discovery must have no Runtime dependency or
business writes. Fix the insert race with a PostgreSQL barrier after both writers'
get-or-create misses and before the second save; one committed winner must replay
identically, while changed payload and fresh-instance overwrite still fail.

Fresh initialization must create no inferred work-return delivery. Run the complete local
CI and exact public Core query loop before freezing evidence for independent
review; automatic wake and explicit consumption confirmation belong to a later
behavior boundary.

Compile the actual ORM JSONB predicate and verify its PostgreSQL plan naturally
uses the `(insertedAt,eventId)` partial index with competing indexes intact;
do not force sequential scans off. Verify that the initial schema includes the
partial index without changing source, authorization, binding or receipt facts.
Check fresh migrations, drift and release-leaf
assertions locally; this is not a deployment test.

The consumer/admission tests use committed event fixtures, separately from the
provider commit-boundary tests above. Neither certifies full child model
execution. The descriptor is not registered as a model tool for ordinary or
managed Sessions, and no automatic wake, general retry/progress policy, stop
cascade or cross-Session waiter change is certified. Accepted is not processed.

Explicit first-consumption acceptance additionally requires strict start v2,
single-notice first-root uniqueness, operation replay/conflict, busy retention,
original membership and current child read authority, and atomic Run/input/
authorization/receipt rollback. Use Core's native input factory and real fenced
PostgreSQL append to test canonical origin, changed input/wrong-kind rejection,
lost-ack replay and production ledger reopen with distinct Run and Turn IDs.
Fresh initialization must create no inferred attempts. Verify database rejection
of a null delivery sequence and retention of the queue's explicit initial host
owner through admission and restart. Admission does not certify Read or handling.

Automatic first-admission acceptance separately requires a real worker notice-ledger
scan to reach the production first `main` model request without a manual consume.
Prove committed-only discovery, stable operation identity, busy retention, dual
worker/lost-response/restart uniqueness, frozen upper bounds, and progress beyond
persistently failing sources. Concurrent child Sessions must execute independently
while coordinator admissions stay serial. Failed, cancelled and Final-without-handled
attempts must never regain automatic eligibility; a new notice's current input
must reference only itself. Inject acceptance-to-schedule loss and repair it through
the ordinary lifecycle reconciler as the same Run and authorization. Discovery
must remain read-only and preserve original membership/current read authority at
admission. These gates certify first automatic admission, not explicit retry,
Read, handled confirmation, UI wiring or the complete handling lifecycle.

The first release has no existing released deployment data to migrate. Its
recovery acceptance covers current-version persistence, process restart and
owned execution loss through production `execute_agent_run`, as specified below.
An earlier public Git pin or a mixed test producer is not a released deployment
data baseline. Retain historical uncovered-wait rejection evidence; do not report
that rejection as a successful recovery or relax checkpoint validation.

After a version has been released with persisted deployment data, a compatible
upgrade must use a golden sample produced by that exact released stack and resume
through the candidate's production `execute_agent_run`, preserving Run, Turn,
job and authorization identity within the documented supported recovery boundary.
A hand-restored snapshot or mixed producer does not satisfy that acceptance.
Failures within that supported boundary remain release blockers. This acceptance
does not certify automatic worker wake, retries or full child model execution.

Current runtime Job wait handoffs have the following acceptance.
The normal PostgreSQL outbox gate runs `postgres_wait_handoff`: inject failure
before the Session reference commit, lose the successful response, expire or
reclaim the original lease, and replay recovery. Require atomic publication,
immutable state after overwriting the mutable Session snapshot, one checkpoint,
one accepted call and one source Job. Altered attachments and source bindings
must fail. Current-schema reopen preserves payloads, immutable attachments and
observations, including `input_uptake`. Incomplete or unsupported version ledgers
and altered schemas must fail without writes; no development-version conversion
or inferred handoff is supported. The default PostgreSQL gate also runs
`postgres_current_schema` to require reopen and rejection coverage.

Use production `execute_agent_run` to produce a new wait and complete its source
Job under the current pin. Compare retained and lost owned execution arms; both
must reach the next real model request with source evidence and unchanged Run,
Turn and authorization. The lost arm must yield and claim a fresh lifecycle owner
before one replacement Execution. Record checkpoint/reference positions, sealed
state, call/result/dispatch counts, source fingerprints and executable hashes.
A `cfg(test)` host seam establishes runner and storage behavior only; isolated
real-container fault validation remains required before release and runs after
the isolated stack has been built. Keep the historical uncovered-wait rejection
distinct from current-boundary recovery results; it is a negative safety case,
not a cross-release migration result for this first release.

## User-app delegation acceptance

Use synthetic applications, tokens and isolated PostgreSQL/storage fixtures.
Browser consent must require explicit unique operation scopes, enforce active
application and available definition selection, return a token only once with
no-store, persist only its digest, and support permanent revocation. Cover exact
issuer/audience, expiry, replaced membership identities, inactive users and
definitions, mixed cookie/bearer rejection, unchanged browser CSRF and denied
management access. The minimal Settings UI must exercise consent, one-time token
display, revocation and platform application registration/status controls.

Delegated usage must reuse private Agent/Session identities, shared browser/app
operation receipts and digest conflicts, including deleted-resource 410 within
the granted assistant and 404 for another assistant. Exercise identical POST
URL/payload retries after deleting a message's Session and after deleting the
parent Agent of an accepted Session creation. Current grants must still be valid;
an accepted receipt never bypasses revocation, and new operations still require
active resources. Preserve the original receipt/Run application origin. Verify
history/citation payloads, actual authorized download bytes, source ACLs, cancellation requested
without fabricated terminal state, and multipart SessionAssetLink identities.
Run authorization must retain all Session asset refs and only the current
message's attachment refs. Global Library and arbitrary existing-file attachment
management remain inaccessible to applications.

Use explicit barriers for revocation before acceptance, acceptance before
revocation, browser/app concurrent operation replay and revocation between upload
storage and linking. Verify no losing writes or lifecycle dispatch. Accepted Runs
must retain immutable user/application/grant origin; withdrawn, expired or
app-revoked grants must reject subsequent connector dispatch even with an old
configuration snapshot.

The real HTTP Session stream test opens an idle stream, commits revocation through
the browser API, and requires stream closure and producer cancellation within
fifteen seconds. Deterministic stream unit tests also cover authority-check
failure/timeouts, blocked I/O and item-before-authority ordering. Keep the original
live snapshot/revision and committed-cursor contracts. This evidence does not
promise withdrawal of already dispatched external effects.

Fresh initialization creates no application/grant origins or grants. Ordinary
admission preserves ownership, Session/Run/receipt identities, instructions,
digests and encrypted credentials.
Run the fresh migration/drift gate and normal local release gate as well as focused
tests. Local red/green receipts are under `test-results/app-delegation-*.log`;
focused results alone do not establish full release-gate acceptance. These tests
do not issue real persistent credentials or certify a production deployment.

## Persistent login and native business application acceptance

Run `app_core.test_persistent_login`, `app_core.test_persistent_login_migration`, `app_core.test_account_security`,
`app_core.test_persistent_app_delegations`,
`app_core.test_persistent_app_delegation_migration`,
`app_core.test_native_agent_delegation`,
`app_core.test_business_agent_branches`, `app_core.test_business_agent_branch_migration`,
`app_core.test_business_agent_branch_sessions`,
`app_core.test_business_agent_execution`,
`app_core.test_app_delegation_scope_parity`, and the existing delegated usage,
concurrency, connector and real HTTP stream suites on a disposable PostgreSQL
database. Full API discovery, fresh SQLite migration/drift, schema constraints
and the settings page's real browser fixture remain required.

The login boundary must show validity through the former eight-hour deadline and
years without requests, persistent cookies, no writes on unchanged authenticated
reads, valid old-session adoption without
identity replacement, rejection of expired old sessions and preserved explicit
logout/password/disabled-account behavior. This uses controlled time, not waits.

Explicit null expiry grants must remain available beyond 24 hours, while omitted
expiry keeps its one-hour default and existing finite deadlines are retained.
Exercise owner-only atomic credential rotation, stale/concurrent version
conflicts, one-time no-store output, digest-only storage, immutable audit metadata,
rotation between authentication/preparation/admission and idle stream closure.
The migration must preserve existing grant fields and input bytes without
inventing audit events or application origins.

Native tests must exercise the actual owned Agent and coordination binding,
per-input application provenance, actor-specific idempotent replay, mixed browser
and application intake, delayed admission version checks and actual main-model
uptake Read facts. Accepted inputs must continue as native owner execution after
credential withdrawal, retaining the Agent, Session and input identity. Use the
real work materializer and existing return/consume/recovery regressions; do not
loosen managed/delegated child-work prohibitions to satisfy the tests.

Business branch tests must prove durable identity under time passage, credential
rotation and reissuance; separate identities for different external users,
applications and roots; strict external identity validation without aliases;
one branch under concurrent first resolution; no private root data cloning; and
no replacement of deleted resources. Freeze branch identity during authentication
and recheck it with the credential version before admission and storage reads.
Reject missing/mismatched branch selectors and cross-branch Agent, Session, file
and artifact requests. Workspace Session lists must return only the selected
branch's coordination Session and authoritative work Sessions, including when
the caller omits `agentId`; unrelated Sessions using the same Agent stay excluded.
Runtime message/work Session references must reject sibling
or root Sessions even when the platform owner matches. New coordinator Runs use
updated root policy while existing and child Runs keep their accepted instruction
snapshots. Ordinary native and managed-definition behavior remains covered.

Open actual authorized bytes for files referenced by committed `send_message`
and real child-work outputs. Reject substituted, stale, ordinary-session,
cross-Agent and revoked references, including authority changes before storage
open. The settings fixture must cover permanent and finite targets, native scope
selection, nullable expiry, credential versions and rotation conflict recovery in
English/Chinese at desktop/mobile sizes, plus owner-only read-only branch tree
inspection without creating users or work. Synthetic browser fixtures and mocked
model/profile providers do not certify a live external business integration or
deployment.

Application details must select the exact grant when an application has multiple
targets. Verify overview/API/user/credential navigation, keyboard tab selection,
collapsed consent entry, readable scopes, retained one-time-token behavior,
rotation conflict recovery and stale/aborted branch reads in English and Chinese,
desktop and mobile, and light and dark themes. Code examples must preserve the
configured API path prefix, strict request/response keys and returned object
identities, use backend environment credentials, and stop on rejected requests.
Published examples must select a real model ID before submitting, while native
examples must omit the branch header during resolution and include it afterwards.
Branch expansion must use actual coordination and work bindings, bounded
pagination and current owner membership. Cover deleted metadata, invalid
bindings without repair, cross-tree cursors and ordinary/sibling/root exclusions.
Run the focused branch metadata and existing branch tests, Web unit tests,
production lint/typecheck/build, and the actual settings component's isolated
browser fixture. No runtime semantic change or migration is implied by these
metadata and presentation checks.

## Authorization consolidation acceptance (2026-09-15)

`scripts/workspace/ci.ps1 -SkipFrontendTests` passed against a dedicated disposable local
PostgreSQL container: 467 API tests executed with no skips or expected failures,
Rust workspace checks/tests and PostgreSQL outbox gates, 14 Python-signed Rust
verification vectors, deployment contracts, migrations, worker/processor tests,
MCP client checks, Web production build and Compose structure. Frontend unit tests
were not rerun for this backend change. Core's focused `query_loop` also passed.

The 27 focused Python tests include digest/signature/binding rejection order,
per-input membership and generation checks, in-place authorization tampering,
blob disappearance and request-local digest reuse. Rust startup rejection order
is characterized separately. No real runsc isolation claim is derived from these
checks. The temporary PostgreSQL container and its volume were removed.

An additional `cargo clippy --workspace --all-targets --locked -- -D warnings`
run remains blocked by two unchanged `main.rs` findings: `too_many_arguments` in
`terminalize_agent_run_failure`, and `collapsible_match` in the live reasoning
handler. They were not suppressed or mixed into this authorization change.
The Rust toolchain and both pinned Rust build images now use 1.95.0 to match Core.

For migration acceptance, also run the deployment identity comparison and actual Docker context exclusion audit. Browser interaction acceptance remains manual and is not added to CI. Disposable test databases and empty-volume gates must never reuse deployed volumes.
