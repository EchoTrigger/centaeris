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

Process shutdown is owned by the Host and Runtime lifecycle. Closing the last
local client must not leave an unowned Runtime or child process consuming work.

## Extensions

Plugins and Skills are runtime files, not source dependencies. A request freezes
one resolved activation before the model sees contributed Skills, CLI paths,
MCP tools, or Hooks. Core owns their composition and execution semantics; the
package does not receive a second Agent loop.

This repository defines and validates the public package contracts but does not
contain concrete commercial packages, hosted configuration, credentials, or
customer data.
