"""Conditional note commits and stable operation receipts over real HTTP."""

import json
from concurrent.futures import ThreadPoolExecutor
from threading import Barrier
from uuid import uuid4

from django.contrib.auth import get_user_model
from django.db import connections
from django.test import Client, TransactionTestCase

from .models import LibraryNoteRevision, LibraryNoteSaveReceipt, UserLibraryObject


class LibraryNoteVersionTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        self.user = get_user_model().objects.create_user(username="version-writer")
        self.client.force_login(self.user)

    def request(self, method, path, payload):
        return getattr(self.client, method)(path, data=json.dumps(payload), content_type="application/json")

    def create(self, **changes):
        payload = {"saveId": uuid4().hex, "displayName": "note", "markdown": "first", **changes}
        response = self.request("post", "/api/library/notes", payload)
        self.assertEqual(response.status_code, 201, response.content)
        return response.json(), payload

    def update(self, note_id, generation, **changes):
        return self.request("put", f"/api/library/{note_id}/note", {
            "saveId": uuid4().hex, "baseGeneration": generation,
            "displayName": "note", "markdown": "second", **changes,
        })

    def test_create_replay_does_not_create_duplicate_and_changed_request_conflicts(self):
        first, payload = self.create()
        replay = self.request("post", "/api/library/notes", payload)
        self.assertEqual(replay.status_code, 201)
        self.assertEqual(replay.json(), first)
        self.assertEqual(UserLibraryObject.objects.count(), 1)
        self.assertEqual(LibraryNoteRevision.objects.count(), 1)
        conflict = self.request("post", "/api/library/notes", {**payload, "markdown": "different"})
        self.assertEqual(conflict.status_code, 409)
        self.assertEqual(conflict.json()["error"], "note_save_id_conflict")

    def test_replay_precedes_generation_check_and_receipt_survives_revision_cleanup(self):
        first, _ = self.create()
        note_id = first["object"]["id"]
        save_id = uuid4().hex
        response = self.update(note_id, 1, saveId=save_id)
        self.assertEqual(response.status_code, 200, response.content)
        self.assertEqual(response.json()["appliedGeneration"], 2)
        later = self.update(note_id, 2, markdown="third")
        self.assertEqual(later.status_code, 200)
        LibraryNoteRevision.objects.filter(note_id=note_id, generation=2).delete()
        replay = self.update(note_id, 1, saveId=save_id)
        self.assertEqual(replay.json(), response.json())
        note = UserLibraryObject.objects.get(pk=note_id)
        self.assertEqual(note.contentGeneration, 3)

    def test_stale_update_and_legacy_request_do_not_change_body(self):
        first, _ = self.create()
        note_id = first["object"]["id"]
        stale = self.update(note_id, 0)
        self.assertEqual(stale.status_code, 409)
        legacy = self.request("put", f"/api/library/{note_id}/note", {"displayName": "note", "markdown": "legacy"})
        self.assertEqual(legacy.status_code, 400)
        note = UserLibraryObject.objects.get(pk=note_id)
        self.assertEqual(note.noteHead.markdown, "first")

    def test_noop_and_title_change_have_explicit_version_semantics(self):
        first, _ = self.create()
        note_id = first["object"]["id"]
        noop = self.update(note_id, 1, markdown="first")
        self.assertEqual(noop.status_code, 200)
        self.assertEqual(noop.json()["appliedGeneration"], 1)
        self.assertEqual(LibraryNoteRevision.objects.count(), 1)
        renamed = self.request("patch", f"/api/library/{note_id}", {
            "saveId": uuid4().hex, "baseGeneration": 1, "displayName": "renamed",
        })
        self.assertEqual(renamed.status_code, 200, renamed.content)
        note = UserLibraryObject.objects.get(pk=note_id)
        self.assertEqual((note.contentGeneration, note.noteHead.displayName, note.noteHead.markdown), (2, "renamed", "first"))
        legacy = self.request("patch", f"/api/library/{note_id}", {"displayName": "bypass"})
        self.assertEqual(legacy.status_code, 400)

    def test_two_independent_connections_cannot_both_apply_same_base(self):
        first, _ = self.create()
        note_id = first["object"]["id"]
        cookie = self.client.cookies.copy()
        barrier = Barrier(2)

        def writer(body):
            client = Client()
            client.cookies = cookie.copy()
            try:
                barrier.wait(timeout=10)
                response = client.put(f"/api/library/{note_id}/note", data=json.dumps({
                    "saveId": uuid4().hex, "baseGeneration": 1, "displayName": "note", "markdown": body,
                }), content_type="application/json")
                return response.status_code
            finally:
                connections.close_all()

        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(writer, ["writer-a", "writer-b"]))
        self.assertEqual(sorted(results), [200, 409])
        self.assertEqual(LibraryNoteRevision.objects.count(), 2)

    def test_deleted_note_is_not_recreated_by_receipt_replay(self):
        first, payload = self.create()
        note_id = first["object"]["id"]
        self.assertEqual(self.client.delete(f"/api/library/{note_id}").status_code, 200)
        replay = self.request("post", "/api/library/notes", payload)
        self.assertEqual(replay.status_code, 404)
        self.assertEqual(UserLibraryObject.objects.count(), 1)
        self.assertEqual(LibraryNoteSaveReceipt.objects.count(), 1)

    def test_trash_restore_creates_new_generation_with_exact_body(self):
        first, _ = self.create(markdown="# independent title\r\n正文")
        note_id = first["object"]["id"]
        self.client.delete(f"/api/library/{note_id}")
        response = self.client.post(f"/api/library/{note_id}/restore")
        self.assertEqual(response.status_code, 200, response.content)
        note = UserLibraryObject.objects.get(pk=note_id)
        self.assertEqual(note.contentGeneration, 2)
        self.assertEqual(note.noteHead.generation, 2)
        self.assertEqual(note.noteHead.markdown, "# independent title\r\n正文")

    def test_history_routes_are_owner_scoped_and_restore_is_idempotent(self):
        first, _ = self.create()
        note_id = first["object"]["id"]
        self.update(note_id, 1)
        history = self.client.get(f"/api/library/{note_id}/note/revisions")
        self.assertEqual(history.status_code, 200)
        self.assertEqual([item["generation"] for item in history.json()["revisions"]], [2, 1])
        protected = self.request("patch", f"/api/library/{note_id}/note/protection", {"generation": 1, "protected": True})
        self.assertEqual(protected.status_code, 200)
        payload = {"saveId": uuid4().hex, "baseGeneration": 2, "revisionGeneration": 1}
        restored = self.request("post", f"/api/library/{note_id}/note/restore", payload)
        self.assertEqual(restored.status_code, 200)
        replay = self.request("post", f"/api/library/{note_id}/note/restore", payload)
        self.assertEqual(replay.json(), restored.json())
        other = get_user_model().objects.create_user(username="history-outsider")
        self.client.force_login(other)
        self.assertEqual(self.client.get(f"/api/library/{note_id}/note/revisions").status_code, 404)
        self.assertEqual(self.request("post", f"/api/library/{note_id}/note/restore", payload).status_code, 404)
