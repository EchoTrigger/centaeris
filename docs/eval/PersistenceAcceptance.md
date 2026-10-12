# Persistence acceptance

## Local work history and admission receipts

`runtime/schedules.sqlite3` and `runtime/process-completions/index.sqlite3`
are Host-private indexed stores. Their one-time imports preserve the previous
schedule JSON backup and original completion JSON records. Import markers and
records commit together. Completed history does not participate in scheduler or
completion-worker queries. Start receipts use `runtime/work-receipts.sqlite3`,
keyed by service identity and operation key; historical counts do not close
admission and eviction does not permit a duplicate process start.

The Runtime tests cover migration backup/reopen, more than the previous history
ceilings, pending-only reads in the presence of invalid unrelated historical
bodies, retained receipt identity and rejection of newer private schemas.
`scripts/test-host-automation.mjs` and `scripts/test-process-agent.mjs` additionally
exercise lost acknowledgements and restart against real Runtime transports.
Only forward migration is supported; retained JSON files are recovery backups,
not stores for concurrently running an older Runtime.

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

## Host-private Session catalog

User-data layout v1 migrates forward to v2 before enabling the catalog. The
migration preserves the original creation timestamp and source records, saves
`layout.json.pre-v1-to-v2.backup`, and atomically replaces only `layout.json`.
Retry after a saved backup is idempotent; a conflicting backup or unknown field
fails without replacing the manifest. Older Runtime binaries reject layout v2,
preventing source writes that would bypass catalog dirty intents. Desktop and
TUI clients can continue using the current shared Runtime protocol; an older
bundled Runtime cannot reopen the upgraded profile. This changes the reported
`layoutSchemaVersion`, not the Core protocol, Session format, or runtime-store
schema version.


The Runtime maintains `runtime/session-catalog.sqlite3` as a rebuildable read index
of authoritative Session logs. It stores Session summaries, source paths, run
summaries and run ownership. It does not replace Session records or introduce a
Core contract. Its private schema has a strict version independent
of the runtime store schema; unsupported versions fail without overwriting data.

The first catalog use discovers sources once and atomically records the pending
import list. Each projected source commits independently, so an interrupted
import resumes outstanding entries. Normal reopen does not rescan the directory.
Source mutation records a durable dirty intent before touching JSONL/CAS files.
Readers reconcile changed sources under the existing Session log lock and publish
summaries and run locators in one index transaction. Interrupted creation,
append, and deletion are recovered from these intents. File writes outside the
Runtime are not automatically discovered by normal catalog reads; restoring
Session JSONL without the derived index triggers reconstruction. Explicit Session
diagnostics remains the full-source inspection and orphan-CAS maintenance path.

Warm catalog reads use summaries; ID lookups use indexed Session/run identities.
Unrelated source content is not loaded. Private catalog schema v1 migrates to v2
transactionally by deriving paging entries from saved summaries, without changing
Session JSONL, the user-data layout version, or the runtime-store schema.

Desktop and TUI use `session/catalog`: recent and pinned pages are separate,
workspace scopes are indexed, and pages use a `(ordering,id)` keyset rather than
OFFSET. The default page is 50 summaries and the maximum is 100. Revision cursors
include the index generation and query scope. A changed page snapshot returns
`reset: true`; clients reload that scope instead of silently skipping moved rows.
Changes are coalesced per Session, include deletions, and are returned in batches
of at most 100. Cursors older than 4,096 revisions require a fresh snapshot;
expired deletion markers are removed. Desktop observes only changes every five
seconds while visible; focus does not reload the conversation. Workspace expansion
loads its page and Load more retrieves older summaries. TUI loads the selected
workspace and uses N for the next page. Explicit `/resume <query>` search may walk
all pages of the selected workspace. `session/list` remains the explicit full-list
operation; normal Desktop/TUI discovery does not use it. Delete responses include
`deletedSessionIds`, so paged clients can discard child views without previously
loading all child summaries.

Ordinary appends reuse Core's existing `reduce_event` and `AgentRunSessionState`.
Host reducer checkpoints are disposable in-memory state. The cache keeps at most
eight idle Sessions; active Sessions retain their state separately and do not
count toward this idle limit. There is no byte-budget or 64 MiB memory guarantee.
Event fingerprints preserve duplicate/conflict
checks. Successful appends advance the checkpoint only after source sync. Errors
discard the tentative state. Warm catalog repair publishes only changed run rows;
stream delivery reads requested events by location instead of replaying the full
run. Content-location caches have Session and entry-count limits.

Internal scheduler parent checks, child-runtime binding reads, and terminal-delivery
binding discovery share full-list visibility rules: duplicated source IDs are
hidden even when only one source is valid, and subagent Sessions need a unique
valid main parent. These operations construct only selected summaries; delivery
iterates candidate metadata and returns at most two visible matches to detect
ambiguity, including an extra child without a Run. All dirty sources are still
reconciled first. This change does not claim target-only disk I/O, constant-time
cold startup, or a two-row SQL scan. Strict Session lookup, public full-history
reads, and complete deletion-subtree enumeration keep their existing contracts.
Narrow reads validate the selected summary, referenced parent, and visited binding
candidates; they do not build all public summaries or query the idle registry as
an incidental health probe. They therefore do not promise the former full-list's
propagation of every unrelated summary/registry error. Source/index and query
failures still propagate. Cached summary identities must match their indexed
source identity; a mismatch fails loudly in full and narrow views. This is strict
validation of a disposable corrupted cache, not an assertion that every old error
path remains identical.

Cold cache misses, eviction, source identity changes, and tombstone rewrites rebuild
the affected Session through authoritative Core reduction. These are **not durable
Core reducer checkpoints**. The existing full runtime-snapshot reconstruction and
explicit full-history requests still read the selected Session. These paths have
no constant-latency guarantee as that individual Session grows. There is no claim
that every operation becomes independent of history length. With the index already
established and no dirty sources awaiting repair, warm catalog queries do not read
unrelated full source documents. Cold initialization and dirty repair may read the
affected sources, including sources outside the requested page or workspace;
read-state misses and eviction may require full reconstruction of those sources.
Page size bounds returned summaries, not that preparation cost. Tested warm
append/summary/stream operations avoid full historical rereads while their validated
read state remains available. No Core semantics or Session record format changed,
and no additional cleanup UI was introduced.

Catalog regression tests include zero source-document reads on warm queries,
durable-intent interruption/reopen, portable reconstruction, and query plans.
Run the large-source acceptance when changing the index:

```powershell
cargo test --locked -p centaeris-runtime --bin centaeris-runtime session_catalog::tests -- --test-threads=1
cargo test --locked -p centaeris-runtime --bin centaeris-runtime session_catalog::tests::hundred_megabytes_of_unrelated_history_do_not_enter_warm_reads -- --ignored --exact --test-threads=1
```
