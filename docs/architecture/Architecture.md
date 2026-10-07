# Architecture

## Runtime ownership

`packages/core` is the only owner of runtime semantics. It defines sessions,
turns, model requests, tool execution, runtime events, checkpoints, context
projection, and continuation. Core does not depend on Electron, a terminal,
Docker, HTTP control planes, or a concrete database.

Hosts and adapters translate external systems into Core contracts:

| Package | Responsibility |
| --- | --- |
| `packages/runtime_sqlite` | Local `RuntimeStore` schema, transactions, and persistence |
| `packages/mcp` | Lazy MCP connection, discovery validation, and call translation |
| `packages/runtime` | Local composition of Core, storage, model providers, processes, and host protocol |
| `packages/desktop` | Electron window, native shell integration, and controlled renderer bridge |
| `packages/tui` | Terminal projection and input handling |
| `packages/ui` | Host-agnostic Desktop renderer |

An adapter may translate identities, transport, storage, or process behavior. It
must not define a second prompt, tool loop, continuation state machine, or
Session truth.

## Request flow

1. A Host opens or creates a Session through the Local Runtime protocol.
2. The Local Runtime resolves the active model, working directory, extension
   activation, and `ExecutionHost` binding.
3. Core builds the model request from durable Session facts and the current
   request context.
4. Core validates and executes built-in or dynamic tools through the frozen
   execution and provider bindings.
5. Durable tool receipts, model output, continuation state, and terminal
   outcome are committed through the owning `SessionLogPort` or `RuntimeStore`
   adapter before Hosts publish their durable projections.
6. Desktop and TUI render the same canonical events. They may differ in layout,
   but not in runtime meaning.

## Model request admission

Core's `model::admission` owns fixed concurrency, round-robin selection across
waiting runs, FIFO within each run, cancellation cleanup, and shared Retry-After
cooldown. Hosts explicitly share one `ModelAdmission` instance per quota domain;
provider/model labels or credentials are not used to infer domain identity.

The Local Runtime currently uses one conservative process-wide domain with four
slots, shared by main AgentRuns, subagent jobs, automatic/manual compaction and
model connectivity checks. Main AgentRuns and subagent jobs use their existing
run identities; manual compaction uses its Session/turn identity, and connectivity
checks share a diagnostic queue. Different providers can therefore delay each
other. There is no account/project quota configuration, adaptive concurrency,
persistent admission state, or coordination across Runtime processes/machines.

`AdmittedJsonHttpTransport` wraps each actual HTTP attempt inside the existing
protocol-adapter retry loops. A slot covers the complete JSON response body or
SSE stream, including idle time. Success, transport error, stream cancellation,
or dropping the caller's future releases it once. Dropping a queued future removes
its queue entry without sending a request. Core's existing cancellation path
drops the generation future, so Stop also cancels admission waiting. HTTP timeouts
start after admission; admission waiting itself has no new deadline.

Retry backoff and response parsing do not hold a slot; every retry joins the
queue again. A completed retryable HTTP response (408, 429 or 5xx) with a valid
`Retry-After` installs a shared cooldown before releasing its slot, even when
no retries remain. Delay-seconds and HTTP dates are accepted; expired dates mean
zero delay, malformed/unrepresentable values are ignored, and a later shorter
cooldown cannot shorten an existing deadline. Cooldown stops new attempts and
does not interrupt streams already in flight. A failed body read does not return
response headers through the current transport contract, so it cannot contribute
a Retry-After cooldown. No request/event/persistent schema changes are involved.

Behavioral tests use controlled transports and paused time for admission, retry,
cooldown and cancellation, plus a query-loop cancellation test and a Local Runtime
shared-domain wiring test. These establish local coordination behavior, not a
provider-specific rate limit or throughput claim.

## Outbound HTTP product identity

The Local Runtime model HTTP client (JSON and SSE) and the MCP adapter's plugin
and host-owned HTTP clients send `User-Agent: centaeris/<version>` by default.
The version comes from their Cargo workspace package version, covered by the
product-version parity gate. Explicit model-provider request headers override
this default case-insensitively. The host-owned MCP client retains its existing
`x-`-only custom-header contract.

This identifies the sending software, not an authenticated user or the currently
attached Desktop/TUI viewer. It does not change browser page requests, Runtime
wire contracts, or persistent records. Request-level loopback tests cover the
model JSON/SSE defaults and overrides, plus both MCP connection paths.

OpenCode Go model requests additionally carry `x-opencode-session` from the
Runtime conversation ID, stable across runs and compaction. Child runs use their
child conversation ID; unrelated providers do not receive this routing header.
The Go catalog resolves omitted effort to explicit `high`, with `low`, `high`,
and `max` selectable and sent as `reasoning_effort`.

## Persistence

Local state lives below the user data root, which defaults to `~/.centaeris`.
SQLite owns indexed runtime state. Session JSONL and observation content remain
durable files with strict identities. The database adapter hydrates
storage-private content before Core decodes it; private storage references do
not become public Session events.

Core compiles without SQLite. SQLite integration and fault-injection tests live
with `runtime_sqlite`; private Core tests use narrow fakes.

## Execution

The Local Runtime executes against the user's selected working directory.
`ExecutionHost` file identities are opaque to Core. Hosts enforce their own
platform process boundary and translate results back into canonical tool
receipts. Windows Desktop and TUI run a native Windows Runtime that executes
host commands through Git for Windows Bash; Linux and macOS run the same Runtime
natively. The local Host claims no OS sandbox and reports that policy is not
enforced.

Core's execution policy and process contracts are independent of concrete
isolation backends. Hosts report whether policy is enforced and explicit error
categories. The [ExecutionHost contract](ExecutionHostContract.md) records the
current native local Host and the macOS Runtime CI results.

Process shutdown is owned by the Host and Runtime lifecycle. AgentRuns belong
to the Runtime service and survive the last local client disconnecting. Explicit
cancel and service shutdown use Core interruption semantics; idle shutdown
waits until active runs and background jobs have finished.

Local background commands use the same Runtime-owned process service for Desktop,
TUI and the native `process_*` dynamic tools. The Agent adapter persists admission
intent, provenance and bounded completion/output records. Its completion worker
uses normal idempotent Session run admission after the Session becomes idle;
cancelled/failed origins do not auto-resume. Internal admission shares client-run
Session leases, deletion fences and shutdown exclusion, while client connections
remain observers. This adds no second Core execution loop or Core wire schema.
See [process completion delivery](../reference/RuntimeProtocol.md#agent-background-commands-and-completion-delivery).

## Extensions

Plugins and Skills are runtime files, not source dependencies. A request freezes
one resolved activation before the model sees contributed Skills, CLI paths,
MCP tools, or Hooks. Core owns their composition and execution semantics; the
package does not receive a second Agent loop.

This repository contains local and hosted product components. packages/core
owns host-agnostic contracts and runtime semantics; packages/api owns hosted
control-plane facts, and packages/runtime_server adapts those contracts. Private
plugins, credentials, customer data and local deployment configuration remain external.

## Desktop Git workbench

The Desktop Review tab uses additive `workspace_git_review_get`,
`workspace_git_review_diff_get`, `workspace_git_stage` and `workspace_git_unstage`
Host-surface commands. `workspace_git_commit` remains available at the Host boundary;
Review has no manual commit form. Git/gh agent operations are the primary commit workflow. Existing Git read commands keep
their meaning. System Git owns repository semantics; Core has no Git workbench
state. The Runtime adapter uses argument arrays and literal pathspecs, parses
NUL-delimited porcelain v2 status, and displays index and working-tree diffs separately.
Review operates on a selected repository root, including linked worktree roots;
opening a subdirectory asks the user to select the root rather than silently
changing the scope of a commit.

Writes carry the reviewed HEAD and a digest of `ls-files --stage -z`. The adapter
rejects an already-stale HEAD/index, serializes its own operations per canonical
worktree root, and runs Git outside the Runtime host-state mutex. This is an
optimistic preflight check, not an atomic fence against external Git processes.
Staging takes the current working file; it does not promise to stage the exact
bytes of an earlier diff. Refresh is explicit and every completed write is
followed by a fresh read. Conflicts and unfinished merge/rebase/cherry-pick/revert
operations require Git CLI resolution before a Host commit.

Commit messages go to `git commit --file=-` through stdin. User identity, hooks,
and signing remain Git-owned; no hook bypass or automatic write retry is added.
Each Git process has a 15-second bound and 1 MiB output capture limit. Incomplete
status/index output is rejected; diff output may be explicitly truncated. A hook
failure, timeout or lost response can leave an uncertain outcome. Callers must
reconcile before retrying; the Review tab does not
invoke commits or add preventive prompts or a confirmation dialog.
This feature does not provide durable command receipts or exactly-once commits.
Successful Host commit responses include the resulting HEAD, checked against the expected parent;
external processes can still race this observation.

The shared `packages/runtime/generated/workspace-git-samples.json` fixture is
checked against Rust serialization/strict request decoding and Desktop bridge
payloads. Real temporary-repository tests cover partial staging, unborn HEAD,
renames, literal paths, conflicts, hooks, and stale review rejection.

### Review view sources

`workspace_git_view_get` is an additive read endpoint for `unstaged`, `staged`
and `branch` views. It returns per-file numstat values (`null` for unavailable or
binary counts), rename identities and an optional requested file diff. Unstaged
also lists untracked and conflicted files without inventing textual diff statistics.
Branch compares the merge base of the chosen reference and HEAD against HEAD;
uncommitted edits are excluded. The base can be entered explicitly. Default
selection uses origin's symbolic HEAD, then origin/main, origin/master, main or
master when resolvable. No remote fetch is performed. There is no last-turn view:
the current application has no authoritative per-turn Git checkpoint for it.

Review presents collapsible file rows, a path filter, current-diff line search,
copy-path/open-file actions and a file menu for staging. It reads only the expanded
file diff, rejects late results after selection changes, and refreshes on window
focus/visibility return or every 60 seconds while the view is visible. Explicit
refresh remains available. All data is a live Git read, not an atomic worktree
snapshot. Git's machine stdout is captured as UTF-8 bytes without the general
process-output encoding heuristic; execution-host display decoding is unchanged.

## Local Host automation

OpenSSH command adaptation and persistent local schedules live in Runtime Host.
The scheduler uses ordinary idempotent Session/AgentRun admission; clocks and
plan storage do not enter Core. Desktop and TUI share the Host methods and Agent
tools. See [Local automation](../reference/LocalAutomation.md) for lifecycle,
command syntax, recovery boundaries and local-only limitations.

### Session catalog read boundary

The local Runtime owns a derived SQLite Session catalog at
`runtime/session-catalog.sqlite3`. Session JSONL and observation CAS remain the
authoritative records. Catalog queries return metadata and run summaries without
hydrating unrelated history. Before mutating a source, the Runtime durably queues
its catalog repair; index publication occurs after source projection under the
Session log lock. The first import is resumable and ordinary reopen uses the saved
index. Warm dirty-source repair uses the existing Core incremental reducer state;
cold cache misses and rewrites reconstruct the affected source.

The Host adds `session/catalog` for bounded pages and revision-based discovery,
and deletion responses enumerate the actual deleted Session identities. Core
reducers, Session record formats and Runtime store schema remain unchanged.
Reducer checkpoints are bounded, disposable memory; full runtime-snapshot recovery
still reconstructs the selected Session from its authoritative records.
See [persistence acceptance](../eval/PersistenceAcceptance.md) for reconstruction,
maintenance boundaries and source-read regression tests.
