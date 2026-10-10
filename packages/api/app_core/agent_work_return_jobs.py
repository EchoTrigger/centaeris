"""Queue return publication through Core jobs; repair omissions at a bounded rate."""
import uuid
import time
from django.conf import settings

from django.db import connection, transaction
from django.db.models import Exists, OuterRef
from django.utils import timezone

from .agent_work_returns import _bound_work, WorkReturnError
from .models import AgentRun, AgentWorkReturn, AgentWorkSession, WorkReturnAuditCursor
from .runtime_job_client import schedule_runtime_job


def _audit_interval_ms():
    return settings.WORK_RETURN_AUDIT_INTERVAL_SECONDS * 1000


def _audit_lease_ms():
    # Claim transport and one bounded worker control round share this lease.
    return 2 * settings.RUNTIME_CONTROL_TIMEOUT_SECONDS * 1000


def _now_ms():
    if connection.vendor == "postgresql":
        with connection.cursor() as cursor:
            cursor.execute("SELECT (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint")
            return cursor.fetchone()[0]
    return int(timezone.now().timestamp() * 1000)


def schedule_work_return_job(run):
    deadline = time.monotonic() + settings.RUNTIME_CONTROL_TIMEOUT_SECONDS
    if connection.in_atomic_block:
        raise RuntimeError("work_return_schedule_requires_committed_source")
    work, child = _bound_work(run.pk)
    if work is None:
        return "notWork"
    if AgentWorkReturn.objects.filter(work=work, child_run=child).exists():
        return "delivered"
    job_id = "agent_work.return:" + child.pk
    body = {"schema": "runtime.job.schedule.v1", "jobId": job_id,
        "jobKind": "agent_work.return", "runAtMs": _now_ms(), "maxRetries": 10,
        "idempotencyKey": job_id, "sessionId": child.session_id,
        "payloadRef": "record:agent_work:" + child.pk}
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise RuntimeError("work_return_schedule_deadline_exceeded")
    result = schedule_runtime_job(body, timeout=remaining)
    job = result["job"]
    if any(job.get(key) != body[key] for key in ("jobId", "jobKind", "idempotencyKey", "sessionId", "payloadRef")):
        raise RuntimeError("work_return_schedule_binding_invalid")
    return result["disposition"]


def claim_work_return_audit(limit):
    now = _now_ms()
    with transaction.atomic():
        WorkReturnAuditCursor.objects.get_or_create(pk="work_returns")
        cursor = WorkReturnAuditCursor.objects.select_for_update(skip_locked=True).filter(pk="work_returns").first()
        if cursor is None or cursor.leaseExpiresAtMs > now or cursor.nextPageAtMs > now:
            return {"schema": "workspace.agent_work.return_audit.claimed.v1", "disposition": "idle"}
        if cursor.through is None:
            cursor.through = AgentWorkSession.objects.order_by("pk").values_list("pk", flat=True).last()
            cursor.after = None
        cursor.leaseOwner = uuid.uuid4().hex
        cursor.leaseExpiresAtMs = now + _audit_lease_ms()
        # Claim itself spends an audit interval, even if its response is lost.
        cursor.nextPageAtMs = now + _audit_interval_ms()
        cursor.save()
        after, through, owner = cursor.after, cursor.through, cursor.leaseOwner
    rows = AgentWorkSession.objects.filter(pk__lte=through) if through is not None else AgentWorkSession.objects.none()
    if after is not None:
        rows = rows.filter(pk__gt=after)
    notices = AgentWorkReturn.objects.filter(work_id=OuterRef("pk"), child_run_id=OuterRef("operation__agentRunId"))
    page = list(rows.annotate(materialized=Exists(notices)).order_by("pk").values_list(
        "pk", "operation__agentRunId", "materialized")[:limit + 1])
    entries = [{"cursor": identity, "workAgentRunId": run, "materialized": materialized}
               for identity, run, materialized in page[:limit]]
    return {"schema": "workspace.agent_work.return_audit.claimed.v1", "disposition": "claimed",
        "leaseOwner": owner, "after": after, "through": through,
        "next": entries[-1]["cursor"] if len(page) > limit else None, "entries": entries}


def finish_work_return_audit(owner, after, complete):
    with transaction.atomic():
        cursor = WorkReturnAuditCursor.objects.select_for_update().get(pk="work_returns")
        now = _now_ms()
        if cursor.leaseOwner != owner or cursor.leaseExpiresAtMs <= now:
            raise WorkReturnError(409, "work_return_audit_lease_lost")
        if after is not None and (cursor.through is None or after > cursor.through
                or cursor.after is not None and after < cursor.after):
            raise WorkReturnError(400, "work_return_audit_cursor_invalid")
        if complete:
            cursor.after = cursor.through = None
            cursor.nextPageAtMs = now + _audit_interval_ms()
        else:
            cursor.after = after
            cursor.nextPageAtMs = now + _audit_interval_ms()
        cursor.leaseOwner = ""
        cursor.leaseExpiresAtMs = 0
        cursor.save()
