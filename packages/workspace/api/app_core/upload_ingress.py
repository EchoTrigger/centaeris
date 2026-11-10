"""Check actual ingress before the initial body spool and multipart copies."""
from contextvars import ContextVar
import io
import json

from asgiref.sync import sync_to_async
from django.conf import settings
from django.core.exceptions import RequestAborted
from django.core.files.uploadedfile import UploadedFile
from django.core.files.uploadhandler import FileUploadHandler, StopUpload
from django.core.handlers.asgi import ASGIHandler
from django.db import close_old_connections
from django.http import JsonResponse

from .upload_capacity import UploadCapacityError, mark_cleanup_unknown, release_ingress, reserve_ingress
from .upload_temp import UploadTempTracker, initialize_upload_temp_root

CURRENT_TRACKER = ContextVar("upload_temp_tracker", default=None)
SNAPSHOT_PATHS = {"/internal/agent-runs/session-workspace/commit", "/internal/agent-runs/execution-workspace/stage"}


@sync_to_async(thread_sensitive=True)
def database_call(operation, *args, **kwargs):
    close_old_connections()
    try:
        return operation(*args, **kwargs)
    finally:
        close_old_connections()


async def reject(send, status, error):
    body = json.dumps({"error": error}).encode()
    await send({"type": "http.response.start", "status": status,
        "headers": [(b"content-type", b"application/json"), (b"content-length", str(len(body)).encode())]})
    await send({"type": "http.response.body", "body": body})


class ManagedUploadASGIHandler(ASGIHandler):
    async def read_body(self, receive):
        tracker = CURRENT_TRACKER.get()
        if tracker is None:
            return await super().read_body(receive)
        body = None
        try:
            while True:
                message = await receive()
                if message["type"] == "http.disconnect":
                    raise RequestAborted()
                chunk = message.get("body", b"")
                if chunk:
                    if body is None:
                        body = tracker.open_file(body=True)
                    body.write(chunk)
                if not message.get("more_body", False):
                    break
        except BaseException:
            if body is not None:
                body.close()
            raise
        if body is None:
            body = io.BytesIO()
        body.seek(0)
        return body


class StorageIngressApplication:
    def __init__(self, application):
        self.application = application

    async def __call__(self, scope, receive, send):
        if scope["type"] != "http":
            return await self.application(scope, receive, send)
        headers = {}
        normalized_headers = []
        singleton_headers = {b"content-type", b"content-length", b"transfer-encoding"}
        for name, value in scope.get("headers", []):
            name = name.lower()
            if name in singleton_headers and name in headers:
                return await reject(send, 400, "upload_headers_ambiguous")
            headers[name] = value
            normalized_headers.append((name, value))
        if b"content-length" in headers and b"transfer-encoding" in headers:
            return await reject(send, 400, "upload_headers_ambiguous")
        # Django must classify the same bytes headers as this adapter. Preserve
        # order and other repeated headers while normalizing names consistently.
        scope = {**scope, "headers": normalized_headers}
        content_type = headers.get(b"content-type", b"").split(b";", 1)[0].strip().lower()
        if scope.get("path") in SNAPSHOT_PATHS or content_type != b"multipart/form-data":
            return await self.application(scope, receive, send)
        maximum = settings.UPLOAD_BODY_MAX_BYTES
        declared = headers.get(b"content-length")
        if declared is not None:
            if not declared.isdigit() or len(declared) > 19:
                return await reject(send, 400, "upload_length_invalid")
            length = int(declared)
            if length > maximum:
                return await reject(send, 413, "upload_body_too_large")
            maximum = length
        lease = tracker = None
        if maximum:
            try:
                lease = await database_call(reserve_ingress, maximum, upload_slot=True)
                root, pool_ref = initialize_upload_temp_root()
                tracker = UploadTempTracker(root, pool_ref, lease.pk)
            except (UploadCapacityError, OSError, ValueError):
                if lease is not None:
                    await database_call(mark_cleanup_unknown, lease.pk)
                return await reject(send, 429, "upload_capacity_exhausted")
        scope = {**scope, "upload_temp_tracker": tracker}
        token = CURRENT_TRACKER.set(tracker)
        seen = 0
        exceeded = False
        ended = False

        async def bounded_receive():
            nonlocal seen, exceeded, lease, ended
            message = await receive()
            if message["type"] == "http.request":
                if ended:
                    return {"type": "http.disconnect"}
                seen += len(message.get("body", b""))
                if seen > maximum:
                    exceeded = True
                    return {"type": "http.disconnect"}
                ended = not message.get("more_body", False)
                if seen == 0 and not message.get("more_body", False) and lease is not None:
                    # No body has touched the spool. Release before a potentially
                    # long response; subsequent body frames cannot reach it.
                    if tracker.cleanup():
                        await database_call(release_ingress, lease.pk)
                        lease = None
            return message

        async def bounded_send(message):
            if not exceeded:
                await send(message)

        try:
            await self.application(scope, bounded_receive, bounded_send)
        finally:
            CURRENT_TRACKER.reset(token)
            if lease is not None:
                cleaned = tracker.cleanup() if tracker is not None else False
                if cleaned:
                    await database_call(release_ingress, lease.pk)
                else:
                    await database_call(mark_cleanup_unknown, lease.pk)
        if exceeded:
            await reject(send, 413, "upload_body_too_large")


class ActualBytesUploadHandler(FileUploadHandler):
    def __init__(self, request):
        super().__init__(request)
        self.total = self.files = self.current = 0

    def stop(self, error, status=413):
        self.request.storage_upload_error = error
        self.request.storage_upload_status = status
        raise StopUpload(connection_reset=True)

    def new_file(self, *args, **kwargs):
        super().new_file(*args, **kwargs)
        self.files += 1
        self.current = 0
        if self.files > 50:
            self.stop("upload_batch_too_large", 400)

    def receive_data_chunk(self, raw_data, start):
        self.current += len(raw_data)
        self.total += len(raw_data)
        if self.current > settings.UPLOAD_FILE_MAX_BYTES:
            self.stop("upload_file_too_large")
        if self.total > settings.UPLOAD_BODY_MAX_BYTES:
            self.stop("upload_body_too_large")
        return raw_data

    def file_complete(self, file_size):
        return None


class ManagedTemporaryUploadHandler(FileUploadHandler):
    def __init__(self, request, tracker):
        super().__init__(request)
        self.tracker = tracker

    def new_file(self, *args, **kwargs):
        super().new_file(*args, **kwargs)
        self.file = self.tracker.open_file()

    def receive_data_chunk(self, raw_data, start):
        self.file.write(raw_data)
        return None

    def file_complete(self, file_size):
        self.file.seek(0)
        return UploadedFile(self.file, self.file_name, self.content_type, file_size,
            self.charset, self.content_type_extra)


class UploadLimitMiddleware:
    def __init__(self, get_response):
        self.get_response = get_response

    def __call__(self, request):
        if request.content_type == "multipart/form-data" and request.path_info not in SNAPSHOT_PATHS:
            handlers = [ActualBytesUploadHandler(request)]
            tracker = getattr(request, "scope", {}).get("upload_temp_tracker")
            if tracker is not None:
                handlers.append(ManagedTemporaryUploadHandler(request, tracker))
                request.upload_handlers = handlers
            else:
                request.upload_handlers.insert(0, handlers[0])
            # A late StopUpload can leave earlier completed parts in FILES.
            # Refuse the whole parse outcome before a view publishes any part.
            _ = request.POST
            error = getattr(request, "storage_upload_error", None)
            if error:
                return JsonResponse({"error": error}, status=request.storage_upload_status)
        return self.get_response(request)
