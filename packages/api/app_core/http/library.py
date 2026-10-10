import os
import unicodedata

from django.contrib.auth import get_user_model
from django.conf import settings
from django.core.files.storage import default_storage
from django.db import transaction
from django.utils import timezone
from ninja import Router, Status
from pydantic import Field

from app_core.assets import (
    MAX_DIRECT_INPUT_BYTES,
    captured_input_fields,
    delete_stored_object,
    delete_stored_object_for_gc,
    safe_filename,
    source_object_is_granted,
    store_bytes,
    store_upload,
    tombstone_superseded_derived_resources,
    tombstone_stored_object,
)
from app_core.app_delegations import DelegationRejected, require_request_delegation
from app_core.models import (
    Artifact,
    DerivedResource,
    Session,
    SessionAssetLink,
    SourceObject,
    UserLibraryLink,
    UserLibraryObject,
    new_library_object_id,
)
from app_core.trash_retention import trash_cutoff, trash_is_restorable
from app_core.workspace_access import locked_workspace_membership_for, workspace_membership_for

from .response_schema import (
    COMMON_ERROR_RESPONSES,
    DeletedResponse,
    LibraryNoteEnvelope,
    LibraryObjectEnvelope,
    LibraryObjectsEnvelope,
    SessionAssetEnvelope,
    SessionAssetsEnvelope,
    SessionUploadEnvelope,
)
from .schema import StrictSchema
from .security import session_auth, usage_auth
from .serialization import (
    serialize_artifact,
    serialize_library_object,
    serialize_source_object,
)


router = Router(tags=["library-assets"], by_alias=True)
MAX_UPLOAD_BATCH_FILES = 50


class CreateLibraryFolderRequest(StrictSchema):
    display_name: str = Field(alias="displayName")
    parent_folder_id: str = Field(default="", alias="parentFolderId")


class CreateLibraryNoteRequest(StrictSchema):
    display_name: str = Field(alias="displayName")
    markdown: str
    parent_folder_id: str = Field(default="", alias="parentFolderId")


class UpdateLibraryObjectRequest(StrictSchema):
    display_name: str | None = Field(default=None, alias="displayName")
    parent_folder_id: str | None = Field(default=None, alias="parentFolderId")


class UpdateLibraryNoteRequest(StrictSchema):
    display_name: str = Field(alias="displayName")
    markdown: str


class AttachSessionAssetRequest(StrictSchema):
    asset_kind: str = Field(alias="assetKind")
    asset_id: str = Field(alias="assetId")


class DetachSessionAssetRequest(StrictSchema):
    asset_link_id: str = Field(alias="assetLinkId")


class LibraryNameConflict(ValueError):
    pass


@router.get(
    "/library",
    auth=session_auth,
    response={200: LibraryObjectsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_library(request):
    try:
        parent_folder = _library_parent(
            request.user,
            request.GET.get("parentFolderId", ""),
        )
    except UserLibraryObject.DoesNotExist:
        return Status(404, {"error": "library_folder_not_found"})
    items = (
        UserLibraryObject.objects.filter(
            owner=request.user,
            parentFolder=parent_folder,
        )
        .exclude(status="deleted")
        .order_by("-createdAt")
    )
    return {"objects": [serialize_library_object(item) for item in items]}


@router.post(
    "/library",
    auth=session_auth,
    response={201: LibraryObjectsEnvelope} | COMMON_ERROR_RESPONSES,
)
def upload_library_objects(request):
    stored = []
    try:
        uploads = _require_uploads(request, {"parentFolderId"})
        parent_folder = _library_parent(
            request.user,
            request.POST.get("parentFolderId", ""),
        )
        stored = _store_upload_batch(
            uploads,
            f"users/{request.user.id}/library",
        )
    except (ValueError, UserLibraryObject.DoesNotExist) as error:
        return Status(400, {"error": str(error)})
    try:
        with transaction.atomic():
            items = _create_uploaded_library_objects(
                request.user,
                stored,
                parent_folder,
            )
    except Exception as database_error:
        try:
            _delete_stored_upload_batch(stored)
        except RuntimeError as cleanup_error:
            raise cleanup_error from database_error
        raise
    return Status(
        201,
        {"objects": [serialize_library_object(item) for item in items]},
    )


@router.post(
    "/library/folders",
    auth=session_auth,
    response={201: LibraryObjectEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_library_folder(request, payload: CreateLibraryFolderRequest):
    try:
        display_name = _require_library_display_name(payload.display_name)
        parent = _library_parent(request.user, payload.parent_folder_id)
    except (ValueError, UserLibraryObject.DoesNotExist):
        return Status(400, {"error": "library_folder_request_invalid"})
    with transaction.atomic():
        _lock_library_owner(request.user)
        try:
            _require_library_name_available(request.user, parent, display_name)
        except LibraryNameConflict:
            return Status(409, {"error": "library_name_conflict"})
        folder = UserLibraryObject.objects.create(
            id=new_library_object_id(),
            owner=request.user,
            displayName=display_name,
            objectKind="folder",
            contentType="application/vnd.centaeris.folder",
            sizeBytes=0,
            status="ready",
            parentFolder=parent,
        )
        UserLibraryLink.objects.create(
            libraryObject=folder,
            sourceKind="manual",
        )
    return Status(201, {"object": serialize_library_object(folder)})


@router.post(
    "/library/notes",
    auth=session_auth,
    response={201: LibraryObjectEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_library_note(request, payload: CreateLibraryNoteRequest):
    try:
        display_name = _require_library_display_name(payload.display_name)
        markdown = payload.markdown
        parent = _library_parent(request.user, payload.parent_folder_id)
        metadata = store_bytes(
            markdown.encode("utf-8"),
            f"users/{request.user.id}/library",
            (
                f"{display_name}.md"
                if not display_name.lower().endswith(".md")
                else display_name
            ),
            "text/markdown",
        )
    except (ValueError, UserLibraryObject.DoesNotExist):
        return Status(400, {"error": "library_note_request_invalid"})
    try:
        with transaction.atomic():
            _lock_library_owner(request.user)
            _require_library_name_available(request.user, parent, display_name)
            note = UserLibraryObject.objects.create(
                owner=request.user,
                displayName=display_name,
                objectKind="note",
                contentType=metadata["contentType"],
                sizeBytes=metadata["sizeBytes"],
                sha256=metadata["sha256"],
                storageKey=metadata["storageKey"],
                status="ready",
                contentGeneration=1,
                parentFolder=parent,
            )
            UserLibraryLink.objects.create(
                libraryObject=note,
                sourceKind="manual",
            )
    except LibraryNameConflict:
        delete_stored_object(metadata["storageKey"])
        return Status(409, {"error": "library_name_conflict"})
    except Exception:
        delete_stored_object(metadata["storageKey"])
        raise
    return Status(201, {"object": serialize_library_object(note)})


@router.get(
    "/library/{library_object_id}",
    auth=session_auth,
    response={200: LibraryObjectEnvelope} | COMMON_ERROR_RESPONSES,
)
def get_library_object(request, library_object_id: str):
    try:
        item = _library_object(request.user, library_object_id)
        if item.status == "deleted":
            raise UserLibraryObject.DoesNotExist
    except UserLibraryObject.DoesNotExist:
        return Status(404, {"error": "library_object_not_found"})
    return {"object": serialize_library_object(item)}


@router.patch(
    "/library/{library_object_id}",
    auth=session_auth,
    response={200: LibraryObjectEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_library_object(
    request,
    library_object_id: str,
    payload: UpdateLibraryObjectRequest,
):
    changed_fields = payload.model_fields_set
    if not changed_fields:
        return Status(400, {"error": "library_update_request_invalid"})
    try:
        with transaction.atomic():
            _lock_library_owner(request.user)
            item = _library_object(
                request.user,
                library_object_id,
                locked=True,
            )
            if item.status == "deleted":
                raise UserLibraryObject.DoesNotExist
            display_name = (
                _require_library_display_name(payload.display_name)
                if "display_name" in changed_fields
                else item.displayName
            )
            parent = (
                _library_parent(
                    request.user,
                    payload.parent_folder_id,
                    item.id,
                )
                if "parent_folder_id" in changed_fields
                else item.parentFolder
            )
            name_changed = display_name != item.displayName
            parent_changed = (parent.id if parent else None) != item.parentFolder_id
            if not name_changed and not parent_changed:
                return {"object": serialize_library_object(item)}
            _require_library_name_available(
                request.user,
                parent,
                display_name,
                exclude_id=item.id,
            )
            item.displayName = display_name
            item.parentFolder = parent
            item.save(update_fields=["displayName", "parentFolder", "updatedAt"])
    except UserLibraryObject.DoesNotExist:
        return Status(404, {"error": "library_object_not_found"})
    except LibraryNameConflict:
        return Status(409, {"error": "library_name_conflict"})
    except (ValueError, TypeError):
        return Status(400, {"error": "library_update_request_invalid"})
    return {"object": serialize_library_object(item)}


@router.delete(
    "/library/{library_object_id}",
    auth=session_auth,
    response={200: DeletedResponse} | COMMON_ERROR_RESPONSES,
)
def delete_library_object(request, library_object_id: str):
    try:
        with transaction.atomic():
            _lock_library_owner(request.user)
            item = _library_object(
                request.user,
                library_object_id,
                locked=True,
            )
            if item.status == "deleted":
                raise UserLibraryObject.DoesNotExist
            if (
                item.objectKind == "folder"
                and item.children.exclude(status="deleted").exists()
            ):
                return Status(409, {"error": "library_folder_not_empty"})
            tombstone_stored_object(item, deleted_by=request.user)
    except UserLibraryObject.DoesNotExist:
        return Status(404, {"error": "library_object_not_found"})
    return {"deleted": True}


@router.post(
    "/library/{library_object_id}/restore",
    auth=session_auth,
    response={200: LibraryObjectEnvelope} | COMMON_ERROR_RESPONSES,
)
def restore_library_object(request, library_object_id: str):
    try:
        with transaction.atomic():
            _lock_library_owner(request.user)
            item = _library_object(
                request.user,
                library_object_id,
                locked=True,
            )
            if item.status != "deleted":
                raise UserLibraryObject.DoesNotExist
            if not trash_is_restorable(item.deletedAt, item.purgedAt):
                return Status(410, {"error": "library_object_expired"})
            parent = item.parentFolder
            if parent is not None and (
                parent.owner_id != request.user.id
                or parent.objectKind != "folder"
                or parent.status != "ready"
            ):
                return Status(409, {"error": "library_restore_parent_unavailable"})
            storage_error = _prepare_library_storage_restore(item)
            if storage_error:
                return Status(409, {"error": storage_error})
            display_name = _available_numbered_library_name(
                request.user,
                parent,
                item.displayName,
                exclude_id=item.id,
            )
            item.displayName = display_name
            item.status = item.deletedFromStatus
            item.deletedAt = None
            item.deletedBy = None
            item.purgedAt = None
            item.deletedFromStatus = ""
            item.contentGeneration += 1
            item.save(
                update_fields=[
                    "displayName",
                    "status",
                    "deletedAt",
                    "deletedBy",
                    "purgedAt",
                    "deletedFromStatus",
                    "contentGeneration",
                    "updatedAt",
                ]
            )
    except UserLibraryObject.DoesNotExist:
        return Status(404, {"error": "library_object_not_found"})
    return {"object": serialize_library_object(item)}


@router.delete(
    "/library/{library_object_id}/trash",
    auth=session_auth,
    response={200: DeletedResponse} | COMMON_ERROR_RESPONSES,
)
def permanently_delete_library_object(request, library_object_id: str):
    try:
        with transaction.atomic():
            _lock_library_owner(request.user)
            item = _library_object(
                request.user,
                library_object_id,
                locked=True,
            )
            if item.status != "deleted":
                return Status(409, {"error": "library_object_not_deleted"})
            if item.purgedAt is not None:
                return Status(410, {"error": "library_object_purged"})
            now = timezone.now()
            item.purgedAt = now
            item.save(update_fields=["purgedAt", "updatedAt"])
            DerivedResource.objects.filter(
                ownerKind="userLibraryObject",
                ownerId=item.id,
            ).exclude(state="cleaned").update(tombstonedAt=trash_cutoff(now))
    except UserLibraryObject.DoesNotExist:
        return Status(404, {"error": "library_object_not_found"})
    return {"deleted": True}


@router.get(
    "/library/{library_object_id}/note",
    auth=session_auth,
    response={200: LibraryNoteEnvelope} | COMMON_ERROR_RESPONSES,
)
def get_library_note(request, library_object_id: str):
    try:
        item = _library_object(request.user, library_object_id)
        if item.objectKind != "note" or item.status != "ready":
            raise UserLibraryObject.DoesNotExist
        if not item.storageKey or not default_storage.exists(item.storageKey):
            return Status(409, {"error": "library_note_not_available"})
        try:
            with default_storage.open(item.storageKey, "rb") as handle:
                markdown = handle.read().decode("utf-8")
        except UnicodeDecodeError:
            return Status(409, {"error": "library_note_not_utf8"})
    except UserLibraryObject.DoesNotExist:
        return Status(404, {"error": "library_note_not_found"})
    return {"object": serialize_library_object(item), "markdown": markdown}


@router.put(
    "/library/{library_object_id}/note",
    auth=session_auth,
    response={200: LibraryNoteEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_library_note(
    request,
    library_object_id: str,
    payload: UpdateLibraryNoteRequest,
):
    metadata = None
    try:
        display_name = _require_library_display_name(payload.display_name)
        try:
            with transaction.atomic():
                _lock_library_owner(request.user)
                item = _library_object(
                    request.user,
                    library_object_id,
                    locked=True,
                )
                if item.objectKind != "note" or item.status != "ready":
                    raise UserLibraryObject.DoesNotExist
                _require_library_name_available(
                    request.user,
                    item.parentFolder,
                    display_name,
                    exclude_id=item.id,
                )
                metadata = store_bytes(
                    payload.markdown.encode("utf-8"),
                    f"users/{request.user.id}/library",
                    (
                        f"{display_name}.md"
                        if not display_name.lower().endswith(".md")
                        else display_name
                    ),
                    "text/markdown",
                )
                old_storage_key = item.storageKey
                item.displayName = display_name
                item.contentType = metadata["contentType"]
                item.sizeBytes = metadata["sizeBytes"]
                item.sha256 = metadata["sha256"]
                item.storageKey = metadata["storageKey"]
                item.contentGeneration += 1
                item.save(
                    update_fields=[
                        "displayName",
                        "contentType",
                        "sizeBytes",
                        "sha256",
                        "storageKey",
                        "contentGeneration",
                        "updatedAt",
                    ]
                )
                tombstone_superseded_derived_resources(item)
                delete_stored_object(old_storage_key)
        except Exception:
            if metadata is not None:
                delete_stored_object(metadata["storageKey"])
            raise
    except UserLibraryObject.DoesNotExist:
        return Status(404, {"error": "library_note_not_found"})
    except LibraryNameConflict:
        return Status(409, {"error": "library_name_conflict"})
    except (ValueError, TypeError):
        return Status(400, {"error": "library_note_request_invalid"})
    return {
        "object": serialize_library_object(item),
        "markdown": payload.markdown,
    }


@router.post(
    "/sessions/{session_id}/uploads",
    auth=usage_auth("attachments:write"),
    response={201: SessionUploadEnvelope} | COMMON_ERROR_RESPONSES,
)
def upload_session_library_objects(request, session_id: str):
    stored = []
    try:
        session = Session.objects.select_related("workspace").get(
            id=session_id,
            owner=request.user,
            status="active",
        )
        if workspace_membership_for(request.user, session.workspace_id) is None:
            raise Session.DoesNotExist
        uploads = _require_uploads(request, set())
        if any(upload.size > MAX_DIRECT_INPUT_BYTES for upload in uploads):
            raise ValueError("attachment_too_large")
        stored = _store_upload_batch(
            uploads,
            f"users/{request.user.id}/library",
        )
    except Session.DoesNotExist:
        return Status(404, {"error": "session_not_found"})
    except ValueError as error:
        return Status(400, {"error": str(error)})
    try:
        with transaction.atomic():
            membership = locked_workspace_membership_for(request.user, session.workspace_id)
            if membership is None:
                raise DelegationRejected("session_not_found", 404)
            require_request_delegation(request, "attachments:write", session_id=session_id, lock=True)
            session = Session.objects.select_for_update().filter(id=session_id, owner=request.user, status="active").first()
            if session is None:
                raise DelegationRejected("session_not_found", 404)
            library_objects = _create_uploaded_library_objects(request.user, stored)
            asset_links = []
            for library_object in library_objects:
                asset_link, _created = SessionAssetLink.objects.get_or_create(
                    workspace=session.workspace,
                    session=session,
                    userLibraryObject=library_object,
                    attachedBy=request.user,
                    **captured_input_fields(library_object),
                    defaults={
                        "capturedDisplayName": library_object.displayName,
                        "capturedContentType": library_object.contentType,
                    },
                )
                asset_links.append(asset_link)
    except Exception as database_error:
        try:
            _delete_stored_upload_batch(stored)
        except RuntimeError as cleanup_error:
            raise cleanup_error from database_error
        raise
    return Status(
        201,
        {
            "libraryObjects": [
                serialize_library_object(item) for item in library_objects
            ],
            "assets": [_serialize_session_asset_link(item) for item in asset_links],
        },
    )


@router.get(
    "/sessions/{session_id}/assets",
    auth=usage_auth("sessions:read"),
    response={200: SessionAssetsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_session_assets(request, session_id: str):
    session = _session_for_assets(request.user, session_id)
    if session is None:
        return Status(404, {"error": "session_not_found"})
    return {
        "assets": [
            _serialize_session_asset_link(link) for link in session.assetLinks.all()
        ]
    }


@router.post(
    "/sessions/{session_id}/assets",
    auth=session_auth,
    response={201: SessionAssetEnvelope} | COMMON_ERROR_RESPONSES,
)
def attach_session_asset(
    request,
    session_id: str,
    payload: AttachSessionAssetRequest,
):
    session = _session_for_assets(request.user, session_id)
    if session is None:
        return Status(404, {"error": "session_not_found"})
    try:
        asset_kind = payload.asset_kind
        asset_id = payload.asset_id.strip()
        field, asset = _resolve_linkable_asset(
            request.user,
            session,
            asset_kind,
            asset_id,
        )
        captured = captured_input_fields(asset)
    except (ValueError, KeyError) as error:
        if str(error) == "attachment_too_large":
            return Status(400, {"error": "attachment_too_large"})
        return Status(403, {"error": "asset_not_accessible"})
    link, _ = SessionAssetLink.objects.get_or_create(
        workspace=session.workspace,
        session=session,
        attachedBy=request.user,
        **captured,
        defaults={
            "capturedDisplayName": asset.displayName,
            "capturedContentType": (asset.contentType or "application/octet-stream"),
        },
        **{field: asset},
    )
    return Status(201, {"asset": _serialize_session_asset_link(link)})


@router.delete(
    "/sessions/{session_id}/assets",
    auth=usage_auth("attachments:write"),
    response={200: DeletedResponse} | COMMON_ERROR_RESPONSES,
)
def detach_session_asset(
    request,
    session_id: str,
    payload: DetachSessionAssetRequest,
):
    session = _session_for_assets(request.user, session_id)
    if session is None:
        return Status(404, {"error": "session_not_found"})
    with transaction.atomic():
        if locked_workspace_membership_for(request.user, session.workspace_id) is None:
            return Status(404, {"error": "session_not_found"})
        require_request_delegation(request, "attachments:write", session_id=session_id, lock=True)
        try:
            link = session.assetLinks.select_for_update().get(id=payload.asset_link_id.strip())
        except (ValueError, SessionAssetLink.DoesNotExist):
            return Status(404, {"error": "asset_link_not_found"})
        link.delete()
    return {"deleted": True}


def _require_uploads(request, allowed_form_fields: set[str]):
    uploads = request.FILES.getlist("files")
    if not uploads:
        raise ValueError("files_required")
    if set(request.FILES) != {"files"} or not set(request.POST).issubset(
        allowed_form_fields
    ):
        raise ValueError("upload_fields_invalid")
    if len(uploads) > MAX_UPLOAD_BATCH_FILES:
        raise ValueError("upload_batch_too_large")
    return uploads


def _store_upload_batch(uploads, area: str) -> list[dict]:
    total = 0
    for upload in uploads:
        size = 0
        for chunk in upload.chunks():
            size += len(chunk)
            total += len(chunk)
            if size > settings.UPLOAD_FILE_MAX_BYTES:
                raise ValueError("upload_file_too_large")
            if total > settings.UPLOAD_BODY_MAX_BYTES:
                raise ValueError("upload_body_too_large")
        upload.seek(0)
    stored = []
    try:
        for upload in uploads:
            stored.append(store_upload(upload, area))
    except Exception as upload_error:
        try:
            _delete_stored_upload_batch(stored)
        except RuntimeError as cleanup_error:
            raise cleanup_error from upload_error
        raise
    return stored


def _delete_stored_upload_batch(stored: list[dict]) -> None:
    failures = []
    for metadata in stored:
        try:
            delete_stored_object_for_gc(metadata["storageKey"])
        except Exception as error:
            failures.append(error)
    if failures:
        raise RuntimeError("upload_batch_cleanup_failed") from failures[0]


def _create_uploaded_library_object(
    user,
    metadata: dict,
    parent_folder=None,
) -> UserLibraryObject:
    _lock_library_owner(user)
    # Serialize hash lookup and creation for this owner, across all folders.
    # Reuse identity without renaming, moving, or reviving a deleted object.
    reusable = UserLibraryObject.objects.filter(
        owner=user, status="ready", sha256=metadata["sha256"],
    ).order_by("createdAt", "id").first()
    if reusable is not None:
        delete_stored_object_for_gc(metadata["storageKey"])
        return reusable
    display_name = metadata["displayName"]
    index = 0
    while True:
        candidate = _numbered_upload_name(display_name, index)
        collisions = list(
            UserLibraryObject.objects.filter(
                owner=user,
                parentFolder=parent_folder,
                displayName__iexact=candidate,
            )
            .exclude(status="deleted")
            .order_by("createdAt", "id")
        )
        if not collisions:
            break
        index += 1

    item = UserLibraryObject.objects.create(
        owner=user,
        displayName=candidate,
        objectKind=(
            "image" if metadata["contentType"].startswith("image/") else "file"
        ),
        contentType=metadata["contentType"],
        sizeBytes=metadata["sizeBytes"],
        sha256=metadata["sha256"],
        storageKey=metadata["storageKey"],
        status="ready",
        contentGeneration=1,
        parentFolder=parent_folder,
    )
    UserLibraryLink.objects.create(libraryObject=item, sourceKind="upload")
    return item


def _create_uploaded_library_objects(
    user,
    stored: list[dict],
    parent_folder=None,
) -> list[UserLibraryObject]:
    items = []
    seen = set()
    for metadata in stored:
        item = _create_uploaded_library_object(user, metadata, parent_folder)
        if item.id not in seen:
            items.append(item)
            seen.add(item.id)
    return items


def _numbered_upload_name(display_name: str, index: int) -> str:
    if not isinstance(display_name, str) or not display_name or len(display_name) > 255:
        raise ValueError("upload_filename_invalid")
    if index == 0:
        return display_name
    stem, extension = os.path.splitext(display_name)
    suffix = f"({index})"
    stem_limit = 255 - len(extension) - len(suffix)
    if stem_limit < 1:
        raise ValueError("upload_filename_invalid")
    return f"{stem[:stem_limit]}{suffix}{extension}"


def _lock_library_owner(user) -> None:
    get_user_model().objects.select_for_update().only("pk").get(pk=user.pk)


def _library_name_taken(
    user,
    parent_folder,
    display_name: str,
    *,
    exclude_id: str = "",
) -> bool:
    query = UserLibraryObject.objects.filter(
        owner=user,
        parentFolder=parent_folder,
        displayName__iexact=display_name,
    ).exclude(status="deleted")
    if exclude_id:
        query = query.exclude(id=exclude_id)
    return query.exists()


def _require_library_name_available(
    user,
    parent_folder,
    display_name: str,
    *,
    exclude_id: str = "",
) -> None:
    if _library_name_taken(
        user,
        parent_folder,
        display_name,
        exclude_id=exclude_id,
    ):
        raise LibraryNameConflict("library_name_conflict")


def _available_numbered_library_name(
    user,
    parent_folder,
    display_name: str,
    *,
    exclude_id: str = "",
) -> str:
    index = 0
    while True:
        candidate = _numbered_upload_name(display_name, index)
        if not _library_name_taken(
            user,
            parent_folder,
            candidate,
            exclude_id=exclude_id,
        ):
            return candidate
        index += 1


def _prepare_library_storage_restore(item: UserLibraryObject) -> str:
    if not item.storageKey:
        return ""
    resources = list(
        DerivedResource.objects.select_for_update().filter(
            ownerKind="userLibraryObject",
            ownerId=item.id,
            resourceKind="storageObject",
            resourceKey=item.storageKey,
        )
    )
    if any(resource.state == "cleaning" for resource in resources):
        return "library_object_restore_busy"
    if any(resource.state == "cleaned" for resource in resources) or not default_storage.exists(
        item.storageKey
    ):
        return "library_object_not_restorable"
    DerivedResource.objects.filter(id__in=[resource.id for resource in resources]).update(
        state="active",
        tombstonedAt=None,
        leaseOwner="",
        leaseExpiresAt=None,
        lastFailure="",
        cleanedAt=None,
        updatedAt=timezone.now(),
    )
    return ""


def _library_parent(user, parent_folder_id: str, moving_object_id: str = ""):
    parent_folder_id = str(parent_folder_id or "").strip()
    if not parent_folder_id:
        return None
    parent = UserLibraryObject.objects.select_related("parentFolder").get(
        id=parent_folder_id,
        owner=user,
        objectKind="folder",
        status="ready",
    )
    visited = set()
    while parent is not None:
        if parent.id in visited or parent.id == moving_object_id:
            raise ValueError("library_folder_cycle")
        visited.add(parent.id)
        parent = parent.parentFolder
    return UserLibraryObject.objects.get(
        id=parent_folder_id,
        owner=user,
        objectKind="folder",
        status="ready",
    )


def _library_object(user, library_object_id: str, *, locked: bool = False):
    query = UserLibraryObject.objects
    if locked:
        query = query.select_for_update()
    else:
        query = query.select_related("parentFolder")
    return query.get(id=library_object_id, owner=user)


def _require_library_display_name(value) -> str:
    if not isinstance(value, str):
        raise ValueError("library_display_name_invalid")
    normalized = unicodedata.normalize("NFC", value).strip()
    if not normalized or len(normalized) > 255:
        raise ValueError("library_display_name_invalid")
    safe_filename(normalized)
    return normalized


def _session_for_assets(user, session_id: str):
    session = Session.objects.select_related("workspace").filter(
        id=session_id,
        owner=user,
    ).first()
    if session is None or workspace_membership_for(user, session.workspace_id) is None:
        return None
    return session


def _resolve_linkable_asset(user, session, asset_kind, asset_id):
    try:
        if asset_kind == "sourceObject":
            item = SourceObject.objects.select_related("source").get(
                id=asset_id,
                workspace=session.workspace,
                objectType="file",
                status="ready",
            )
            if not source_object_is_granted(user, item):
                raise ValueError("source_object_not_granted")
            return "sourceObject", item
        if asset_kind == "userLibraryObject":
            return "userLibraryObject", UserLibraryObject.objects.get(
                id=asset_id,
                owner=user,
                status="ready",
                objectKind__in={"file", "image", "note", "savedArtifact"},
            )
        if asset_kind == "artifact":
            return "artifact", Artifact.objects.get(
                id=asset_id,
                session=session,
                status="published",
            )
    except (
        SourceObject.DoesNotExist,
        UserLibraryObject.DoesNotExist,
        Artifact.DoesNotExist,
    ) as error:
        raise ValueError("asset_not_accessible") from error
    raise ValueError("unsupported_asset_kind")


def _serialize_session_asset_link(link: SessionAssetLink) -> dict:
    if link.sourceObject:
        return {
            "id": link.id,
            "assetKind": "sourceObject",
            "displayName": link.capturedDisplayName,
            "contentType": link.capturedContentType,
            "asset": serialize_source_object(link.sourceObject),
        }
    if link.userLibraryObject:
        return {
            "id": link.id,
            "assetKind": "userLibraryObject",
            "displayName": link.capturedDisplayName,
            "contentType": link.capturedContentType,
            "asset": serialize_library_object(link.userLibraryObject),
        }
    return {
        "id": link.id,
        "assetKind": "artifact",
        "displayName": link.capturedDisplayName,
        "contentType": link.capturedContentType,
        "asset": serialize_artifact(link.artifact),
    }
