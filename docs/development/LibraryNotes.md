# Library note persistence

Note publication must preserve the previously acknowledged body if the database
commit fails. File-backed saves register the old storage object as a derived
resource inside the reference-switch transaction; the existing deleted-resource
collector reclaims it only after the newer content generation commits. Cleanup
failure is retried with the collector's persisted backoff and cannot fail a save
or delete its current body. Uncommitted replacement files retain the existing
compensation and orphan-scan recovery path.

Run the PostgreSQL failure tests with:

```powershell
python manage.py test app_core.test_library_note_save --settings=api.test_settings --noinput
```

These tests use `TransactionTestCase` to observe actual commit failures and GC
claims, alongside the existing library HTTP lifecycle acceptance test.

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
