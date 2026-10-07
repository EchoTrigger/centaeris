import unicodedata
from typing import Literal

from django.db import transaction
from django.utils import timezone
from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.assets import (
    delete_stored_object,
    granted_source_objects,
    normalize_display_path,
    store_upload,
)
from app_core.models import (
    Source,
    SourceGrant,
    SourceObject,
    WorkspaceGroup,
)
from app_core.trash_retention import trash_is_restorable
from app_core.workspace_access import (
    WORKSPACE_ADMIN_ROLES,
    locked_workspace_membership_for,
    source_access_is_at_least,
    source_access_map_for_workspace_member,
    workspace_membership_for,
)

from .response_schema import (
    COMMON_ERROR_RESPONSES,
    SourceEnvelope,
    SourceGrantEnvelope,
    SourceGrantsEnvelope,
    SourceObjectEnvelope,
    SourceObjectsEnvelope,
    SourcesEnvelope,
)
from .schema import OkResponse, StrictSchema
from .security import session_auth
from .serialization import (
    serialize_source,
    serialize_source_object,
)


router = Router(tags=["sources"], by_alias=True)


def _normalize_source_name(value: str) -> str:
    normalized = unicodedata.normalize("NFC", value.strip())
    if not normalized:
        raise ValueError("source_name_required")
    return normalized


class CreateSourceRequest(StrictSchema):
    source_type: str = Field(alias="sourceType")
    name: str = Field(min_length=1, max_length=255)

    @field_validator("name")
    @classmethod
    def normalize_name(cls, value: str) -> str:
        return _normalize_source_name(value)


class UpdateSourceRequest(StrictSchema):
    name: str = Field(min_length=1, max_length=255)

    @field_validator("name")
    @classmethod
    def normalize_name(cls, value: str) -> str:
        return _normalize_source_name(value)


class CreateSourceGrantRequest(StrictSchema):
    workspace_group_id: str = Field(alias="workspaceGroupId")
    access_level: Literal["read", "write", "control"] = Field(alias="accessLevel")


class UpdateSourceGrantRequest(StrictSchema):
    access_level: Literal["read", "write", "control"] = Field(alias="accessLevel")


def _serialize_source_grant(grant: SourceGrant) -> dict:
    return {
        "id": grant.id,
        "sourceId": grant.source_id,
        "workspaceGroupId": grant.workspaceGroup_id,
        "accessLevel": grant.accessLevel,
    }


@router.get(
    "/workspaces/{workspace_id}/sources",
    auth=session_auth,
    response={200: SourcesEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_sources(request, workspace_id: str):
    access_by_source = source_access_map_for_workspace_member(
        request.user,
        workspace_id,
    )
    if access_by_source is None:
        return Status(404, {"error": "workspace_not_found"})
    sources = (
        Source.objects.filter(id__in=access_by_source)
        .exclude(status="deleted")
        .order_by("name")
    )
    return {
        "sources": [
            serialize_source(source, access_by_source[source.id]) for source in sources
        ]
    }


@router.post(
    "/workspaces/{workspace_id}/sources",
    auth=session_auth,
    response={201: SourceEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_source(
    request,
    workspace_id: str,
    payload: CreateSourceRequest,
):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id)
        if membership is None or membership.role not in WORKSPACE_ADMIN_ROLES:
            return Status(404, {"error": "workspace_not_found"})
        source_type = payload.source_type.strip()
        if source_type not in {"uploadedFile", "fileTree"}:
            return Status(400, {"error": "invalid_source"})
        source = Source.objects.create(
            workspace_id=workspace_id,
            sourceType=source_type,
            name=payload.name,
            createdBy=request.user,
        )
    return Status(201, {"source": serialize_source(source, "control")})


@router.patch(
    "/workspaces/{workspace_id}/sources/{source_id}",
    auth=session_auth,
    response={200: SourceEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_source(
    request,
    workspace_id: str,
    source_id: str,
    payload: UpdateSourceRequest,
):
    with transaction.atomic():
        locked = _locked_source_for_access(
            request.user,
            workspace_id,
            source_id,
            "control",
        )
        if locked is None:
            return Status(404, {"error": "source_not_found"})
        _membership, source = locked
        if source.status == "deleted":
            return Status(410, {"error": "source_deleted"})
        if source.name == payload.name:
            return Status(409, {"error": "source_name_unchanged"})
        source.name = payload.name
        source.save(update_fields=["name", "updatedAt"])
    return {"source": serialize_source(source, "control")}


@router.post(
    "/workspaces/{workspace_id}/sources/{source_id}/restore",
    auth=session_auth,
    response={200: SourceEnvelope} | COMMON_ERROR_RESPONSES,
)
def restore_source(request, workspace_id: str, source_id: str):
    with transaction.atomic():
        locked = _locked_source_for_access(
            request.user,
            workspace_id,
            source_id,
            "control",
        )
        if locked is None:
            return Status(404, {"error": "source_not_found"})
        _membership, source = locked
        if source.status == "deleted":
            if not trash_is_restorable(source.deletedAt, source.purgedAt):
                return Status(410, {"error": "source_expired"})
            source.status = source.deletedFromStatus
            source.deletedFromStatus = ""
            source.deletedAt = None
            source.deletedBy = None
            source.purgedAt = None
            source.save(
                update_fields=[
                    "status",
                    "deletedFromStatus",
                    "deletedAt",
                    "deletedBy",
                    "purgedAt",
                    "updatedAt",
                ]
            )
        else:
            return Status(409, {"error": "source_not_restorable"})
    return {"source": serialize_source(source, "control")}


@router.delete(
    "/workspaces/{workspace_id}/sources/{source_id}",
    auth=session_auth,
    response={200: OkResponse} | COMMON_ERROR_RESPONSES,
)
def delete_source(request, workspace_id: str, source_id: str):
    with transaction.atomic():
        locked = _locked_source_for_access(
            request.user,
            workspace_id,
            source_id,
            "control",
        )
        if locked is None:
            return Status(404, {"error": "source_not_found"})
        _membership, source = locked
        if source.status == "deleted":
            return Status(410, {"error": "source_deleted"})
        source.deletedFromStatus = source.status
        source.status = "deleted"
        source.deletedAt = timezone.now()
        source.deletedBy = request.user
        source.save(
            update_fields=[
                "status",
                "deletedFromStatus",
                "deletedAt",
                "deletedBy",
                "updatedAt",
            ]
        )
    return {"ok": True}


@router.delete(
    "/workspaces/{workspace_id}/sources/{source_id}/trash",
    auth=session_auth,
    response={200: OkResponse} | COMMON_ERROR_RESPONSES,
)
def permanently_delete_source(request, workspace_id: str, source_id: str):
    with transaction.atomic():
        locked = _locked_source_for_access(
            request.user,
            workspace_id,
            source_id,
            "control",
        )
        if locked is None:
            return Status(404, {"error": "source_not_found"})
        _membership, source = locked
        if source.status != "deleted":
            return Status(409, {"error": "source_not_deleted"})
        if source.purgedAt is not None:
            return Status(410, {"error": "source_purged"})
        source.purgedAt = timezone.now()
        source.save(update_fields=["purgedAt", "updatedAt"])
    return {"ok": True}


@router.get(
    "/workspaces/{workspace_id}/sources/{source_id}/objects",
    auth=session_auth,
    response={200: SourceObjectsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_source_objects(request, workspace_id: str, source_id: str):
    selected = _source_for_access(
        request.user,
        workspace_id,
        source_id,
        "read",
    )
    if selected is None:
        return Status(404, {"error": "source_not_found"})
    _membership, source = selected
    unavailable = _source_content_unavailable(source)
    if unavailable is not None:
        return unavailable
    objects = [
        serialize_source_object(item)
        for item in granted_source_objects(request.user, source)
    ]
    return {"objects": objects}


@router.post(
    "/workspaces/{workspace_id}/sources/{source_id}/objects",
    auth=session_auth,
    response={201: SourceObjectEnvelope} | COMMON_ERROR_RESPONSES,
)
def upload_source_object(request, workspace_id: str, source_id: str):
    selected = _source_for_member(request.user, workspace_id, source_id)
    if selected is None:
        return Status(404, {"error": "source_not_found"})
    membership, source = selected
    try:
        upload = request.FILES.get("file")
        if upload is None:
            raise ValueError("file_required")
        display_path = normalize_display_path(
            request.POST.get("displayPath") or upload.name
        )
        if not source_access_is_at_least(
            membership,
            source,
            "write",
        ):
            return Status(404, {"error": "source_not_found"})
        unavailable = _source_content_unavailable(source)
        if unavailable is not None:
            return unavailable
        metadata = store_upload(
            upload,
            f"workspaces/{workspace_id}/sources/{source.id}",
        )
    except ValueError as error:
        return Status(400, {"error": str(error)})
    try:
        with transaction.atomic():
            locked = _locked_source_for_access(
                request.user,
                workspace_id,
                source_id,
                "write",
            )
            if locked is None:
                delete_stored_object(metadata["storageKey"])
                return Status(404, {"error": "source_not_found"})
            _membership, source = locked
            unavailable = _source_content_unavailable(source)
            if unavailable is not None:
                delete_stored_object(metadata["storageKey"])
                return unavailable
            item = SourceObject.objects.create(
                workspace_id=source.workspace_id,
                source=source,
                objectType="file",
                displayPath=display_path,
                displayName=metadata["displayName"],
                contentType=metadata["contentType"],
                sizeBytes=metadata["sizeBytes"],
                sha256=metadata["sha256"],
                storageKey=metadata["storageKey"],
                sourceVersion=metadata["sha256"],
                status="ready",
            )
            source.status = "ready"
            source.failureReason = ""
            source.save(update_fields=["status", "failureReason", "updatedAt"])
    except Exception:
        delete_stored_object(metadata["storageKey"])
        raise
    return Status(201, {"object": serialize_source_object(item)})


@router.get(
    "/workspaces/{workspace_id}/sources/{source_id}/grants",
    auth=session_auth,
    response={200: SourceGrantsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_source_grants(request, workspace_id: str, source_id: str):
    selected = _source_for_access(
        request.user,
        workspace_id,
        source_id,
        "control",
    )
    if selected is None:
        return Status(404, {"error": "source_not_found"})
    _membership, source = selected
    if source.status == "deleted":
        return Status(410, {"error": "source_deleted"})
    return {
        "grants": [
            _serialize_source_grant(grant)
            for grant in source.grants.order_by("createdAt", "id")
        ]
    }


@router.post(
    "/workspaces/{workspace_id}/sources/{source_id}/grants",
    auth=session_auth,
    response={201: SourceGrantEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_source_grant(
    request,
    workspace_id: str,
    source_id: str,
    payload: CreateSourceGrantRequest,
):
    with transaction.atomic():
        locked = _locked_source_for_access(
            request.user,
            workspace_id,
            source_id,
            "control",
        )
        if locked is None:
            return Status(404, {"error": "source_not_found"})
        membership, source = locked
        if source.status == "deleted":
            return Status(410, {"error": "source_deleted"})
        workspace_group_id = payload.workspace_group_id.strip()
        if not workspace_group_id:
            return Status(400, {"error": "invalid_source_grant"})
        workspace_group = WorkspaceGroup.objects.select_for_update().filter(
            id=workspace_group_id,
            workspace_id=membership.workspace_id,
        ).first()
        if workspace_group is None:
            return Status(400, {"error": "invalid_source_grant"})
        if SourceGrant.objects.filter(
            source=source,
            workspaceGroup=workspace_group,
        ).exists():
            return Status(409, {"error": "source_grant_exists"})
        grant = SourceGrant.objects.create(
            workspace_id=membership.workspace_id,
            source=source,
            workspaceGroup=workspace_group,
            accessLevel=payload.access_level,
            createdBy=request.user,
        )
    return Status(
        201,
        {"grant": _serialize_source_grant(grant)},
    )


@router.patch(
    "/workspaces/{workspace_id}/sources/{source_id}/grants/{grant_id}",
    auth=session_auth,
    response={200: SourceGrantEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_source_grant(
    request,
    workspace_id: str,
    source_id: str,
    grant_id: str,
    payload: UpdateSourceGrantRequest,
):
    with transaction.atomic():
        locked = _locked_source_for_access(
            request.user,
            workspace_id,
            source_id,
            "control",
        )
        if locked is None:
            return Status(404, {"error": "source_not_found"})
        membership, source = locked
        if source.status == "deleted":
            return Status(410, {"error": "source_deleted"})
        grant = SourceGrant.objects.select_for_update().filter(
            id=grant_id,
            workspace_id=membership.workspace_id,
            source=source,
        ).first()
        if grant is None:
            return Status(404, {"error": "source_grant_not_found"})
        if grant.accessLevel == payload.access_level:
            return Status(409, {"error": "source_grant_access_unchanged"})
        grant.accessLevel = payload.access_level
        grant.save(update_fields=["accessLevel"])
    return {"grant": _serialize_source_grant(grant)}


@router.delete(
    "/workspaces/{workspace_id}/sources/{source_id}/grants/{grant_id}",
    auth=session_auth,
    response={200: OkResponse} | COMMON_ERROR_RESPONSES,
)
def delete_source_grant(
    request,
    workspace_id: str,
    source_id: str,
    grant_id: str,
):
    with transaction.atomic():
        locked = _locked_source_for_access(
            request.user,
            workspace_id,
            source_id,
            "control",
        )
        if locked is None:
            return Status(404, {"error": "source_not_found"})
        membership, source = locked
        if source.status == "deleted":
            return Status(410, {"error": "source_deleted"})
        grant = SourceGrant.objects.select_for_update().filter(
            id=grant_id,
            workspace_id=membership.workspace_id,
            source=source,
        ).first()
        if grant is None:
            return Status(404, {"error": "source_grant_not_found"})
        grant.delete()
    return {"ok": True}


def _source_content_unavailable(source: Source):
    if source.status == "deleted":
        return Status(410, {"error": "source_deleted"})
    return None


def _source_for_member(user, workspace_id: str, source_id: str):
    membership = workspace_membership_for(user, workspace_id)
    if membership is None:
        return None
    try:
        source = Source.objects.get(id=source_id, workspace_id=workspace_id)
    except Source.DoesNotExist:
        return None
    return membership, source


def _source_for_access(
    user,
    workspace_id: str,
    source_id: str,
    required_access: str,
):
    selected = _source_for_member(user, workspace_id, source_id)
    if selected is None:
        return None
    membership, source = selected
    if not source_access_is_at_least(
        membership,
        source,
        required_access,
    ):
        return None
    return membership, source


def _locked_source_for_access(
    user,
    workspace_id: str,
    source_id: str,
    required_access: str,
):
    membership = locked_workspace_membership_for(user, workspace_id)
    if membership is None:
        return None
    source = Source.objects.select_for_update().filter(
        id=source_id,
        workspace_id=workspace_id,
    ).first()
    if source is None or not source_access_is_at_least(
        membership,
        source,
        required_access,
    ):
        return None
    return membership, source
