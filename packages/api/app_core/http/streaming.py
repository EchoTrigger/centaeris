from asgiref.sync import sync_to_async
from django.http import JsonResponse
from ninja import Router

from app_core.models import AgentRun, SessionEvent
from app_core.app_delegations import session_authority_is_current
from app_core.agent_run_stream import (
    parse_last_event_cursor,
    require_cursor_not_future,
    stream_agent_run_session_items_async,
)
from app_core.workspace_access import (
    agent_run_membership_is_current,
    workspace_membership_for,
)
from .security import usage_auth
from .stream_response import OwnedAsyncStreamingHttpResponse


router = Router(tags=["streaming"], by_alias=True)


@router.get(
    "/sessions/{session_id}/agent-runs/{agent_run_id}/events",
    auth=usage_auth("events:read"),
    response=None,
)
async def agent_run_events(request, session_id: str, agent_run_id: str):
    prepared = await _prepare_agent_run_stream(
        user_id=request.user.id,
        session_id=session_id,
        agent_run_id=agent_run_id,
        last_event_id=request.headers.get("Last-Event-ID", ""),
    )
    if isinstance(prepared, JsonResponse):
        return prepared
    agent_run, cursor = prepared
    user_id = request.user.id
    delegation = request.app_delegation
    delegation_id = delegation.id if delegation is not None else None
    credential_version = request.app_delegation_credential_version if delegation is not None else None
    business_branch_id = request.business_branch_id if delegation is not None else None

    async def authority_check():
        return await sync_to_async(session_authority_is_current, thread_sensitive=True)(
            user_id, session_id, delegation_id=delegation_id, scope="events:read",
            credential_version=credential_version,
            business_branch_id=business_branch_id,
        )

    response = OwnedAsyncStreamingHttpResponse(
        stream_agent_run_session_items_async(agent_run, cursor, authority_check=authority_check),
        content_type="text/event-stream",
    )
    response["Cache-Control"] = "no-cache"
    response["X-Accel-Buffering"] = "no"
    return response


@sync_to_async(thread_sensitive=True)
def _prepare_agent_run_stream(
    *,
    user_id: int,
    session_id: str,
    agent_run_id: str,
    last_event_id: str,
):
    try:
        agent_run = AgentRun.objects.select_related(
            "session",
            "workspace",
            "modelConfig",
            "user",
        ).get(
            id=agent_run_id,
            session_id=session_id,
            user_id=user_id,
        )
    except AgentRun.DoesNotExist:
        return JsonResponse({"error": "agent_run_not_found"}, status=404)
    if (
        agent_run.status in {"queued", "running"}
        and not agent_run_membership_is_current(agent_run)
    ) or (
        agent_run.status in {"completed", "failed", "cancelled"}
        and workspace_membership_for(agent_run.user, agent_run.workspace_id) is None
    ):
        return JsonResponse({"error": "agent_run_not_found"}, status=404)
    try:
        cursor = parse_last_event_cursor(last_event_id, agent_run.id)
        require_cursor_not_future(agent_run, cursor)
    except ValueError as error:
        return JsonResponse({"error": str(error)}, status=400)
    if agent_run.status in {"failed", "cancelled"} and not SessionEvent.objects.filter(
        agent_run=agent_run, payload__type="user_message",
    ).exists():
        return JsonResponse({"error": "agent_run_not_admitted"}, status=409)
    return agent_run, cursor
