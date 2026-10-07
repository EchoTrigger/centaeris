"""Permission-checked, read-only Office previews derived by LibreOffice."""

import html
from pathlib import PurePosixPath
from urllib.parse import quote

from asgiref.sync import sync_to_async
from django.conf import settings
from django.contrib.auth import get_user_model
from django.db import transaction
from django.http import HttpResponse, JsonResponse
from ninja import Router

from app_core.material_contract import KnowledgeError
from app_core.material_identity import processing_spec_digest, representation_id
from app_core.material_task_source import resolve_task_source
from app_core.models import (
    Artifact,
    DerivedRepresentation,
    MaterialProcessingTask,
    MaterialProcessor,
    SourceObject,
    UserLibraryObject,
)
from app_core.workspace_access import source_access_is_at_least, workspace_membership_for

from .security import session_auth
from .storage_stream import stored_file_response


router = Router(tags=["office-preview"])
FORMATS = {"docx", "xlsx", "pptx"}


def _owner(user_id, kind, object_id):
    user = get_user_model().objects.filter(pk=user_id, is_active=True).first()
    if user is None:
        return None
    if kind == "userLibraryObject":
        return UserLibraryObject.objects.filter(
            pk=object_id,
            owner=user,
            status="ready",
            deletedAt=None,
            objectKind__in=["file", "savedArtifact"],
        ).first()
    if kind == "artifact":
        item = Artifact.objects.filter(
            pk=object_id,
            agent_run__user=user,
            session__owner=user,
            status="published",
            deletedAt=None,
        ).first()
        return item if item and workspace_membership_for(user, item.workspace_id) else None
    if kind == "sourceObject":
        item = SourceObject.objects.select_related("source").filter(
            pk=object_id,
            status="ready",
            deletedAt=None,
            objectType="file",
            source__status="ready",
        ).first()
        membership = workspace_membership_for(user, item.workspace_id) if item else None
        return item if membership and source_access_is_at_least(membership, item.source, "read") else None
    return None


def _error(code, status):
    response = JsonResponse({"error": code}, status=status)
    response["Cache-Control"] = "no-store"
    return response


def _loading(display_name, lang, refresh_url):
    message = (
        "正在生成只读预览…"
        if lang == "zh-CN"
        else "Generating a read-only preview…"
    )
    body = f"""<!doctype html><html lang="{lang}"><head><meta charset="utf-8">
<meta http-equiv="refresh" content="1;url={html.escape(refresh_url, quote=True)}"><title>{html.escape(display_name)}</title>
<style>html,body{{height:100%;margin:0}}body{{display:grid;place-items:center;background:#f7f7f5;color:#555;font:14px system-ui,sans-serif}}</style>
</head><body><p role="status">{message}</p></body></html>"""
    response = HttpResponse(body, status=202, content_type="text/html; charset=utf-8")
    response["Cache-Control"] = "no-store"
    response["Retry-After"] = "1"
    response["Refresh"] = f"1;url={refresh_url}"
    response["Content-Security-Policy"] = (
        f"default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'self' {settings.WEB_ORIGIN}"
    )
    response.xframe_options_exempt = True
    return response


def _preview_selection(user_id, owner_kind, object_id, lang, *, expected_identity=None, refresh_url=None):
    if lang not in {"zh-CN", "en"}:
        return _error("office_preview_language_invalid", 400)
    item = _owner(user_id, owner_kind, object_id)
    if item is None:
        return _error("office_preview_not_found", 404)
    if expected_identity is not None and (item.contentGeneration != expected_identity['generation'] or item.sha256 != expected_identity['sha256']):
        return _error("office_preview_version_changed", 409)
    extension = PurePosixPath(item.displayName).suffix.lower().lstrip(".")
    if extension not in FORMATS:
        return _error("office_preview_unsupported", 415)
    processor = MaterialProcessor.objects.select_related("specification").filter(name="document").first()
    if processor is None:
        return _error("office_preview_processor_unavailable", 503)
    specification = processor.specification
    try:
        spec_digest = processing_spec_digest(specification.payload)
    except KnowledgeError:
        return _error("office_preview_processor_invalid", 503)
    if spec_digest != specification.pk:
        return _error("office_preview_processor_invalid", 503)
    identity = {
        "ownerKind": owner_kind,
        "ownerId": item.pk,
        "generation": item.contentGeneration,
        "sha256": item.sha256,
    }
    preview_id = representation_id(identity, spec_digest)
    representation = DerivedRepresentation.objects.filter(pk=preview_id).first()
    if representation is not None:
        if (
            representation.ownerKind != owner_kind
            or representation.ownerId != item.pk
            or representation.ownerContentGeneration != item.contentGeneration
            or representation.ownerSha256 != item.sha256
            or representation.processingSpecification_id != spec_digest
        ):
            return _error("office_preview_identity_conflict", 409)
        if not representation.previewPdfKey or not representation.previewPdfSizeBytes:
            return _error("office_preview_result_missing", 503)
        filename = str(PurePosixPath(item.displayName).with_suffix(".pdf").name)
        return (
            representation.previewPdfKey,
            filename,
            representation.previewPdfSizeBytes,
        )
    payload = {
        "schema": "workspace.material.processing.v1",
        "inputIdentity": identity,
        "specDigest": spec_digest,
        "sizeBytes": item.sizeBytes,
    }
    with transaction.atomic():
        task, _ = MaterialProcessingTask.objects.get_or_create(
            representationId=preview_id,
            defaults={
                "executionBackend": "platform",
                "processingSpecification": specification.payload,
                "payload": payload,
            },
        )
        if (
            task.executionBackend != "platform"
            or task.processingSpecification != specification.payload
            or task.payload != payload
        ):
            return _error("office_preview_task_conflict", 409)
        try:
            resolve_task_source(task)
        except KnowledgeError as error:
            return _error(error.code, error.status)
        if task.status == "completed":
            return _error("office_preview_result_missing", 503)
        if task.status not in {"pending", "running"}:
            return _error("office_preview_task_invalid", 503)
    refresh_url = refresh_url or (
        f"/api/office-preview/{quote(owner_kind, safe='')}/"
        f"{quote(object_id, safe='')}?lang={lang}"
    )
    return _loading(item.displayName, lang, refresh_url)


@router.get(
    "/office-preview/{owner_kind}/{object_id}",
    auth=session_auth,
    response=None,
)
async def office_preview(request, owner_kind: str, object_id: str, lang: str = "zh-CN"):
    selected = await sync_to_async(_preview_selection, thread_sensitive=True)(
        request.user.id,
        owner_kind,
        object_id,
        lang,
    )
    if not isinstance(selected, tuple):
        return selected
    storage_key, filename, content_length = selected
    response = await stored_file_response(
        storage_key,
        "application/pdf",
        filename,
        as_attachment=False,
        content_length=content_length,
    )
    response["Cache-Control"] = "no-store"
    return response


def _spreadsheet_selection(user_id, owner_kind, object_id, lang):
    item = _owner(user_id, owner_kind, object_id)
    if item is None:
        return _error("spreadsheet_preview_not_found", 404)
    if PurePosixPath(item.displayName).suffix.lower() != ".xlsx":
        return _error("spreadsheet_preview_unsupported", 415)
    selected = _preview_selection(user_id, owner_kind, object_id, lang)
    if not isinstance(selected, tuple):
        if getattr(selected, "status_code", None) == 202:
            refresh_url = (
                f"/api/spreadsheet-preview/{quote(owner_kind, safe='')}/"
                f"{quote(object_id, safe='')}?lang={lang}"
            )
            return _loading(item.displayName, lang, refresh_url)
        return selected
    representation = DerivedRepresentation.objects.filter(previewPdfKey=selected[0]).first()
    if representation is None or not representation.workbookPreviewKey or not representation.workbookPreviewSizeBytes:
        return _error("spreadsheet_preview_result_missing", 503)
    return (
        representation.workbookPreviewKey,
        str(PurePosixPath(item.displayName).with_suffix(".json").name),
        representation.workbookPreviewSizeBytes,
    )


@router.get(
    "/spreadsheet-preview/{owner_kind}/{object_id}",
    auth=session_auth,
    response=None,
)
async def spreadsheet_preview(request, owner_kind: str, object_id: str, lang: str = "zh-CN"):
    selected = await sync_to_async(_spreadsheet_selection, thread_sensitive=True)(
        request.user.id,
        owner_kind,
        object_id,
        lang,
    )
    if not isinstance(selected, tuple):
        return selected
    storage_key, filename, content_length = selected
    response = await stored_file_response(
        storage_key,
        "application/json",
        filename,
        as_attachment=False,
        content_length=content_length,
    )
    response["Cache-Control"] = "no-store"
    return response
