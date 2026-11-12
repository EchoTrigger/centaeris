# Session body search

Workspace search matches titles and committed user-visible text in the existing
PostgreSQL records. It covers `user_message.payload.text`,
`phase_event.payload.message` when `stage` is `model_process_summary`, and
`assistant_message.payload.modelMarkdown`. Tool arguments, reasoning and other
status records are excluded. Uncommitted streaming text is not searched.

## Storage and authorization

`workspace_session_search_text(jsonb)` selects those three text fields. Partial
GIN expression indexes use pg_bigm's `gin_bigm_ops` over lowercased text; a second
GIN index covers lowercased Session titles. Existing event/Session indexes retain
their scope and sequence roles. Original records remain authoritative; search
does not create a second message store or reconstruct Runtime semantics.

Migration `0054_session_body_search` creates the extension, immutable extraction
function and indexes. Indexes are built concurrently, including historical rows,
and PostgreSQL maintains them for later commits and deletions. The PostgreSQL
image builds pg_bigm from a pinned upstream release archive with a checked SHA256
and preloads it. Use that image for both application and test databases; a plain
PostgreSQL image does not supply the required extension. Application migrations
must run with authority to create the extension and function.

Each HTTP request reuses the Session-list membership/delegation checks. Its SQL
scope includes workspace, owner, active status, interactive purpose and absence
of archival. Administrator status does not grant access to other users' Sessions.
Authorization is applied to matching rows, not after a global result limit.

## HTTP and interaction

`GET /api/workspaces/{workspaceId}/sessions/search?query=...&cursor=...` accepts a
literal, case-insensitive substring of 1–256 characters after trimming. `%` and
`_` are literal characters, not caller-provided SQL wildcards. Unknown and repeated
parameters are rejected. Materials retain their existing name-only search.

Results are grouped by Session, ordered by descending update time then ascending
Session ID, in pages of 20. Each result includes the latest matching message's
type, a bounded context snippet, event/message identity and source sequence.
Title-only matches have no message locator. The opaque cursor binds the query,
user and workspace; changing any of them requires a fresh first page.

The existing sidebar/Ctrl-or-Cmd+K entry shows body-match labels and highlights
literal hits. Typing aborts older requests and waits briefly before searching;
stale results cannot replace the current query. More results retain the query.
Failures preserve already-loaded matches and allow retry. Opening search pushes
one browser history entry. Its `#search?q=...&filter=...` fragment retains the
query without revalidating route loaders for each keystroke; typing and filtering
replace that entry. Selecting a result pushes its Session route with `at` (event
identity) and `highlight` (literal query). Browser Back restores the query,
loaded pages, selection and result scroll position from a memory cache scoped
to the authenticated user and Workspace. A reload rebuilds results from the URL.

`GET /api/sessions/{sessionId}/transcript?anchorEventId=...` authorizes the Session
and visible source event before requesting its sequence from Runtime. The
response is `workspace.transcript.target.v1` with `targetBlockId` and a normal
bounded transcript `page`. Projection-progress retries retain the frozen
`sourceHighWater` and `projectionGeneration`. Runtime seeks the projection's
position index, using Core's bound cursor contract, and returns a page ending at
the target with preceding context. Earlier pages use the existing cursor.

This historical window stays separate from the live tail. The UI expands a
matched process, scrolls to its block or first highlighted word and pauses
automatic following. Rendered text is highlighted without modifying Markdown;
referenced long text uses the ordinary content reader. The first loaded hit is
positioned once; later text arrival must not override user scrolling. Return to
latest replaces the positioning URL and restores the live view. Reading a search
window does not acknowledge the latest conversation as read.

## Verification

Focused Django coverage lives in `app_core.test_session_search`; component
coverage lives in `workspace-search.test.tsx` and the existing workspace-home
search test. Migration coverage inserts text before indexing and verifies both
historical and later committed messages through the HTTP search endpoint.

An isolated PostgreSQL 18 probe with 100,000 synthetic documents naturally used
GIN Bitmap Index Scan for both a two-character Chinese substring and a
case-normalized English identifier inside long messages. This validates the
index operator path, not production latency or capacity. Evaluate real query
plans, GIN size, write cost and common-term selectivity as the corpus grows.

References: [pg_bigm](https://pgbigm.github.io/pg_bigm/pg_bigm_en.html),
[GIN](https://www.postgresql.org/docs/18/gin.html),
[B-tree and other indexes](https://www.postgresql.org/docs/18/indexes-types.html).
