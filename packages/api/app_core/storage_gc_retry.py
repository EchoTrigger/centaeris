"""Persist cleanup backoff without turning unknown bytes into free capacity."""

from dataclasses import dataclass
from datetime import timedelta
import hashlib
import logging
import uuid

from django.conf import settings
from django.db import connection, transaction
from django.utils import timezone

from .models import StorageCleanupRetry
from .resource_commit import require_durable_commit


logger = logging.getLogger(__name__)


def cleanup_attempt_limit():
    return getattr(settings, "GC_MAX_CLEANUP_ATTEMPTS", 5)


def next_cleanup_time(attempts, now):
    base = getattr(settings, "GC_RETRY_BASE_SECONDS", 86400)
    maximum = getattr(settings, "GC_RETRY_MAX_SECONDS", 604800)
    # Bound work for imported counters using the configured delay ceiling.
    exponent = min(max(attempts - 1, 0), (maximum // base).bit_length())
    seconds = min(maximum, base * (2 ** exponent))
    return now + timedelta(seconds=seconds)


@dataclass(frozen=True)
class StorageCleanupResult:
    state: str
    failure: str = ""


def claim_storage_key(storage_key, collector):
    digest = hashlib.sha256(storage_key.encode("utf-8")).hexdigest()
    lease_owner = f"gc_{uuid.uuid4().hex}"
    now = timezone.now()
    with transaction.atomic():
        require_durable_commit(connection, RuntimeError("storage_cleanup_durability_required"))
        StorageCleanupRetry.objects.get_or_create(
            keyDigest=digest, defaults={"storageKey": storage_key, "collector": collector},
        )
        retry = StorageCleanupRetry.objects.select_for_update().get(pk=digest)
        if retry.storageKey != storage_key or retry.collector != collector:
            raise RuntimeError("storage_cleanup_identity_conflict")
        if retry.state in {"cleaned", "quarantined"}:
            return None
        if retry.nextCleanupAt is not None and retry.nextCleanupAt > now:
            return None
        if retry.state == "cleaning" and retry.leaseExpiresAt and retry.leaseExpiresAt > now:
            return None
        if retry.cleanupAttempts >= cleanup_attempt_limit():
            retry.state = "quarantined"
            retry.quarantinedAt = now
            retry.leaseOwner = ""
            retry.leaseExpiresAt = None
            retry.save(update_fields=["state", "quarantinedAt", "leaseOwner", "leaseExpiresAt", "updatedAt"])
            logger.error("Storage cleanup quarantined: collector=%s keyDigest=%s attempts=%s", collector, digest, retry.cleanupAttempts)
            return None
        retry.state = "cleaning"
        retry.cleanupAttempts += 1
        retry.nextCleanupAt = None
        retry.leaseOwner = lease_owner
        retry.leaseExpiresAt = now + timedelta(minutes=5)
        retry.save(update_fields=["state", "cleanupAttempts", "nextCleanupAt", "leaseOwner", "leaseExpiresAt", "updatedAt"])
        return retry


def clean_storage_key(storage_key, collector, delete, *, claim=None):
    """The caller first verifies the key; claims precede physical I/O.

    Snapshot collectors claim outside their owner-fencing transaction, so a
    process crash cannot roll back the attempt count. Return failures so that
    owner transactions commit backoff even when a physical delete fails.
    """
    retry = claim if claim is not None else claim_storage_key(storage_key, collector)
    if retry is None:
        return StorageCleanupResult("blocked")
    if retry.storageKey != storage_key or retry.collector != collector:
        raise RuntimeError("storage_cleanup_identity_conflict")

    failure = ""
    try:
        delete(storage_key)
    except Exception as error:
        failure = (str(error) or type(error).__name__)[:4000]
    now = timezone.now()
    exhausted = bool(failure) and retry.cleanupAttempts >= cleanup_attempt_limit()
    state = "quarantined" if exhausted else "failed" if failure else "cleaned"
    changed = StorageCleanupRetry.objects.filter(
        pk=retry.pk, state="cleaning", leaseOwner=retry.leaseOwner,
    ).update(
        state=state, lastFailure=failure, leaseOwner="", leaseExpiresAt=None,
        nextCleanupAt=next_cleanup_time(retry.cleanupAttempts, now) if failure and not exhausted else None,
        quarantinedAt=now if exhausted else None, cleanedAt=None if failure else now, updatedAt=now,
    )
    if changed != 1:
        raise RuntimeError("storage_cleanup_lease_lost")
    if exhausted:
        logger.error("Storage cleanup quarantined: collector=%s keyDigest=%s attempts=%s", collector, retry.pk, retry.cleanupAttempts)
    return StorageCleanupResult("failed" if failure else "cleaned", failure)
