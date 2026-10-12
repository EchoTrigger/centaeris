"""Database note commits cannot lose acknowledged bodies on failure."""
from unittest.mock import patch
from uuid import uuid4

from django.contrib.auth import get_user_model
from django.db import DatabaseError, connection
from django.test import TransactionTestCase

from .library_notes import create_note, save_note
from .models import LibraryNoteRevision, LibraryNoteSaveReceipt, UserLibraryObject


class LibraryNoteCommitTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="note-writer")
        result = create_note(self.user, save_id=uuid4().hex, display_name="note", markdown="acknowledged")
        self.note = UserLibraryObject.objects.get(pk=result["object"]["id"])

    def save_note(self):
        return save_note(self.user, self.note.id, save_id=uuid4().hex, base_generation=1,
                         display_name="note", markdown="latest")

    def test_failed_database_commit_preserves_old_body_and_reference(self):
        with patch.object(connection, "_commit", side_effect=DatabaseError("injected commit failure")):
            with self.assertRaisesRegex(DatabaseError, "injected commit failure"):
                self.save_note()
        self.note.refresh_from_db()
        self.assertEqual(self.note.noteHead.markdown, "acknowledged")
        self.assertEqual(self.note.contentGeneration, 1)
        self.assertEqual(LibraryNoteRevision.objects.count(), 1)
        self.assertEqual(LibraryNoteSaveReceipt.objects.count(), 1)

    def test_storage_failure_cannot_fail_database_save(self):
        with patch("django.core.files.storage.default_storage.save", side_effect=OSError("unavailable")), \
             patch("django.core.files.storage.default_storage.delete", side_effect=OSError("denied")):
            result = self.save_note()
        self.assertEqual(result["appliedGeneration"], 2)
        self.note.refresh_from_db()
        self.assertEqual(self.note.noteHead.markdown, "latest")
        self.assertEqual(self.note.storageKey, "")
