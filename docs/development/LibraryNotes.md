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
