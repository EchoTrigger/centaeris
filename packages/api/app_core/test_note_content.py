"""Every selected note body must match one immutable identity."""

import hashlib
import tempfile
from types import SimpleNamespace

from asgiref.sync import async_to_sync
from django.contrib.auth import get_user_model
from django.core.files.storage import default_storage
from django.test import TestCase, override_settings

from .assets import store_bytes
from .http.downloads import _select_library_object, _stream_selected
from .http.library import get_library_note
from .models import LibraryNoteRevision, UserLibraryObject


class NoteContentTests(TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="note-content-")
        self.addCleanup(directory.cleanup)
        override = override_settings(MEDIA_ROOT=directory.name)
        override.enable()
        self.addCleanup(override.disable)
        self.user = get_user_model().objects.create_user(username="reader")
        self.body = "# 独立正文标题\r\n\r\n正文 🌱".encode("utf-8")
        metadata = store_bytes(self.body, f"users/{self.user.id}/library", "note.md", "text/markdown")
        self.note = UserLibraryObject.objects.create(
            owner=self.user, displayName="文件名称", objectKind="note", status="ready",
            contentType="text/markdown", contentGeneration=7,
            storageKey=metadata["storageKey"], sha256=metadata["sha256"], sizeBytes=len(self.body),
        )

    def test_editor_and_download_preserve_exact_utf8_bytes(self):
        opened = get_library_note(SimpleNamespace(user=self.user), self.note.id)
        self.assertEqual(opened["markdown"].encode("utf-8"), self.body)
        selected = async_to_sync(_select_library_object)(self.user.id, self.note.id, preview=False)

        async def consume():
            response = await _stream_selected(selected)
            body = b"".join([part async for part in response.streaming_content])
            self.assertEqual(int(response["Content-Length"]), len(body))
            return body

        downloaded = async_to_sync(consume)()
        self.assertEqual(downloaded, self.body)
        self.assertEqual(self.note.sha256, "sha256:" + hashlib.sha256(downloaded).hexdigest())

    def test_other_owner_cannot_select_note(self):
        other = get_user_model().objects.create_user(username="other-reader")
        selected = async_to_sync(_select_library_object)(other.id, self.note.id, preview=False)
        self.assertEqual(selected.status_code, 404)

    def install_revision(self):
        revision = LibraryNoteRevision.objects.create(
            note=self.note, generation=7, displayName=self.note.displayName,
            markdown=self.body.decode("utf-8"), sha256=self.note.sha256,
            sizeBytes=len(self.body), createdBy=self.user,
        )
        self.note.noteHead = revision
        self.note.storageKey = ""
        self.note.save(update_fields=["noteHead", "storageKey"])
        return revision

    def test_database_note_uses_same_editor_and_download_contract(self):
        self.install_revision()
        self.test_editor_and_download_preserve_exact_utf8_bytes()

    def test_selected_body_survives_later_head_change(self):
        from .note_content import content_source
        self.install_revision()
        selected = content_source(self.note)
        later = LibraryNoteRevision.objects.create(
            note=self.note, generation=8, displayName=self.note.displayName,
            markdown="later", sha256="sha256:" + hashlib.sha256(b"later").hexdigest(),
            sizeBytes=5, createdBy=self.user,
        )
        self.note.noteHead = later
        self.note.contentGeneration = 8
        self.note.sha256, self.note.sizeBytes = later.sha256, later.sizeBytes
        self.note.save()
        with selected.open() as content:
            self.assertEqual(content.read(), self.body)

    def test_revision_model_rejects_in_place_body_update(self):
        revision = self.install_revision()
        revision.markdown = "changed"
        with self.assertRaisesRegex(ValueError, "note_revision_immutable"):
            revision.save()

    def test_database_note_processing_reads_body_and_rejects_stale_generation(self):
        from .material_contract import KnowledgeError
        from .material_identity import processing_spec_digest, representation_id
        from .material_task_source import resolve_task_source
        from .test_platform_mcp import processing_specification
        self.install_revision()
        identity = {"ownerKind": "userLibraryObject", "ownerId": self.note.id,
                    "generation": 7, "sha256": self.note.sha256}
        spec = processing_specification()
        digest = processing_spec_digest(spec)
        task = SimpleNamespace(executionBackend="platform", processingSpecification=spec,
                               pk=representation_id(identity, digest),
                               payload={"schema": "workspace.material.processing.v1", "inputIdentity": identity,
                                        "specDigest": digest, "sizeBytes": len(self.body)})
        with resolve_task_source(task).content_source.open() as content:
            self.assertEqual(content.read(), self.body)
        UserLibraryObject.objects.filter(pk=self.note.id).update(contentGeneration=8)
        with self.assertRaisesRegex(KnowledgeError, "material_source_identity_changed"):
            resolve_task_source(task)
