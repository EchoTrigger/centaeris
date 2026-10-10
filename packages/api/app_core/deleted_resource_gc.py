import uuid
import logging
from dataclasses import dataclass
from datetime import timedelta

from django.core.files.storage import default_storage
from django.db import connection, models, transaction
from django.utils import timezone

from .assets import delete_stored_object_for_gc
from .assets import tombstone_stored_object
from .transcript_output import purge_session_outputs
from .storage_gc_retry import clean_storage_key, cleanup_attempt_limit, next_cleanup_time
from .resource_commit import require_durable_commit
from .models import (
    Agent,
    Artifact,
    DerivedRepresentation,
    DerivedResource,
    Session,
    Source,
    SourceObject,
    UserLibraryObject,
)


logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class DeletedResourceGcReport:
    planned: list[DerivedResource]
    cleaned: list[DerivedResource]
    blocked: list[DerivedResource]
    failures: list[DerivedResource]


@dataclass(frozen=True)
class OrphanedLibraryGcReport:
    planned: list[str]
    cleaned: list[str]
    failures: list[str]


@dataclass(frozen=True)
class TrashExpirationReport:
    agents: int
    sessions: int
    sources: int
    library_objects: int


def expire_trash(cutoff, dry_run: bool) -> TrashExpirationReport:
    agent_ids = list(Agent.objects.filter(
        status="deleted", purgedAt__isnull=True, deletedAt__lte=cutoff,
    ).values_list("id", flat=True))
    session_ids = list(Session.objects.filter(
        status="deleted", purgedAt__isnull=True, deletedAt__lte=cutoff,
    ).values_list("id", flat=True))
    library_ids = list(UserLibraryObject.objects.filter(
        status="deleted", purgedAt__isnull=True, deletedAt__lte=cutoff,
    ).values_list("id", flat=True))
    sources = list(Source.objects.filter(status="deleted").filter(
        models.Q(purgedAt__isnull=True, deletedAt__lte=cutoff)
        | models.Q(
            purgedAt__isnull=False,
            sourceObjects__status__in=("processing", "ready", "failed"),
        )
    ).distinct())
    source_ids = [source.id for source in sources if source.purgedAt is None]
    report = TrashExpirationReport(
        agents=len(agent_ids),
        sessions=len(session_ids),
        sources=len(source_ids),
        library_objects=len(library_ids),
    )
    if dry_run:
        return report
    now = timezone.now()
    Agent.objects.filter(id__in=agent_ids, purgedAt__isnull=True).update(purgedAt=now)
    Session.objects.filter(id__in=session_ids, purgedAt__isnull=True).update(purgedAt=now)
    # Includes already-purged owners, so an interrupted GC pass is repairable.
    owners = Session.objects.filter(
        models.Q(purgedAt__isnull=False) | models.Q(agent__purgedAt__isnull=False),
        events__transcriptoutputcapture__purgedAt__isnull=True,
        events__transcriptoutputcapture__isnull=False,
    ).values_list("id", flat=True).distinct()
    for session_id in owners.iterator():
        with transaction.atomic():
            session = Session.objects.select_for_update().get(id=session_id)
            purge_session_outputs(session, now)
    UserLibraryObject.objects.filter(id__in=library_ids, purgedAt__isnull=True).update(purgedAt=now)
    for source in sources:
        with transaction.atomic():
            locked = Source.objects.select_for_update().get(id=source.id)
            if locked.status != "deleted":
                continue
            forced = locked.purgedAt is not None
            for item in locked.sourceObjects.select_for_update().exclude(status="deleted"):
                tombstone_stored_object(item, locked.deletedAt)
            if forced:
                DerivedResource.objects.filter(
                    ownerKind="sourceObject",
                    ownerId__in=locked.sourceObjects.values_list("id", flat=True),
                ).exclude(state="cleaned").update(tombstonedAt=cutoff)
            elif locked.deletedAt <= cutoff:
                locked.purgedAt = now
                locked.save(update_fields=["purgedAt", "updatedAt"])
    return report


def collect_orphaned_library_gc(cutoff, dry_run: bool) -> OrphanedLibraryGcReport:
    """Reclaim library bytes that have no database owner.

    A library copy is written to storage inside the publish transaction before
    the UserLibraryObject row commits; a crash in that window leaves orphaned
    bytes under users/<uid>/library/<libId>/. Only keys older than the cutoff
    are reclaimed so an in-flight publish cannot be truncated, and keys still
    referenced by any UserLibraryObject (including tombstoned ones) are never
    touched - those belong to the DerivedResource GC path.
    """
    planned = []
    cleaned = []
    failures = []
    for key in _scan_orphaned_library_keys(cutoff):
        if dry_run:
            planned.append(key)
            continue
        try:
            result = clean_storage_key(key, "orphanedLibrary", delete_stored_object_for_gc)
            if result.state == "cleaned":
                cleaned.append(key)
            elif result.state == "failed":
                failures.append(f"{key}: {result.failure}")
        except Exception as error:
            failures.append(f"{key}: {error}")
    return OrphanedLibraryGcReport(planned, cleaned, failures)


def _scan_orphaned_library_keys(cutoff) -> list[str]:
    try:
        user_dirs, _ = default_storage.listdir("users")
    except FileNotFoundError:
        return []
    orphaned = []
    for user_dir in sorted(user_dirs):
        library_prefix = f"users/{user_dir}/library"
        try:
            library_dirs, _ = default_storage.listdir(library_prefix)
        except FileNotFoundError:
            continue
        for library_dir in sorted(library_dirs):
            prefix = f"{library_prefix}/{library_dir}"
            try:
                _, files = default_storage.listdir(prefix)
            except FileNotFoundError:
                continue
            for filename in sorted(files):
                key = f"{prefix}/{filename}"
                if UserLibraryObject.objects.filter(storageKey=key).exists():
                    continue
                try:
                    modified = default_storage.get_modified_time(key)
                except (FileNotFoundError, OSError):
                    continue
                if modified <= cutoff:
                    orphaned.append(key)
    return orphaned


def collect_deleted_resource_gc(cutoff, dry_run: bool) -> DeletedResourceGcReport:
    lease_owner = f"gc_{uuid.uuid4().hex}"
    due = sorted(
        DerivedResource.objects.filter(tombstonedAt__lte=cutoff).exclude(state="cleaned"),
        key=lambda resource: (
            resource.ownerKind,
            resource.ownerId,
            resource.deletionGeneration,
            resource.resourceKind == "storageObject",
            resource.id,
        ),
    )
    planned = []
    cleaned = []
    blocked = []
    failures = []
    for resource in due:
        if resource.resourceKind == "storageObject" and _has_uncleaned_derived_resources(resource):
            blocked.append(resource)
            continue
        if dry_run:
            planned.append(resource)
            continue
        claimed = _claim_resource(resource.id, lease_owner)
        if claimed is None:
            blocked.append(resource)
            continue
        try:
            _verify_deleted_owner(claimed)
            _clean_resource(claimed)
        except Exception as error:
            _mark_failed(claimed.id, lease_owner, str(error))
            failures.append(claimed)
            continue
        _mark_cleaned(claimed.id, lease_owner)
        _delete_stale_representation_if_fully_cleaned(claimed)
        cleaned.append(claimed)
    return DeletedResourceGcReport(planned, cleaned, blocked, failures)


def _has_uncleaned_derived_resources(resource: DerivedResource) -> bool:
    return DerivedResource.objects.filter(
        ownerKind=resource.ownerKind,
        ownerId=resource.ownerId,
        deletionGeneration=resource.deletionGeneration,
    ).exclude(resourceKind="storageObject").exclude(state="cleaned").exists()


def _claim_resource(resource_id: str, lease_owner: str) -> DerivedResource | None:
    now = timezone.now()
    with transaction.atomic():
        require_durable_commit(connection, RuntimeError("storage_cleanup_durability_required"))
        resource = DerivedResource.objects.select_for_update().get(id=resource_id)
        if resource.state == "cleaned":
            return None
        if resource.state == "cleaning" and resource.leaseExpiresAt and resource.leaseExpiresAt > now:
            return None
        if resource.state not in {"pending", "failed", "cleaning"}:
            return None
        if resource.nextCleanupAt and resource.nextCleanupAt > now:
            return None
        if resource.cleanupAttempts >= cleanup_attempt_limit():
            resource.state = "quarantined"
            resource.quarantinedAt = now
            resource.leaseOwner = ""
            resource.leaseExpiresAt = None
            resource.save(update_fields=["state", "quarantinedAt", "leaseOwner", "leaseExpiresAt", "updatedAt"])
            logger.error("Derived resource cleanup quarantined: resourceId=%s attempts=%s", resource.pk, resource.cleanupAttempts)
            return None
        resource.state = "cleaning"
        resource.leaseOwner = lease_owner
        resource.leaseExpiresAt = now + timedelta(minutes=5)
        resource.cleanupAttempts += 1
        resource.lastFailure = ""
        resource.nextCleanupAt = None
        resource.save(update_fields=[
            "state", "leaseOwner", "leaseExpiresAt", "cleanupAttempts", "lastFailure", "nextCleanupAt", "updatedAt",
        ])
        return resource


def _verify_deleted_owner(resource: DerivedResource) -> None:
    model = {
        "sourceObject": SourceObject,
        "userLibraryObject": UserLibraryObject,
        "artifact": Artifact,
    }[resource.ownerKind]
    fields = ["status", "contentGeneration", "deletionGeneration", "deletedAt"]
    if resource.ownerKind == "userLibraryObject":
        fields.append("purgedAt")
    owner = model.objects.filter(id=resource.ownerId).only(*fields).first()
    if owner is None:
        raise ValueError("derived resource owner is missing")
    deleted = (
        owner.status == "deleted"
        and owner.deletionGeneration == resource.deletionGeneration
        and owner.deletedAt is not None
        and (
            resource.ownerKind != "userLibraryObject"
            or owner.purgedAt is not None
        )
    )
    stale = (
        resource.ownerContentGeneration > 0
        and owner.contentGeneration > resource.ownerContentGeneration
    )
    if not (deleted or stale):
        raise ValueError("derived resource owner generation is no longer reclaimable")


def _clean_resource(resource: DerivedResource) -> None:
    if resource.resourceKind == "storageObject":
        delete_stored_object_for_gc(resource.resourceKey)
        return
    raise ValueError("unsupported derived resource kind")


def _mark_cleaned(resource_id: str, lease_owner: str) -> None:
    now = timezone.now()
    updated = DerivedResource.objects.filter(
        id=resource_id,
        state="cleaning",
        leaseOwner=lease_owner,
    ).update(
        state="cleaned",
        leaseOwner="",
        leaseExpiresAt=None,
        cleanedAt=now,
        nextCleanupAt=None,
        updatedAt=now,
    )
    if updated != 1:
        raise RuntimeError("derived resource GC lease was lost before completion")


def _mark_failed(resource_id: str, lease_owner: str, reason: str) -> None:
    now = timezone.now()
    with transaction.atomic():
        resource = DerivedResource.objects.select_for_update().filter(
            id=resource_id, state="cleaning", leaseOwner=lease_owner,
        ).first()
        if resource is None:
            raise RuntimeError("derived resource GC lease was lost before failure recording")
        exhausted = resource.cleanupAttempts >= cleanup_attempt_limit()
        resource.state = "quarantined" if exhausted else "failed"
        resource.leaseOwner = ""
        resource.leaseExpiresAt = None
        resource.lastFailure = reason[:4000]
        resource.nextCleanupAt = None if exhausted else next_cleanup_time(resource.cleanupAttempts, now)
        resource.quarantinedAt = now if exhausted else None
        resource.save(update_fields=[
            "state", "leaseOwner", "leaseExpiresAt", "lastFailure", "nextCleanupAt", "quarantinedAt", "updatedAt",
        ])
        if exhausted:
            logger.error("Derived resource cleanup quarantined: resourceId=%s attempts=%s", resource.pk, resource.cleanupAttempts)


def _delete_stale_representation_if_fully_cleaned(resource: DerivedResource) -> None:
    if DerivedResource.objects.filter(
        ownerKind=resource.ownerKind,
        ownerId=resource.ownerId,
        ownerContentGeneration=resource.ownerContentGeneration,
    ).exclude(state="cleaned").exists():
        return
    DerivedRepresentation.objects.filter(
        ownerKind=resource.ownerKind,
        ownerId=resource.ownerId,
        ownerContentGeneration=resource.ownerContentGeneration,
    ).delete()
