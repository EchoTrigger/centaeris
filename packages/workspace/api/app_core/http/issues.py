"""Workspace member issue records and transactional activity history."""
from typing import Literal

from django.core.exceptions import ValidationError
from django.db import transaction
from django.utils import timezone
from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.models import Issue, IssueEvent, WorkspaceMembership
from app_core.workspace_access import locked_workspace_membership_for, workspace_membership_for
from .query import valid_query_fields
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["issues"], by_alias=True)
IssueStatus = Literal["backlog", "todo", "inProgress", "done", "canceled"]


class IssueFields(StrictSchema):
    title: str = Field(min_length=1, max_length=240)
    description: str = Field(default="", max_length=65536)
    status: IssueStatus = "todo"
    assignee_id: str | None = None

    @field_validator("title")
    @classmethod
    def normalize_title(cls, value):
        value = value.strip()
        if not value:
            raise ValueError("issue_title_required")
        return value


class IssuePatch(StrictSchema):
    version: int = Field(ge=1)
    title: str = Field(default="", max_length=240)
    description: str = Field(default="", max_length=65536)
    status: IssueStatus = "todo"
    assignee_id: str | None = None
    archived: bool = False

    _normalize_title = field_validator("title")(IssueFields.normalize_title.__func__)


class IssueResponse(StrictSchema):
    id: str
    workspace_id: str
    title: str
    description: str
    status: IssueStatus
    assignee_id: str | None
    creator_id: str
    version: int
    archived_at: str | None
    created_at: str
    updated_at: str


class IssueEnvelope(StrictSchema):
    issue: IssueResponse


class IssuesEnvelope(StrictSchema):
    issues: list[IssueResponse]
    total: int


class IssueActor(StrictSchema):
    id: str
    name: str


class IssueChange(StrictSchema):
    before: str | int | bool | None
    after: str | int | bool | None


class IssueEventResponse(StrictSchema):
    id: str
    issue_id: str
    actor: IssueActor
    type: Literal["created", "updated", "archived", "restored"]
    changes: dict[str, IssueChange]
    created_at: str


class EventsEnvelope(StrictSchema):
    events: list[IssueEventResponse]
    total: int


class AssigneesEnvelope(StrictSchema):
    assignees: list[IssueActor]


def _actor(user):
    return {"id": str(user.pk), "name": user.get_full_name() or user.email or user.username}


def _serialize(issue):
    return {"id": issue.id, "workspaceId": issue.workspace_id, "title": issue.title,
        "description": issue.description, "status": issue.status,
        "assigneeId": str(issue.assignee_id) if issue.assignee_id is not None else None,
        "creatorId": str(issue.creator_id), "version": issue.version,
        "archivedAt": issue.archivedAt.isoformat() if issue.archivedAt else None,
        "createdAt": issue.createdAt.isoformat(), "updatedAt": issue.updatedAt.isoformat()}


def _pagination(request, extra=()):
    if not valid_query_fields(request.GET, {"limit", "offset", *extra}):
        return None
    limit, offset = request.GET.get("limit", "50"), request.GET.get("offset", "0")
    if not limit.isascii() or not limit.isdecimal() or not offset.isascii() or not offset.isdecimal():
        return None
    if len(limit) > 3 or len(offset) > 10:
        return None
    limit, offset = int(limit), int(offset)
    return (limit, offset) if 1 <= limit <= 100 and offset <= 2147483647 else None


def _assignee_valid(workspace_id, assignee_id):
    if assignee_id is None:
        return True
    user_field = WorkspaceMembership._meta.get_field("user").target_field
    try:
        user_id = user_field.to_python(assignee_id)
    except (ValidationError, ValueError, TypeError):
        return False
    if str(user_id) != assignee_id:
        return False
    return WorkspaceMembership.objects.filter(workspace_id=workspace_id, user_id=user_id).exists()


@router.get("/workspaces/{workspace_id}/issues", auth=session_auth,
    response={200: IssuesEnvelope} | COMMON_ERROR_RESPONSES)
def list_issues(request, workspace_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    page = _pagination(request, ("mine", "archived"))
    if page is None or any(request.GET.get(key, "false") not in {"true", "false"} for key in ("mine", "archived")):
        return Status(400, {"error": "issue_query_invalid"})
    issues = Issue.objects.filter(workspace_id=workspace_id, archivedAt__isnull=request.GET.get("archived", "false") == "false")
    if request.GET.get("mine", "false") == "true":
        issues = issues.filter(assignee=request.user)
    limit, offset = page
    return {"issues": [_serialize(item) for item in issues.order_by("-updatedAt", "id")[offset:offset + limit]], "total": issues.count()}


@router.post("/workspaces/{workspace_id}/issues", auth=session_auth,
    response={201: IssueEnvelope} | COMMON_ERROR_RESPONSES)
def create_issue(request, workspace_id: str, payload: IssueFields):
    with transaction.atomic():
        if locked_workspace_membership_for(request.user, workspace_id) is None:
            return Status(404, {"error": "workspace_not_found"})
        if not _assignee_valid(workspace_id, payload.assignee_id):
            return Status(400, {"error": "issue_assignee_invalid"})
        issue = Issue.objects.create(workspace_id=workspace_id, creator=request.user,
            title=payload.title, description=payload.description, status=payload.status, assignee_id=payload.assignee_id)
        data = _serialize(issue)
        IssueEvent.objects.create(issue=issue, actor=request.user, type="created", changes={
            key: {"before": None, "after": data[key]} for key in ("title", "description", "status", "assigneeId")})
    return Status(201, {"issue": data})


@router.get("/workspaces/{workspace_id}/issues/assignees", auth=session_auth,
    response={200: AssigneesEnvelope} | COMMON_ERROR_RESPONSES)
def list_assignees(request, workspace_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    return {"assignees": [_actor(member.user) for member in WorkspaceMembership.objects.filter(
        workspace_id=workspace_id).select_related("user").order_by("user_id")]}


@router.get("/workspaces/{workspace_id}/issues/{issue_id}", auth=session_auth,
    response={200: IssueEnvelope} | COMMON_ERROR_RESPONSES)
def get_issue(request, workspace_id: str, issue_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    issue = Issue.objects.filter(workspace_id=workspace_id, id=issue_id).first()
    return {"issue": _serialize(issue)} if issue else Status(404, {"error": "issue_not_found"})


@router.patch("/workspaces/{workspace_id}/issues/{issue_id}", auth=session_auth,
    response={200: IssueEnvelope} | COMMON_ERROR_RESPONSES)
def update_issue(request, workspace_id: str, issue_id: str, payload: IssuePatch):
    with transaction.atomic():
        if locked_workspace_membership_for(request.user, workspace_id) is None:
            return Status(404, {"error": "workspace_not_found"})
        issue = Issue.objects.select_for_update().filter(workspace_id=workspace_id, id=issue_id).first()
        if issue is None:
            return Status(404, {"error": "issue_not_found"})
        if issue.version != payload.version:
            return Status(409, {"error": "issue_version_conflict"})
        fields = payload.model_fields_set - {"version"}
        if "assignee_id" in fields and not _assignee_valid(workspace_id, payload.assignee_id):
            return Status(400, {"error": "issue_assignee_invalid"})
        before = _serialize(issue)
        changes = {}
        for field in fields:
            if field == "archived":
                old = issue.archivedAt is not None
                if old != payload.archived:
                    issue.archivedAt = timezone.now() if payload.archived else None
                    changes["archived"] = {"before": old, "after": payload.archived}
            else:
                key = "assigneeId" if field == "assignee_id" else field
                value = getattr(payload, field)
                if before[key] != value:
                    setattr(issue, field, value)
                    changes[key] = {"before": before[key], "after": value}
        if changes:
            issue.version += 1
            issue.save()
            kind = ("archived" if payload.archived else "restored") if "archived" in changes else "updated"
            IssueEvent.objects.create(issue=issue, actor=request.user, type=kind, changes=changes)
        return {"issue": _serialize(issue)}


@router.get("/workspaces/{workspace_id}/issues/{issue_id}/events", auth=session_auth,
    response={200: EventsEnvelope} | COMMON_ERROR_RESPONSES)
def list_events(request, workspace_id: str, issue_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    issue = Issue.objects.filter(workspace_id=workspace_id, id=issue_id).first()
    if issue is None:
        return Status(404, {"error": "issue_not_found"})
    page = _pagination(request)
    if page is None:
        return Status(400, {"error": "issue_query_invalid"})
    limit, offset = page
    events = issue.events.select_related("actor").order_by("createdAt", "id")
    return {"events": [{"id": event.id, "issueId": issue.id, "actor": _actor(event.actor),
        "type": event.type, "changes": event.changes, "createdAt": event.createdAt.isoformat()}
        for event in events[offset:offset + limit]], "total": events.count()}


