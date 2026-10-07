"""Read-only confirmation authority and projection of committed Session facts."""
import json
from pathlib import Path

from django.core.exceptions import ObjectDoesNotExist
from django.db import connection, transaction

from .agent_messages import _digest
from .agent_work_consumption import bound_host_input
from .agent_work_query import _signed_native_run
from .agent_work_returns import _bound_work, _payload, WorkReturnError
from .app_delegations import session_authority_is_current
from .models import AgentCoordinationSession, AgentRun, AgentWorkConsumeAttempt, AgentWorkReturn, SessionEvent, WorkspaceMembership
from .runtime_contract import require_opaque_ref
from .workspace_access import agent_run_membership_is_current

_CONTRACT = json.loads((Path(__file__).parent / "contracts/confirm_work_return.json").read_text(encoding="utf-8"))
VALIDATED_SCHEMA = "workspace.agent_work.return_confirm.validated.v1"


def confirmation_contract_digest():
    return _digest("centaeris.dynamic_tool_contract.v1", _CONTRACT)


def validate_confirmation_args(args):
    if not isinstance(args, dict) or set(args) != {"notice_id", "attempt_id"}:
        raise ValueError("agent_work_confirmation_input_invalid")
    for name, value in args.items():
        require_opaque_ref(name, value)
        if len(value) > 160:
            raise ValueError("agent_work_confirmation_input_invalid")
    return args


def confirmation_record(run, attempt, call_id):
    args = {"notice_id": attempt.notice_id, "attempt_id": attempt.pk}
    return {"schema": VALIDATED_SCHEMA, "agentId": run.session.agent_id,
        "sessionId": run.session_id, "agentRunId": run.pk, "toolCallId": call_id,
        "authorizationDigest": run.authorization.digest,
        "inputDigest": _digest("workspace.agent_work.return_confirm.input.v1", args),
        "noticeId": attempt.notice_id, "attemptId": attempt.pk,
        "noticeIdentity": attempt.notice.payload["identity"]}


def _notice_for_caller(notice, caller):
    work, child = _bound_work(notice.child_run_id)
    if work is None:
        raise ValueError("agent_work_confirmation_source_missing")
    source = work.source_run
    if (work.pk != notice.work_id or work.coordination_session_id != caller.session_id
            or source.workspace_id != caller.workspace_id or source.user_id != caller.user_id
            or child.workspace_id != caller.workspace_id or child.user_id != caller.user_id
            or source.membership_ref != caller.membership_ref or child.membership_ref != caller.membership_ref
            or not agent_run_membership_is_current(source) or not agent_run_membership_is_current(child)
            or source.authorization.payload["thinkingMode"] != (source.thinkingMode or None)
            or child.authorization.payload["thinkingMode"] != (child.thinkingMode or None)
            or not session_authority_is_current(caller.user_id, caller.session_id, scope="messages:submit")
            or not session_authority_is_current(caller.user_id, child.session_id, scope="events:read")
            or notice.payload != _payload(work, child, notice.payload["terminal"])
            or notice.fact_kind != notice.payload["terminal"]["kind"]
            or notice.fact_ref != notice.payload["terminal"]["factRef"]):
        raise ValueError("agent_work_confirmation_source_rejected")
    return work


@transaction.atomic
def validate_work_return_confirmation(body):
    with connection.cursor() as cursor:
        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
    try:
        validate_confirmation_args({"notice_id": body["noticeId"], "attempt_id": body["attemptId"]})
        caller = AgentRun.objects.select_related("session__agent", "authorization", "user", "workspace").get(pk=body["agentRunId"])
        agent = caller.session.agent
        if (_signed_native_run(caller) != body["authorizationDigest"]
                or caller.status not in {"queued", "running"} or not caller.user.is_active
                or caller.workspace.status != "active" or agent.status != "active"
                or body["coordinationSessionId"] != caller.session_id
                or caller.session.status != "active" or caller.session.purgedAt is not None
                or agent.owner_id != caller.user_id or agent.workspace_id != caller.workspace_id
                or caller.session.owner_id != caller.user_id or caller.session.workspace_id != caller.workspace_id
                or not agent_run_membership_is_current(caller)
                or not WorkspaceMembership.objects.filter(pk=caller.membership_ref, role__in=["owner", "admin", "member"]).exists()
                or not AgentCoordinationSession.objects.filter(agent=agent, session_id=caller.session_id).exists()):
            raise ValueError("agent_work_confirmation_caller_rejected")
        attempt = AgentWorkConsumeAttempt.objects.select_related("notice__work", "operation").get(
            pk=body["attemptId"], notice_id=body["noticeId"], coordinator_run=caller)
        _notice_for_caller(attempt.notice, caller)
        bound_host_input(attempt, caller)
        return confirmation_record(caller, attempt, body["toolCallId"])
    except (ObjectDoesNotExist, ValueError, TypeError, KeyError, AttributeError) as error:
        raise WorkReturnError(403, "agent_work_confirmation_authority_rejected") from error


def _owned_record(stored, run, kind):
    wire = stored.payload
    return (not stored.session_level and stored.projects_to_agent_run_stream
        and stored.agent_run_sequence is not None and isinstance(wire, dict)
        and stored.workspace_id == run.workspace_id and stored.session_id == run.session_id
        and stored.agent_run_id == run.pk and wire.get("schemaVersion") == "session.event.v1"
        and wire.get("eventVersion") == 1 and wire.get("type") == kind
        and wire.get("eventId") == stored.eventId and type(wire.get("sequence")) is int
        and wire["sequence"] == stored.sequence and wire.get("sessionId") == run.session_id
        and wire.get("agentRunId") == run.pk and isinstance(wire.get("turnId"), str)
        and bool(wire["turnId"].strip()) and isinstance(wire.get("payload"), dict))


def committed_confirmation(attempt):
    """Fold effective canonical pairs, without mutating the attempt or its ACK."""
    if attempt.coordinator_run_id is None:
        return None
    run = attempt.coordinator_run
    records = SessionEvent.objects.filter(workspace_id=run.workspace_id, session_id=run.session_id,
        agent_run=run, session_level=False, projects_to_agent_run_stream=True,
        payload__type__in=["tool_call", "tool_result"], payload__payload__toolName="confirm_work_return").order_by("sequence")
    calls = {}
    for stored in records:
        wire = stored.payload
        if not isinstance(wire, dict) or not isinstance(wire.get("payload"), dict) or not isinstance(wire.get("turnId"), str):
            continue
        payload = wire["payload"]
        key = (wire.get("turnId"), payload.get("callId"))
        if not isinstance(key[1], str) or not key[1].strip():
            continue
        if wire.get("type") == "tool_call":
            calls[key] = stored if _owned_record(stored, run, "tool_call") else None
            continue
        call = calls.get(key)
        if (call is None or not _owned_record(stored, run, "tool_result")
                or payload.get("resultState") != "successWithOutput" or call.sequence >= stored.sequence):
            continue
        call_payload = call.payload["payload"]
        if (call_payload.get("providerId") != _CONTRACT["providerId"]
                or call_payload.get("toolContractDigest") != confirmation_contract_digest()
                or call_payload.get("normalizedInput") != {"notice_id": attempt.notice_id, "attempt_id": attempt.pk}):
            continue
        try:
            if json.loads(payload.get("modelContent", "")) != confirmation_record(run, attempt, key[1]):
                continue
        except (ValueError, TypeError, KeyError):
            continue
        return {"eventId": stored.eventId, "callEventId": call.eventId, "sequence": stored.sequence,
                "agentRunId": run.pk, "turnId": key[0], "toolCallId": key[1]}
    return None


def query_work_return_views(work, caller):
    """The existing source Run/call query already identifies this child work."""
    views = []
    for notice in AgentWorkReturn.objects.filter(work=work).select_related("work").order_by("id"):
        _notice_for_caller(notice, caller)
        attempt = AgentWorkConsumeAttempt.objects.select_related("coordinator_run__session", "coordinator_run__authorization", "notice").filter(notice=notice).first()
        confirmation = committed_confirmation(attempt) if attempt is not None else None
        views.append({"notice": notice.payload,
            "attempt": {"attemptId": attempt.pk, "agentRunId": attempt.coordinator_run_id} if attempt is not None else None,
            "handled": confirmation is not None, "confirmation": confirmation})
    return views
