import asyncio
import io
import json
import threading
from queue import Empty
from unittest.mock import AsyncMock, patch

from asgiref.sync import sync_to_async
from django.core.exceptions import PermissionDenied
from django.test import SimpleTestCase, override_settings

from .http import storage_stream


class RecordingHandle(io.BytesIO):
    def __init__(self, body):
        super().__init__(body)
        self.read_threads = []
        self.close_threads = []

    def read(self, size=-1):
        self.read_threads.append(threading.get_ident())
        return super().read(size)

    def close(self):
        self.close_threads.append(threading.get_ident())
        return super().close()


@override_settings(STORAGE_STREAM_CHUNK_BYTES=2)
class AuthorizedStorageStreamTests(SimpleTestCase):
    def setUp(self):
        self.pool = storage_stream._StorageLanePool(1)
        patcher = patch.object(storage_stream, "_lane_pool", self.pool)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.addCleanup(self.shutdown_pool)

    def shutdown_pool(self):
        try:
            while True:
                self.pool._available.get_nowait().executor.shutdown(wait=True)
        except Empty:
            pass

    async def assert_lane_reusable(self):
        handle = RecordingHandle(b"again")
        stream = await storage_stream.open_storage_stream(
            "ignored", authorized_open=AsyncMock(return_value=handle),
        )
        self.assertEqual(b"".join([chunk async for chunk in stream]), b"again")
        self.assertTrue(handle.closed)
        self.assertEqual(len(handle.close_threads), 1)

    async def test_default_opener_retains_lane_affinity_and_storage_checks(self):
        handle = RecordingHandle(b"abc")
        opened_on = []

        def open_handle(key, mode):
            opened_on.append(threading.get_ident())
            self.assertEqual((key, mode), ("stored/key", "rb"))
            return handle

        with patch.object(storage_stream.default_storage, "exists", return_value=True) as exists, \
                patch.object(storage_stream.default_storage, "open", side_effect=open_handle):
            stream = await storage_stream.open_storage_stream("stored/key")
            self.assertEqual(b"".join([chunk async for chunk in stream]), b"abc")
        exists.assert_called_once_with("stored/key")
        self.assertEqual(len(handle.close_threads), 1)
        self.assertEqual(len(set(opened_on + handle.read_threads + handle.close_threads)), 1)

    async def test_authorized_opener_keeps_thread_sensitive_work_outside_storage_lane(self):
        handle = RecordingHandle(b"abc")
        opened_on = []

        def authorize_and_open():
            opened_on.append(threading.get_ident())
            return handle

        async def authorized_open():
            return await sync_to_async(authorize_and_open, thread_sensitive=True)()

        with patch.object(storage_stream.default_storage, "exists") as exists, \
                patch.object(storage_stream.default_storage, "open") as ordinary_open:
            stream = await storage_stream.open_storage_stream("ignored", authorized_open=authorized_open)
            self.assertEqual(b"".join([chunk async for chunk in stream]), b"abc")
        exists.assert_not_called()
        ordinary_open.assert_not_called()
        lane_threads = handle.read_threads + handle.close_threads
        self.assertEqual(len(set(lane_threads)), 1)
        self.assertNotEqual(opened_on[0], lane_threads[0])
        self.assertEqual(len(handle.close_threads), 1)

    async def test_authorization_denial_is_preserved_and_releases_capacity(self):
        opener = AsyncMock(side_effect=PermissionDenied("not_authorized"))
        with self.assertRaisesRegex(PermissionDenied, "not_authorized"):
            await storage_stream.stored_file_response(
                "ignored", "text/plain", "file.txt", authorized_open=opener,
            )
        opener.assert_awaited_once_with()
        await self.assert_lane_reusable()

    async def test_unavailable_authorized_object_returns_409_and_releases_capacity(self):
        response = await storage_stream.stored_file_response(
            "ignored", "text/plain", "file.txt",
            authorized_open=AsyncMock(side_effect=storage_stream.StoredObjectUnavailable("gone")),
        )
        self.assertEqual(response.status_code, 409)
        self.assertEqual(json.loads(response.content), {"error": "stored_object_not_available"})
        await self.assert_lane_reusable()

    async def test_capacity_exhaustion_never_calls_the_authorization_opener(self):
        occupied = await storage_stream.open_storage_stream(
            "ignored", authorized_open=AsyncMock(return_value=RecordingHandle(b"first")),
        )
        opener = AsyncMock()
        try:
            response = await storage_stream.stored_file_response(
                "ignored", "text/plain", "file.txt", authorized_open=opener,
            )
            self.assertEqual(response.status_code, 503)
            self.assertEqual(json.loads(response.content), {"error": "storage_stream_capacity_exhausted"})
            opener.assert_not_called()
        finally:
            await occupied.aclose()
        await self.assert_lane_reusable()

    async def test_cancellation_waits_for_authorized_open_then_closes_the_handle(self):
        handle = RecordingHandle(b"first")
        started, release = asyncio.Event(), asyncio.Event()

        async def authorized_open():
            started.set()
            await release.wait()
            return handle

        opening = asyncio.create_task(storage_stream.open_storage_stream("ignored", authorized_open=authorized_open))
        await asyncio.wait_for(started.wait(), timeout=2)
        opening.cancel()
        await asyncio.sleep(0)
        denied_opener = AsyncMock()
        response = await storage_stream.stored_file_response(
            "ignored", "text/plain", "file.txt", authorized_open=denied_opener,
        )
        self.assertEqual(response.status_code, 503)
        denied_opener.assert_not_called()
        opening.cancel()
        release.set()
        with self.assertRaises(asyncio.CancelledError):
            await asyncio.wait_for(opening, timeout=2)
        self.assertTrue(handle.closed)
        self.assertEqual(len(handle.close_threads), 1)
        await self.assert_lane_reusable()

    async def test_authorized_opener_cancellation_releases_capacity(self):
        opener = AsyncMock(side_effect=asyncio.CancelledError())
        with self.assertRaises(asyncio.CancelledError):
            await storage_stream.open_storage_stream("ignored", authorized_open=opener)
        await self.assert_lane_reusable()

    async def test_response_construction_failure_closes_handle_and_releases_capacity(self):
        handle = RecordingHandle(b"first")
        with patch.object(storage_stream, "OwnedAsyncStreamingHttpResponse", side_effect=RuntimeError("response_failed")), \
                self.assertRaisesRegex(RuntimeError, "response_failed"):
            await storage_stream.stored_file_response(
                "ignored", "text/plain", "file.txt", authorized_open=AsyncMock(return_value=handle),
            )
        self.assertTrue(handle.closed)
        self.assertEqual(len(handle.close_threads), 1)
        await self.assert_lane_reusable()

    async def test_header_construction_failure_closes_handle_and_releases_capacity(self):
        handle = RecordingHandle(b"first")
        with patch.object(storage_stream, "content_disposition_header", side_effect=RuntimeError("header_failed")), \
                self.assertRaisesRegex(RuntimeError, "header_failed"):
            await storage_stream.stored_file_response(
                "ignored", "text/plain", "file.txt", authorized_open=AsyncMock(return_value=handle),
            )
        self.assertTrue(handle.closed)
        self.assertEqual(len(handle.close_threads), 1)
        await self.assert_lane_reusable()

    async def test_unconsumed_response_aclose_owns_the_handle(self):
        handle = RecordingHandle(b"first")
        response = await storage_stream.stored_file_response(
            "ignored", "text/plain", "file.txt", authorized_open=AsyncMock(return_value=handle),
        )
        await response.aclose()
        await response.aclose()
        self.assertFalse(handle.read_threads)
        self.assertTrue(handle.closed)
        self.assertEqual(len(handle.close_threads), 1)
        await self.assert_lane_reusable()

    async def test_unconsumed_response_sync_close_owns_the_handle(self):
        handle = RecordingHandle(b"first")
        response = await storage_stream.stored_file_response(
            "ignored", "text/plain", "file.txt", authorized_open=AsyncMock(return_value=handle),
        )
        response.close()
        await response.aclose()
        self.assertFalse(handle.read_threads)
        self.assertTrue(handle.closed)
        self.assertEqual(len(handle.close_threads), 1)
        await self.assert_lane_reusable()
