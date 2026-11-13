from app_core.issue_access import visible_issues
"""Caller-private results from an Issue's linked interactive Session."""
from datetime import UTC, datetime
from uuid import UUID

from ninja import Router, Status

from app_core.models import AgentRun, Artifact, Issue, IssueExecution, SessionEvent
from app_core.workspace_access import workspace_membership_for
from .issues import _pagination
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["issue-executions"], by_alias=True)
TEXT_LIMIT = 4000


class ExecutionArtifactResponse(StrictSchema):
    id: str
    display_name: str
    download_url: str


class ExecutionActivityResponse(StrictSchema):
    id: str
    type: str
    text: str
    text_truncated: bool
    created_at: str


class ExecutionRunResponse(StrictSchema):
    id: str
    status: str
    created_at: str
    summary: str
    summary_truncated: bool
    artifacts: list[ExecutionArtifactResponse]
    activity: list[ExecutionActivityResponse]


class ExecutionRunsEnvelope(StrictSchema):
    runs: list[ExecutionRunResponse]
    total: int


@router.get("/workspaces/{workspace_id}/issues/{issue_id}/executions/{execution_id}",
    auth=session_auth, response={200: ExecutionRunsEnvelope} | COMMON_ERROR_RESPONSES)
def read_issue_execution(request, workspace_id: str, issue_id: str, execution_id: UUID):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    if not visible_issues(request.user, workspace_id).filter(id=issue_id).exists():
        return Status(404, {"error": "issue_not_found"})
    execution = IssueExecution.objects.filter(id=execution_id, issue_id=issue_id,
        issuer=request.user, session__workspace_id=workspace_id, session__owner=request.user,
        session__status="active", session__purpose="interactive", session__archivedAt__isnull=True,
        session__purgeRequestedAt__isnull=True, session__purgedAt__isnull=True,
        agent_run__workspace_id=workspace_id, agent_run__user=request.user).first()
    if execution is None:
        return Status(404, {"error": "issue_execution_not_found"})
    page = _pagination(request)
    if page is None:
        return Status(400, {"error": "issue_query_invalid"})
    limit, offset = page
    runs = AgentRun.objects.filter(workspace_id=workspace_id, session_id=execution.session_id,
        user=request.user).order_by("-createdAt", "id")
    total = runs.count()
    selected = list(runs[offset:offset + limit])
    run_ids = [run.id for run in selected]
    summaries, activity, artifacts = {}, {}, {}
    # Runtime atomically clears this flag on rewrite tombstone targets. Consume
    # that committed projection rather than reconstructing Core's tombstone ledger.
    events = SessionEvent.objects.filter(workspace_id=workspace_id, session_id=execution.session_id,
        agent_run_id__in=run_ids, session_level=False, projects_to_agent_run_stream=True,
        payload__type__in=["assistant_message", "phase_event"]).order_by("sequence")
    for event in events:
        wire = event.payload
        body = wire["payload"]
        if wire["type"] == "assistant_message":
            summaries[event.agent_run_id] = body["modelMarkdown"]
        elif body.get("stage") == "model_process_summary":
            text = body["message"]
            activity.setdefault(event.agent_run_id, []).append({
                "id": event.eventId, "type": "model_process_summary", "text": text[:TEXT_LIMIT],
                "textTruncated": len(text) > TEXT_LIMIT,
                "createdAt": datetime.fromtimestamp(event.createdAtMs / 1000, tz=UTC).isoformat(),
            })
    for artifact in Artifact.objects.filter(workspace_id=workspace_id, session_id=execution.session_id,
        agent_run_id__in=run_ids, createdBy=request.user, status="published", deletedAt__isnull=True,
        publishedAt__isnull=False).order_by("publishedAt", "id"):
        artifacts.setdefault(artifact.agent_run_id, []).append({"id": artifact.id,
            "displayName": artifact.displayName, "downloadUrl": f"/api/artifacts/{artifact.id}/download"})
    return {"runs": [{"id": run.id, "status": run.status, "createdAt": run.createdAt.isoformat(),
        "summary": summaries.get(run.id, "")[:TEXT_LIMIT],
        "summaryTruncated": len(summaries.get(run.id, "")) > TEXT_LIMIT,
        "activity": activity.get(run.id, []), "artifacts": artifacts.get(run.id, [])}
        for run in selected], "total": total}
