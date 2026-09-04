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
receipts. The current verified Windows implementation uses Git for Windows Bash
as a host process and does not claim an operating-system sandbox.

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
