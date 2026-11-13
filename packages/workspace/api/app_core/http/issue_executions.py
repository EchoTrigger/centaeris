"""Issue origins for caller-owned interactive sessions."""
from django.db import transaction
from ninja import Router, Status
from pydantic import Field

from app_core.hosted_operations import (
    OPERATION_ID_PATTERN, HostedOperationError, lock_operation_identity,
    replay_operation, request_digest, serialize_operation,
)
from app_core.models import Issue, IssueExecution, Session
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


class IssueExecutionResponse(StrictSchema):
    id: str
    session_id: str
    agent_run_id: str
    title: str
    status: str
    created_at: str


class IssueExecutionsEnvelope(StrictSchema):
    executions: list[IssueExecutionResponse]
    total: int


def _execution_context(request, workspace_id, issue_id, payload, digest):
    # Same lock order as normal submission: authority, operation identity, resources.
    membership = shared_submission_membership_for(request.user, workspace_id)
    if membership is None:
        raise HostedOperationError(404, "workspace_not_found")
    lock_operation_identity(request.user, workspace_id, "submitMessage", payload.operation_id)
    issue = Issue.objects.select_for_update().filter(id=issue_id, workspace_id=workspace_id).first()
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
            prompt = f"{issue.title}\n\n{issue.description}".strip()
            message = workspaces.SessionMessageRequest.model_validate({
                "operationId": payload.operation_id, "text": prompt,
                "modelConfigRef": payload.model_config_ref, "thinkingMode": payload.thinking_mode,
            })
            session = Session.objects.create(workspace=workspace, owner=request.user, purpose="interactive")
            agent_run, receipt = workspaces._accept_session_message(
                request, workspace, session, message, prompt, [], execution_profile, digest,
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
    if not Issue.objects.filter(id=issue_id, workspace_id=workspace_id).exists():
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
    return {"executions": [{"id": str(item.pk), "sessionId": item.session_id,
        "agentRunId": item.agent_run_id, "title": item.session.title,
        "status": item.agent_run.status, "createdAt": item.createdAt.isoformat()}
        for item in executions[offset:offset + limit]], "total": executions.count()}
