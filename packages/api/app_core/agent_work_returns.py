"""Deliver retained work facts. No coordination admission or handling is implied."""
from django.core.exceptions import ObjectDoesNotExist
from django.db import IntegrityError, connection, transaction

from .agent_messages import _digest
from .agent_work import work_operation_id
from .agent_work_query import _record, _signed_native_run
from .app_delegations import session_authority_is_current
from .models import AgentCoordinationSession, AgentRun, AgentWorkReturn, AgentWorkSession, SessionEvent
from .runtime_client import agent_run_lifecycle_job_id
from .runtime_job_client import get_runtime_job
from .session_event import TERMINAL_STATES
from .workspace_access import agent_run_membership_is_current


class WorkReturnError(ValueError):
    def __init__(self, status, code):
        super().__init__(code)
        self.status, self.code = status, code


def _bound_work(run_id, *, source_event=...):
    child = AgentRun.objects.select_related("session__agent", "authorization", "user").get(id=run_id)
    work = AgentWorkSession.objects.select_related("operation", "source_run__authorization",
        "source_run__session__agent", "coordination_session").filter(pk=child.session_id).first()
    if work is None or work.operation.agentRunId != child.id:
        return None, child
    source, receipt = work.source_run, work.operation
    _signed_native_run(source)
    _signed_native_run(child)
    if (source.session_id != work.coordination_session_id or child.session_id == source.session_id
            or source.workspace_id != child.workspace_id or source.user_id != child.user_id
            or child.session.agent_id != source.session.agent_id
            or child.session.owner_id != child.user_id or child.session.workspace_id != child.workspace_id
            or source.session.owner_id != source.user_id or source.session.workspace_id != source.workspace_id
            or receipt.sessionId != child.session_id or receipt.turnId != child.turn_id
            or receipt.user_id != child.user_id or receipt.workspace_id != child.workspace_id
            or receipt.command != "submitMessage"
            or receipt.operationId != work_operation_id(source.id, work.source_turn_id, work.source_call_id)):
        raise WorkReturnError(409, "agent_work_return_binding_rejected")
    origin = SessionEvent.objects.filter(pk=work.source_event_id).first() if source_event is ... else source_event
    if origin is not None and (origin.agent_run_id != source.id
            or _record(origin, source, work.source_call_id) != work.source_turn_id
            or origin.payload["type"] != "tool_result"
            or origin.payload["payload"]["resultState"] != "successWithOutput"):
        raise WorkReturnError(409, "agent_work_return_binding_rejected")
    return work, child


def _terminal_fact(child):
    records = SessionEvent.objects.filter(agent_run=child, session_level=False)
    terminals = list(records.filter(payload__type__in=TERMINAL_STATES).order_by("agent_run_sequence")[:2])
    if terminals:
        event = terminals[0]
        from .session_payload import decode_session_payload
        wire = decode_session_payload(event.payload)
        last = records.order_by("-agent_run_sequence").values_list("eventId", flat=True).first()
        if (len(terminals) != 1 or last != event.eventId or event.session_id != child.session_id
                or event.workspace_id != child.workspace_id or wire.get("schemaVersion") != "session.event.v1"
                or wire.get("eventVersion") != 1 or wire.get("eventId") != event.eventId
                or wire.get("sequence") != event.sequence or wire.get("createdAtMs") != event.createdAtMs
                or wire.get("sessionId") != child.session_id or wire.get("agentRunId") != child.id
                or not isinstance(wire.get("turnId"), str) or not wire["turnId"]
                or not isinstance(wire.get("payload"), dict)):
            raise WorkReturnError(409, "agent_work_return_terminal_untrusted")
        return {"kind": "sessionTerminal", "factRef": event.eventId,
            "state": TERMINAL_STATES[wire["type"]], "eventType": wire["type"],
            "reasonType": wire["payload"].get("reasonType"),
            "atMs": event.createdAtMs, "throughSequence": event.sequence}
    if child.preAdmissionCancelledAt is not None:
        if records.exists():
            raise WorkReturnError(409, "agent_work_return_terminal_untrusted")
        # Runtime commits this hosted receipt only after validating the durable
        # cancellation request and current lifecycle lease. It creates no Session event.
        return {"kind": "preAdmissionCancellation", "factRef": child.id,
            "state": "cancelled", "eventType": None, "reasonType": "preAdmissionCancellation",
            "atMs": int(child.preAdmissionCancelledAt.timestamp() * 1000), "throughSequence": 0}
    if child.status == "failed" and child.transitionReason == "agent_run_lifecycle_dead_lettered":
        job_id = agent_run_lifecycle_job_id(child.id)
        job = get_runtime_job(job_id)
        if (job is None or job.get("status") not in {"failed", "dead_lettered"}
                or job.get("jobId") != job_id or job.get("jobKind") != "agent_run.lifecycle"
                or job.get("sessionId") != child.session_id or job.get("payloadRef") != "record:agent_run:" + child.id
                or job.get("idempotencyKey") != job_id + ":" + child.authorization.digest
                or type(job.get("updatedAtMs")) is not int):
            raise WorkReturnError(409, "agent_work_return_lifecycle_untrusted")
        return {"kind": "lifecycleFailure", "factRef": job_id, "state": "executionFailed",
            "eventType": None, "reasonType": "agent_run_lifecycle_dead_lettered",
            "atMs": job["updatedAtMs"], "throughSequence": None}
    return None


def _payload(work, child, fact):
    identity = {"workspaceId": child.workspace_id, "userId": child.user_id,
        "agentId": child.session.agent_id, "coordinationSessionId": work.coordination_session_id,
        "workSessionId": child.session_id, "workAgentRunId": child.id,
        "sourceAgentRunId": work.source_run_id, "sourceTurnId": work.source_turn_id,
        "sourceToolCallId": work.source_call_id, "sourceEventId": work.source_event_id,
        "operationId": work.operation.operationId, "factKind": fact["kind"], "factRef": fact["factRef"],
        "factDigest": _digest("workspace.agent_work.return_fact.v1", fact)}
    notice_id = "work-return:" + _digest("workspace.agent_work.return_identity.v1", identity).removeprefix("sha256:")
    return {"schema": "workspace.agent_work.return_notice.v1", "noticeId": notice_id,
        "source": "workspace.agent_work.return", "identity": identity, "terminal": fact,
        "content": {"source": "untrustedWorkOutput", "sessionRef": child.session_id,
                    "throughSequence": fact["throughSequence"]}}


def materialize_work_return(run_id):
    if connection.in_atomic_block:
        raise WorkReturnError(409, "agent_work_return_source_uncommitted")
    try:
        work, child = _bound_work(run_id)
        if work is None:
            return "notWork", None
        fact = _terminal_fact(child)
        if fact is None:
            return "pending", None
        payload = _payload(work, child, fact)
        with transaction.atomic():
            notice, created = AgentWorkReturn.objects.get_or_create(id=payload["noticeId"], defaults={
                "work": work, "child_run": child, "fact_kind": fact["kind"], "fact_ref": fact["factRef"], "payload": payload})
            if (notice.work_id != work.pk or notice.child_run_id != child.id or notice.fact_kind != fact["kind"]
                    or notice.fact_ref != fact["factRef"] or notice.payload != payload):
                raise WorkReturnError(409, "agent_work_return_conflict")
        return "delivered" if created else "duplicate", notice.payload
    except WorkReturnError:
        raise
    except AgentRun.DoesNotExist as error:
        raise WorkReturnError(404, "agent_work_return_source_not_found") from error
    except IntegrityError as error:
        raise WorkReturnError(409, "agent_work_return_conflict") from error
    except (ObjectDoesNotExist, ValueError, TypeError, KeyError) as error:
        raise WorkReturnError(409, "agent_work_return_binding_rejected") from error


@transaction.atomic
def query_work_return(body):
    with connection.cursor() as cursor:
        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
    try:
        caller = AgentRun.objects.select_related("session__agent", "authorization", "user").get(id=body["agentRunId"])
        digest = _signed_native_run(caller)
        agent = caller.session.agent
        if (digest != body["authorizationDigest"] or caller.status not in {"queued", "running"}
                or not caller.user.is_active or not agent_run_membership_is_current(caller)
                or agent.owner_id != caller.user_id or agent.workspace_id != caller.workspace_id
                or caller.session.owner_id != caller.user_id or caller.session.workspace_id != caller.workspace_id
                or body["coordinationSessionId"] != caller.session_id
                or not AgentCoordinationSession.objects.filter(agent=agent, session_id=caller.session_id).exists()
                or not session_authority_is_current(caller.user_id, caller.session_id, scope="events:read")):
            raise WorkReturnError(403, "agent_work_return_authority_rejected")
        notice = AgentWorkReturn.objects.select_related("work").get(pk=body["noticeId"])
        work, child = _bound_work(notice.child_run_id)
        if (work is None or work.pk != notice.work_id or work.coordination_session_id != caller.session_id
                or child.user_id != caller.user_id or child.workspace_id != caller.workspace_id
                or not agent_run_membership_is_current(work.source_run) or not agent_run_membership_is_current(child)
                or not session_authority_is_current(caller.user_id, child.session_id, scope="events:read")
                or notice.payload != _payload(work, child, notice.payload["terminal"])
                or notice.fact_kind != notice.payload["terminal"]["kind"]
                or notice.fact_ref != notice.payload["terminal"]["factRef"]):
            raise WorkReturnError(403, "agent_work_return_authority_rejected")
    except WorkReturnError:
        raise
    except (ObjectDoesNotExist, ValueError, TypeError, KeyError) as error:
        raise WorkReturnError(403, "agent_work_return_authority_rejected") from error
    return {"schema": "workspace.agent_work.return_query_result.v1", "agentRunId": caller.id,
        "authorizationDigest": digest, "coordinationSessionId": caller.session_id,
        "toolCallId": body["toolCallId"], "notice": notice.payload}


@transaction.atomic
def discover_work_returns(after, through, limit):
    # Page facts only. A slow lifecycle Job read belongs to one materialization
    # request, never to discovery of the rest of the persisted source ledger.
    with connection.cursor() as cursor:
        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
    rows = AgentWorkSession.objects.order_by("session_id")
    if through is None:
        through = rows.values_list("session_id", flat=True).last()
    if through is None:
        return {"schema": "workspace.agent_work.returns.discovered.v1", "entries": [], "through": None, "next": None}
    rows = rows.filter(session_id__lte=through)
    if after is not None:
        rows = rows.filter(session_id__gt=after)
    page = list(rows.values_list("session_id", "operation__agentRunId")[:limit + 1])
    return {"schema": "workspace.agent_work.returns.discovered.v1", "through": through,
        "next": page[limit-1][0] if len(page) > limit else None,
        "entries": [{"cursor": session_id, "workAgentRunId": run_id} for session_id, run_id in page[:limit]]}
