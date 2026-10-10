"""Observable upload boundaries; no provider or container is dispatched.

The pre-change baseline has no ingress adapter, so the same downstream ASGI
application runs directly there. This exposes acceptance and cleanup failures
instead of treating a missing module as regression evidence.
"""
import asyncio
import importlib.util
import json
from pathlib import Path
import tempfile
from unittest.mock import patch

from django.contrib.auth import get_user_model
from django.core.files.uploadedfile import SimpleUploadedFile
from django.core.management import call_command
from django.core.management.base import CommandError
from django.test import TestCase, TransactionTestCase, override_settings

from .assets import store_upload
from .models import UserLibraryObject


def ingress(application):
    if importlib.util.find_spec("app_core.upload_ingress") is None:
        return application
    from .upload_ingress import StorageIngressApplication
    return StorageIngressApplication(application)


async def request(application, body=b"x", *, content_type=b"multipart/form-data", chunks=None, length=True):
    sent = []
    messages = iter(chunks or [{"type": "http.request", "body": body, "more_body": False}])
    headers = [(b"content-type", content_type)]
    if length:
        headers.append((b"content-length", str(len(body)).encode()))

    async def receive():
        return next(messages, {"type": "http.disconnect"})

    async def send(message):
        sent.append(message)

    await application({"type": "http", "path": "/api/library", "headers": headers}, receive, send)
    return sent


class UploadContentBoundaryTests(TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="upload-content-boundary-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        isolated = override_settings(MEDIA_ROOT=str(self.root / "stored"),
            FILE_UPLOAD_TEMP_DIR=str(self.root / "temp"), UPLOAD_FILE_MAX_BYTES=3,
            UPLOAD_BODY_MAX_BYTES=4096, UPLOAD_TEMP_MAX_BYTES=16384, UPLOAD_MAX_CONCURRENT=2)
        isolated.enable()
        self.addCleanup(isolated.disable)
        self.root.joinpath("temp").mkdir()
        self.owner = get_user_model().objects.create_user(username="upload-content-boundary")
        self.client.force_login(self.owner)

    def test_actual_file_bytes_and_late_multipart_failure_publish_nothing(self):
        upload = SimpleUploadedFile("false-size.txt", b"four")
        upload.size = 1
        with self.assertRaisesRegex(ValueError, "upload_file_too_large"):
            store_upload(upload, f"users/{self.owner.pk}/library")
        response = self.client.post("/api/library", {"files": [
            SimpleUploadedFile("first.txt", b"ok"), SimpleUploadedFile("late.txt", b"four")]})
        self.assertEqual(response.status_code, 413, response.content)
        self.assertFalse(UserLibraryObject.objects.filter(owner=self.owner).exists())
        stored = self.root / "stored"
        self.assertEqual([p for p in stored.rglob("*") if p.is_file()], [])


class UploadTemporaryBoundaryTests(TransactionTestCase):
    serialized_rollback = True

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="upload-temp-boundary-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.pool = self.root / "pool"
        self.pool.mkdir()
        isolated = override_settings(MEDIA_ROOT=str(self.root / "stored"),
            FILE_UPLOAD_TEMP_DIR=str(self.pool), UPLOAD_FILE_MAX_BYTES=8,
            UPLOAD_BODY_MAX_BYTES=8, UPLOAD_TEMP_MAX_BYTES=128, UPLOAD_MAX_CONCURRENT=1)
        isolated.enable()
        self.addCleanup(isolated.disable)

    def test_unknown_length_and_false_length_are_checked_before_downstream_spooling(self):
        for length, chunks in ((False, [
            {"type": "http.request", "body": b"123456", "more_body": True},
            {"type": "http.request", "body": b"789", "more_body": False}]),
            (True, [{"type": "http.request", "body": b"123", "more_body": False}])):
            with self.subTest(length=length):
                seen = []
                async def app(scope, receive, send):
                    while True:
                        message = await receive()
                        if message["type"] == "http.disconnect":
                            break
                        seen.append(message["body"])
                        if not message.get("more_body", False):
                            break
                    await send({"type": "http.response.start", "status": 200, "headers": []})
                    await send({"type": "http.response.body", "body": b"ok"})
                sent = asyncio.run(request(ingress(app), body=b"x", chunks=chunks, length=length))
                self.assertEqual(sent[0]["status"], 413)
                self.assertEqual(seen, [b"123456"] if not length else [])

    def test_last_upload_slot_is_shared_but_control_json_remains_available(self):
        async def run():
            entered, release = asyncio.Event(), asyncio.Event()
            async def app(scope, receive, send):
                message = await receive()
                if message["body"] == b"held":
                    entered.set()
                    await release.wait()
                await send({"type": "http.response.start", "status": 200, "headers": []})
                await send({"type": "http.response.body", "body": b"ok"})
            application = ingress(app)
            first = asyncio.create_task(request(application, b"held"))
            try:
                await asyncio.wait_for(entered.wait(), timeout=5)
                second = await request(application, b"new")
                control = await request(application, b"{}", content_type=b"application/json")
                self.assertEqual(second[0]["status"], 429)
                self.assertEqual(control[0]["status"], 200)
            finally:
                release.set()
                await first
            self.assertEqual((await request(application, b"after"))[0]["status"], 200)
        asyncio.run(run())

    def test_normal_response_with_failed_close_keeps_hold_until_offline_physical_proof(self):
        opened = []
        close_patches = []
        async def app(scope, receive, send):
            message = await receive()
            tracker = scope.get("upload_temp_tracker")
            handle = tracker.open_file() if tracker else tempfile.NamedTemporaryFile(dir=self.pool, delete=False)
            handle.write(message["body"])
            handle.flush()
            opened.append(handle)
            # Django may swallow an upload close error. A normal response must
            # not convert that uncertainty into free capacity.
            failing_close = patch.object(handle, "close", side_effect=OSError("close failed"))
            failing_close.start()
            close_patches.append(failing_close)
            self.addCleanup(failing_close.stop)
            self.addCleanup(handle.file.close)
            try:
                handle.close()
            except OSError:
                pass
            await send({"type": "http.response.start", "status": 200, "headers": []})
            await send({"type": "http.response.body", "body": b"ok"})
        application = ingress(app)
        self.assertEqual(asyncio.run(request(application, b"held"))[0]["status"], 200)
        self.assertTrue(Path(opened[0].name).exists())
        self.assertEqual(asyncio.run(request(application, b"new"))[0]["status"], 429)
        close_patches[0].stop()
        opened[0].close()
        marker = self.pool / ".centaeris-upload-temp.v1"
        original = marker.read_bytes()
        body = json.loads(original)
        body["poolRef"] = "0" * 32
        marker.write_text(json.dumps(body), encoding="utf-8")
        with self.assertRaises(CommandError):
            call_command("reconcile_upload_capacity", api_workers_stopped=True)
        self.assertTrue(Path(opened[0].name).exists())
        marker.write_bytes(original)
        actual_unlink = Path.unlink
        def denied(path, *args, **kwargs):
            if path == Path(opened[0].name):
                raise PermissionError("unlink denied")
            return actual_unlink(path, *args, **kwargs)
        with patch.object(Path, "unlink", denied):
            with self.assertRaises(CommandError):
                call_command("reconcile_upload_capacity", api_workers_stopped=True)
        self.assertEqual(asyncio.run(request(application, b"new"))[0]["status"], 429)
        call_command("reconcile_upload_capacity", api_workers_stopped=True)
        self.assertFalse(Path(opened[0].name).exists())

    def test_application_exception_with_confirmed_temp_cleanup_allows_next_upload(self):
        async def app(scope, receive, send):
            message = await receive()
            if message["body"] == b"failure":
                handle = scope["upload_temp_tracker"].open_file()
                handle.write(message["body"])
                handle.flush()
                raise OSError("application failed")
            await send({"type": "http.response.start", "status": 200, "headers": []})
            await send({"type": "http.response.body", "body": b"ok"})
        application = ingress(app)
        with self.assertRaisesRegex(OSError, "application failed"):
            asyncio.run(request(application, b"failure"))
        self.assertEqual(list(self.pool.glob("*/part-*.tmp")), [])
        self.assertEqual(asyncio.run(request(application, b"after"))[0]["status"], 200)

    def test_dangling_request_namespace_alias_never_refunds_its_hold(self):
        from .upload_capacity import UploadCapacityError, release_ingress, reserve_ingress
        from .models import UploadCapacity
        lease = reserve_ingress(4)
        namespace = self.pool / lease.pk
        namespace.symlink_to(self.root / "absent-target", target_is_directory=True)
        with self.assertRaisesRegex(UploadCapacityError, "upload_temp_cleanup_unconfirmed"):
            release_ingress(lease.pk)
        with self.assertRaises(CommandError):
            call_command("reconcile_upload_capacity", api_workers_stopped=True)
        counter = UploadCapacity.objects.get(pk=1)
        self.assertEqual((counter.reservedBytes, counter.activeUploads), (8, 1))

    def test_initial_spool_disconnect_closes_handles_and_only_crash_requires_offline_recovery(self):
        from django.core.exceptions import RequestAborted
        from .upload_ingress import ManagedUploadASGIHandler
        from .upload_capacity import reserve_ingress
        from .upload_temp import UploadTempTracker, initialize_upload_temp_root
        from .models import UploadCapacity
        handles = []
        async def app(scope, receive, send):
            tracker = scope["upload_temp_tracker"]
            with self.assertRaises(RequestAborted):
                await ManagedUploadASGIHandler().read_body(receive)
            handles.extend(tracker.handles)
            await send({"type": "http.response.start", "status": 400, "headers": []})
            await send({"type": "http.response.body", "body": b"disconnected"})
        asyncio.run(request(ingress(app), chunks=[
            {"type": "http.request", "body": b"part", "more_body": True},
            {"type": "http.disconnect"}], length=False))
        self.assertTrue(handles)
        self.assertTrue(all(handle.closed for handle in handles))
        self.assertEqual(list(self.pool.glob("*/body-*.tmp")), [])
        self.assertEqual(UploadCapacity.objects.get(pk=1).reservedBytes, 0)
        # Simulate a process ending after its committed reserve and physical
        # spool, without running request-finally. A fresh admission must see it.
        lease = reserve_ingress(4)
        root, pool_ref = initialize_upload_temp_root()
        tracker = UploadTempTracker(root, pool_ref, lease.pk)
        orphan = tracker.open_file(body=True)
        orphan.write(b"held")
        orphan.close()
        async def healthy(scope, receive, send):
            await receive()
            await send({"type": "http.response.start", "status": 200, "headers": []})
            await send({"type": "http.response.body", "body": b"ok"})
        self.assertEqual(asyncio.run(request(ingress(healthy)))[0]["status"], 429)
        self.assertTrue(Path(orphan.name).exists())
        call_command("reconcile_upload_capacity", api_workers_stopped=True)
        self.assertFalse(Path(orphan.name).exists())
        self.assertEqual(asyncio.run(request(ingress(healthy)))[0]["status"], 200)

    def test_confirmed_cleanup_removes_liability_and_repeated_release_cannot_refund_another_request(self):
        from .models import UploadCapacity, UploadLease
        from .upload_capacity import mark_cleanup_unknown, release_ingress, reserve_ingress
        identities = []
        async def app(scope, receive, send):
            await receive()
            identities.append(scope["upload_temp_tracker"].lease_id)
            await send({"type": "http.response.start", "status": 200, "headers": []})
            await send({"type": "http.response.body", "body": b"ok"})
        application = ingress(app)
        for _ in range(2):
            self.assertEqual(asyncio.run(request(application, b"{}", content_type=b"application/json"))[0]["status"], 200)
        self.assertFalse(UploadLease.objects.exists(), "Completed requests must not accumulate historical liabilities")
        active = reserve_ingress(4)
        for identity in identities:
            release_ingress(identity)
            release_ingress(identity)
            mark_cleanup_unknown(identity)
        counter = UploadCapacity.objects.get(pk=1)
        self.assertEqual((counter.reservedBytes, counter.activeUploads), (8, 1))
        self.assertEqual(list(UploadLease.objects.values_list("pk", flat=True)), [active.pk])
        release_ingress(active.pk)
        release_ingress(active.pk)
        self.assertFalse(UploadLease.objects.exists())
        counter.refresh_from_db()
        self.assertEqual((counter.reservedBytes, counter.activeUploads), (0, 0))
