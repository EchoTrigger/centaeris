"""Backfill preserves source bytes and never installs a stale file capture."""

import tempfile
from io import StringIO
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.core.files.storage import default_storage
from django.core.management import call_command
from django.test import TestCase, override_settings

from .assets import store_bytes
from .models import LibraryNoteRevision, UserLibraryObject


class NoteMigrationTests(TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="note-backfill-")
        self.addCleanup(directory.cleanup)
        override = override_settings(MEDIA_ROOT=directory.name)
        override.enable()
        self.addCleanup(override.disable)
        self.user = get_user_model().objects.create_user(username="legacy-note-owner")
        self.body = "# 正文标题\r\n正文 🌱".encode("utf-8")
        metadata = store_bytes(self.body, f"users/{self.user.id}/library", "note.md", "text/markdown")
        self.note = UserLibraryObject.objects.create(
            owner=self.user, objectKind="note", displayName="independent filename", status="ready",
            contentType="text/markdown", contentGeneration=9, storageKey=metadata["storageKey"],
            sha256=metadata["sha256"], sizeBytes=len(self.body),
        )

    def test_dry_run_does_not_publish_and_backfill_is_reentrant(self):
        call_command("migrate_library_notes", dry_run=True, stdout=StringIO())
        self.assertFalse(LibraryNoteRevision.objects.exists())
        call_command("migrate_library_notes", stdout=StringIO())
        call_command("migrate_library_notes", stdout=StringIO())
        self.note.refresh_from_db()
        self.assertEqual(self.note.noteHead.markdown.encode("utf-8"), self.body)
        self.assertEqual(self.note.noteHead.generation, 9)
        self.assertEqual(self.note.noteHead.displayName, "independent filename")
        self.assertEqual(LibraryNoteRevision.objects.count(), 1)
        self.assertTrue(default_storage.exists(self.note.storageKey))

    def test_corrupt_hash_and_invalid_utf8_are_rejected_without_head(self):
        from .note_migration import migrate_note
        UserLibraryObject.objects.filter(pk=self.note.id).update(sha256="sha256:" + "0" * 64)
        with self.assertRaisesRegex(ValueError, "note_migration_identity_mismatch"):
            migrate_note(self.note.id)
        self.note.refresh_from_db()
        self.assertIsNone(self.note.noteHead_id)

    def test_invalid_utf8_is_not_silently_replaced(self):
        from .note_migration import migrate_note
        metadata = store_bytes(b"\xff", f"users/{self.user.id}/library", "invalid.md", "text/markdown")
        UserLibraryObject.objects.filter(pk=self.note.id).update(
            storageKey=metadata["storageKey"], sha256=metadata["sha256"], sizeBytes=1,
        )
        with self.assertRaises(UnicodeDecodeError):
            migrate_note(self.note.id)
        self.assertFalse(LibraryNoteRevision.objects.exists())

    def test_concurrent_metadata_change_does_not_install_old_capture(self):
        from .note_migration import migrate_note
        original_open = default_storage.open
        def change_then_open(*args, **kwargs):
            UserLibraryObject.objects.filter(pk=self.note.id).update(displayName="changed")
            return original_open(*args, **kwargs)
        with patch.object(default_storage, "open", side_effect=change_then_open):
            with self.assertRaisesRegex(ValueError, "note_migration_source_changed"):
                migrate_note(self.note.id)
        self.assertFalse(LibraryNoteRevision.objects.exists())

    def test_rollback_export_uses_latest_revision_not_original_file(self):
        from .library_notes import save_note
        from .note_migration import export_note, migrate_note
        from uuid import uuid4
        migrate_note(self.note.id)
        save_note(self.user, self.note.id, save_id=uuid4().hex, base_generation=9,
                  display_name="latest title", markdown="最新正文")
        export_note(self.note.id)
        self.note.refresh_from_db()
        with default_storage.open(self.note.storageKey, "rb") as content:
            self.assertEqual(content.read(), "最新正文".encode("utf-8"))
        self.assertIsNone(self.note.noteHead_id)
        self.assertEqual(self.note.contentGeneration, 10)
