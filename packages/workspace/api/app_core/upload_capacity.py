"""A shared temporary-byte/slot counter and durable request liabilities."""
import uuid
import os
from django.conf import settings
from django.db import connection, transaction

from .models import UploadCapacity, UploadLease
from .resource_commit import require_durable_commit
from .upload_temp import MARKER, LEASE_NAME, initialize_upload_temp_root, namespace_files, remove_namespace


class UploadCapacityError(ValueError):
    pass


def _locked_capacity():
    require_durable_commit(connection, UploadCapacityError("upload_capacity_durability_required"))
    UploadCapacity.objects.get_or_create(pk=1)
    return UploadCapacity.objects.select_for_update().get(pk=1)


def _validate_pool(root, pool_ref):
    for path in root.iterdir():
        if path.name == MARKER:
            continue
        if LEASE_NAME.fullmatch(path.name) is None:
            raise UploadCapacityError("upload_temp_unmanaged_object")
        lease = UploadLease.objects.filter(pk=path.name, poolRef=pool_ref).first()
        if lease is None:
            raise UploadCapacityError("upload_temp_pool_identity_mismatch")
        namespace_files(root, pool_ref, lease.pk)


@transaction.atomic
def reserve_ingress(max_body_bytes, *, upload_slot=True):
    if type(max_body_bytes) is not int or not 0 < max_body_bytes <= (2**63 - 1) // 2:
        raise UploadCapacityError("upload_body_budget_invalid")
    root, pool_ref = initialize_upload_temp_root()
    capacity = _locked_capacity()
    _validate_pool(root, pool_ref)
    byte_hold = 2 * max_body_bytes
    if (capacity.reservedBytes + byte_hold > settings.UPLOAD_TEMP_MAX_BYTES
            or (upload_slot and capacity.activeUploads >= settings.UPLOAD_MAX_CONCURRENT)):
        raise UploadCapacityError("upload_capacity_exhausted")
    capacity.reservedBytes += byte_hold
    capacity.activeUploads += int(upload_slot)
    capacity.save(update_fields=["reservedBytes", "activeUploads"])
    return UploadLease.objects.create(id=uuid.uuid4().hex, poolRef=pool_ref,
        byteHold=byte_hold, uploadSlot=upload_slot)


@transaction.atomic
def mark_cleanup_unknown(lease_id):
    _locked_capacity()
    UploadLease.objects.filter(pk=lease_id).update(state="unknown")


@transaction.atomic
def release_ingress(lease_id):
    capacity = _locked_capacity()
    lease = UploadLease.objects.select_for_update().filter(pk=lease_id).first()
    if lease is None:
        return
    root, pool_ref = initialize_upload_temp_root()
    if pool_ref != lease.poolRef or os.path.lexists(root / lease.pk):
        raise UploadCapacityError("upload_temp_cleanup_unconfirmed")
    if capacity.reservedBytes < lease.byteHold or capacity.activeUploads < int(lease.uploadSlot):
        raise UploadCapacityError("upload_capacity_corrupt")
    capacity.reservedBytes -= lease.byteHold
    capacity.activeUploads -= int(lease.uploadSlot)
    capacity.save(update_fields=["reservedBytes", "activeUploads"])
    # Confirmed temporary liabilities have no historical receipt obligation.
    # Counter reduction and removal commit together; missing IDs are idempotent.
    lease.delete()


@transaction.atomic
def reconcile_ingress_pool():
    root, pool_ref = initialize_upload_temp_root()
    _locked_capacity()
    _validate_pool(root, pool_ref)
    leases = list(UploadLease.objects.filter(poolRef=pool_ref))
    # Validate every owned namespace before deleting the first file.
    for lease in leases:
        namespace_files(root, pool_ref, lease.pk)
    for lease in leases:
        remove_namespace(root, pool_ref, lease.pk)
        release_ingress(lease.pk)
    return len(leases)
