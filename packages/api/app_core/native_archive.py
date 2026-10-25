"""Read-only boundaries for data retained from an earlier hosted deployment.

This module owns no provider SDK, producer, resumer, or retirement operation.
Existing references remain authoritative until a separately reviewed migration.
"""

from django.db import connection
from .models import AgentRun, SessionEnvironment, WorkspaceSnapshotPublication


class NativeArchiveBlocked(ValueError):
    pass


def require_native_environment_profile(profile):
    if profile is None:
        return
    from .environment_profile import validate_environment_profile

    try:
        validate_environment_profile(profile)
    except (TypeError, ValueError, KeyError):
        raise NativeArchiveBlocked(
            "environment_profile_unsupported_create_new_environment"
        ) from None


def require_native_archive_activation():
    # Pending/staged (or unknown) responsibility cannot be assumed settled merely
    # because this native service does not contain its previous execution path.
    if WorkspaceSnapshotPublication.objects.exclude(state="committed").exists():
        raise NativeArchiveBlocked(
            "native_activation_blocked_unsettled_snapshot_publications"
        )

    for environment in SessionEnvironment.objects.exclude(
        environment_profile=None
    ).iterator():
        try:
            require_native_environment_profile(environment.environment_profile)
        except NativeArchiveBlocked:
            if (
                not environment.controller_released
                or AgentRun.objects.filter(
                    environment=environment, status__in=("queued", "running")
                ).exists()
            ):
                raise NativeArchiveBlocked(
                    "native_activation_blocked_unsettled_archived_environments"
                ) from None


def archived_storage_key_is_referenced(storage_key):
    return WorkspaceSnapshotPublication.objects.filter(storage_key=storage_key).exists()


def archive_reference_blocks_gc(session_id, storage_key, *, temporary=False):
    if not connection.in_atomic_block:
        raise RuntimeError("archive_reference_owner_transaction_required")
    references = WorkspaceSnapshotPublication.objects.select_for_update().filter(
        session_id=session_id
    )
    if temporary:
        references = references.filter(
            storage_key__startswith=storage_key.rsplit("/", 1)[0] + "/"
        )
    else:
        references = references.filter(storage_key=storage_key)
    # ALL released references retain bytes, including committed publications.
    return references.exists()
