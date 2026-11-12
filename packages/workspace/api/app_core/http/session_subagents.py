"""Child Agent reads remain authorized by the owning interactive Session."""
from ninja import Router

from app_core.session_subagents import read_session_subagents

from .query import valid_query_fields
from .security import usage_auth
from .workspaces import _authorized_transcript_session, _transcript_json_response


router = Router(tags=["session-subagents"], by_alias=True)


def _read(request, session_id: str, runtime_job_id: str | None = None):
    session = _authorized_transcript_session(request.user, session_id)
    if session is None:
        return _transcript_json_response({"error": "session_not_found"}, status=404)
    if not valid_query_fields(request.GET, set()):
        return _transcript_json_response({"error": "session_subagents_query_invalid"}, status=400)
    try:
        result = read_session_subagents(session, runtime_job_id)
    except LookupError:
        return _transcript_json_response({"error": "session_subagent_not_found"}, status=404)
    except RuntimeError:
        return _transcript_json_response({"error": "session_subagent_unavailable"}, status=503)
    return _transcript_json_response(result)


@router.get(
    "/sessions/{session_id}/subagents", auth=usage_auth("sessions:read"), response=None
)
def session_subagents(request, session_id: str):
    return _read(request, session_id)


@router.get(
    "/sessions/{session_id}/subagents/{runtime_job_id}",
    auth=usage_auth("sessions:read"),
    response=None,
)
def session_subagent(request, session_id: str, runtime_job_id: str):
    return _read(request, session_id, runtime_job_id)
