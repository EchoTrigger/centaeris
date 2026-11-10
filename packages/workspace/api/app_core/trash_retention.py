from datetime import timedelta

from django.utils import timezone


TRASH_RETENTION = timedelta(days=30)


def trash_cutoff(now=None):
    return (now or timezone.now()) - TRASH_RETENTION


def trash_is_restorable(deleted_at, purged_at, now=None):
    return (
        deleted_at is not None
        and purged_at is None
        and deleted_at > trash_cutoff(now)
    )
