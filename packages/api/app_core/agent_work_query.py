"""Read retained dispatch facts and admission in one consistent, read-only snapshot."""
from django.conf import settings
from django.core.exceptions import ObjectDoesNotExist
from django.db import connection, transaction

from .agent_messages import _digest
from .agent_work import work_contract_digest, work_operation_id
from .agent_work_validation import validate_work_args
from .app_delegations import session_authority_is_current
from .hosted_operations import serialize_operation
from .models import AgentCoordinationSession, AgentRun, AgentWorkSession, HostedOperationReceipt, SessionEvent
from .runtime_contract import (agent_run_binding_matches, authorization_digest,
                               _verify_authorization_digest_signature)
from .workspace_access import agent_run_membership_is_current


class WorkQueryError(ValueError):
    def __init__(self, status, code):
        super().__init__(code)
        self.status, self.code = status, code


def _signed_native_run(run):
    authorization = run.authorization
    digest = authorization_digest(authorization.payload)
    _verify_authorization_digest_signature(digest, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
                                           authorization.signature)
    if (digest != authorization.digest or not agent_run_binding_matches(authorization.payload, run)
            or run.session.agent.definition_id or run.definition_version_id
            or run.acting_app_id or run.app_delegation_id):
        raise WorkQueryError(403, "agent_work_authority_rejected")
    return digest


def _record(record, run, call_id):
    from .session_payload import decode_session_payload
    wire = decode_session_payload(record.payload)
    payload = wire.get("payload") if isinstance(wire, dict) else None
    if (not isinstance(payload, dict) or record.session_level
            or record.workspace_id != run.workspace_id or record.session_id != run.session_id
            or wire.get("schemaVersion") != "session.event.v1" or wire.get("eventVersion") != 1
            or wire.get("eventId") != record.eventId or wire.get("sequence") != record.sequence
            or wire.get("sessionId") != run.session_id or wire.get("agentRunId") != run.id
            or not isinstance(wire.get("turnId"), str) or not wire["turnId"]
            or payload.get("toolName") != "dispatch_work" or payload.get("callId") != call_id):
        raise WorkQueryError(409, "agent_work_request_untrusted")
    if wire.get("type") == "tool_call":
        if (payload.get("providerId") != "workspace.agent_work"
                or payload.get("toolContractDigest") != work_contract_digest()):
            raise WorkQueryError(409, "agent_work_request_untrusted")
        try:
            validate_work_args(payload.get("normalizedInput"))
        except ValueError as error:
            raise WorkQueryError(409, "agent_work_request_untrusted") from error
    elif payload.get("resultState") not in (
            "successWithOutput", "successNoOutput", "successNoMatches", "failed", "denied", "aborted"):
        raise WorkQueryError(409, "agent_work_request_untrusted")
    return wire["turnId"]


@transaction.atomic
def query_work_request(body):
    # This boundary is entered outside application transactions. PostgreSQL
    # enforces no writes and keeps source/binding/receipt reads on one snapshot.
    with connection.cursor() as cursor:
        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
    try:
        caller = AgentRun.objects.select_related("session__agent", "authorization", "user").get(id=body["agentRunId"])
        digest = _signed_native_run(caller)
        agent = caller.session.agent
        if (digest != body["authorizationDigest"] or not agent_run_membership_is_current(caller)
                or not caller.user.is_active or caller.status not in {"queued", "running"}
                or agent.owner_id != caller.user_id or agent.workspace_id != caller.workspace_id
                or caller.session.owner_id != caller.user_id or caller.session.workspace_id != caller.workspace_id
                or body["coordinationSessionId"] != caller.session_id
                or not AgentCoordinationSession.objects.filter(agent=agent, session_id=caller.session_id).exists()
                or not session_authority_is_current(caller.user_id, caller.session_id, scope="sessions:read")):
            raise WorkQueryError(403, "agent_work_authority_rejected")
        source_run = AgentRun.objects.select_related("session__agent", "authorization").get(id=body["sourceAgentRunId"])
        _signed_native_run(source_run)
        if (source_run.session_id != caller.session_id or source_run.workspace_id != caller.workspace_id
                or source_run.user_id != caller.user_id
                or not session_authority_is_current(caller.user_id, source_run.session_id, scope="events:read")):
            raise WorkQueryError(403, "agent_work_authority_rejected")
    except (ObjectDoesNotExist, ValueError) as error:
        raise WorkQueryError(403, "agent_work_authority_rejected") from error

    call_id = body["sourceToolCallId"]
    records = list(SessionEvent.objects.filter(agent_run=source_run, payload__type__in=["tool_call", "tool_result"],
        payload__payload__callId=call_id).order_by("sequence"))
    bindings = list(AgentWorkSession.objects.select_related("operation").filter(source_run=source_run, source_call_id=call_id))
    turns = {_record(record, source_run, call_id) for record in records} | {b.source_turn_id for b in bindings}
    if len(turns) > 1:
        raise WorkQueryError(409, "agent_work_identity_ambiguous")
    response = {"schema": "workspace.agent_work.query_result.v1", "agentId": agent.id,
        "sessionId": caller.session_id, "agentRunId": caller.id, "toolCallId": body["toolCallId"],
        "authorizationDigest": digest, "sourceAgentRunId": source_run.id, "sourceToolCallId": call_id,
        "status": "notRecorded", "source": None, "operation": None, "work": None}
    if not turns:
        return response
    identity = {"sourceAgentRunId": source_run.id, "sourceTurnId": turns.pop(), "sourceToolCallId": call_id}
    calls, successes = [], []
    for record in records:
        payload = record.payload["payload"]
        if record.payload["type"] == "tool_call":
            calls.append(record)
        else:
            if not calls:
                raise WorkQueryError(409, "agent_work_request_untrusted")
            if payload.get("resultState") == "successWithOutput":
                successes.append(record)
    if calls and any(c.payload["payload"]["normalizedInput"] != calls[0].payload["payload"]["normalizedInput"] for c in calls):
        raise WorkQueryError(409, "agent_work_request_conflict")
    success = successes[0] if successes else None
    binding = bindings[0] if bindings else None
    if binding and (binding.coordination_session_id != source_run.session_id
            or success and binding.source_event_id != success.eventId):
        raise WorkQueryError(409, "agent_work_admission_conflict")
    source_event_id = success.eventId if success else binding.source_event_id if binding else None
    source_records = [success, next(c for c in reversed(calls) if c.sequence < success.sequence)] if success else ([] if binding else calls)
    response["source"] = {**identity, "sourceEventId": source_event_id,
        "projectsToAgentRunStream": all(r.projects_to_agent_run_stream for r in source_records) if source_records else None}
    key = work_operation_id(source_run.id, identity["sourceTurnId"], call_id)
    operation = HostedOperationReceipt.objects.filter(user=caller.user, workspace_id=caller.workspace_id,
        command="submitMessage", operationId=key).first()
    if binding and (operation is None or binding.operation_id != operation.pk):
        raise WorkQueryError(409, "agent_work_admission_conflict")
    if operation is None:
        response["status"] = "pending" if success else "notRecorded"
        return response
    if not source_event_id:
        raise WorkQueryError(409, "agent_work_request_incomplete")
    if success:
        request_digest = _digest("workspace.agent_work.request.v1", {**identity,
            "sourceEventId": success.eventId, "input": calls[0].payload["payload"]["normalizedInput"]}).removeprefix("sha256:")
        if operation.requestDigest != request_digest:
            raise WorkQueryError(409, "agent_work_admission_conflict")
    child = AgentRun.objects.select_related("session").filter(id=operation.agentRunId).first()
    if child is None:
        raise WorkQueryError(409, "agent_work_admission_incomplete")
    if (child.session_id != operation.sessionId or child.turn_id != operation.turnId
            or child.user_id != caller.user_id or child.workspace_id != caller.workspace_id
            or child.session.owner_id != caller.user_id or child.session.workspace_id != caller.workspace_id
            or child.session.agent_id != agent.id):
        raise WorkQueryError(403, "agent_work_authority_rejected")
    available = child.session.status == "active" and child.session.purgedAt is None
    if available and not session_authority_is_current(caller.user_id, child.session_id, scope="sessions:read"):
        raise WorkQueryError(403, "agent_work_authority_rejected")
    returns = []
    if available and binding is not None:
        from .agent_work_confirmation import query_work_return_views
        try:
            returns = query_work_return_views(binding, caller)
        except (ObjectDoesNotExist, ValueError, TypeError, KeyError) as error:
            raise WorkQueryError(403, "agent_work_return_authority_rejected") from error
    from .agent_work_output import completed_work_output
    from .agent_work_returns import WorkReturnError
    try:
        output = completed_work_output(child) if available and binding is not None else None
    except WorkReturnError as error:
        raise WorkQueryError(error.status, error.code) from error
    response.update(status="admitted", operation=serialize_operation(operation),
        work={"available": available, "agentRunStatus": child.status if available else None,
              "returns": returns, "output": output})
    return response
