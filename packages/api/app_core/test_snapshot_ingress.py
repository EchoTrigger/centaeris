"""Measure snapshot body buffering through the production ASGI application.

Spool byte assertions characterize the current transport cost, not a requirement
to keep buffering before authentication. A streaming change should update those
measurements while retaining rejection, framing, and cleanup behavior.
"""
import asyncio
import hashlib
import json
from pathlib import Path
import tempfile
from unittest.mock import patch

from asgiref.testing import ApplicationCommunicator
from django.conf import settings
from django.test import SimpleTestCase, override_settings


SNAPSHOT_PATHS = (
    "/internal/agent-runs/session-workspace/commit",
    "/internal/agent-runs/execution-workspace/stage",
)


class ObservedTemporaryFile:
    """Delegate real file I/O and record successful bytes without forcing rollover."""

    def __init__(self, file, written):
        self.file = file
        self.written = written
        self.write_bytes = 0
        self.read_bytes = 0
        self.write_digest = hashlib.sha256()

    def write(self, content):
        count = self.file.write(content)
        self.write_bytes += count
        self.write_digest.update(content[:count])
        self.written.set()
        return count

    def read(self, size=-1):
        content = self.file.read(size)
        self.read_bytes += len(content)
        return content

    def close(self):
        return self.file.close()

    def __getattr__(self, name):
        return getattr(self.file, name)


class SnapshotIngressMeasurements(SimpleTestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="snapshot-ingress-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        isolated = override_settings(
            FILE_UPLOAD_MAX_MEMORY_SIZE=64,
            UPLOAD_BODY_MAX_BYTES=8,
            UPLOAD_TEMP_MAX_BYTES=128,
            UPLOAD_MAX_CONCURRENT=1,
            FILE_UPLOAD_TEMP_DIR=str(self.root / "managed"),
        )
        isolated.enable()
        self.addCleanup(isolated.disable)
        # Exercise the production middleware, ASGI body reader, router, and
        # internal-token authentication. Invalid metadata stops before any DB I/O.
        from api.asgi import application
        self.application = application
        self.files = []
        self.written = asyncio.Event()
        temporary_file = tempfile.TemporaryFile

        def open_observed(*args, **kwargs):
            kwargs["dir"] = self.root
            file = ObservedTemporaryFile(temporary_file(*args, **kwargs), self.written)
            self.files.append(file)
            self.addCleanup(file.close)
            return file

        observer = patch.object(tempfile, "TemporaryFile", side_effect=open_observed)
        observer.start()
        self.addCleanup(observer.stop)

    def communicator(self, path, body, *, token=None, content_type=b"application/octet-stream",
                     extra_headers=()):
        headers = [
            (b"host", b"127.0.0.1"),
            (b"content-type", content_type),
            (b"content-length", str(len(body)).encode()),
            *extra_headers,
        ]
        if token is not None:
            headers.append((b"x-internal-token", token.encode()))
        return ApplicationCommunicator(self.application, {
            "type": "http", "asgi": {"version": "3.0"}, "http_version": "1.1",
            "method": "POST", "scheme": "http", "path": path,
            "raw_path": path.encode(), "query_string": b"", "root_path": "",
            "headers": headers, "client": ("127.0.0.1", 10000),
            "server": ("127.0.0.1", 80),
        })

    async def response(self, communicator):
        start = await communicator.receive_output(timeout=5)
        self.assertEqual(start["type"], "http.response.start")
        body = bytearray()
        while True:
            message = await communicator.receive_output(timeout=5)
            self.assertEqual(message["type"], "http.response.body")
            body.extend(message.get("body", b""))
            if not message.get("more_body", False):
                break
        await communicator.wait(timeout=5)
        return start["status"], json.loads(body)

    async def exchange(self, path, body, **kwargs):
        communicator = self.communicator(path, body, **kwargs)
        try:
            await communicator.send_input({"type": "http.request", "body": body,
                                           "more_body": False})
            return await self.response(communicator)
        finally:
            communicator.stop()

    def assert_closed_spool(self, file, body, *, read_bytes):
        self.assertTrue(file.closed)
        self.assertEqual(file.write_bytes, len(body))
        self.assertEqual(file.write_digest.hexdigest(), hashlib.sha256(body).hexdigest())
        self.assertEqual(file.read_bytes, read_bytes)

    def test_current_snapshot_ingress_rolls_over_only_above_the_memory_threshold(self):
        for path in SNAPSHOT_PATHS:
            for size in (64, 65):
                with self.subTest(path=path, size=size):
                    body = b"\0" * 4 + b"x" * (size - 4)
                    previous = len(self.files)
                    status, error = asyncio.run(self.exchange(
                        path, body, token=settings.INTERNAL_API_TOKEN))
                    self.assertEqual(status, 400)
                    expected = ("session_workspace_commit_invalid" if path == SNAPSHOT_PATHS[0]
                                else "execution_workspace_stage_invalid")
                    self.assertEqual(error, {"error": expected})
                    created = self.files[previous:]
                    self.assertEqual(len(created), int(size > 64))
                    if created:
                        self.assert_closed_spool(created[0], body, read_bytes=4)

    def test_current_chunked_body_spool_counts_rollover_copy_and_waits_for_eof(self):
        body = b"\0" * 4 + b"x" * 116

        async def exercise():
            communicator = self.communicator(SNAPSHOT_PATHS[0], body,
                                              token=settings.INTERNAL_API_TOKEN)
            try:
                # The first chunk is buffered in memory. The next crosses the
                # threshold, copying both chunks into the real temporary file.
                for chunk in (body[:40], body[40:80]):
                    await communicator.send_input({"type": "http.request", "body": chunk,
                                                   "more_body": True})
                await asyncio.wait_for(self.written.wait(), timeout=5)
                self.assertEqual(sum(file.write_bytes for file in self.files), 80)
                self.assertTrue(await communicator.receive_nothing(timeout=0.02))
                self.assertTrue(all(file.read_bytes == 0 for file in self.files))
                await communicator.send_input({"type": "http.request", "body": body[80:],
                                               "more_body": False})
                return await self.response(communicator)
            finally:
                communicator.stop()

        self.assertEqual(asyncio.run(exercise()),
                         (400, {"error": "session_workspace_commit_invalid"}))
        self.assertEqual(len(self.files), 1)
        self.assert_closed_spool(self.files[0], body, read_bytes=4)

    def test_rejected_internal_tokens_keep_401_and_close_the_current_pre_auth_spool(self):
        body = b"\0" * 4 + b"x" * 100
        for path in SNAPSHOT_PATHS:
            for token in (None, "invalid-token"):
                with self.subTest(path=path, token=token):
                    previous = len(self.files)
                    status, _ = asyncio.run(self.exchange(path, body, token=token))
                    self.assertEqual(status, 401)
                    created = self.files[previous:]
                    self.assertEqual(len(created), 1)
                    self.assert_closed_spool(created[0], body, read_bytes=0)

    def test_disconnect_closes_the_spooled_prefix_without_dispatching_a_response(self):
        prefix = b"x" * 80

        async def exercise():
            communicator = self.communicator(SNAPSHOT_PATHS[1], prefix + b"remaining",
                                              token=settings.INTERNAL_API_TOKEN)
            try:
                await communicator.send_input({"type": "http.request", "body": prefix,
                                               "more_body": True})
                await asyncio.wait_for(self.written.wait(), timeout=5)
                await communicator.send_input({"type": "http.disconnect"})
                await communicator.wait(timeout=5)
                self.assertTrue(await communicator.receive_nothing(timeout=0.02))
            finally:
                communicator.stop()

        asyncio.run(exercise())
        self.assertEqual(len(self.files), 1)
        self.assert_closed_spool(self.files[0], prefix, read_bytes=0)

    def test_snapshot_authentication_still_runs_when_the_public_upload_pool_is_exhausted(self):
        from .upload_capacity import UploadCapacityError
        with patch("app_core.upload_ingress.reserve_ingress",
                   side_effect=UploadCapacityError("upload_capacity_exhausted")) as reserve:
            status, error = asyncio.run(self.exchange(
                "/api/library", b"part", content_type=b"multipart/form-data; boundary=test"))
            self.assertEqual((status, error), (429, {"error": "upload_capacity_exhausted"}))
            self.assertEqual(self.files, [])
            reserve.assert_called_once()
            for path in SNAPSHOT_PATHS:
                status, _ = asyncio.run(self.exchange(path, b"\0" * 80))
                self.assertEqual(status, 401)
            reserve.assert_called_once()
        self.assertTrue(all(file.closed for file in self.files))

    def test_ambiguous_snapshot_framing_is_rejected_before_temporary_file_writes(self):
        for path in SNAPSHOT_PATHS:
            with self.subTest(path=path):
                status, error = asyncio.run(self.exchange(
                    path, b"x" * 80, token=settings.INTERNAL_API_TOKEN,
                    extra_headers=[(b"Content-Length", b"1")]))
                self.assertEqual((status, error), (400, {"error": "upload_headers_ambiguous"}))
                self.assertEqual(self.files, [])
