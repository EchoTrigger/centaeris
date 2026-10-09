# Centaeris Core

`packages/core` (`centaeris-core`) is the single owner of host-agnostic Runtime
semantics: sessions, turns, model requests, tool execution, runtime events,
checkpoints, context projection, and continuation. It does not depend on
Electron, a terminal, Docker, HTTP control planes, or a concrete database.

Hosts and adapters (`runtime`, `runtime_sqlite`, `mcp`, `desktop`, `tui`,
`runtime_server`, `api`) translate external systems into these contracts. They
may translate identities, transport, storage, or process behavior, but they
must not define a second prompt, tool loop, continuation state machine, or
Session truth.

## Modules

- `execution` — execution policy and process contracts, independent of a
  concrete isolation backend.
- `extension` — Plugin, Skill, MCP, and Hook composition and activation.
- `model` — model request admission, provider bindings, and retry/cooldown.
- `runtime` — Session, turn, and continuation semantics.
- `session` — durable Session record contracts.
- `tool` — built-in and dynamic tool contracts, limits, and receipts.

See [Architecture](../../docs/architecture/Architecture.md) for ownership and
request flow, and [Runtime protocol](../../docs/reference/RuntimeProtocol.md)
for the Host boundary Core is adapted across.

## Focused gate

```powershell
cargo test --locked -p centaeris-core query_loop
```

Every Rust change in the workspace runs at least this gate; see
[Testing](../../docs/development/Testing.md) for the full local gate and test
quality policy.
