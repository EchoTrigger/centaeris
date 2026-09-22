# Persistent data upgrade acceptance

The released `v1.0.0` tag resolves to
`5ba7cb73540ddb208cde40637a5044dd914b2a97` (2026-09-09). Its SQLite schema
is version 1 and has no transcript projection tables. The later development
schema versions 2/3 contain derived transcript rows that may have snake_case
enum fields. These are two separate upgrade cases.

Schema 4 uses the existing SQLite backup and transaction mechanism, and marks
pre-v4 projection generations invalid. Runtime rebuilds a replacement from the
session log and observation content, then publishes that generation. It does
not rewrite the log, accept old wire aliases, or discard unknown/corrupt facts.
The first history load can therefore show projection loading while rebuilding.
Even already-canonical pre-v4 projections are rebuilt once; newly created
databases start at schema 4 without this invalidation.

Backups are beside the database as `runtime.db.pre-vN-to-v4-<timestamp>.backup`.
Failed migrations roll back the invalidation and schema-history write together.
Reopening a migrated store does not repeat the migration. Old clients must not
open the upgraded database; the existing downgrade guard rejects that schema.
Rollback requires the matching pre-upgrade backup and its original source data.

## Focused checks

```powershell
cargo test --locked -p centaeris-runtime --bin centaeris-runtime transcript_runtime::tests -- --test-threads=1
cargo test --locked -p centaeris-runtime-sqlite -- --test-threads=1
cargo test --locked -p centaeris-core query_loop
```

The released fixture covers user text, reasoning, tool output references,
citation facts, final answer, an external source object, and an opaque persisted
snapshot. Tests verify projection, unchanged source bytes, restart, appended
conversation input, and a second restart. They do not call a model provider.
The development-store regression changes a stored reasoning field to
`request_id`, reproducing the reported decode failure before the fix. It checks
restart before rebuilding and that completed rebuilds are not repeated.
SQLite fault injection fails the schema-version write after invalidation and
checks rollback, the pre-migration backup, retry, and idempotence.

Memory uses the separate hosted Markdown protocol, not this local SQLite schema.
On 2026-09-21, Workspace's `scripts/memory-protocol-acceptance.py` passed against
local image `sha256:e97081eda70d049a8df7960408f5188ff116df4bb1a9f4ccba092806f07d476e`:
26 isolated helper containers covered discovery, read/write, guarded update,
fresh-container restore, scope isolation, and forgetting. This verifies current
Memory persistence, not a claim that Desktop 1.0.0 stored hosted Memory. No real
user data or deployment volumes were used.

## Reproduce the released fixtures

Use an isolated checkout at the exact release commit and a new output directory:

```powershell
git worktree add --detach <old-checkout> v1.0.0
python scripts/upgrade-fixtures/generate.py <old-checkout> <new-output-directory>
```

The generator temporarily adds test/export entry points to the old checkout,
then restores those source files. The released Core validates and serializes a
synthetic session; the released Runtime installs its observation manifest and
produces stored JSONL; the released SQLite adapter creates and populates its
database. Python exports that database as reviewable SQL. No binary database is
tracked. Generation is manual; normal tests consume the frozen fixtures under
`packages/runtime/tests/fixtures/upgrade-v1.0.0` without Git, Python, or network.
Copy `session-1.jsonl` as `session.jsonl`, `runtime.sql`, and the observation
directory from the output when intentionally regenerating these fixtures.
