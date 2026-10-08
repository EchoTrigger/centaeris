"""Versioned business API; runtime and coordination identities stay server-side."""
import json
import asyncio
from functools import wraps
from typing import Literal

from asgiref.sync import async_to_sync, sync_to_async
from django.core.files.storage import default_storage
from ninja import Header, Router, Status
from pydantic import Field, field_validator

from app_core import chats as service
from app_core.agent_inputs import AgentInputError, accept_agent_input, serialize_input
from app_core.app_delegations import DelegationRejected, authenticate_delegation
from app_core.assets import DeferredInputResolutionError
from app_core.business_agent_branches import BRANCH_HEADER, require_business_branch, resolve_business_branch, validate_business_user_id
from .agent_previews import PreviewRejected
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import ProductUsageAuth
from .storage_stream import StoredObjectUnavailable, stored_file_response
from .stream_response import OwnedAsyncStreamingHttpResponse, stream_database_call


router = Router(tags=["chats"], by_alias=True)


class ChatAuth(ProductUsageAuth):
    def __call__(self, request):
        if "Authorization" not in request.headers:
            raise DelegationRejected("delegation_invalid", 401)
        if BRANCH_HEADER in request.headers:
            raise DelegationRejected("request_invalid", 400)
        return super().__call__(request)

    def authenticate(self, request, token):
        grant = authenticate_delegation(token, self.scope)
        request.user = grant.user
        request.app_delegation = grant
        request.app_delegation_credential_version = grant.credential_version
        cid = request.resolver_match.kwargs.get("chat_id")
        if cid is not None:
            try:
                branch = require_business_branch(grant, service.unpack(cid, "chat", ""))
            except (AgentInputError, DelegationRejected):
                raise DelegationRejected("chat_not_found", 404)
            request.business_branch = branch
            request.business_branch_id = branch.pk
        return grant.user


class CreateChat(StrictSchema):
    agent_id: str = Field(alias="agentId")
    business_user_id: str = Field(alias="businessUserId")

    @field_validator("business_user_id")
    @classmethod
    def subject(cls, value):
        return validate_business_user_id(value)


class SubmitMessage(StrictSchema):
    text: str
    file_ids: list[str] = Field(default_factory=list, alias="fileIds", max_length=50)


class Chat(StrictSchema):
    id: str
    agent_id: str = Field(alias="agentId")
    business_user_id: str = Field(alias="businessUserId")
    created_at: str = Field(alias="createdAt")


class ChatEnvelope(StrictSchema):
    data: Chat


class Message(StrictSchema):
    id: str
    chat_id: str = Field(alias="chatId")
    author: Literal["user", "agent"]
    text: str
    file_ids: list[str] = Field(alias="fileIds")
    created_at: str = Field(alias="createdAt")
    read_at: str | None = Field(alias="readAt")


class MessageEnvelope(StrictSchema):
    data: Message


class MessagePage(StrictSchema):
    data: list[Message]
    next_cursor: str | None = Field(alias="nextCursor")
    has_more: bool = Field(alias="hasMore")


class File(StrictSchema):
    id: str
    name: str
    content_type: str = Field(alias="contentType")
    size_bytes: int = Field(alias="sizeBytes")


class FileEnvelope(StrictSchema):
    data: list[File]


def boundary(fn):
    @wraps(fn)
    def wrapped(*args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except (AgentInputError, PreviewRejected, DelegationRejected) as error:
            code = {"agent_input_conflict": "idempotency_conflict", "agent_input_admission_busy": "temporarily_unavailable",
                "business_branch_not_found": "chat_not_found", "business_branch_deleted": "chat_deleted",
                "coordination_session_not_found": "chat_not_found", "agent_input_attachment_rejected": "file_not_found",
                "preview_not_available": "file_not_found", "preview_version_changed": "file_changed",
                "agent_input_source_forbidden": "idempotency_source_forbidden", "agent_input_identity_revoked": "idempotency_identity_revoked"}.get(error.code, error.code)
            return Status(error.status, {"error": code})
        except DeferredInputResolutionError:
            return Status(404, {"error": "file_not_found"})
        except (ValueError, TypeError):
            return Status(400, {"error": "request_invalid"})
    return wrapped


def query(request, allowed):
    if set(request.GET) - allowed or any(len(request.GET.getlist(k)) != 1 for k in request.GET):
        raise AgentInputError(400, "request_invalid")


@router.post("/chats", auth=ChatAuth("assistant:use"), response={200: ChatEnvelope, 201: ChatEnvelope} | COMMON_ERROR_RESPONSES)
@boundary
def create_chat(request, payload: CreateChat):
    query(request, set())
    branch, created = resolve_business_branch(request, payload.agent_id, payload.business_user_id,
        published=request.app_delegation.definition_id is not None)
    return Status(201 if created else 200, {"data": {"id": service.opaque("chat", "", branch.pk),
        "agentId": request.app_delegation.agent_id or request.app_delegation.definition_id, "businessUserId": branch.business_user_id,
        "createdAt": branch.created_at.isoformat().replace("+00:00", "Z")}})


@router.post("/chats/{chat_id}/messages", auth=ChatAuth("messages:submit"), response={200: MessageEnvelope, 201: MessageEnvelope} | COMMON_ERROR_RESPONSES)
@boundary
def submit_chat_message(request, chat_id: str, payload: SubmitMessage,
        idempotency_key: str | None = Header(None, alias="Idempotency-Key")):
    query(request, set())
    key = idempotency_key
    if not key:
        raise AgentInputError(400, "idempotency_key_required")
    refs = []
    for fid in payload.file_ids:
        kind, source, ref = service.unpack(fid, "file", chat_id)
        if kind not in {"upload", "input"}:
            raise AgentInputError(400, "file_not_attachable")
        refs.append(ref)
    if len(set(refs)) != len(refs):
        raise AgentInputError(400, "duplicate_file")
    fact, created = accept_agent_input(request.user, request.business_branch.agent_id, key,
        payload.text, sorted(refs), request=request)
    return Status(201 if created else 200, {"data": service.project_input(chat_id, serialize_input(fact))})


@router.get("/chats/{chat_id}/messages", auth=ChatAuth("sessions:read"), response={200: MessagePage} | COMMON_ERROR_RESPONSES)
@boundary
def chat_messages(request, chat_id: str, cursor: str | None = None, limit: int = 50):
    query(request, {"cursor", "limit"})
    if not 1 <= limit <= 100:
        raise AgentInputError(400, "request_invalid")
    return service.history(request, chat_id, cursor, limit)


@router.get("/chats/{chat_id}/messages/{message_id}", auth=ChatAuth("sessions:read"), response={200: MessageEnvelope} | COMMON_ERROR_RESPONSES)
@boundary
def chat_message(request, chat_id: str, message_id: str):
    query(request, set())
    return {"data": service.message(request, chat_id, message_id)}


@router.get("/chats/{chat_id}/events", auth=ChatAuth("sessions:read"), response={200: dict} | COMMON_ERROR_RESPONSES,
    openapi_extra={"responses": {200: {"description": "Retained Message events; resume with Last-Event-ID",
        "content": {"text/event-stream": {"schema": {"type": "string"}}}}}})
@boundary
def chat_events(request, chat_id: str,
        last_event_id: str | None = Header(None, alias="Last-Event-ID")):
    query(request, set())
    cursor = last_event_id
    initial_page = service.event_page(request, chat_id, cursor)
    async def stream():
        nonlocal cursor
        page = initial_page
        while True:
            try:
                events, cursor, more = page
                for event_cursor, name, message in events:
                    await stream_database_call(service.authorize)(request, "sessions:read")
                    yield f"id: {event_cursor}\nevent: {name}\ndata: {json.dumps(message, ensure_ascii=False)}\n\n"
                if not more:
                    await stream_database_call(service.authorize)(request, "sessions:read")
                    yield f": keepalive\nid: {cursor}\n\n"
                    await asyncio.sleep(2)
                page = await stream_database_call(service.event_page)(request, chat_id, cursor)
            except (DelegationRejected, AgentInputError):
                return
    response = OwnedAsyncStreamingHttpResponse(stream(), content_type="text/event-stream")
    response["Cache-Control"] = "no-store"
    response["X-Accel-Buffering"] = "no"
    return response


@router.post("/chats/{chat_id}/files", auth=ChatAuth("attachments:write"), response={201: FileEnvelope} | COMMON_ERROR_RESPONSES,
    openapi_extra={"requestBody": {"required": True, "content": {"multipart/form-data": {"schema": {
        "type": "object", "required": ["files"], "additionalProperties": False,
        "properties": {"files": {"type": "array", "minItems": 1, "items": {"type": "string", "format": "binary"}}}}}}}})
@boundary
def chat_upload(request, chat_id: str):
    from .library import upload_session_library_objects
    query(request, set())
    result = upload_session_library_objects(request, request.business_branch.session_id)
    if result.status_code != 201:
        return result
    # The existing uploader owns validation, storage cleanup and locked admission.
    from app_core.models import SessionAssetLink
    files = []
    for asset in result.value["assets"]:
        link = SessionAssetLink.objects.get(pk=asset["id"])
        files.append({"id": service.opaque("file", chat_id, ["upload", None, link.pk]),
            "name": link.capturedDisplayName, "contentType": link.capturedContentType,
            "sizeBytes": link.capturedSizeBytes})
    return Status(201, {"data": files})


@router.get("/chats/{chat_id}/files/{file_id}", auth=ChatAuth("artifacts:read"), response={200: dict} | COMMON_ERROR_RESPONSES)
@boundary
def chat_file(request, chat_id: str, file_id: str):
    query(request, set())
    resolved, key = service.resolve_file(request, chat_id, file_id)
    def authorized_open():
        # Re-resolve immediately before opening; IDs never replace current ACLs.
        try:
            current, current_key = service.resolve_file(request, chat_id, file_id)
            if (current, current_key) != (resolved, key):
                raise StoredObjectUnavailable()
            return default_storage.open(key, "rb")
        except (AgentInputError, DelegationRejected, PreviewRejected, DeferredInputResolutionError, FileNotFoundError) as error:
            raise StoredObjectUnavailable() from error
    response = async_to_sync(stored_file_response)(key, resolved["contentType"], resolved["displayName"],
        as_attachment=True, content_length=resolved["sizeBytes"], authorized_open=sync_to_async(authorized_open))
    response["Cache-Control"] = "no-store"
    response["X-Content-Type-Options"] = "nosniff"
    return response
