"""File-level snapshots share immutable content and retain current publication fencing."""
import hashlib
import io
import json
import os
import tempfile
from pathlib import Path
from threading import Barrier, Lock
from unittest.mock import patch

from django.db import connection, transaction
from django.test import SimpleTestCase, TransactionTestCase

from .http import internal
from .models import Agent, Session, WorkspaceFilePack, WorkspaceFileObject, WorkspaceSnapshotObject
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


class RecordingWriter:
    def __init__(self, handle):
        self.handle = handle
        self.fd = handle.fileno()
        self.flushed = False

    @property
    def closed(self):
        return self.handle.closed

    def write(self, chunk):
        return self.handle.write(chunk)

    def flush(self):
        self.handle.flush()
        self.flushed = True

    def fileno(self):
        return self.handle.fileno()

    def close(self):
        self.handle.close()

    def __enter__(self):
        return self

    def __exit__(self, *_error):
        self.close()


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

    def test_batch_flushes_all_private_files_before_sync_and_final_authority(self):
        contents = [b"a" * 17000, b"b" * 17000, b"c" * 17000]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        writers, synced = [], []
        real_fdopen, real_fsync = os.fdopen, os.fsync
        def open_writer(fd, mode):
            writer = RecordingWriter(real_fdopen(fd, mode))
            writers.append(writer)
            return writer
        def sync_file(fd):
            if any(writer.fd == fd and not writer.closed for writer in writers):
                self.assertEqual(len(writers), len(contents))
                self.assertTrue(all(writer.flushed for writer in writers))
                synced.append(fd)
            return real_fsync(fd)
        authority_calls = 0
        def authorize(body):
            nonlocal authority_calls
            authority_calls += 1
            if authority_calls == 2:
                self.assertEqual(len(synced), len(contents))
                self.assertTrue(all(writer.closed for writer in writers))
            return self.authorized(body)
        with patch("app_core.workspace_file_objects.os.fdopen", side_effect=open_writer), \
                patch("app_core.workspace_file_objects.os.fsync", side_effect=sync_file):
            self.assertEqual(upload_files(io.BytesIO(b"".join(contents)), self.identity(), files, authorize), files)
        self.assertEqual(authority_calls, 2)
        self.assertEqual(len(synced), len(contents))
        self.assertTrue(all(writer.closed for writer in writers))

    def test_batch_file_syncs_overlap_before_final_authority(self):
        contents = [b"a" * 17000, b"b" * 17000, b"c" * 17000]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        writers, synced = [], []
        real_fdopen, real_fsync = os.fdopen, os.fsync
        rendezvous = Barrier(2, timeout=3)
        lock = Lock()
        active = peak = 0
        def open_writer(fd, mode):
            writer = RecordingWriter(real_fdopen(fd, mode))
            writers.append(writer)
            return writer
        def sync_file(fd):
            nonlocal active, peak
            if not any(writer.fd == fd and not writer.closed for writer in writers):
                return real_fsync(fd)
            with lock:
                synced.append(fd)
                ordinal = len(synced)
                active += 1
                peak = max(peak, active)
            try:
                if ordinal <= 2:
                    rendezvous.wait()
                return real_fsync(fd)
            finally:
                with lock:
                    active -= 1
        authority_calls = 0
        def authorize(body):
            nonlocal authority_calls
            authority_calls += 1
            if authority_calls == 2:
                self.assertEqual(len(synced), len(contents))
                self.assertEqual(active, 0)
                self.assertTrue(all(writer.closed for writer in writers))
            return self.authorized(body)
        with patch("app_core.workspace_file_objects.os.fdopen", side_effect=open_writer), \
                patch("app_core.workspace_file_objects.os.fsync", side_effect=sync_file):
            upload_files(io.BytesIO(b"".join(contents)), self.identity(), files, authorize)
        self.assertGreaterEqual(peak, 2)
        self.assertEqual(authority_calls, 2)
        self.assertEqual(len(synced), len(contents))

    def test_batch_sync_failure_closes_all_private_files_without_publication(self):
        contents = [b"a" * 17000, b"b" * 17000, b"c" * 17000]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        writers, synced = [], []
        sync_lock = Lock()
        real_fdopen, real_fsync = os.fdopen, os.fsync
        def open_writer(fd, mode):
            writer = RecordingWriter(real_fdopen(fd, mode))
            writers.append(writer)
            return writer
        def fail_second_sync(fd):
            if any(writer.fd == fd and not writer.closed for writer in writers):
                with sync_lock:
                    synced.append(fd)
                    fail = len(synced) == 2
                if fail:
                    raise OSError("injected file sync failure")
            return real_fsync(fd)
        authority_calls = 0
        def authorize(body):
            nonlocal authority_calls
            authority_calls += 1
            return self.authorized(body)
        with patch("app_core.workspace_file_objects.os.fdopen", side_effect=open_writer), \
                patch("app_core.workspace_file_objects.os.fsync", side_effect=fail_second_sync):
            with self.assertRaisesMessage(OSError, "injected file sync failure"):
                upload_files(io.BytesIO(b"".join(contents)), self.identity(), files, authorize)
        self.assertEqual(authority_calls, 1)
        self.assertEqual(len(synced), len(contents))
        self.assertEqual(len(writers), len(contents))
        self.assertTrue(all(writer.flushed and writer.closed for writer in writers))
        self.assertTrue(all(file.verifiedPublicationToken == "" for file in WorkspaceFileObject.objects.all()))
        self.assertEqual(list(self.storage_root.rglob("*.file")), [])
        self.assertEqual(list(self.storage_root.rglob("*.pack")), [])
        self.assertEqual(list(self.storage_root.rglob(".centaeris-immutable-*.tmp")), [])

    def test_batch_write_failure_closes_opened_and_unopened_private_descriptors(self):
        contents = [b"a" * 17000, b"b" * 17000, b"c" * 17000]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        writers, descriptors = [], []
        real_fdopen, real_mkstemp = os.fdopen, tempfile.mkstemp
        class FailedWriter(RecordingWriter):
            def write(self, chunk):
                if len(writers) == 2:
                    self.handle.write(chunk[:1])
                    raise OSError("injected private write failure")
                return super().write(chunk)
        def open_writer(fd, mode):
            writer = FailedWriter(real_fdopen(fd, mode))
            writers.append(writer)
            return writer
        def temporary(*args, **kwargs):
            fd, path = real_mkstemp(*args, **kwargs)
            descriptors.append(fd)
            return fd, path
        with patch("app_core.workspace_file_objects.os.fdopen", side_effect=open_writer), \
                patch("app_core.workspace_file_objects.tempfile.mkstemp", side_effect=temporary):
            with self.assertRaisesMessage(OSError, "injected private write failure"):
                upload_files(io.BytesIO(b"".join(contents)), self.identity(), files, self.authorized)
        self.assertEqual(len(descriptors), len(contents))
        self.assertTrue(all(writer.closed for writer in writers))
        for fd in descriptors:
            with self.assertRaises(OSError):
                os.fstat(fd)
        self.assertTrue(all(file.verifiedPublicationToken == "" for file in WorkspaceFileObject.objects.all()))
        self.assertEqual(list(self.storage_root.rglob("*.file")), [])
        self.assertEqual(list(self.storage_root.rglob("*.pack")), [])
        self.assertEqual(list(self.storage_root.rglob(".centaeris-immutable-*.tmp")), [])

    def test_batch_tampering_never_publishes_any_file(self):
        contents = [b"first", b"second"]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        with self.assertRaises(WorkspaceFileError):
            upload_files(io.BytesIO(contents[0] + b"broken"), self.identity(), files, self.authorized)
        self.assertTrue(all(file.verifiedPublicationToken == "" for file in WorkspaceFileObject.objects.all()))
        self.assertEqual(list(self.storage_root.rglob("*.file")), [])
        self.assertEqual(list(self.storage_root.rglob("*.pack")), [])
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
        self.assertEqual(list(self.storage_root.rglob("*.pack")), [])

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
        self.assertEqual(list(self.storage_root.rglob("*.pack")), [])

    def test_small_files_share_one_durable_pack_and_read_exact_ranges(self):
        contents = [b"first", b"", b"last"]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        with patch("app_core.workspace_file_objects.os.fsync", wraps=os.fsync) as sync:
            upload_files(io.BytesIO(b"".join(contents)), self.identity(), files, self.authorized)
        self.assertEqual(sync.call_count, 1)
        pack = WorkspaceFilePack.objects.get()
        self.assertEqual(pack.sizeBytes, sum(map(len, contents)))
        self.assertEqual(len(list(self.storage_root.rglob("*.pack"))), 1)
        self.assertEqual(list(self.storage_root.rglob("*.file")), [])
        for content, offset in zip(contents, [0, 5, 5]):
            file = WorkspaceFileObject.objects.get(sha256=descriptor(content)["sha256"])
            self.assertEqual((file.pack_id, file.packOffset), (pack.pk, offset))
            with open_file(self.session, file.sha256) as stored:
                self.assertEqual(stored.read(2) + stored.read(), content)
                self.assertEqual(stored.read(), b"")
        self.assertEqual(self.reserve(files), [])
        self.snapshot(manifest([(str(index), content, False) for index, content in enumerate(contents)]), 1)

    def test_single_small_and_large_file_remain_loose(self):
        for content in [b"small", b"large" * 4000]:
            self.reserve([descriptor(content)])
            self.upload(content)
        self.assertEqual(WorkspaceFilePack.objects.count(), 0)
        self.assertEqual(len(list(self.storage_root.rglob("*.file"))), 2)

    def test_pack_member_and_size_bounds_split_upload(self):
        contents = [bytes([index]) * 16384 for index in range(33)]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        upload_files(io.BytesIO(b"".join(contents)), self.identity(), files, self.authorized)
        pack = WorkspaceFilePack.objects.get()
        self.assertEqual(pack.sizeBytes, 512 * 1024)
        self.assertEqual(pack.files.count(), 32)
        self.assertEqual(WorkspaceFileObject.objects.filter(pack__isnull=True).count(), 1)
        with open_file(self.session, files[-1]["sha256"]) as stored:
            self.assertEqual(stored.read(), contents[-1])

    def test_collected_pack_is_missing_and_can_be_reuploaded(self):
        contents = [b"first", b"second"]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        upload_files(io.BytesIO(b"".join(contents)), self.identity(), files, self.authorized)
        old_pack = WorkspaceFilePack.objects.select_related("object").get()
        with transaction.atomic():
            Agent.objects.select_for_update().get(pk=self.session.agent_id)
            session = Session.objects.select_for_update().get(pk=self.session.pk)
            collect_key(session, old_pack.object.storageKey, kind="filePack")
        (self.storage_root / old_pack.object.storageKey).unlink()
        with self.assertRaisesMessage(WorkspaceFileError, "workspace_file_unavailable"):
            open_file(self.session, files[0]["sha256"])
        self.assertEqual(self.reserve(files), files)
        upload_files(io.BytesIO(b"".join(contents)), self.identity(), files, self.authorized)
        for file, content in zip(files, contents):
            current = WorkspaceFileObject.objects.get(sha256=file["sha256"])
            self.assertNotEqual(current.pack_id, old_pack.pk)
            with open_file(self.session, file["sha256"]) as stored:
                self.assertEqual(stored.read(), content)

    def test_pack_sync_failure_does_not_publish_and_closes_writer(self):
        contents = [b"first", b"second"]
        files = [descriptor(content) for content in contents]
        self.reserve(files)
        writers = []
        real_fdopen = os.fdopen
        def open_writer(fd, mode):
            writer = RecordingWriter(real_fdopen(fd, mode))
            writers.append(writer)
            return writer
        with patch("app_core.workspace_file_objects.os.fdopen", side_effect=open_writer), \
                patch("app_core.workspace_file_objects.os.fsync", side_effect=OSError("pack sync failed")):
            with self.assertRaisesMessage(OSError, "pack sync failed"):
                upload_files(io.BytesIO(b"".join(contents)), self.identity(), files, self.authorized)
        self.assertEqual(len(writers), 1)
        self.assertTrue(writers[0].closed)
        self.assertEqual(list(self.storage_root.rglob("*.pack")), [])
        self.assertEqual(list(self.storage_root.rglob(".centaeris-immutable-*.tmp")), [])
        self.assertTrue(all(file.verifiedPublicationToken == "" for file in WorkspaceFileObject.objects.all()))

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
