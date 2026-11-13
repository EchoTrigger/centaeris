"""Recipient-scoped Issue result submissions, responses and file delivery."""
from typing import Literal
from uuid import UUID

from asgiref.sync import sync_to_async
from django.db import transaction
from django.http import JsonResponse
from ninja import Router, Status
from pydantic import Field

from app_core.hosted_operations import HostedOperationError
from app_core.issue_deliverables import (get_issue, respond_to_deliverable, select_deliverable_artifact,
    serialize_deliverable, submit_deliverable, visible_deliverables)
from app_core.note_content import content_response
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["issue-deliverables"], by_alias=True)


class DeliverableVersion(StrictSchema):
    version: int = Field(ge=1)


class DeliverableSubmit(DeliverableVersion):
    execution_id: str = Field(pattern=r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
    agent_run_id: str = Field(min_length=1, max_length=64)
    summary: str = Field(default="", max_length=32768)
    artifact_ids: list[str] = Field(max_length=16)
    recipient_id: str = Field(pattern=r"^[1-9][0-9]*$", max_length=64)
    replaces_id: str | None = Field(default=None, pattern=r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")


class DeliverableRespond(DeliverableVersion):
    action: Literal["feedback", "accept"]
    text: str = Field(default="", max_length=32768)


class DeliverablePerson(StrictSchema):
    id: str
    name: str


class DeliverableArtifact(StrictSchema):
    id: str
    display_name: str
    download_url: str


class DeliverableResponse(StrictSchema):
    id: str
    actor: DeliverablePerson
    action: Literal["feedback", "accept"]
    text: str
    created_at: str


class DeliverableRecord(StrictSchema):
    id: str
    issue_id: str
    execution_id: str
    agent_run_id: str
    author: DeliverablePerson
    recipient: DeliverablePerson
    revision: int
    replaces_id: str | None
    summary: str
    artifacts: list[DeliverableArtifact]
    responses: list[DeliverableResponse]
    created_at: str
    is_current: bool


class DeliverableEnvelope(StrictSchema):
    version: int
    deliverable: DeliverableRecord


class DeliverablesEnvelope(StrictSchema):
    version: int
    deliverables: list[DeliverableRecord]


@router.get("/workspaces/{workspace_id}/issues/{issue_id}/deliverables", auth=session_auth,
    response={200: DeliverablesEnvelope} | COMMON_ERROR_RESPONSES)
def list_deliverables(request, workspace_id: str, issue_id: str):
    try:
        issue = get_issue(request.user, workspace_id, issue_id)
        items = visible_deliverables(request.user, workspace_id).filter(issue=issue).select_related(
            "issue", "author", "recipient").order_by("-revision", "id")
        return {"version": issue.version, "deliverables": [serialize_deliverable(item) for item in items]}
    except HostedOperationError as error:
        return Status(error.status, {"error": error.code})


@router.post("/workspaces/{workspace_id}/issues/{issue_id}/deliverables", auth=session_auth,
    response={201: DeliverableEnvelope} | COMMON_ERROR_RESPONSES)
def create_deliverable(request, workspace_id: str, issue_id: str, payload: DeliverableSubmit):
    try:
        with transaction.atomic():
            issue = get_issue(request.user, workspace_id, issue_id, version=payload.version)
            item = submit_deliverable(request.user, issue, payload)
            return Status(201, {"version": issue.version, "deliverable": serialize_deliverable(item)})
    except HostedOperationError as error:
        return Status(error.status, {"error": error.code})


@router.post("/workspaces/{workspace_id}/issues/{issue_id}/deliverables/{deliverable_id}/responses",
    auth=session_auth, response={200: DeliverableEnvelope} | COMMON_ERROR_RESPONSES)
def create_response(request, workspace_id: str, issue_id: str, deliverable_id: UUID, payload: DeliverableRespond):
    try:
        with transaction.atomic():
            issue = get_issue(request.user, workspace_id, issue_id, version=payload.version)
            item = visible_deliverables(request.user, workspace_id).filter(pk=deliverable_id, issue=issue).first()
            if item is None:
                raise HostedOperationError(404, "deliverable_not_found")
            respond_to_deliverable(request.user, issue, item, payload)
            return {"version": issue.version, "deliverable": serialize_deliverable(item)}
    except HostedOperationError as error:
        return Status(error.status, {"error": error.code})


@router.get("/workspaces/{workspace_id}/issues/{issue_id}/deliverables/{deliverable_id}/artifacts/{artifact_id}",
    auth=session_auth, response=None)
async def download_deliverable_artifact(request, workspace_id: str, issue_id: str, deliverable_id: UUID, artifact_id: str):
    try:
        selected = await sync_to_async(select_deliverable_artifact, thread_sensitive=True)(
            request.user, workspace_id, issue_id, deliverable_id, artifact_id)
    except HostedOperationError as error:
        return JsonResponse({"error": error.code}, status=error.status)
    storage_key, content_type, filename, size = selected
    return await content_response(storage_key, content_type, filename, as_attachment=True, content_length=size)
