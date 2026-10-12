"""Note saves preserve acknowledged bytes across commit and cleanup failures."""

import tempfile
from datetime import timedelta
from types import SimpleNamespace
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.core.files.storage import default_storage
from django.db import DatabaseError, connection
from django.test import TransactionTestCase, override_settings
from django.utils import timezone

from .assets import store_bytes
from .deleted_resource_gc import collect_deleted_resource_gc
from .http.library import UpdateLibraryNoteRequest, update_library_note
from .models import DerivedResource, UserLibraryObject


class LibraryNoteFileSaveTests(TransactionTestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="note-save-")
        self.addCleanup(directory.cleanup)
        storage = override_settings(MEDIA_ROOT=directory.name)
        storage.enable()
        self.addCleanup(storage.disable)
        self.user = get_user_model().objects.create_user(username="note-writer")
        metadata = store_bytes(b"acknowledged", f"users/{self.user.id}/library", "note.md", "text/markdown")
        self.note = UserLibraryObject.objects.create(
            owner=self.user, displayName="note", objectKind="note", status="ready",
            storageKey=metadata["storageKey"], sha256=metadata["sha256"],
            sizeBytes=metadata["sizeBytes"], contentType="text/markdown", contentGeneration=1,
        )
        self.old_key = self.note.storageKey

    def save_note(self):
        return update_library_note(SimpleNamespace(user=self.user), self.note.id,
                                   UpdateLibraryNoteRequest(displayName="note", markdown="latest"))

    def test_failed_database_commit_preserves_old_body_and_reference(self):
        with patch.object(connection, "_commit", side_effect=DatabaseError("injected commit failure")):
            with self.assertRaisesRegex(DatabaseError, "injected commit failure"):
                self.save_note()
        self.note.refresh_from_db()
        self.assertEqual(self.note.storageKey, self.old_key)
        with default_storage.open(self.old_key, "rb") as content:
            self.assertEqual(content.read(), b"acknowledged")
        self.assertFalse(DerivedResource.objects.filter(resourceKey=self.old_key).exists())

    def test_cleanup_failure_does_not_fail_save_or_delete_current_body(self):
        with patch("app_core.http.library.delete_stored_object", side_effect=OSError("storage unavailable")):
            result = self.save_note()
        self.assertEqual(result["markdown"], "latest")
        self.note.refresh_from_db()
        self.assertNotEqual(self.note.storageKey, self.old_key)
        self.assertTrue(default_storage.exists(self.old_key))
        resource = DerivedResource.objects.get(resourceKey=self.old_key)
        self.assertEqual((resource.state, resource.ownerContentGeneration), ("pending", 1))
        with patch("app_core.deleted_resource_gc.delete_stored_object_for_gc", side_effect=OSError("denied")):
            report = collect_deleted_resource_gc(timezone.now() + timedelta(seconds=1), False)
        self.assertEqual(len(report.failures), 1)
        with default_storage.open(self.note.storageKey, "rb") as content:
            self.assertEqual(content.read(), b"latest")

    def test_committed_old_body_is_reclaimed_by_existing_collector(self):
        self.save_note()
        report = collect_deleted_resource_gc(timezone.now() + timedelta(seconds=1), False)
        self.assertEqual(len(report.cleaned), 1)
        self.assertFalse(default_storage.exists(self.old_key))
        self.note.refresh_from_db()
        self.assertTrue(default_storage.exists(self.note.storageKey))
