import asyncio
import logging
from concurrent.futures import ThreadPoolExecutor
from queue import Empty, Queue
from threading import Lock
from typing import Awaitable, BinaryIO, Callable, TypeVar

from django.conf import settings
from django.core.files.storage import default_storage
from django.http import JsonResponse
from django.utils.http import content_disposition_header

from .stream_response import OwnedAsyncStreamingHttpResponse


logger = logging.getLogger(__name__)
_Result = TypeVar("_Result")


class StorageStreamCapacityExhausted(RuntimeError):
    pass


class StoredObjectUnavailable(RuntimeError):
    pass


class _StorageLane:
    def __init__(self, index: int):
        self.index = index
        self.executor = ThreadPoolExecutor(
            max_workers=1,
            thread_name_prefix=f"storage-stream-{index}",
        )

    async def run(self, operation: Callable[[], _Result]) -> _Result:
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(self.executor, operation)

    def retire(self) -> None:
        self.executor.shutdown(wait=False, cancel_futures=False)


class _StorageLanePool:
    def __init__(self, capacity: int):
        self._available: Queue[_StorageLane] = Queue(maxsize=capacity)
        self._next_index = capacity
        self._index_lock = Lock()
        for index in range(capacity):
            self._available.put_nowait(_StorageLane(index))

    def acquire(self) -> _StorageLane:
        try:
            return self._available.get_nowait()
        except Empty as error:
            raise StorageStreamCapacityExhausted(
                "storage_stream_capacity_exhausted"
            ) from error

    def release(self, lane: _StorageLane) -> None:
        self._available.put_nowait(lane)

    def retire(self, lane: _StorageLane) -> None:
        lane.retire()
        with self._index_lock:
            replacement_index = self._next_index
            self._next_index += 1
        self._available.put_nowait(_StorageLane(replacement_index))


_lane_pool = _StorageLanePool(settings.STORAGE_STREAM_LANES)


class AsyncStorageStream:
    def __init__(
        self,
        lane: _StorageLane,
        handle: BinaryIO,
        *,
        chunk_bytes: int,
    ):
        self._lane = lane
        self._handle = handle
        self._chunk_bytes = chunk_bytes
        self._closed = False

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self._closed:
            raise StopAsyncIteration
        try:
            chunk = await self._lane.run(
                lambda: self._handle.read(self._chunk_bytes)
            )
        except asyncio.CancelledError:
            await self.aclose()
            raise
        except Exception as error:
            logger.error(
                "Storage stream read failed",
                extra={
                    "storageStreamLane": self._lane.index,
                    "transitionReason": "storage_stream_read_failed",
                    "exceptionType": type(error).__name__,
                },
            )
            await self.aclose()
            raise
        if not chunk:
            await self.aclose()
            raise StopAsyncIteration
        return chunk

    async def aclose(self) -> None:
        await self._close()

    async def _close(self) -> None:
        if self._closed:
            return
        self._closed = True
        close_task = asyncio.create_task(self._lane.run(self._handle.close))
        cancellation = None
        close_error = None
        try:
            await asyncio.shield(close_task)
        except asyncio.CancelledError as error:
            cancellation = error
            while not close_task.done():
                try:
                    await asyncio.shield(close_task)
                except asyncio.CancelledError as repeated_error:
                    cancellation = repeated_error
        except Exception as error:
            close_error = error
        if close_error is None and close_task.done():
            try:
                close_task.result()
            except Exception as error:
                close_error = error
        if close_error is not None:
            logger.error(
                "Storage stream close failed; retiring lane",
                extra={
                    "storageStreamLane": self._lane.index,
                    "transitionReason": "storage_stream_close_failed",
                    "exceptionType": type(close_error).__name__,
                },
            )
            _lane_pool.retire(self._lane)
        else:
            _lane_pool.release(self._lane)
        if cancellation is not None:
            raise cancellation


async def open_storage_stream(
    storage_key: str,
    *,
    authorized_open: Callable[[], Awaitable[BinaryIO]] | None = None,
) -> AsyncStorageStream:
    lane = _lane_pool.acquire()

    def open_handle() -> BinaryIO:
        if not storage_key or not default_storage.exists(storage_key):
            raise StoredObjectUnavailable("stored_object_not_available")
        try:
            return default_storage.open(storage_key, "rb")
        except FileNotFoundError as error:
            raise StoredObjectUnavailable("stored_object_not_available") from error

    async def selected_open() -> BinaryIO:
        if authorized_open is not None:
            return await authorized_open()
        return await lane.run(open_handle)

    open_task = asyncio.create_task(selected_open())
    cancellation = None
    while not open_task.done():
        try:
            await asyncio.shield(open_task)
        except asyncio.CancelledError as error:
            cancellation = error
        except Exception:
            break
    try:
        handle = open_task.result()
    except BaseException:
        _lane_pool.release(lane)
        if cancellation is not None:
            raise cancellation
        raise
    stream = AsyncStorageStream(
        lane,
        handle,
        chunk_bytes=settings.STORAGE_STREAM_CHUNK_BYTES,
    )
    if cancellation is not None:
        try:
            await stream.aclose()
        finally:
            raise cancellation
    return stream


async def stored_file_response(
    storage_key: str,
    content_type: str,
    filename: str,
    *,
    as_attachment: bool = True,
    content_length: int | None = None,
    authorized_open: Callable[[], Awaitable[BinaryIO]] | None = None,
):
    if content_length is not None and content_length < 0:
        raise ValueError("storage stream content length must be non-negative")
    try:
        stream = await open_storage_stream(storage_key, authorized_open=authorized_open)
    except StorageStreamCapacityExhausted:
        return JsonResponse(
            {"error": "storage_stream_capacity_exhausted"},
            status=503,
        )
    except StoredObjectUnavailable:
        return JsonResponse({"error": "stored_object_not_available"}, status=409)

    try:
        if not as_attachment and content_type.partition(";")[0].strip().lower() in {"text/plain", "text/markdown"} and "charset=" not in content_type.lower():
            content_type += "; charset=utf-8"
        response = OwnedAsyncStreamingHttpResponse(
            stream,
            content_type=content_type or "application/octet-stream",
        )
        disposition = content_disposition_header(as_attachment, filename)
        if disposition:
            response["Content-Disposition"] = disposition
        if content_length is not None:
            response["Content-Length"] = str(content_length)
        return response
    except BaseException:
        await stream.aclose()
        raise
