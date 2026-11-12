"""Child Agent reads remain authorized by the owning interactive Session."""
from django.http import HttpResponse
from ninja import Router, Status

from app_core.session_subagents import read_session_subagents

from .query import valid_query_fields
from .response_schema import (
    COMMON_ERROR_RESPONSES,
    SessionSubagentEnvelope,
    SessionSubagentsEnvelope,
)
from .security import usage_auth
from .workspaces import _authorized_transcript_session


router = Router(tags=["session-subagents"], by_alias=True)


def _read(request, session_id: str, runtime_job_id: str | None = None):
    session = _authorized_transcript_session(request.user, session_id)
    if session is None:
        return Status(404, {"error": "session_not_found"})
    if not valid_query_fields(request.GET, set()):
        return Status(400, {"error": "session_subagents_query_invalid"})
    try:
        result = read_session_subagents(session, runtime_job_id)
    except LookupError:
        return Status(404, {"error": "session_subagent_not_found"})
    except RuntimeError:
        return Status(503, {"error": "session_subagent_unavailable"})
    return result


@router.get(
    "/sessions/{session_id}/subagents",
    auth=usage_auth("sessions:read"),
    response={200: SessionSubagentsEnvelope} | COMMON_ERROR_RESPONSES,
)
def session_subagents(request, response: HttpResponse, session_id: str):
    response["Cache-Control"] = "no-store"
    return _read(request, session_id)


@router.get(
    "/sessions/{session_id}/subagents/{runtime_job_id}",
    auth=usage_auth("sessions:read"),
    response={200: SessionSubagentEnvelope} | COMMON_ERROR_RESPONSES,
)
def session_subagent(request, response: HttpResponse, session_id: str, runtime_job_id: str):
    response["Cache-Control"] = "no-store"
    return _read(request, session_id, runtime_job_id)
