import hashlib
import os
import re
import stat
import unicodedata
import uuid

from django.core.files.base import ContentFile
from django.core.files.storage import default_storage
from django.utils import timezone

from .models import (
    Artifact,
    DerivedResource,
    SessionAssetLink,
    SourceObject,
    UserLibraryObject,
)
from .workspace_access import (
    source_access_is_at_least,
    workspace_membership_for,
)


MAX_DIRECT_INPUT_BYTES = 64 * 1024 * 1024


def safe_filename(value: str) -> str:
    normalized = unicodedata.normalize("NFC", value)
    if any(unicodedata.category(character).startswith("C") for character in normalized):
        raise ValueError("filename contains a control character")
    name = normalized.strip()
    if not name or "/" in name or "\\" in name or name in {".", ".."}:
        raise ValueError("filename must be a basename")
    safe = re.sub(r"[^\w.-]+", "_", name, flags=re.UNICODE).strip("._")
    if not safe:
        raise ValueError("filename has no safe characters")
    return safe


def allocated_virtual_paths(agent_run) -> dict[str, str]:
    declared = agent_run.authorization.payload["assetRefs"]
    candidates = {
        item["inputRef"]: _virtual_filename(item["displayName"]) for item in declared
    }
    groups = {}
    for input_ref, candidate in candidates.items():
        groups.setdefault(candidate.casefold(), []).append(input_ref)
    allocated = {
        input_ref: (
            _virtual_filename(
                next(
                    item["displayName"]
                    for item in declared
                    if item["inputRef"] == input_ref
                ),
                input_ref,
            )
            if len(groups[candidate.casefold()]) > 1
            else candidate
        )
        for input_ref, candidate in candidates.items()
    }
    if len({path.casefold() for path in allocated.values()}) != len(allocated):
        raise ValueError("AgentRun input virtualPath collision")
    return allocated


def _virtual_filename(display_name: str, input_ref: str | None = None) -> str:
    filename = safe_filename(display_name)
    stem, extension = os.path.splitext(filename)
    suffix = f"_{safe_filename(input_ref)}" if input_ref else ""
    budget = 240 - len(f"{suffix}{extension}".encode("utf-8"))
    while stem and len(stem.encode("utf-8")) > budget:
        stem = stem[:-1]
    if not stem or budget <= 0:
        raise ValueError("filename cannot fit a safe virtualPath")
    return f"{stem}{suffix}{extension}"


def normalize_display_path(value: str) -> str:
    path = value.strip().strip("/")
    if (
        not path
        or "\\" in path
        or any(part in {"", ".", ".."} for part in path.split("/"))
    ):
        raise ValueError("displayPath must be a relative POSIX path")
    return path


def source_object_is_granted(user, source_object: SourceObject) -> bool:
    if source_object.source.status != "ready":
        return False
    membership = workspace_membership_for(user, source_object.workspace_id)
    return source_access_is_at_least(
        membership,
        source_object.source,
        "read",
    ) if membership else False


def granted_source_objects(user, source):
    if source.status != "ready":
        return []
    membership = workspace_membership_for(user, source.workspace_id)
    if membership is None or not source_access_is_at_least(membership, source, "read"):
        return []
    return list(source.sourceObjects.exclude(status="deleted").order_by("displayPath"))


def store_upload(upload, area: str) -> dict:
    display_name = unicodedata.normalize("NFC", upload.name)
    filename = safe_filename(display_name)
    digest = hashlib.sha256()
    size_bytes = 0
    for chunk in upload.chunks():
        digest.update(chunk)
        size_bytes += len(chunk)
    upload.seek(0)
    requested_storage_key = f"{area}/{uuid.uuid4().hex}/{filename}"
    try:
        storage_key = default_storage.save(requested_storage_key, upload)
    except Exception:
        try:
            delete_stored_object_for_gc(requested_storage_key)
        except Exception as cleanup_error:
            raise RuntimeError("upload_storage_cleanup_failed") from cleanup_error
        raise
    return {
        "displayName": display_name,
        "safeFilename": filename,
        "contentType": upload.content_type or "application/octet-stream",
        "sizeBytes": size_bytes,
        "sha256": f"sha256:{digest.hexdigest()}",
        "storageKey": storage_key,
    }


def store_bytes(content: bytes, area: str, filename: str, content_type: str) -> dict:
    display_name = unicodedata.normalize("NFC", filename)
    safe = safe_filename(display_name)
    storage_key = default_storage.save(
        f"{area}/{uuid.uuid4().hex}/{safe}", ContentFile(content)
    )
    return {
        "displayName": display_name,
        "safeFilename": safe,
        "contentType": content_type,
        "sizeBytes": len(content),
        "sha256": f"sha256:{hashlib.sha256(content).hexdigest()}",
        "storageKey": storage_key,
    }


def store_immutable_bytes_at_key(content: bytes, storage_key: str) -> dict:
    if not storage_key or storage_key.startswith("/") or ".." in storage_key.split("/"):
        raise ValueError("immutable storage key is invalid")
    expected_sha256 = f"sha256:{hashlib.sha256(content).hexdigest()}"
    try:
        final_path = default_storage.path(storage_key)
    except Exception as error:
        raise RuntimeError("immutable_storage_path_unavailable") from error
    parent = os.path.dirname(final_path)
    os.makedirs(parent, exist_ok=True)
    temporary_path = os.path.join(parent, f".centaeris-immutable-{uuid.uuid4().hex}.tmp")
    descriptor = -1
    try:
        descriptor = os.open(temporary_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as temporary:
            descriptor = -1
            temporary.write(content)
            temporary.flush()
            os.fsync(temporary.fileno())
        try:
            os.link(temporary_path, final_path)
        except FileExistsError:
            if _read_exact_immutable_file(final_path, len(content)) != content:
                raise RuntimeError("immutable_storage_identity_conflict")
            return {
                "sizeBytes": len(content),
                "sha256": expected_sha256,
                "storageKey": storage_key,
                "created": False,
            }
        os.unlink(temporary_path)
        temporary_path = ""
        _sync_directory(parent)
        if _read_exact_immutable_file(final_path, len(content)) != content:
            raise RuntimeError("immutable_storage_integrity_mismatch")
        return {
            "sizeBytes": len(content),
            "sha256": expected_sha256,
            "storageKey": storage_key,
            "created": True,
        }
    except RuntimeError:
        raise
    except OSError as error:
        raise RuntimeError("immutable_storage_publish_failed") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        if temporary_path and os.path.exists(temporary_path):
            _remove_immutable_path(temporary_path)


def _read_exact_immutable_file(path: str, expected_size: int) -> bytes:
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise RuntimeError("immutable_storage_read_failed") from error
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size != expected_size:
            raise RuntimeError("immutable_storage_identity_conflict")
        with os.fdopen(descriptor, "rb", closefd=False) as stored:
            content = stored.read(expected_size + 1)
        after = os.fstat(descriptor)
        if before.st_size != after.st_size or len(content) != expected_size:
            raise RuntimeError("immutable_storage_identity_conflict")
        return content
    finally:
        os.close(descriptor)


def _remove_immutable_path(path: str) -> None:
    try:
        os.unlink(path)
    except FileNotFoundError:
        return
    except OSError as error:
        raise RuntimeError("immutable_storage_cleanup_failed") from error


def _sync_directory(path: str) -> None:
    if os.name == "nt":
        return
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def delete_stored_object(storage_key: str) -> None:
    if storage_key:
        default_storage.delete(storage_key)


def delete_stored_object_for_gc(storage_key: str) -> None:
    if not storage_key:
        raise ValueError("GC storage resource key is empty")
    if not default_storage.exists(storage_key):
        return
    default_storage.delete(storage_key)
    if default_storage.exists(storage_key):
        raise OSError("GC storage object remains after delete")


def stored_object_identity(owner) -> tuple[str, str]:
    if isinstance(owner, SourceObject):
        return "sourceObject", owner.id
    if isinstance(owner, UserLibraryObject):
        return "userLibraryObject", owner.id
    if isinstance(owner, Artifact):
        return "artifact", owner.id
    raise ValueError("unsupported stored object owner")


def captured_input_fields(owner) -> dict:
    owner_kind, owner_id = stored_object_identity(owner)
    generation = owner.contentGeneration
    size_bytes = owner.sizeBytes
    sha256 = owner.sha256
    if (
        not isinstance(generation, int)
        or generation <= 0
        or not isinstance(size_bytes, int)
        or size_bytes < 0
        or not isinstance(sha256, str)
        or not re.fullmatch(r"sha256:[0-9a-f]{64}", sha256)
    ):
        raise ValueError("asset_input_identity_invalid")
    if size_bytes > MAX_DIRECT_INPUT_BYTES:
        raise ValueError("attachment_too_large")
    return {
        "capturedOwnerKind": owner_kind,
        "capturedOwnerId": owner_id,
        "capturedContentGeneration": generation,
        "capturedSizeBytes": size_bytes,
        "capturedSha256": sha256,
    }


def register_derived_resource(
    owner, resource_kind: str, resource_key: str
) -> DerivedResource:
    owner_kind, owner_id = stored_object_identity(owner)
    state = "pending" if owner.status == "deleted" else "active"
    resource, _created = DerivedResource.objects.get_or_create(
        ownerKind=owner_kind,
        ownerId=owner_id,
        ownerContentGeneration=owner.contentGeneration,
        deletionGeneration=owner.deletionGeneration,
        resourceKind=resource_kind,
        resourceKey=resource_key,
        defaults={
            "state": state,
            "tombstonedAt": owner.deletedAt if state == "pending" else None,
        },
    )
    if resource.state == "active" and state == "pending":
        resource.state = "pending"
        resource.tombstonedAt = owner.deletedAt
        resource.save(update_fields=["state", "tombstonedAt", "updatedAt"])
    return resource


def tombstone_superseded_derived_resources(owner) -> None:
    owner_kind, owner_id = stored_object_identity(owner)
    if owner.contentGeneration <= 0:
        raise ValueError("asset_input_identity_invalid")
    DerivedResource.objects.filter(
        ownerKind=owner_kind,
        ownerId=owner_id,
        ownerContentGeneration__lt=owner.contentGeneration,
        state="active",
    ).update(
        state="pending",
        tombstonedAt=timezone.now(),
        leaseOwner="",
        leaseExpiresAt=None,
        lastFailure="",
    )


def tombstone_stored_object(owner, deleted_at=None, deleted_by=None) -> None:
    if owner.status == "deleted":
        raise ValueError("stored object is already deleted")
    deleted_from_status = owner.status
    owner.status = "deleted"
    owner.deletedAt = deleted_at or timezone.now()
    owner.deletionGeneration += 1
    update_fields = ["status", "deletedAt", "deletionGeneration"]
    if deleted_by is not None and hasattr(owner, "deletedBy"):
        owner.deletedBy = deleted_by
        update_fields.append("deletedBy")
    if hasattr(owner, "deletedFromStatus"):
        owner.deletedFromStatus = deleted_from_status
        update_fields.append("deletedFromStatus")
    if any(field.name == "updatedAt" for field in owner._meta.fields):
        update_fields.append("updatedAt")
    owner.save(update_fields=update_fields)
    owner_kind, owner_id = stored_object_identity(owner)
    DerivedResource.objects.filter(
        ownerKind=owner_kind,
        ownerId=owner_id,
    ).exclude(state="cleaned").update(
        deletionGeneration=owner.deletionGeneration,
        state="pending",
        tombstonedAt=owner.deletedAt,
        leaseOwner="",
        leaseExpiresAt=None,
        lastFailure="",
    )
    if owner.storageKey:
        register_derived_resource(owner, "storageObject", owner.storageKey)


class DeferredInputResolutionError(RuntimeError):
    def __init__(self, error_code: str):
        super().__init__(error_code)
        self.errorCode = error_code


def deferred_input_refs(agent_run) -> list[dict]:
    references = []
    links = SessionAssetLink.objects.filter(session=agent_run.session).select_related(
        "sourceObject__source",
        "userLibraryObject",
        "artifact",
    )
    for link in links:
        if not (link.sourceObject or link.userLibraryObject or link.artifact):
            raise ValueError(f"session asset link has no owner: {link.id}")
        references.append(
            {
                "schema": "runtime.declared_input.v1",
                "inputRef": link.id,
                "displayName": link.capturedDisplayName,
                "contentType": link.capturedContentType,
                "inputIdentity": {
                    "ownerKind": link.capturedOwnerKind,
                    "ownerId": link.capturedOwnerId,
                    "generation": link.capturedContentGeneration,
                    "sha256": link.capturedSha256,
                },
                "sizeBytes": link.capturedSizeBytes,
            }
        )
    return sorted(references, key=lambda item: item["inputRef"])


def resolved_input_for_link(agent_run, link: SessionAssetLink, virtual_path: str):
    if link.sourceObject:
        owner = link.sourceObject
        if owner.source.status == "deleted":
            raise DeferredInputResolutionError("source_deleted")
        if owner.status == "deleted":
            raise DeferredInputResolutionError("asset_removed")
        if owner.status != "ready" or not source_object_is_granted(agent_run.user, owner):
            raise DeferredInputResolutionError("access_revoked")
        _require_captured_identity(link, owner)
        return resolved_input(
            link.id,
            link.capturedDisplayName,
            link.capturedContentType,
            link.capturedOwnerId,
            link.capturedOwnerKind,
            virtual_path,
            link.capturedSizeBytes,
            link.capturedSha256,
            str(link.capturedContentGeneration),
            "workspaceSource",
        )
    if link.userLibraryObject:
        owner = link.userLibraryObject
        if owner.status == "deleted":
            raise DeferredInputResolutionError("asset_removed")
        if owner.owner_id != agent_run.user_id or owner.status != "ready":
            raise DeferredInputResolutionError("access_revoked")
        _require_captured_identity(link, owner)
        return resolved_input(
            link.id,
            link.capturedDisplayName,
            link.capturedContentType,
            link.capturedOwnerId,
            link.capturedOwnerKind,
            virtual_path,
            link.capturedSizeBytes,
            link.capturedSha256,
            str(link.capturedContentGeneration),
            "userProvided",
        )
    owner = link.artifact
    if owner is None:
        raise DeferredInputResolutionError("asset_unavailable")
    if owner.status == "deleted":
        raise DeferredInputResolutionError("asset_removed")
    if owner.session_id != agent_run.session_id or owner.status != "published":
        raise DeferredInputResolutionError("access_revoked")
    _require_captured_identity(link, owner)
    return resolved_input(
        link.id,
        link.capturedDisplayName,
        link.capturedContentType,
        link.capturedOwnerId,
        link.capturedOwnerKind,
        virtual_path,
        link.capturedSizeBytes,
        link.capturedSha256,
        str(link.capturedContentGeneration),
        "generatedArtifact",
    )


def _require_captured_identity(link: SessionAssetLink, owner) -> None:
    owner_kind, owner_id = stored_object_identity(owner)
    if (
        link.capturedOwnerKind != owner_kind
        or link.capturedOwnerId != owner_id
        or link.capturedContentGeneration != owner.contentGeneration
        or link.capturedSizeBytes != owner.sizeBytes
        or link.capturedSha256 != owner.sha256
    ):
        raise DeferredInputResolutionError("stale_generation")


def resolved_input(
    input_ref,
    display_name,
    content_type,
    object_ref,
    owner_kind,
    virtual_path,
    size_bytes,
    sha256,
    source_version,
    evidence_kind,
):
    return {
        "schema": "runtime.resolved_input.v1",
        "inputRef": input_ref,
        "objectRef": object_ref,
        "ownerKind": owner_kind,
        "virtualPath": virtual_path,
        "displayName": display_name,
        "contentType": content_type,
        "sizeBytes": size_bytes or 0,
        "sha256": sha256,
        "sourceVersion": source_version,
        "evidenceKind": evidence_kind,
        "citationAllowed": owner_kind in {"sourceObject", "userLibraryObject"},
    }
