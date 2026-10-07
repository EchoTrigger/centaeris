"""Bounded source discovery. Cursors are progress hints, never admission facts."""
from django.core.exceptions import ObjectDoesNotExist
from django.db import connection, transaction
from django.db.models import Q
from django.utils import timezone
from django.utils.dateparse import parse_datetime

from .agent_work import _source_request, WorkMaterializationError, work_operation_id
from .agent_work_query import _signed_native_run, WorkQueryError
from .models import AgentCoordinationSession, HostedOperationReceipt, SessionEvent


# Keep this predicate identical to the migration's partial index condition.
SOURCE_PREDICATE = Q(session_level=False, projects_to_agent_run_stream=True,
    payload__type="tool_result", payload__payload__toolName="dispatch_work",
    payload__payload__resultState="successWithOutput")


def scan_cursor(value):
    if value is None:
        return None
    if not isinstance(value, dict) or set(value) != {"insertedAt", "eventId"}:
        raise ValueError("agent_work_discover_invalid")
    if not isinstance(value["eventId"], str) or not 1 <= len(value["eventId"]) <= 160 or not value["eventId"].strip():
        raise ValueError("agent_work_discover_invalid")
    timestamp = parse_datetime(value["insertedAt"]) if isinstance(value["insertedAt"], str) else None
    if timestamp is None or timezone.is_naive(timestamp):
        raise ValueError("agent_work_discover_invalid")
    return timestamp, value["eventId"]


def _cursor(row):
    return {"insertedAt":row["insertedAt"].isoformat(), "eventId":row["eventId"]}


def source_page(after, through, limit):
    rows = SessionEvent.objects.filter(SOURCE_PREDICATE)
    if through is None:
        last = rows.order_by("-insertedAt", "-eventId").values("insertedAt", "eventId").first()
        if last is None:
            return [], None, None
        through = scan_cursor(_cursor(last))
    stamp, event_id = through
    rows = rows.filter(Q(insertedAt__lt=stamp) | Q(insertedAt=stamp, eventId__lte=event_id))
    if after is not None:
        stamp, event_id = after
        rows = rows.filter(Q(insertedAt__gt=stamp) | Q(insertedAt=stamp, eventId__gt=event_id))
    page = list(rows.order_by("insertedAt", "eventId").values("insertedAt", "eventId")[:limit + 1])
    return page[:limit], {"insertedAt":through[0].isoformat(), "eventId":through[1]}, _cursor(page[limit-1]) if len(page) > limit else None


@transaction.atomic
def discover_work_requests(after, through, limit):
    with connection.cursor() as cursor:
        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
    rows, upper, next_cursor = source_page(after, through, limit)
    entries, candidates = [], []
    scopes = Q()
    for row in rows:
        entry = {"cursor":_cursor(row), "sourceEventId":None}
        entries.append(entry)
        try:
            result, run, _ = _source_request(row["eventId"])
            _signed_native_run(run)
            if not AgentCoordinationSession.objects.filter(agent_id=run.session.agent_id, session_id=run.session_id).exists():
                continue
            key = work_operation_id(run.id, result.payload["turnId"], result.payload["payload"]["callId"])
        except (WorkMaterializationError, WorkQueryError, ObjectDoesNotExist, ValueError, TypeError, KeyError):
            continue
        scope = (run.user_id, run.workspace_id, key)
        candidates.append((entry, scope))
        scopes |= Q(user_id=scope[0], workspace_id=scope[1], command="submitMessage", operationId=scope[2])
    admitted = set(HostedOperationReceipt.objects.filter(scopes).values_list("user_id", "workspace_id", "operationId")) if candidates else set()
    for entry, scope in candidates:
        if scope not in admitted:
            entry["sourceEventId"] = entry["cursor"]["eventId"]
    return {"schema":"workspace.agent_work.discovered.v1", "entries":entries, "through":upper, "next":next_cursor}
