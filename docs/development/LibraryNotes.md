# Library note persistence

Note publication must preserve the previously acknowledged body if the database
commit fails. The initial file-save repair deferred reclamation to the existing
derived-resource collector. Ordinary note saves now commit only database rows:
an immutable revision, current head and operation receipt in one transaction.
Storage failure cannot fail such a commit or delete its current body.

Run the PostgreSQL failure tests with:

```powershell
python manage.py test app_core.test_library_note_save --settings=api.test_settings --noinput
```

These tests use `TransactionTestCase` to observe actual commit failures alongside
the existing library HTTP lifecycle acceptance test.

## Immutable body reads

`LibraryNoteRevision` owns one UTF-8 body and its generation. `noteHead` selects
the current version. Existing file-backed notes remain readable during backfill;
ordinary files continue using the storage backend. Internal content sources
capture database revision bytes when selected, so later history pruning cannot
invalidate an already selected read. Metadata and the head are selected together.
Authorization remains in the existing caller: retaining an older revision does
not bypass captured-input generation checks.

Editor reads, downloads, deferred Agent inputs and platform material processing
share this content selection. `test_note_content` checks exact bytes including
CRLF and Unicode, owner isolation, immutable reads and stale-generation rejection.

## Conditional saves

POST requires a lowercase 32-hex `saveId`. PUT additionally requires
`baseGeneration`. Reuse the exact operation ID and request after an unknown
network outcome. Owner-scoped receipts are checked before the generation fence;
changed reuse fails with `note_save_id_conflict`. A receipt confirms the applied
generation, not the present head. Deleted notes cannot be resurrected by replay.
Receipts retain small metadata independently of body history, for the lifetime
of the owner; there is no expiration that turns an old creation into a new one.

Titles are independent of literal Markdown. Title changes create a revision and
increment `contentGeneration`, including generic renames and trash restoration;
old captured Agent inputs therefore remain stale. Moves alone do not change the
content generation. PUT responses confirm the operation without echoing the body.

## Deployment and rollback

Apply the new schema first, then run `migrate_library_notes --dry-run`, followed
by bounded `--limit` batches until `remaining=0`. Missing, corrupt, oversized or
invalid UTF-8 bodies fail closed. The backfill reads storage before entering a
short transaction and verifies the captured key, generation, hash, size, title,
status and head before publication. Files are retained. Do not enable the new
writer until all live notes are verified and history capacity policy is installed.

Coordinate API/Web deployment and refresh old clients: requests lacking operation
identity are rejected rather than interpreted under the new contract. Unmigrated
notes remain readable but saving returns `note_migration_required`.

Rollback requires a write maintenance window and
`migrate_library_notes --export-for-rollback --dry-run`, then export batches until
`remaining=0`. Export verifies the latest database revision's bytes, publishes
that file reference and clears the head. Verify hashes before rolling back code;
old original files cannot represent edits made after migration. Keep the added
schema and migration history. Interrupted exports retain orphan-scan recovery.

## History capacity and reclamation

Each committed changed body runs the retention policy in its transaction.
PostgreSQL rejects changes to committed revision identity/body fields, including
bulk ORM updates; only the protection flag is mutable. The reversible trigger
migration is covered by actual update rejection tests.
`NOTE_MAX_BYTES` defaults to 1 MiB. Per-note retained bodies, including current and
protected versions, are bounded by `NOTE_HISTORY_MAX_BYTES` (16 MiB) and
`NOTE_HISTORY_MAX_VERSIONS` (128). Optional history prefers the newest
`NOTE_HISTORY_RECENT` (20) versions, then one version per UTC day within
`NOTE_HISTORY_DAYS` (30); byte/count limits override optional retention. These
defaults bound per-note amplification while retaining recent small-note edits;
they are operational starting values, not measured workload capacity. Full-body
database writes, WAL, backups and owner-lifetime receipt metadata still consume
space. This policy does not establish a whole-user/database storage quota.

Protecting a version must leave room for one maximum-size future current body
and one revision slot, so history cannot starve ordinary saves. Current heads and
protected history are never pruned from live/recoverable notes. Purged notes can
release all bodies; receipts remain. Restoration creates a new increasing version.
Owner-scoped HTTP history listing, protection and restoration keep existing Agent
input authorization unchanged.

Run `gc_note_history --dry-run` before explicit reclamation. Saves also prune to
the same policy. After backfill validation and the rollback retention decision,
run `migrate_library_notes --release-files --dry-run`, then release batches: this
only clears old references and records retryable GC resources. Actual deletion
uses the existing `gc_deleted_resources` collector, with its grace period and
backoff. Exporting for rollback remains possible from the database afterwards.
