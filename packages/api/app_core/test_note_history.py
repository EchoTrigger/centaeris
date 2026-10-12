"""History budgets retain the head and protected bytes; receipts outlive bodies."""

from uuid import uuid4
from django.contrib.auth import get_user_model
from django.test import TestCase, override_settings

from .library_notes import create_note, save_note
from .models import LibraryNoteRevision, LibraryNoteSaveReceipt, UserLibraryObject


class NoteHistoryTests(TestCase):
    def setUp(self):
        self.user = get_user_model().objects.create_user(username="history-owner")
        result = create_note(self.user, save_id=uuid4().hex, display_name="note", markdown="one")
        self.note = UserLibraryObject.objects.get(pk=result["object"]["id"])

    def save(self, body):
        self.note.refresh_from_db()
        return save_note(self.user, self.note.id, save_id=uuid4().hex,
                         base_generation=self.note.contentGeneration, display_name="note", markdown=body)

    @override_settings(NOTE_MAX_BYTES=8, NOTE_HISTORY_MAX_BYTES=16, NOTE_HISTORY_MAX_VERSIONS=4,
                       NOTE_HISTORY_RECENT=2, NOTE_HISTORY_DAYS=1)
    def test_admission_prunes_unprotected_history_within_byte_and_count_budgets(self):
        from .note_history import protect_revision
        protect_revision(self.user, self.note.id, 1, True)
        for body in ["two", "three", "four", "five", "six"]:
            self.save(body)
        revisions = list(LibraryNoteRevision.objects.filter(note=self.note))
        self.assertLessEqual(sum(item.sizeBytes for item in revisions), 16)
        self.assertLessEqual(len(revisions), 4)
        self.assertTrue(any(item.generation == 1 for item in revisions))
        self.note.refresh_from_db()
        self.assertTrue(any(item.id == self.note.noteHead_id for item in revisions))
        self.assertEqual(LibraryNoteSaveReceipt.objects.count(), 6)

    def test_dry_run_does_not_delete_and_restore_creates_new_version(self):
        from .note_history import collect_history, restore_revision
        self.save("two")
        before = LibraryNoteRevision.objects.count()
        collect_history(self.note.id, dry_run=True)
        self.assertEqual(LibraryNoteRevision.objects.count(), before)
        result = restore_revision(self.user, self.note.id, save_id=uuid4().hex,
                                  base_generation=2, revision_generation=1)
        self.assertEqual(result["appliedGeneration"], 3)
        self.note.refresh_from_db()
        self.assertEqual(self.note.noteHead.markdown, "one")

    @override_settings(NOTE_MAX_BYTES=8, NOTE_HISTORY_MAX_BYTES=10)
    def test_protection_reserves_capacity_for_next_current_body(self):
        from .note_history import protect_revision
        self.save("12345678")
        with self.assertRaisesRegex(ValueError, "note_history_protection_capacity"):
            protect_revision(self.user, self.note.id, 2, True)
        self.save("abcdefgh")

    def test_released_migration_file_is_scheduled_not_physically_deleted(self):
        from .assets import store_bytes
        from .note_migration import release_migration_file
        from .models import DerivedResource
        from django.core.files.storage import default_storage
        metadata = store_bytes(b"old", f"users/{self.user.id}/library", "old.md", "text/markdown")
        self.note.storageKey = metadata["storageKey"]
        self.note.save(update_fields=["storageKey"])
        release_migration_file(self.note.id, dry_run=True)
        self.assertFalse(DerivedResource.objects.exists())
        release_migration_file(self.note.id)
        self.note.refresh_from_db()
        self.assertEqual(self.note.storageKey, "")
        self.assertTrue(default_storage.exists(metadata["storageKey"]))
        self.assertEqual(DerivedResource.objects.get(resourceKey=metadata["storageKey"]).state, "pending")
