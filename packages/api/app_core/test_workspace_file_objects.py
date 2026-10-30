"""File-level snapshots share immutable content and retain current publication fencing."""
import hashlib
import io
import json
from pathlib import Path
from unittest.mock import patch

from django.db import connection, transaction
from django.test import SimpleTestCase, TransactionTestCase

from .http import internal
from .models import Agent, Session, WorkspaceFileObject, WorkspaceSnapshotObject
from .test_execution_publication_fencing import ExecutionPublicationFixture
from .testing import create_session
from .workspace_file_objects import (WorkspaceFileError, file_key, live_reservation_keys, open_file,
    referenced_file_keys, register_snapshot_files, require_manifest_files, reserve_files, upload_file, upload_files, FileSequence)
from .workspace_snapshot_publication import _object, collect_key


def descriptor(content):
    return {"sha256": "sha256:" + hashlib.sha256(content).hexdigest(), "sizeBytes": len(content)}


def manifest(entries):
    return {"schema": "workspace.snapshot.v1", "files": [
        {"path": path, **descriptor(content), "executable": executable}
        for path, content, executable in entries]}


class OnEnd(io.BytesIO):
    def __init__(self, content, on_end):
        super().__init__(content)
        self.on_end = on_end

    def read(self, size=-1):
        result = super().read(size)
        if not result and self.on_end is not None:
            callback, self.on_end = self.on_end, None
            callback()
        return result


class WorkspaceFileObjectTests(ExecutionPublicationFixture, TransactionTestCase):
    serialized_rollback = True

    def identity(self):
        return {"jobId": self.job_id, "leaseOwner": self.owner,
                "agentRunId": self.run.pk, "authorizationDigest": self.authorization.digest}

    def authorized(self, body):
        return internal._locked_session_workspace_agent_run(body)

    def reserve(self, files):
        body = self.identity()
        with transaction.atomic():
            run, session, _ = self.authorized(body)
            return reserve_files(session, run, body, files)

    def upload(self, content, *, source=None):
        body = self.identity()
        file = descriptor(content)
        return upload_file(io.BytesIO(content) if source is None else source, body, file, self.authorized)

    def snapshot(self, document, generation):
        body = self.identity()
        encoded = json.dumps(document, ensure_ascii=False, separators=(",", ":")).encode()
        key = (f"workspaces/{self.session.workspace_id}/sessions/{self.session.pk}/snapshots/{generation}/"
               f"{hashlib.sha256(encoded).hexdigest()}.snapshot")
        with transaction.atomic():
            run, session, _ = self.authorized(body)
            files = require_manifest_files(session, run, body, document)
            snapshot = _object(session, run, key, "snapshot")
            register_snapshot_files(snapshot, files)
        return key

    def collect(self, key):
        with transaction.atomic():
            Agent.objects.select_for_update().get(pk=self.session.agent_id)
            session = Session.objects.select_for_update().get(pk=self.session.pk)
            return collect_key(session, key, kind="file")

    def test_changed_file_reuses_unchanged_content_and_indexes_both_snapshots(self):
        first = manifest([("base", b"unchanged", False), ("changing", b"first", True)])
        self.assertEqual(self.reserve([descriptor(b"unchanged"), descriptor(b"first")]),
                         [descriptor(b"unchanged"), descriptor(b"first")])
        self.upload(b"unchanged")
        self.upload(b"first")
        first_key = self.snapshot(first, 1)
        second = manifest([("base", b"unchanged", False), ("changing", b"second", True)])
        self.assertEqual(self.reserve([descriptor(b"unchanged"), descriptor(b"second")]), [descriptor(b"second")])
        self.upload(b"second")
        second_key = self.snapshot(second, 2)
        self.assertEqual(WorkspaceFileObject.objects.count(), 3)
        shared = file_key(self.session, descriptor(b"unchanged")["sha256"])
        self.assertEqual(referenced_file_keys([first_key]) & referenced_file_keys([second_key]), {shared})
        self.assertEqual(len(referenced_file_keys([first_key, second_key])), 3)
        with open_file(self.session, descriptor(b"second")["sha256"]) as stored:
            self.assertEqual(stored.read(), b"second")

    def test_reused_content_is_published_from_verified_metadata_without_body_reads(self):
        self.reserve([descriptor(b"content")])
        self.upload(b"content")
        self.assertEqual(self.reserve([descriptor(b"content"), descriptor(b"content")]), [])
        with patch("app_core.workspace_file_objects.default_storage.open", side_effect=AssertionError("body reread")):
            key = self.snapshot(manifest([("a", b"content", False), ("b", b"content", True)]), 1)
        self.assertEqual(referenced_file_keys([key]), {file_key(self.session, descriptor(b"content")["sha256"])})

    def test_truncated_extra_and_tampered_uploads_never_publish_content(self):
        content = b"correct"
        self.reserve([descriptor(content)])
        for received in [content[:-1], content + b"extra", b"invalid"]:
            with self.subTest(received=received), self.assertRaises(WorkspaceFileError):
                self.upload(content, source=io.BytesIO(received))
            self.assertFalse((self.storage_root / file_key(self.session, descriptor(content)["sha256"])).exists())
        self.assertEqual(list(self.storage_root.rglob(".centaeris-immutable-*.tmp")), [])
        self.assertEqual(WorkspaceFileObject.objects.get().verifiedPublicationToken, "")

    def test_file_upload_requires_current_reservation(self):
        with self.assertRaisesMessage(WorkspaceFileError, "workspace_file_reservation_lost"):
            self.upload(b"unreserved")
        self.assertFalse(any(self.storage_root.rglob("*.file")))

    def test_expired_lease_after_stream_cannot_publish(self):
        content = b"pending"
        self.reserve([descriptor(content)])
        source = OnEnd(content, lambda: self._lease(self.owner, expired=True))
        with self.assertRaisesMessage(internal.SessionWorkspaceError, "session_workspace_lease_lost"):
            self.upload(content, source=source)
        self.assertEqual(WorkspaceFileObject.objects.get().verifiedPublicationToken, "")
        self.assertFalse((self.storage_root / file_key(self.session, descriptor(content)["sha256"])).exists())

    def test_old_stream_cannot_publish_into_a_new_file_incarnation(self):
        content = b"same identity"
        self.reserve([descriptor(content)])
        key = file_key(self.session, descriptor(content)["sha256"])
        previous = WorkspaceSnapshotObject.objects.get(storageKey=key).publicationToken
        def replace_incarnation():
            self.collect(key)
            self.reserve([descriptor(content)])
        with self.assertRaisesMessage(internal.SessionWorkspaceError, "session_workspace_publication_collected"):
            self.upload(content, source=OnEnd(content, replace_incarnation))
        self.assertNotEqual(WorkspaceSnapshotObject.objects.get(storageKey=key).publicationToken, previous)
        self.upload(content)
        with open_file(self.session, descriptor(content)["sha256"]) as stored:
            self.assertEqual(stored.read(), content)

    def test_only_current_job_lease_keeps_reservation_live(self):
        content = b"reserved"
        self.reserve([descriptor(content)])
        key = file_key(self.session, descriptor(content)["sha256"])
        self.assertEqual(live_reservation_keys(self.session), {key})
        self._lease(self.replacement)
        self.assertEqual(live_reservation_keys(self.session), set())
        self.owner = self.replacement
        self.reserve([descriptor(content)])
        self.assertEqual(live_reservation_keys(self.session), {key})
        with connection.cursor() as cursor:
            cursor.execute("UPDATE runtime.runtime_jobs SET lease_purpose='control' WHERE job_id=%s", [self.job_id])
        self.assertEqual(live_reservation_keys(self.session), {key})
        self._lease(self.owner, expired=True)
        self.assertEqual(live_reservation_keys(self.session), set())

    def test_empty_file_and_session_isolation(self):
        self.reserve([descriptor(b"")])
        self.upload(b"")
        key = self.snapshot(manifest([("empty", b"", False)]), 1)
        self.assertEqual(len(referenced_file_keys([key])), 1)
        other = create_session(workspace=self.run.workspace, owner=self.run.user)
        with self.assertRaisesMessage(WorkspaceFileError, "workspace_file_unavailable"):
            open_file(other, descriptor(b"")["sha256"])
        with open_file(self.session, descriptor(b"")["sha256"]) as stored:
            self.assertEqual(stored.read(), b"")

    def test_batch_tampering_never_publishes_any_file(self):
        contents = [b"first", b"second"]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        with self.assertRaises(WorkspaceFileError):
            upload_files(io.BytesIO(contents[0] + b"broken"), self.identity(), files, self.authorized)
        self.assertTrue(all(file.verifiedPublicationToken == "" for file in WorkspaceFileObject.objects.all()))
        self.assertEqual(list(self.storage_root.rglob("*.file")), [])
        self.assertEqual(list(self.storage_root.rglob(".centaeris-immutable-*.tmp")), [])

    def test_batch_lease_loss_after_stream_prevents_every_publish(self):
        contents = [b"first", b"second"]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        source = OnEnd(b"".join(contents), lambda: self._lease(self.owner, expired=True))
        with self.assertRaisesMessage(internal.SessionWorkspaceError, "session_workspace_lease_lost"):
            upload_files(source, self.identity(), files, self.authorized)
        self.assertTrue(all(file.verifiedPublicationToken == "" for file in WorkspaceFileObject.objects.all()))
        self.assertEqual(list(self.storage_root.rglob("*.file")), [])

    def test_one_replaced_incarnation_fences_the_entire_batch_before_rename(self):
        contents = [b"first", b"second"]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        def replace_second():
            self.collect(file_key(self.session, files[1]["sha256"]))
            self.reserve(files)
        source = OnEnd(b"".join(contents), replace_second)
        with self.assertRaisesMessage(internal.SessionWorkspaceError, "session_workspace_publication_collected"):
            upload_files(source, self.identity(), files, self.authorized)
        self.assertTrue(all(file.verifiedPublicationToken == "" for file in WorkspaceFileObject.objects.all()))
        self.assertEqual(list(self.storage_root.rglob("*.file")), [])

    def test_conflicting_size_for_one_content_identity_is_rejected(self):
        content = descriptor(b"identity")
        with self.assertRaises(WorkspaceFileError):
            self.reserve([content, content | {"sizeBytes": content["sizeBytes"] + 1}])
        self.assertEqual(WorkspaceFileObject.objects.count(), 0)


class WorkspaceFileSequenceTests(SimpleTestCase):
    def test_early_close_releases_every_accepted_handle(self):
        handles = [io.BytesIO(b"one"), io.BytesIO(b"two"), io.BytesIO()]
        owner = FileSequence([(handle, len(handle.getvalue())) for handle in handles])
        self.assertEqual(owner.read(1), b"o")
        owner.close()
        self.assertTrue(all(handle.closed for handle in handles))
        owner.close()

    def test_read_failure_closes_current_and_remaining_handles(self):
        class FailedRead(io.BytesIO):
            def read(self, size=-1):
                raise OSError("accepted file read failed")
        first, failed, last = io.BytesIO(b"one"), FailedRead(b"two"), io.BytesIO(b"three")
        owner = FileSequence([(first, 3), (failed, 3), (last, 5)])
        with self.assertRaisesMessage(OSError, "accepted file read failed"):
            owner.read()
        self.assertTrue(all(handle.closed for handle in (first, failed, last)))
