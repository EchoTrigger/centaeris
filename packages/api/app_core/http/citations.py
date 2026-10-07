from ninja import Router, Status
from django.http import HttpResponse

from app_core.models import AgentRun, SessionCitationProjection, SessionEvent
from app_core.session_event import citation_snapshot
from app_core.workspace_access import workspace_membership_for

from .response_schema import CitationEnvelope, CitationSnapshotResponse, TranscriptCitationsResponse, COMMON_ERROR_RESPONSES
from .security import usage_auth


router = Router(tags=["citations"], by_alias=True)


@router.get("/sessions/{session_id}/agent-runs/{agent_run_id}/citations", auth=usage_auth("sessions:read"),
            response={200: CitationSnapshotResponse} | COMMON_ERROR_RESPONSES)
def run_citations(request, response: HttpResponse, session_id: str, agent_run_id: str):
    response["Cache-Control"] = "no-store"
    run = AgentRun.objects.filter(pk=agent_run_id, session_id=session_id, user=request.user).first()
    if run is None or workspace_membership_for(request.user, run.workspace_id) is None:
        return Status(404, {"error": "agent_run_not_found"})
    if request.GET:
        return Status(400, {"error": "citation_snapshot_query_invalid"})
    return citation_snapshot(run)


@router.get("/sessions/{session_id}/transcript/citations", auth=usage_auth("sessions:read"),
            response={200: TranscriptCitationsResponse} | COMMON_ERROR_RESPONSES)
def transcript_citations(request, response: HttpResponse, session_id: str):
    from .workspaces import _authorized_transcript_session, _canonical_waterline

    response["Cache-Control"] = "no-store"
    session = _authorized_transcript_session(request.user, session_id)
    if session is None:
        return Status(404, {"error": "session_not_found"})
    values = request.GET.getlist("sequence")
    if (set(request.GET) != {"sequence"} or not 1 <= len(values) <= 128
            or not all(_canonical_waterline(value) and int(value) <= 9_223_372_036_854_775_807 for value in values)):
        return Status(400, {"error": "transcript_citations_query_invalid"})
    # Core keeps the original tool_call order key after a tool result. Resolve
    # that Session sequence before consulting the existing run-scoped snapshot.
    records = SessionEvent.objects.filter(session=session, agent_run__user=request.user,
        sequence__in=[int(value) for value in values], payload__type="tool_call",
    ).select_related("agent_run").order_by("sequence")
    snapshots = {}
    bindings = []
    for record in records:
        run_id = record.agent_run_id
        if run_id not in snapshots:
            snapshots[run_id] = citation_snapshot(record.agent_run)
        snapshot = snapshots[run_id]
        call_id = record.payload["payload"]["callId"]
        bindings.append({"sourceSequence": str(record.sequence), "sourceToolCallId": call_id,
            "snapshot": {**snapshot, "citations": [citation for citation in snapshot["citations"]
                if citation["sourceToolCallId"] == call_id]}})
    return {"sessionId": session.id, "bindings": bindings}


@router.get(
    "/citations/{citation_id}",
    auth=usage_auth("sessions:read"),
    response={200: CitationEnvelope} | COMMON_ERROR_RESPONSES,
)
def citation_detail(request, citation_id: str):
    try:
        citation = SessionCitationProjection.objects.select_related("agent_run").get(
            citationId=citation_id,
            agent_run__user=request.user,
        )
    except SessionCitationProjection.DoesNotExist:
        return Status(404, {"error": "citation_not_found"})
    if workspace_membership_for(request.user, citation.workspace_id) is None:
        return Status(404, {"error": "citation_not_found"})
    download_path = {
        "sourceObject": "source-objects",
        "userLibraryObject": "library",
        "artifact": "artifacts",
    }.get(citation.ownerKind)
    if download_path is None:
        return Status(409, {"error": "citation_owner_kind_invalid"})
    return {
        "citation": {
            "citationId": citation.citationId,
            "inputRef": citation.inputRef,
            "displayName": citation.displayName,
            "evidenceKind": citation.evidenceKind,
            "locator": citation.locator,
            "sourceUrl": f"/api/citations/{citation.citationId}",
            "previewUrl": f"/api/citations/{citation.citationId}/preview",
            "downloadUrl": f"/api/{download_path}/{citation.ownerRef}/download",
            "originLabel": "库",
        }
    }
