"""Hosted admission; the workspace is the tenant boundary."""

from django.conf import settings
from django.db import connection

from app_core.models import AgentRun


def queued_admission_error(workspace_id):
    """Called inside the transaction that inserts the run.

    Serialize admissions across API replicas. Departures only reduce the count;
    they need not acquire this lock. A Worker can mark a Run running before
    Runtime obtains capacity, so queued and running Runs remain obligations.
    Count at most the configured limit; terminal history occupies no slot.
    """
    if not connection.in_atomic_block:
        raise RuntimeError("execution_admission_requires_transaction")
    with connection.cursor() as cursor:
        cursor.execute("SELECT pg_try_advisory_xact_lock(731946, 1)")
        if not cursor.fetchone()[0]:
            return 503, "execution_admission_busy"
    waiting = AgentRun.objects.filter(status__in=["queued", "running"])
    tenant_limit = settings.EXECUTION_TENANT_QUEUE_LIMIT
    if len(list(waiting.filter(workspace_id=workspace_id).values_list("id", flat=True)[:tenant_limit])) >= tenant_limit:
        return 429, "workspace_execution_queue_full"
    global_limit = settings.EXECUTION_GLOBAL_QUEUE_LIMIT
    if len(list(waiting.values_list("id", flat=True)[:global_limit])) >= global_limit:
        return 503, "execution_queue_full"
    return None
