# Centaeris Runtime SQLite

`packages/runtime_sqlite` (`centaeris-runtime-sqlite`) is the local SQLite
`RuntimeStore` adapter. It owns schema, transactions, and persistence for the
Local Runtime: runtime state, transcript storage, reliability/retry
bookkeeping, turn supplement data, and external-context hydration, organized
under `src/sqlite_store/` by concern (`sqlite_schema`, `sqlite_transactions`,
`sqlite_transcript`, `sqlite_runtime`, `sqlite_reliability`,
`sqlite_turn_supplement`, `sqlite_external_context`).

Core compiles without SQLite; this adapter implements Core's storage ports and
is the only place SQLite integration and fault-injection tests live. The
database adapter hydrates storage-private content before Core decodes it —
private storage references never become public Session events.

See [Architecture](../../docs/architecture/Architecture.md#persistence) and
[Persistence acceptance](../../docs/eval/PersistenceAcceptance.md) for schema
validation, restart recovery, and history reconstruction guarantees.

## Focused gate

```powershell
cargo test --locked -p centaeris-runtime-sqlite --test core_runtime
```
