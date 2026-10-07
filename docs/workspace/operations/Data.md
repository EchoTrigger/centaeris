# Data and recovery

## Persistent stores

| Store | Contents |
| --- | --- |
| `postgres-data` | Identity, membership, ACL, AgentRun, durable jobs, Runtime facts, and file metadata |
| `storage-data` | Original files and API-owned stored objects |
| `runtime-data` | Runtime-private durable state and generated runtime files |
| `plugin-data` | Installed Plugin directories and catalog |
| `agent-memory` | Agent memory files owned by the hosted memory boundary |

Redis carries bounded transient browser and Runtime live state. Its loss can
interrupt a live connection but must not erase durable history or jobs.

## Session output storage

The hosted `SessionEvent.payload` JSONB column is a query index, not a reason to
change a Core tool result. PostgreSQL cannot represent U+0000 in JSONB strings.
When it occurs, the private `workspace.session_event.storage.v1` encoding keeps
the exact serialized wire under `__centaerisSessionStorage.canonicalJson` and a
JSONB-safe query projection beside it. Readers validate that projection and
decode the original wire before Core recovery, transcript content reads, or API
delivery. A literal `\\u0000` remains distinct from the actual control byte.
Unknown storage versions and mismatched query projections fail closed.

Migration `0006_session_event_payload_storage` retains existing records and the
JSONB indexes. Removing the decoder is refused once encoded output exists.
Canonical execution receipts remain in Runtime's durable text event store.
Transient Session commits retry the same receipt with a bounded delay; an
SQL rejection or exhausted attempt yields through the existing lifecycle job
with `session_record_commit_unavailable`, without spending its failure budget
or marking the Run failed. Recovery reuses Core's durable receipt and closure
planner; it does not execute the tool again. Lease and validation failures
retain their existing stop rules.

## Command receipts

Hosted command receipts retain their request digest and accepted result IDs in
PostgreSQL. They do not store prompts, uploaded bytes, or credentials. Their
deduplication identity does not expire by time in v1, and deleting a Session or
Run must not remove that identity and turn a replay into a new command. Permanent
deletion of the owning user or Workspace may remove its scoped receipts; those
scope identities must never be reused.

The first-release initial schema includes empty receipt storage. API clients
must send the required operation identity. Backups and restores include receipts
with business rows; restoring only one side loses the acceptance guarantee.

## File identity

Database rows identify and authorize files; bytes remain in Storage. A complete
backup therefore includes PostgreSQL and every persistent file volume. Backing
up only one side can leave valid metadata without bytes or unowned bytes without
metadata.

Library and session file uploads reuse the earliest ready library object with
the same SHA-256 for the same user, regardless of filename or folder. Reuse
preserves its name and location and cleans up the newly uploaded storage copy.
Other users and deleted objects are excluded. Different content with a conflicting
name in the target folder receives `(1)`, `(2)`, etc. before the extension.
This does not merge historical duplicates or change manual note/artifact workflows.

Historical tool spill references containing only a workspace path and byte
range cannot prove the original output. The transcript content API returns
`transcript_content_unavailable` for them, including records written before the
fix. This reader policy needs no database migration and does not rewrite or
delete Session events, snapshots, or published artifacts. It does not backfill
old references from current workspace bytes. Committed previews are retained,
and complete inline output remains readable subject to current authorization. Restoring the
old snapshot reader would reintroduce incorrect historical content; reverting
code is not a content-recovery procedure. Full-output retention requires a
separate immutable capture contract.

## Persistent application authorization upgrade

`0002_persistent_app_delegations` is a forward migration from `0001_initial`.
It retains existing users, applications, grant IDs, token digests, deadlines,
revocations, memberships, Agent/Session identities and input facts. Existing
grants receive credential version 1 and retain their definition target; existing
inputs have no invented application origin. Audit rows are recorded for new
operations, not fabricated for historical grants.

Only new explicit consent can create a grant without an expiry or target a native
Agent. The upgrade does not extend, revive or convert existing finite grants.
New input facts retain their accepting application, grant and credential version.
Credentials can rotate without rewriting those facts or the Agent's history.
Back up the database before applying the migration. Older application versions
do not understand native targets or nullable expiry; do not downgrade a database
that has used these capabilities to an older application/schema.

## Persistent browser credential upgrade

`0003_persistent_browser_login` adds a nullable-deadline browser credential table
and atomically transfers existing `django_session` keys, signed data and original
deadlines into it. It then removes the legacy copies so logout and credential
deletion have one authority. No Agent, Session, history, memory or business grant
is recreated. Unexpired old credentials adopt permanent validity on their next
authenticated request; expired credentials retain their rejection behavior.

Stop or drain old API writers before applying this migration and start only the
new version afterward. The PostgreSQL transfer locks the legacy table while
copying; it cannot protect against an old binary writing to that table after
the migration commits. Back up first. The reverse migration refuses to drop the
new table while browser credentials remain; any planned downgrade must revoke
them explicitly rather than fabricate old deadlines or revive legacy copies.

## Persistent business Agent branches

`0004_business_agent_branches` creates empty branch identity storage and does not
derive external users from existing Agents, Sessions or message text. New explicit
resolution uniquely binds `(application, root Agent, businessUserId)` to one private
Agent and its unique coordination Session. Identity comparisons preserve case and
Unicode. The binding is immutable and survives token rotation or reissuance;
revoking a grant stops access without deleting an accepted user's tree.

Each branch has a different Agent ID, so existing Agent memory storage isolates
its private memory. Its coordination and child work Sessions retain independent
histories, snapshot generations, explicit input links and artifacts. Creation
copies configuration only. Root and sibling conversation data, memory and files
remain where they were. Resolution never replaces a deleted branch or deleted
coordination Session as a way to recover access.

The reverse migration refuses to drop branch identity storage once branches
exist. Retaining anonymous Agent rows without their business user mapping would
lose the identity needed to return to the correct persistent tree.

Back up the branch map with the Agents, coordination/work bindings, input facts,
Sessions and file metadata, and include the existing memory and snapshot volumes.
Restoring only the branch map or issuing a new credential is not a history restore.
An upgrade/restore check must return the same branch, Agent and Session for an
existing application/root/business user identity.

## Trash and deletion

Supported product objects use a 30-day trash lifecycle where defined by their
model. The server-side `gc` service reclaims objects after the durable deadline.
Removing a browser row, Redis key, container, or local cache does not perform
permanent deletion.

Plugin uninstall and credential deletion follow separate lifecycle and audit
rules. A running AgentRun keeps its frozen activation and must finish or stop
before required package bytes are removed.

## Backup

Runtime store schema v5 adds the Core `required_completion_delivery` observation
kind. Opening an intact v4 store performs a forward migration under an exclusive
migration-ledger lock, retaining its records and checkpoint attachments. The
complete v4 table, index and constraint shape is checked before any write; unknown
versions or drift fail. Back up and stop old Runtime writers before upgrading.
Older binaries require restoration of the matching database backup for rollback.

Take a consistent PostgreSQL backup and snapshot persistent volumes while
writes are stopped or through a tested coordinated snapshot mechanism. Record
the source revision, migration state, image identities, and volume set with the
backup. Do not copy secrets into a public test report.

## Restore

Restore into an isolated environment first. Use the exact source and image
revision compatible with the backup, restore PostgreSQL and file volumes, then
run read-only integrity checks before accepting new work. A restore drill must
verify login, Session history, file download, Plugin catalog, and one Runtime
request without production model credentials.

No current command promises point-in-time recovery or cross-version downgrade.
Those claims require a dedicated tested implementation.
