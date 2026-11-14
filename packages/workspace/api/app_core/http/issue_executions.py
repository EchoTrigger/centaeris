"""Issue origins for caller-owned interactive sessions."""
from django.db import transaction
from django.db.models import OuterRef, Subquery
from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.hosted_operations import (
    OPERATION_ID_PATTERN, HostedOperationError, lock_operation_identity,
    replay_operation, request_digest, serialize_operation,
)
from app_core.models import AgentRun, Issue, IssueExecution, Session
from app_core.issue_templates import issue_execution_prompt
from app_core.issue_access import visible_issues
from app_core.issue_materials import bind_issue_materials
from app_core.presentation_activity import presentation_question
from app_core.workspace_access import shared_submission_membership_for, workspace_membership_for
from . import workspaces
from .issues import _pagination
from .response_schema import COMMON_ERROR_RESPONSES, HostedOperationResponse
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["issue-executions"], by_alias=True)


class IssueExecutionRequest(StrictSchema):
    operation_id: str = Field(pattern=OPERATION_ID_PATTERN)
    version: int = Field(ge=1)
    model_config_ref: str = Field(min_length=1)
    thinking_mode: str | None = None
    instructions: str = Field(default="", max_length=8192, exclude_if=lambda value: not value)

    @field_validator("instructions")
    @classmethod
    def normalize_instructions(cls, value):
        return value.strip()


class IssueExecutionResponse(StrictSchema):
    id: str
    session_id: str
    agent_run_id: str
    latest_agent_run_id: str
    title: str
    status: str
    waiting_for_input: bool
    created_at: str


class IssueExecutionsEnvelope(StrictSchema):
    executions: list[IssueExecutionResponse]
    total: int


class IssueSessionOrigin(StrictSchema):
    issue_id: str
    execution_id: str
    title: str


class IssueSessionOriginEnvelope(StrictSchema):
    origin: IssueSessionOrigin | None


@router.get("/workspaces/{workspace_id}/sessions/{session_id}/issue-origin", auth=session_auth,
    response={200: IssueSessionOriginEnvelope} | COMMON_ERROR_RESPONSES)
def session_issue_origin(request, workspace_id: str, session_id: str):
    if request.GET:
        return Status(400, {"error": "request_invalid"})
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    session = Session.objects.filter(id=session_id, workspace_id=workspace_id, owner=request.user,
        status="active", purpose="interactive", archivedAt__isnull=True,
        purgeRequestedAt__isnull=True, purgedAt__isnull=True).first()
    if session is None:
        return Status(404, {"error": "session_not_found"})
    execution = IssueExecution.objects.filter(session=session, issuer=request.user,
        issue__in=visible_issues(request.user, workspace_id)).select_related("issue").first()
    return {"origin": {"issueId": execution.issue_id, "executionId": str(execution.pk),
        "title": execution.issue.title} if execution else None}


def _execution_context(request, workspace_id, issue_id, payload, digest):
    # Same lock order as normal submission: authority, operation identity, resources.
    membership = shared_submission_membership_for(request.user, workspace_id)
    if membership is None:
        raise HostedOperationError(404, "workspace_not_found")
    lock_operation_identity(request.user, workspace_id, "submitMessage", payload.operation_id)
    issue = visible_issues(request.user, workspace_id, Issue.objects.select_for_update()).filter(id=issue_id).first()
    if issue is None:
        raise HostedOperationError(404, "issue_not_found")
    receipt = replay_operation(request.user, workspace_id, "submitMessage", payload.operation_id, digest)
    if receipt is not None:
        if not IssueExecution.objects.filter(issue=issue, issuer=request.user,
            session_id=receipt.sessionId, agent_run_id=receipt.agentRunId,
            session__purpose="interactive", session__archivedAt__isnull=True,
            session__purgeRequestedAt__isnull=True).exists():
            raise HostedOperationError(410, "operation_resource_unavailable")
        return membership.workspace, issue, receipt
    if issue.archivedAt is not None:
        raise HostedOperationError(409, "issue_archived")
    if issue.version != payload.version:
        raise HostedOperationError(409, "issue_version_conflict")
    return membership.workspace, issue, None


@router.post("/workspaces/{workspace_id}/issues/{issue_id}/executions", auth=session_auth,
    response={202: HostedOperationResponse} | COMMON_ERROR_RESPONSES)
def create_issue_execution(request, workspace_id: str, issue_id: str, payload: IssueExecutionRequest):
    # Caller input defines replay identity; mutable Issue text never enters this digest.
    digest = request_digest(payload, target={"type": "issueExecution", "issueId": issue_id})
    try:
        with transaction.atomic():
            _, _, receipt = _execution_context(request, workspace_id, issue_id, payload, digest)
            if receipt is not None:
                return Status(202, serialize_operation(receipt))
        try:
            workspaces.selected_session_model(workspace_id, payload.model_config_ref, payload.thinking_mode)
        except workspaces.SessionModelSettingsError as error:
            return Status(400, {"error": str(error)})
        try:
            execution_profile = workspaces.request_execution_profile()
        except RuntimeError:
            return Status(503, {"error": "runtime_execution_profile_unavailable"})
        with transaction.atomic():
            workspace, issue, receipt = _execution_context(request, workspace_id, issue_id, payload, digest)
            if receipt is not None:
                return Status(202, serialize_operation(receipt))
            prompt = issue_execution_prompt(issue)
            if payload.instructions:
                prompt += "\n\n" + payload.instructions
            message = workspaces.SessionMessageRequest.model_validate({
                "operationId": payload.operation_id, "text": prompt,
                "modelConfigRef": payload.model_config_ref, "thinkingMode": payload.thinking_mode,
            })
            session = Session.objects.create(workspace=workspace, owner=request.user, purpose="interactive")
            attachment_refs = bind_issue_materials(issue, session, request.user)
            agent_run, receipt = workspaces._accept_session_message(
                request, workspace, session, message, prompt, attachment_refs, execution_profile, digest,
                initial_title=issue.title[:200],
            )
            IssueExecution.objects.create(issue=issue, issuer=request.user, session=session,
                agent_run=agent_run, issueVersion=issue.version,
                issueTitle=issue.title, issueDescription=issue.description)
    except (HostedOperationError, workspaces.AgentSessionCreationError) as error:
        return Status(error.status, {"error": error.code})
    workspaces._schedule_accepted_agent_run(agent_run)
    return Status(202, serialize_operation(receipt))


@router.get("/workspaces/{workspace_id}/issues/{issue_id}/executions", auth=session_auth,
    response={200: IssueExecutionsEnvelope} | COMMON_ERROR_RESPONSES)
def list_issue_executions(request, workspace_id: str, issue_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    if not visible_issues(request.user, workspace_id).filter(id=issue_id).exists():
        return Status(404, {"error": "issue_not_found"})
    page = _pagination(request)
    if page is None:
        return Status(400, {"error": "issue_query_invalid"})
    limit, offset = page
    executions = IssueExecution.objects.filter(issue_id=issue_id, issuer=request.user,
        session__workspace_id=workspace_id, session__owner=request.user,
        session__status="active", session__purpose="interactive", session__archivedAt__isnull=True,
        session__purgeRequestedAt__isnull=True, agent_run__user=request.user,
        agent_run__workspace_id=workspace_id).select_related("session", "agent_run").order_by("-createdAt", "id")
    latest = AgentRun.objects.filter(session_id=OuterRef("session_id"), user=request.user,
        workspace_id=workspace_id).order_by("-createdAt", "-id").values("id")[:1]
    selected = list(executions.annotate(latest_run_id=Subquery(latest))[offset:offset + limit])
    runs = AgentRun.objects.select_related("authorization").in_bulk(item.latest_run_id for item in selected)
    return {"executions": [{"id": str(item.pk), "sessionId": item.session_id,
        "agentRunId": item.agent_run_id, "title": item.session.title,
        "latestAgentRunId": item.latest_run_id, "status": runs[item.latest_run_id].status,
        "waitingForInput": presentation_question(runs[item.latest_run_id])[0] is not None,
        "createdAt": item.createdAt.isoformat()}
        for item in selected], "total": executions.count()}
