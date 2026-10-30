"""Authenticated, bounded Web delivery adapter for Runtime transcript reads."""
import asyncio
import copy
import hashlib
import json
from contextlib import contextmanager

import redis
from django.contrib.auth import get_user_model
from django.db import connection, transaction
from django.db.models import QuerySet
from django.http import JsonResponse, QueryDict
from ninja import Router

from app_core.agent_identity import validate_agent_id
from app_core.agent_definitions import available_agent_definitions
from app_core.agent_run_stream import _load_live_text_state_async, _capture_signal_tail, _redis_stream_client, agent_run_signal_key
from app_core.app_delegations import session_authority_is_current
from app_core.models import Agent, AgentRun, Session, SessionEvent
from app_core.operation_presentation import operation_records, operation_detail
from app_core.presentation_stream import PresentationScope, PROJECTION_VERSION, stream_presentation
from app_core.presentation_service import create_presentation_hub
from app_core.runtime_client import request_transcript_page, request_transcript_patches, request_transcript_content
from app_core.session_event import committed_session_terminal_state, project_committed_agent_run
from app_core.workspace_access import workspace_membership_for

from .query import valid_query_fields
from .response_schema import COMMON_ERROR_RESPONSES, PresentationOperationResponse
from .security import usage_auth
from .stream_response import OwnedAsyncStreamingHttpResponse, stream_database_call
from .workspaces import list_workspace_sessions, prepare_workspace_session_list


router = Router(tags=["presentation"], by_alias=True)
READ_TIMEOUT_SECONDS = 5.0


@contextmanager
def _database_deadline(*, consistent=False):
    with transaction.atomic():
        with connection.cursor() as cursor:
            if consistent:
                cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            cursor.execute("SET LOCAL statement_timeout = '5s'")
        yield


class PresentationLoader:
    def __init__(self, request, scope):
        # Preserve the existing list authorization and delegated branch boundary.
        self.request = copy.copy(request)
        self.request.GET = QueryDict(mutable=True)
        if getattr(request, "app_delegation", None) is not None:
            self.request.GET["agentId"] = scope.agent_id
        self.scope = scope
        self.user_id = request.user.id
        delegation = getattr(request, "app_delegation", None)
        self.delegation_id = delegation.id if delegation is not None else None
        self.credential_version = getattr(request, "app_delegation_credential_version", None)
        self.business_branch_id = getattr(request, "business_branch_id", None)
        session_key = getattr(getattr(request, "session", None), "session_key", None) or ""
        self.observation_key = (self.user_id, scope, self.delegation_id, self.credential_version,
                                self.business_branch_id, hashlib.sha256(session_key.encode()).hexdigest())

    @stream_database_call
    def authorized(self):
        with _database_deadline():
            if self.scope.session_id is not None:
                if not Session.objects.filter(id=self.scope.session_id, workspace_id=self.scope.workspace_id,
                                               agent_id=self.scope.agent_id).exists():
                    return False
                # The established helper already checks active user, owner,
                # Agent, membership, definition visibility and delegation.
                return session_authority_is_current(self.user_id, self.scope.session_id,
                    delegation_id=self.delegation_id, scope="sessions:read",
                    credential_version=self.credential_version, business_branch_id=self.business_branch_id)
            user = get_user_model().objects.filter(id=self.user_id, is_active=True).first()
            if user is None:
                return False
            membership = workspace_membership_for(user, self.scope.workspace_id)
            if membership is None:
                return False
            agent = Agent.objects.filter(id=self.scope.agent_id, workspace_id=self.scope.workspace_id,
                                         owner_id=self.user_id, status="active").first()
            if agent is None:
                return False
            if agent.definition_id and not available_agent_definitions(membership).filter(id=agent.definition_id).exists():
                return False
            # Recheck the exact list scope, including credential rotation and
            # delegated branch restrictions, without materializing its rows.
            scoped = prepare_workspace_session_list(self.request, self.scope.workspace_id)
            return isinstance(scoped, QuerySet)

    @stream_database_call
    def navigation(self):
        with _database_deadline():
            result = list_workspace_sessions(self.request, self.scope.workspace_id)
            if not isinstance(result, dict):
                raise PermissionError("presentation_authority_revoked")
            return result["sessions"]

    @stream_database_call
    def state(self):
        # Run status and source water describe one PostgreSQL snapshot. A later
        # terminal observation must not be attached to an older delivered H.
        with _database_deadline(consistent=True):
            water = SessionEvent.objects.filter(session_id=self.scope.session_id).order_by("-sequence").values_list("sequence", flat=True).first() or 0
            run = AgentRun.objects.filter(session_id=self.scope.session_id, user_id=self.user_id).order_by("-createdAt", "-id").first()
            if run is not None and run.status in {"queued", "running"}:
                terminal = committed_session_terminal_state(run)
                if terminal is not None:
                    run = project_committed_agent_run(run, terminal)
            return {"sourceHighWater": str(water), "agentRun": None if run is None else {
                "agentRunId": run.id, "status": run.status,
                "startedAt": run.startedAt.isoformat() if run.startedAt else None,
                "completedAt": run.completedAt.isoformat() if run.completedAt else None,
            }}

    @stream_database_call
    def page(self, water, generation=None):
        return request_transcript_page({"schema": "runtime.transcript.page.read.v1",
            "sessionId": self.scope.session_id, "projectionVersion": PROJECTION_VERSION,
            "projectionGeneration": generation, "sourceHighWater": water, "olderCursor": None})

    @stream_database_call
    def patches(self, generation, after, through):
        return request_transcript_patches({"schema": "runtime.transcript.patch.read.v1",
            "sessionId": self.scope.session_id, "projectionVersion": PROJECTION_VERSION,
            "projectionGeneration": generation, "afterSourceHighWater": after, "throughSourceHighWater": through})

    @stream_database_call
    def content(self, generation, reference, offset):
        return request_transcript_content({"schema": "transcript.content.range.read.v1",
            "sessionId": self.scope.session_id, "projectionVersion": PROJECTION_VERSION,
            "projectionGeneration": generation, **reference, "offset": offset, "maxBytes": 65536})

    @stream_database_call
    def operations(self, call_id, through):
        with _database_deadline():
            records = operation_records(SessionEvent.objects.filter(session_id=self.scope.session_id,
                sequence__lte=int(through), payload__type__in=("tool_call", "tool_result"),
                payload__payload__callId=call_id).order_by("sequence"))[:2]
            return [operation_detail(record) for record in records]

    async def live_signal_tail(self, run_id):
        async with asyncio.timeout(5):
            return await _capture_signal_tail(run_id)

    async def wait_live_signal(self, run_id, cursor):
        async with asyncio.timeout(35):
            batches = await _redis_stream_client.xread({agent_run_signal_key(run_id): cursor}, count=100, block=30000)
        return batches[-1][1][-1][0] if batches else cursor

    async def live(self, run_id):
        try:
            # Ephemeral Redis loss must never prevent the next authoritative
            # Postgres observation. A hung read is treated as unavailable too.
            async with asyncio.timeout(.5):
                return await _load_live_text_state_async(run_id)
        except (redis.RedisError, TimeoutError):
            return None


@router.get("/workspaces/{workspace_id}/presentation/events", auth=usage_auth("sessions:read"),
    response={200: None} | COMMON_ERROR_RESPONSES,
    openapi_extra={"responses": {200: {
        "description": "Resumable presentation events; resume with Last-Event-ID",
        "content": {"text/event-stream": {"schema": {"type": "string"}}},
    }}})
async def presentation_events(request, workspace_id: str):
    if not valid_query_fields(request.GET, {"agentId", "sessionId"}, required={"agentId"}):
        return JsonResponse({"error": "presentation_query_invalid"}, status=400)
    agent_id = request.GET["agentId"]
    try:
        validate_agent_id(agent_id)
    except ValueError:
        return JsonResponse({"error": "agent_id_invalid"}, status=400)
    session_id = request.GET.get("sessionId")
    if session_id is not None and (not session_id or len(session_id) > 64):
        return JsonResponse({"error": "presentation_query_invalid"}, status=400)
    scope = PresentationScope(workspace_id, agent_id, session_id)
    loader = PresentationLoader(request, scope)
    try:
        permitted = await asyncio.wait_for(loader.authorized(), READ_TIMEOUT_SECONDS)
    except TimeoutError:
        return JsonResponse({"error": "presentation_authority_unavailable"}, status=503)
    if permitted is not True:
        return JsonResponse({"error": "presentation_scope_not_found"}, status=404)
    hub = getattr(request, "scope", {}).get("centaeris.presentationHub")
    owned_hub = hub is None
    if owned_hub:
        hub = create_presentation_hub()

    async def content():
        try:
            async with hub.observe(loader.observation_key, scope, loader) as observed:
                async for frame in stream_presentation(scope, observed,
                        request.headers.get("Last-Event-ID", ""), read_timeout_seconds=READ_TIMEOUT_SECONDS):
                    yield frame
        finally:
            if owned_hub:
                await hub.aclose()

    response = OwnedAsyncStreamingHttpResponse(content(), content_type="text/event-stream")
    response["Cache-Control"] = "no-store, no-transform"
    response["X-Accel-Buffering"] = "no"
    return response


@stream_database_call
def _operation_download(request, session_id, event_id):
    with _database_deadline():
        session = Session.objects.filter(id=session_id, owner_id=request.user.id, status="active").first()
        if session is None:
            return None
        scope = PresentationScope(session.workspace_id, session.agent_id, session.id)
        loader = PresentationLoader(request, scope)
        # Use the synchronous authorization implementation on this ORM thread,
        # retaining the existing delegated-session and membership checks.
        delegation = getattr(request, "app_delegation", None)
        if not session_authority_is_current(request.user.id, session_id,
                delegation_id=delegation.id if delegation is not None else None, scope="sessions:read",
                credential_version=getattr(request, "app_delegation_credential_version", None),
                business_branch_id=getattr(request, "business_branch_id", None)):
            return None
        record = operation_records(SessionEvent.objects.filter(session_id=session_id,
            eventId=event_id, payload__type__in=("tool_call", "tool_result"))).first()
        return (loader, operation_detail(record, preview=False)) if record is not None else None


@router.get("/sessions/{session_id}/presentation/operation", auth=usage_auth("sessions:read"),
    response={200: PresentationOperationResponse} | COMMON_ERROR_RESPONSES)
async def presentation_operation(request, session_id: str):
    if not valid_query_fields(request.GET, {"sourceEventId"}, required={"sourceEventId"}) or not 0 < len(request.GET["sourceEventId"]) <= 320:
        return JsonResponse({"error": "presentation_operation_query_invalid"}, status=400)
    try:
        result = await asyncio.wait_for(_operation_download(request, session_id, request.GET["sourceEventId"]), READ_TIMEOUT_SECONDS)
    except TimeoutError:
        return JsonResponse({"error": "presentation_operation_unavailable"}, status=503)
    if result is None:
        return JsonResponse({"error": "presentation_operation_not_found"}, status=404)
    loader, detail = result

    async def content():
        buffer = bytearray()
        for piece in json.JSONEncoder(ensure_ascii=False, separators=(",", ":")).iterencode(detail):
            encoded = piece.encode()
            position = 0
            while position < len(encoded):
                size = min(65536 - len(buffer), len(encoded) - position)
                buffer.extend(encoded[position:position + size])
                position += size
                if len(buffer) == 65536:
                    if await asyncio.wait_for(loader.authorized(), READ_TIMEOUT_SECONDS) is not True:
                        return
                    yield bytes(buffer)
                    buffer.clear()
        if buffer:
            if await asyncio.wait_for(loader.authorized(), READ_TIMEOUT_SECONDS) is True:
                yield bytes(buffer)

    response = OwnedAsyncStreamingHttpResponse(content(), content_type="application/json")
    response["Content-Disposition"] = 'attachment; filename="tool-operation.json"'
    response["Cache-Control"] = "no-store"
    return response
