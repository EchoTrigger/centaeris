"""Trusted business subject resolution and bounded owner maintenance."""
from typing import Literal

from django.db.models import Count, F
from django.http import HttpResponse
from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.business_agent_branches import resolve_business_branch, validate_business_user_id
from app_core.models import Agent, BusinessAgentBranch, Session
from app_core.workspace_access import workspace_membership_for
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth, usage_auth


router = Router(tags=["business-agent-branches"], by_alias=True)


class ResolveBusinessBranchRequest(StrictSchema):
    business_user_id: str = Field(alias="businessUserId", min_length=1, max_length=256)

    @field_validator("business_user_id")
    @classmethod
    def valid_subject(cls, value):
        return validate_business_user_id(value)


class BusinessBranchResponse(StrictSchema):
    schema_id: Literal["agent.business_branch.v1"] = Field(alias="schema")
    branch_id: str = Field(alias="branchId")
    root_agent_id: str = Field(alias="rootAgentId")
    business_user_id: str = Field(alias="businessUserId")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")


class MaintainedBranchResponse(StrictSchema):
    branch_id: str = Field(alias="branchId")
    root_agent_id: str = Field(alias="rootAgentId")
    business_user_id: str = Field(alias="businessUserId")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")
    status: Literal["active", "deleted"]
    session_count: int = Field(alias="sessionCount")
    created_at: str = Field(alias="createdAt")


class MaintainedBranchesResponse(StrictSchema):
    branches: list[MaintainedBranchResponse]
    next_after_id: str | None = Field(alias="nextAfterId")


class BranchSessionMetadata(StrictSchema):
    session_id: str = Field(alias="sessionId")
    title: str
    status: Literal["active", "deleted"]
    created_at: str = Field(alias="createdAt")


class BranchWorkSessionMetadata(BranchSessionMetadata):
    source_agent_run_id: str = Field(alias="sourceAgentRunId")


class BranchSessionsResponse(StrictSchema):
    branch_id: str = Field(alias="branchId")
    coordination_session: BranchSessionMetadata = Field(alias="coordinationSession")
    work_sessions: list[BranchWorkSessionMetadata] = Field(alias="workSessions")
    next_after_session_id: str | None = Field(alias="nextAfterSessionId")


def serialize_branch(branch):
    return {"schema": "agent.business_branch.v1", "branchId": branch.pk,
        "rootAgentId": branch.root_agent_id, "businessUserId": branch.business_user_id,
        "agentId": branch.agent_id, "sessionId": branch.session_id}


@router.post("/agents/{root_agent_id}/business-branches/resolve", auth=usage_auth("assistant:use"),
    response={200: BusinessBranchResponse, 201: BusinessBranchResponse} | COMMON_ERROR_RESPONSES)
def resolve_business_agent_branch(request, response: HttpResponse, root_agent_id: str, payload: ResolveBusinessBranchRequest):
    response["Cache-Control"] = "no-store"
    branch, created = resolve_business_branch(request, root_agent_id, payload.business_user_id)
    return Status(201 if created else 200, serialize_branch(branch))


@router.get("/agents/{root_agent_id}/business-branches", auth=session_auth,
    response={200: MaintainedBranchesResponse} | COMMON_ERROR_RESPONSES)
def list_business_agent_branches(request, response: HttpResponse, root_agent_id: str):
    response["Cache-Control"] = "no-store"
    keys = set(request.GET)
    if (not keys <= {"appId", "afterBranchId", "limit"} or "appId" not in keys
            or any(len(request.GET.getlist(key)) != 1 for key in keys)):
        return Status(400, {"error": "business_branch_request_invalid"})
    app_id = request.GET["appId"]
    after = request.GET.get("afterBranchId")
    raw_limit = request.GET.get("limit", "100")
    if (not 1 <= len(app_id) <= 64 or (after is not None and not 1 <= len(after) <= 64)
            or not 1 <= len(raw_limit) <= 3 or not raw_limit.isascii() or not raw_limit.isdecimal()
            or not 1 <= int(raw_limit) <= 200):
        return Status(400, {"error": "business_branch_request_invalid"})
    root = Agent.objects.filter(pk=root_agent_id, owner=request.user, definition__isnull=True,
        business_branch__isnull=True).first()
    if root is None or workspace_membership_for(request.user, root.workspace_id) is None:
        return Status(404, {"error": "business_branch_not_found"})
    query = BusinessAgentBranch.objects.filter(root_agent=root, app_id=app_id)
    if after is not None:
        if not query.filter(pk=after).exists():
            return Status(400, {"error": "business_branch_request_invalid"})
        query = query.filter(pk__gt=after)
    limit = int(raw_limit)
    page = list(query.select_related("agent", "session").annotate(
        session_count=Count("agent__sessions", distinct=True)).order_by("pk")[:limit + 1])
    return {"branches": [{"branchId": item.pk, "rootAgentId": item.root_agent_id,
        "businessUserId": item.business_user_id, "agentId": item.agent_id, "sessionId": item.session_id,
        "status": item.agent.status, "sessionCount": item.session_count, "createdAt": item.created_at.isoformat()}
        for item in page[:limit]], "nextAfterId": page[limit - 1].pk if len(page) > limit else None}


@router.get("/agents/{root_agent_id}/business-branches/{branch_id}/sessions", auth=session_auth,
    response={200: BranchSessionsResponse} | COMMON_ERROR_RESPONSES)
def list_business_branch_sessions(request, response: HttpResponse, root_agent_id: str, branch_id: str):
    response["Cache-Control"] = "no-store"
    keys = set(request.GET)
    if (not keys <= {"appId", "afterSessionId", "limit"} or "appId" not in keys
            or any(len(request.GET.getlist(key)) != 1 for key in keys)):
        return Status(400, {"error": "business_branch_request_invalid"})
    app_id = request.GET["appId"]
    after = request.GET.get("afterSessionId")
    raw_limit = request.GET.get("limit", "50")
    if (not 1 <= len(app_id) <= 64 or (after is not None and not 1 <= len(after) <= 64)
            or not 1 <= len(raw_limit) <= 3 or not raw_limit.isascii() or not raw_limit.isdecimal()
            or not 1 <= int(raw_limit) <= 100):
        return Status(400, {"error": "business_branch_request_invalid"})
    branch = BusinessAgentBranch.objects.select_related("root_agent", "agent", "session").filter(
        pk=branch_id, root_agent_id=root_agent_id, app_id=app_id,
        root_agent__owner=request.user, root_agent__definition__isnull=True,
        root_agent__business_branch__isnull=True, agent__owner=request.user,
        agent__workspace_id=F("root_agent__workspace_id"), agent__definition__isnull=True,
        session__owner=request.user, session__workspace_id=F("root_agent__workspace_id"),
        session__agent_id=F("agent_id"), session__coordination_binding__agent_id=F("agent_id"),
    ).first()
    if branch is None or workspace_membership_for(request.user, branch.root_agent.workspace_id) is None:
        return Status(404, {"error": "business_branch_not_found"})
    work_sessions = Session.objects.filter(
        agent_id=branch.agent_id, owner=request.user, workspace_id=branch.root_agent.workspace_id,
        work_binding__coordination_session_id=branch.session_id,
        work_binding__source_run__session_id=branch.session_id,
        work_binding__source_run__user=request.user,
        work_binding__source_run__workspace_id=branch.root_agent.workspace_id,
    ).exclude(pk=branch.session_id)
    if after is not None:
        if not work_sessions.filter(pk=after).exists():
            return Status(400, {"error": "business_branch_request_invalid"})
        work_sessions = work_sessions.filter(pk__gt=after)
    limit = int(raw_limit)
    page = list(work_sessions.select_related("work_binding").order_by("pk")[:limit + 1])
    def metadata(session):
        return {"sessionId": session.pk, "title": session.title,
                "status": session.status, "createdAt": session.createdAt.isoformat()}
    return {"branchId": branch.pk, "coordinationSession": metadata(branch.session),
        "workSessions": [{**metadata(session), "sourceAgentRunId": session.work_binding.source_run_id}
            for session in page[:limit]],
        "nextAfterSessionId": page[limit - 1].pk if len(page) > limit else None}
