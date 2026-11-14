from app_core.issue_access import visible_issues
"""Attach existing Space objects without granting access to their contents."""
from typing import Literal
from uuid import UUID

from django.db import transaction
from django.utils import timezone
from ninja import Router, Status
from pydantic import Field

from app_core.assets import captured_input_fields
from app_core.hosted_operations import HostedOperationError
from app_core.issue_materials import material_envelope, material_matches, resolve_material
from app_core.models import Issue, IssueEvent, IssueMaterial
from app_core.workspace_access import locked_workspace_membership_for, workspace_membership_for
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["issue-materials"], by_alias=True)


class IssueMaterialVersion(StrictSchema):
    version: int = Field(ge=1)


class IssueMaterialRequest(IssueMaterialVersion):
    asset_kind: Literal["sourceObject", "userLibraryObject"]
    asset_id: str = Field(min_length=1, max_length=64)


class IssueMaterialResponse(StrictSchema):
    id: str
    asset_kind: Literal["sourceObject", "userLibraryObject"]
    asset_id: str | None
    display_name: str | None
    content_type: str | None
    size_bytes: int | None
    available: bool


class IssueMaterialsEnvelope(StrictSchema):
    version: int
    materials: list[IssueMaterialResponse]


def _locked_issue(request, workspace_id, issue_id, version):
    if locked_workspace_membership_for(request.user, workspace_id) is None:
        raise HostedOperationError(404, "workspace_not_found")
    issue = visible_issues(request.user, workspace_id, Issue.objects.select_for_update()).filter(id=issue_id).first()
    if issue is None:
        raise HostedOperationError(404, "issue_not_found")
    if issue.archivedAt is not None:
        raise HostedOperationError(409, "issue_archived")
    if issue.version != version:
        raise HostedOperationError(409, "issue_version_conflict")
    return issue


def _record_change(issue, user, before, after):
    issue.version += 1
    issue.updatedAt = timezone.now()
    issue.save(update_fields=["version", "updatedAt"])
    # Counts disclose the change without sharing private filenames in the Issue activity.
    IssueEvent.objects.create(issue=issue, actor=user, type="updated",
                              changes={"materials": {"before": before, "after": after}})


@router.get("/workspaces/{workspace_id}/issues/{issue_id}/materials", auth=session_auth,
            response={200: IssueMaterialsEnvelope} | COMMON_ERROR_RESPONSES)
def list_issue_materials(request, workspace_id: str, issue_id: str):
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    issue = visible_issues(request.user, workspace_id).filter(id=issue_id).first()
    if issue is None:
        return Status(404, {"error": "issue_not_found"})
    return material_envelope(issue, request.user)


@router.post("/workspaces/{workspace_id}/issues/{issue_id}/materials", auth=session_auth,
             response={200: IssueMaterialsEnvelope} | COMMON_ERROR_RESPONSES)
def attach_issue_material(request, workspace_id: str, issue_id: str, payload: IssueMaterialRequest):
    try:
        with transaction.atomic():
            issue = _locked_issue(request, workspace_id, issue_id, payload.version)
            asset = resolve_material(request.user, workspace_id, payload.asset_kind, payload.asset_id, lock=True)
            if asset is None:
                raise HostedOperationError(403, "issue_material_unavailable")
            try:
                captured_input_fields(asset)
            except ValueError as error:
                raise HostedOperationError(400, str(error)) from error
            before = issue.materials.count()
            material, created = IssueMaterial.objects.get_or_create(
                issue=issue, assetKind=payload.asset_kind, assetId=asset.pk,
                defaults={"attachedBy": request.user, "displayName": asset.displayName,
                          "contentType": asset.contentType or "application/octet-stream",
                          "contentGeneration": asset.contentGeneration, "sizeBytes": asset.sizeBytes, "sha256": asset.sha256})
            if not material_matches(material, asset):
                raise HostedOperationError(409, "issue_material_version_changed")
            if created:
                _record_change(issue, request.user, before, before + 1)
            return material_envelope(issue, request.user)
    except HostedOperationError as error:
        return Status(error.status, {"error": error.code})


@router.delete("/workspaces/{workspace_id}/issues/{issue_id}/materials/{material_id}", auth=session_auth,
               response={200: IssueMaterialsEnvelope} | COMMON_ERROR_RESPONSES)
def detach_issue_material(request, workspace_id: str, issue_id: str, material_id: UUID,
                          payload: IssueMaterialVersion):
    try:
        with transaction.atomic():
            issue = _locked_issue(request, workspace_id, issue_id, payload.version)
            material = issue.materials.filter(pk=material_id).first()
            if material is None:
                raise HostedOperationError(404, "issue_material_not_found")
            before = issue.materials.count()
            material.delete()
            _record_change(issue, request.user, before, before - 1)
            return material_envelope(issue, request.user)
    except HostedOperationError as error:
        return Status(error.status, {"error": error.code})
