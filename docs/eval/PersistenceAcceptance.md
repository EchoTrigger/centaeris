# Persistence acceptance

Runtime creates the current SQLite schema in one transaction. Opening a database
validates its schema version, history, tables, and indexes before using it.
Unsupported or malformed stores fail without modifying stored facts.

The user configuration reader accepts the current schema and validates model
identities and reasoning preferences. Reading configuration does not rewrite it.

Session logs and observation content are authoritative. Transcript projections
are derived from those records and can be rebuilt into a replacement generation.
Rebuilding must preserve source bytes, citations, tool output references, and
conversation continuation. Restarting a completed rebuild must reuse its
published generation.

The focused acceptance commands are:

```powershell
cargo test --locked -p centaeris-runtime --bin centaeris-runtime transcript_runtime::tests -- --test-threads=1
cargo test --locked -p centaeris-runtime --bin centaeris-runtime user_config::tests -- --test-threads=1
cargo test --locked -p centaeris-runtime-sqlite -- --test-threads=1
cargo test --locked -p centaeris-core query_loop
```

The normal workspace test gate includes these checks. Fixtures use synthetic
session records and temporary databases; no user data or model provider is needed.
