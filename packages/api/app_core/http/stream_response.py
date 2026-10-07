import asyncio
import logging

from django.http import StreamingHttpResponse


logger = logging.getLogger(__name__)


class OwnedAsyncStreamingHttpResponse(StreamingHttpResponse):
    """An ASGI streaming response that owns and closes its async iterator."""

    def __init__(self, streaming_content, *args, **kwargs):
        self._owned_source = streaming_content
        self._owner_loop = _running_loop_or_none()
        self._close_task = None
        self._async_closed = False
        super().__init__(streaming_content, *args, **kwargs)
        if not self.is_async:
            raise TypeError("OwnedAsyncStreamingHttpResponse requires an async iterator")
        self._owned_iterator = self._iterator

    @property
    def streaming_content(self):
        return self._iterate_owned()

    @streaming_content.setter
    def streaming_content(self, value):
        self._set_streaming_content(value)

    async def __aiter__(self):
        self._owner_loop = asyncio.get_running_loop()
        try:
            async for part in self._owned_iterator:
                yield self.make_bytes(part)
        finally:
            await self.aclose()

    async def _iterate_owned(self):
        self._owner_loop = asyncio.get_running_loop()
        try:
            async for part in self._owned_iterator:
                yield self.make_bytes(part)
        finally:
            await self.aclose()

    async def aclose(self) -> None:
        if self._async_closed:
            return
        if self._close_task is None:
            self._close_task = asyncio.create_task(self._close_owned_iterator())
        close_task = self._close_task
        cancellation = None
        while not close_task.done():
            try:
                await asyncio.shield(close_task)
            except asyncio.CancelledError as error:
                cancellation = error
        try:
            close_task.result()
        except BaseException as error:
            logger.error(
                "Async response stream close failed",
                extra={
                    "transitionReason": "async_response_stream_close_failed",
                    "streamType": type(self._owned_source).__name__,
                    "exceptionType": type(error).__name__,
                },
            )
        self._async_closed = True
        if cancellation is not None:
            raise cancellation

    async def _close_owned_iterator(self) -> None:
        iterator_closer = getattr(self._owned_iterator, "aclose", None)
        source_closer = getattr(self._owned_source, "aclose", None)
        if iterator_closer is not None:
            await iterator_closer()
        if source_closer is not None and self._owned_source is not self._owned_iterator:
            await source_closer()

    def close(self) -> None:
        if not self._async_closed:
            owner_loop = self._owner_loop
            running_loop = _running_loop_or_none()
            if owner_loop is not None and owner_loop.is_running():
                if running_loop is owner_loop:
                    owner_loop.create_task(self.aclose())
                else:
                    asyncio.run_coroutine_threadsafe(
                        self.aclose(),
                        owner_loop,
                    ).result()
            elif running_loop is not None:
                running_loop.create_task(self.aclose())
            else:
                asyncio.run(self.aclose())
        super().close()


def _running_loop_or_none():
    try:
        return asyncio.get_running_loop()
    except RuntimeError:
        return None
