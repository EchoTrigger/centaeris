"""Platform task lease primitives; not a scheduler or an authorization boundary.

Only explicitly platform-owned tasks are eligible. Callers must resolve current
source authority before execution/publication. Hold fenced_task around the entire
representation DB publication, not just a check before a separate transaction.
The dedicated platform Worker uses these fences through atomic publication.
"""

from contextlib import contextmanager
from dataclasses import dataclass
from datetime import timedelta

from django.db import transaction
from django.db.models.functions import Now

from .models import MaterialProcessingTask


MAX_ATTEMPTS = 3


class LeaseLost(RuntimeError):
    pass


@dataclass(frozen=True)
class MaterialLease:
    representation_id: str
    owner: str
    epoch: int


def _validate(owner, seconds):
    if not isinstance(owner, str) or not owner.strip() or len(owner) > 128:
        raise ValueError("material_lease_owner_invalid")
    if type(seconds) is not int or not 1 <= seconds <= 300:
        raise ValueError("material_lease_duration_invalid")


def _locked(identity):
    # Database time avoids clock skew between API replicas. Evaluate after locking
    # so time spent waiting for another publisher is not part of the new lease.
    task = MaterialProcessingTask.objects.select_for_update().get(pk=identity)
    now = MaterialProcessingTask.objects.filter(pk=identity).annotate(clock=Now()).values_list("clock", flat=True).get()
    return task, now


def claim(identity, owner, seconds):
    _validate(owner, seconds)
    with transaction.atomic():
        task, now = _locked(identity)
        if task.executionBackend != "platform" or task.status not in {"pending", "running"}:
            return None
        if task.leaseExpiresAt is not None and task.leaseExpiresAt > now:
            return None
        if task.attemptCount >= MAX_ATTEMPTS:
            task.status = "failed"
            task.errorCode = "material_processing_attempts_exhausted"
            task.leaseOwner, task.leaseExpiresAt = "", None
            task.save(update_fields=["status", "errorCode", "leaseOwner", "leaseExpiresAt", "updatedAt"])
            return None
        task.leaseEpoch += 1
        task.attemptCount += 1
        task.leaseOwner = owner
        task.leaseExpiresAt = now + timedelta(seconds=seconds)
        task.status, task.errorCode = "running", ""
        task.save(update_fields=["leaseEpoch", "attemptCount", "leaseOwner", "leaseExpiresAt", "status", "errorCode", "updatedAt"])
        return MaterialLease(task.pk, owner, task.leaseEpoch)


def _check(task, now, lease, *, completed_replay=False):
    if (task.executionBackend != "platform"
            or task.leaseOwner != lease.owner or task.leaseEpoch != lease.epoch):
        raise LeaseLost("material_processing_lease_lost")
    if completed_replay and task.status == "completed":
        return
    if (task.status != "running" or task.leaseExpiresAt is None or task.leaseExpiresAt <= now):
        raise LeaseLost("material_processing_lease_lost")


@contextmanager
def fenced_task(lease, *, completed_replay=False):
    with transaction.atomic():
        task, now = _locked(lease.representation_id)
        _check(task, now, lease, completed_replay=completed_replay)
        yield task


def renew(lease, seconds):
    _validate(lease.owner, seconds)
    with transaction.atomic():
        task, now = _locked(lease.representation_id)
        _check(task, now, lease)
        task.leaseExpiresAt = now + timedelta(seconds=seconds)
        task.save(update_fields=["leaseExpiresAt", "updatedAt"])
