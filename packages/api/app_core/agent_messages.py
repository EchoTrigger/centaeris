"""Hosted message authority and rebuildable projection of committed Core facts."""
import hashlib
import json
from pathlib import Path

from django.conf import settings
from django.core.exceptions import ObjectDoesNotExist
from django.db import transaction

from .app_delegations import DelegationRejected, require_run_delegation, session_authority_is_current
from .assets import DeferredInputResolutionError
from .business_agent_scope import run_session_ref_is_current
from .deferred_input import DeferredInputBindingError, input_storage_batch
from .models import AgentCoordinationSession, AgentRun, SessionEvent
from .runtime_contract import (
    agent_run_binding_matches, authorization_digest, require_opaque_ref,
    _verify_authorization_digest_signature,
)
from .workspace_access import agent_run_membership_is_current


_CONTRACT = json.loads((Path(__file__).parent / "contracts/send_message.json").read_text(encoding="utf-8"))
VALIDATED_SCHEMA = "workspace.agent_message.validated.v1"


class AgentMessageRejected(ValueError):
    pass


def _digest(domain, value):
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return "sha256:" + hashlib.sha256(domain.encode("ascii") + b"\0" + encoded).hexdigest()


def message_contract_digest():
    return _digest("centaeris.dynamic_tool_contract.v1", _CONTRACT)


def validate_message_args(args):
    if not isinstance(args, dict) or set(args) != {"body", "session_refs", "file_refs"}:
        raise ValueError("agent_message_input_invalid")
    body = args["body"]
    if not isinstance(body, str) or not body.strip() or len(body.encode("utf-8")) > 65536:
        raise ValueError("agent_message_body_invalid")
    for field in ("session_refs", "file_refs"):
        refs = args[field]
        if not isinstance(refs, list) or len(refs) > 8:
            raise ValueError("agent_message_refs_invalid")
        for ref in refs:
            require_opaque_ref(field, ref)
    return args


def validation_record(run, tool_call_id, args):
    # Diagnostic validation is not a delivery record. Only a matching successful
    # committed tool_result can cause a bubble; the body always comes from input.
    return {"schema": VALIDATED_SCHEMA, "agentId": run.session.agent_id,
            "sessionId": run.session_id, "agentRunId": run.id, "toolCallId": tool_call_id,
            "authorizationDigest": run.authorization.digest,
            "inputDigest": _digest("workspace.agent_message.input.v1", args)}


@transaction.atomic
def validate_agent_message(body):
    args = validate_message_args({"body": body["body"], "session_refs": body["sessionRefs"],
                                  "file_refs": body["fileRefs"]})
    try:
        run = AgentRun.objects.select_related("session__agent", "authorization", "user").get(id=body["agentRunId"])
        authorization = run.authorization
        digest = authorization_digest(authorization.payload)
        _verify_authorization_digest_signature(digest, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY,
                                               authorization.signature)
        if (digest != authorization.digest or digest != body["authorizationDigest"]
                or not agent_run_binding_matches(authorization.payload, run)
                or not agent_run_membership_is_current(run)
                or run.status not in {"queued", "running"}
                or body["coordinationSessionId"] != run.session_id
                or not AgentCoordinationSession.objects.filter(agent_id=run.session.agent_id, session_id=run.session_id).exists()
                or not session_authority_is_current(run.user_id, run.session_id, run.app_delegation_id, "messages:submit")):
            raise AgentMessageRejected("agent_message_authority_rejected")
        require_run_delegation(run)
        for session_ref in args["session_refs"]:
            if not run_session_ref_is_current(run, session_ref):
                raise AgentMessageRejected("agent_message_session_ref_rejected")
        resolve = input_storage_batch(run, digest)
        for input_ref in args["file_refs"]:
            resolve(input_ref)
        return validation_record(run, body["toolCallId"], args)
    except (ObjectDoesNotExist, DelegationRejected, DeferredInputResolutionError,
            DeferredInputBindingError, ValueError) as error:
        raise AgentMessageRejected("agent_message_authority_rejected") from error


def _trusted_message(binding, result, *, call=..., attach_work=True, work_snapshot=None):
    from .session_payload import decode_session_payload
    wire = decode_session_payload(result.payload)
    run = result.agent_run
    payload = wire.get("payload", {})
    if (wire.get("sessionId") != binding.session_id or wire.get("agentRunId") != run.id
            or run.session_id != binding.session_id or run.user_id != binding.agent.owner_id
            or run.workspace_id != binding.agent.workspace_id
            or wire.get("eventId") != result.eventId or wire.get("sequence") != result.sequence
            or not isinstance(wire.get("turnId"), str) or not wire["turnId"]
            or payload.get("toolName") != "send_message" or payload.get("resultState") != "successWithOutput"):
        return None
    call_id = payload.get("callId")
    if not isinstance(call_id, str) or not call_id:
        return None
    if call is ...:
        call = SessionEvent.objects.filter(session_id=binding.session_id, agent_run_id=run.id,
            projects_to_agent_run_stream=True, sequence__lt=result.sequence, payload__type="tool_call",
            payload__turnId=wire["turnId"], payload__payload__callId=call_id).order_by("-sequence").first()
    if call is None:
        return None
    call_wire = decode_session_payload(call.payload)
    input_payload = call_wire.get("payload", {})
    if (call_wire.get("sessionId") != binding.session_id or call_wire.get("agentRunId") != run.id
            or call_wire.get("eventId") != call.eventId or call_wire.get("sequence") != call.sequence
            or input_payload.get("toolName") != "send_message"
            or input_payload.get("providerId") != _CONTRACT["providerId"]
            or input_payload.get("toolContractDigest") != message_contract_digest()):
        return None
    try:
        args = validate_message_args(input_payload.get("normalizedInput"))
    except (ObjectDoesNotExist, ValueError, TypeError, UnicodeError):
        return None
    refs = list(args["session_refs"])
    if attach_work:
        from .agent_work_presentation import work_refs_for_message
        refs = list(dict.fromkeys(refs + work_refs_for_message(binding, result, snapshot=work_snapshot)))
    return {"id": result.eventId, "agentRunId": run.id, "turnId": wire["turnId"],
            "toolCallId": call_id, "sourceSequence": result.sequence, "createdAtMs": result.createdAtMs,
            "body": args["body"], "sessionRefs": refs, "fileRefs": args["file_refs"]}


def committed_agent_messages(binding, after_sequence=0, limit=50):
    # Pages are bounded by successful source results. Ignored/foreign records
    # still advance the cursor, so no page can loop or scan unbounded history.
    results = list(SessionEvent.objects.filter(session_id=binding.session_id,
        workspace_id=binding.agent.workspace_id, projects_to_agent_run_stream=True,
        sequence__gt=after_sequence, payload__type="tool_result", payload__payload__toolName="send_message",
        payload__payload__resultState="successWithOutput").select_related(
            "agent_run__session__agent").order_by("sequence")[:limit + 1])
    messages, seen = [], set()
    for result in results[:limit]:
        message = _trusted_message(binding, result)
        if message is not None:
            identity = (message["agentRunId"], message["turnId"], message["toolCallId"])
            if identity not in seen:
                seen.add(identity)
                messages.append(message)
    return messages, results[limit - 1].sequence if len(results) > limit else None
