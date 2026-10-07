"""Materialize committed private dispatch requests into independent hosted admission."""
import hashlib
import json
import logging
from pathlib import Path

from django.conf import settings
from django.core.exceptions import ObjectDoesNotExist
from django.db import DatabaseError, connection, transaction

from .agent_messages import _digest, validate_message_args
from .agent_run_authorization_factory import create_agent_run_authorization
from .app_delegations import session_authority_is_current
from .assets import DeferredInputResolutionError
from .business_agent_scope import branch_for_agent, run_session_ref_is_current
from .deferred_input import DeferredInputBindingError, input_storage_batch
from .execution_admission import queued_admission_error
from .hosted_operations import accept_operation, replay_operation, serialize_operation
from .models import Agent, AgentCoordinationSession, AgentRun, AgentWorkSession, Session, SessionAssetLink, SessionEvent
from .runtime_client import request_execution_profile, schedule_agent_run_lifecycle
from .runtime_contract import agent_run_binding_matches, authorization_digest, _verify_authorization_digest_signature
from .workspace_access import agent_run_membership_is_current, locked_workspace_membership_for


logger = logging.getLogger(__name__)
_CONTRACT = json.loads((Path(__file__).parent / "contracts/dispatch_work.json").read_text(encoding="utf-8"))


class WorkMaterializationError(ValueError):
    def __init__(self, status, code):
        super().__init__(code)
        self.status, self.code = status, code


def work_contract_digest():
    return _digest("centaeris.dynamic_tool_contract.v1", _CONTRACT)


def work_operation_id(run_id, turn_id, call_id):
    identity = {"sourceAgentRunId": run_id, "sourceTurnId": turn_id, "sourceToolCallId": call_id}
    return "agent-work:" + hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def _source_request(event_id):
    try:
        result = SessionEvent.objects.select_related("agent_run__authorization", "agent_run__user",
            "agent_run__session__agent").get(eventId=event_id)
    except SessionEvent.DoesNotExist as error:
        raise WorkMaterializationError(404, "agent_work_request_not_found") from error
    run, wire = result.agent_run, result.payload
    payload = wire.get("payload", {})
    if (not result.projects_to_agent_run_stream or result.session_level or wire.get("type") != "tool_result"
            or wire.get("eventId") != result.eventId or wire.get("sequence") != result.sequence
            or wire.get("sessionId") != run.session_id or result.session_id != run.session_id
            or wire.get("agentRunId") != run.id or result.workspace_id != run.workspace_id
            or payload.get("toolName") != "dispatch_work" or payload.get("resultState") != "successWithOutput"
            or not isinstance(wire.get("turnId"), str) or not wire["turnId"]
            or not isinstance(payload.get("callId"), str) or not payload["callId"]):
        raise WorkMaterializationError(409, "agent_work_request_untrusted")
    call = SessionEvent.objects.filter(session_id=run.session_id, agent_run_id=run.id, session_level=False,
        projects_to_agent_run_stream=True, sequence__lt=result.sequence, payload__type="tool_call",
        payload__turnId=wire["turnId"], payload__payload__callId=payload["callId"]).order_by("-sequence").first()
    args = call.payload.get("payload", {}).get("normalizedInput") if call else None
    if (call is None or call.workspace_id != run.workspace_id
            or call.payload.get("eventId") != call.eventId or call.payload.get("sequence") != call.sequence
            or call.payload.get("sessionId") != run.session_id or call.payload.get("agentRunId") != run.id
            or call.payload["payload"].get("toolName") != "dispatch_work"
            or call.payload["payload"].get("providerId") != _CONTRACT["providerId"]
            or call.payload["payload"].get("toolContractDigest") != work_contract_digest()
            or not isinstance(args, dict) or set(args) != {"objective", "session_refs", "file_refs"}):
        raise WorkMaterializationError(409, "agent_work_request_untrusted")
    try:
        validate_message_args({"body": args["objective"], "session_refs": args["session_refs"], "file_refs": args["file_refs"]})
    except ValueError as error:
        raise WorkMaterializationError(409, "agent_work_request_untrusted") from error
    return result, run, args


def _locked_source(event_id):
    result, run, args = _source_request(event_id)
    membership = locked_workspace_membership_for(run.user, run.workspace_id)
    agent = Agent.objects.select_for_update().get(pk=run.session.agent_id)
    # Keep hosted Workspace/membership/Agent/Session lock order. Runtime append
    # holds Session before deferred Workspace FK checks, so never wait here.
    try:
        Session.objects.select_for_update(nowait=True).get(pk=run.session_id)
    except DatabaseError as error:
        if getattr(error.__cause__, "sqlstate", None) != "55P03":
            raise
        raise WorkMaterializationError(503, "agent_work_source_busy") from error
    # Rewrite may have committed after the unlocked lookup. Hold the same
    # Session lock as Runtime through source validation and child admission.
    result, run, args = _source_request(event_id)
    if (membership is None or not agent_run_membership_is_current(run)
            or agent.owner_id != run.user_id or agent.workspace_id != run.workspace_id
            or run.session.owner_id != run.user_id or run.session.workspace_id != run.workspace_id or not run.user.is_active
            or not AgentCoordinationSession.objects.filter(agent=agent, session_id=run.session_id).exists()
            or not session_authority_is_current(run.user_id, run.session_id, scope="messages:submit")):
        raise WorkMaterializationError(403, "agent_work_authority_rejected")
    # This first consumer supports native private Agents only; no delegated
    # execution or shared-definition permissions/credentials are inherited.
    if agent.definition_id or run.acting_app_id or run.app_delegation_id:
        raise WorkMaterializationError(403, "agent_work_source_not_supported")
    try:
        authorization = run.authorization
        digest = authorization_digest(authorization.payload)
        _verify_authorization_digest_signature(digest, settings.AGENT_RUN_AUTHORIZATION_SIGNING_KEY, authorization.signature)
        if digest != authorization.digest or not agent_run_binding_matches(authorization.payload, run):
            raise ValueError("source authorization mismatch")
    except (ObjectDoesNotExist, ValueError) as error:
        raise WorkMaterializationError(403, "agent_work_authority_rejected") from error
    first = SessionEvent.objects.filter(session_id=run.session_id, agent_run_id=run.id, session_level=False,
        projects_to_agent_run_stream=True, payload__type="tool_result", payload__turnId=result.payload["turnId"],
        payload__payload__toolName="dispatch_work", payload__payload__callId=result.payload["payload"]["callId"],
        payload__payload__resultState="successWithOutput").order_by("sequence").first()
    if first.eventId != result.eventId:
        original, _, original_args = _source_request(first.eventId)
        if original_args != args:
            raise WorkMaterializationError(409, "agent_work_request_conflict")
        result = original
    identity = {"sourceAgentRunId": run.id, "sourceTurnId": result.payload["turnId"],
                "sourceToolCallId": result.payload["payload"]["callId"]}
    key = work_operation_id(run.id, result.payload["turnId"], result.payload["payload"]["callId"])
    digest = _digest("workspace.agent_work.request.v1",
        {**identity, "sourceEventId": result.eventId, "input": args}).removeprefix("sha256:")
    return result, run, args, key, digest


def _binding(result, run, operation):
    return dict(session_id=operation.sessionId, coordination_session_id=run.session_id, source_run=run,
        source_event_id=result.eventId, source_turn_id=result.payload["turnId"],
        source_call_id=result.payload["payload"]["callId"], operation=operation)


def _response(binding):
    return {"schema": "agent.work.materialized.v1", "sourceEventId": binding.source_event_id,
            "operation": serialize_operation(binding.operation)}


def _replay(result, run, key, digest):
    operation = replay_operation(run.user, run.workspace_id, "submitMessage", key, digest)
    if operation is None:
        return None
    child = AgentRun.objects.select_related("session").get(pk=operation.agentRunId)
    if child.session.agent_id != run.session.agent_id:
        raise WorkMaterializationError(409, "agent_work_admission_conflict")
    binding = AgentWorkSession.objects.filter(operation=operation).first()
    if binding is None:
        binding = AgentWorkSession.objects.create(**_binding(result, run, operation))
    if (binding.source_run_id != run.id or binding.source_turn_id != result.payload["turnId"]
            or binding.source_call_id != result.payload["payload"]["callId"]):
        raise WorkMaterializationError(409, "agent_work_admission_conflict")
    return _response(binding)


def materialize_work_request(event_id):
    # A producer cannot use its source append transaction to start child work.
    if connection.in_atomic_block:
        raise WorkMaterializationError(409, "agent_work_requires_committed_source")
    with transaction.atomic():
        result, run, args, key, digest = _locked_source(event_id)
        replay = _replay(result, run, key, digest)
        if replay is not None:
            return replay, False
    profile = request_execution_profile()
    with transaction.atomic():
        result, run, args, key, digest = _locked_source(event_id)
        replay = _replay(result, run, key, digest)
        if replay is not None:
            return replay, False
        model = run.modelConfig
        if (not model.enabled or not model.isCurrent
                or run.thinkingMode and run.thinkingMode not in model.thinkingModes):
            raise WorkMaterializationError(403, "agent_work_model_unavailable")
        for ref in args["session_refs"]:
            if not run_session_ref_is_current(run, ref):
                raise WorkMaterializationError(403, "agent_work_session_ref_rejected")
        selected_inputs = list(SessionAssetLink.objects.select_related("userLibraryObject", "sourceObject")
            .filter(session=run.session, id__in=args["file_refs"]))
        try:
            resolve = input_storage_batch(run, run.authorization.digest)
            for ref in args["file_refs"]:
                resolve(ref)
        except (DeferredInputResolutionError, DeferredInputBindingError) as error:
            raise WorkMaterializationError(403, "agent_work_file_ref_rejected") from error
        if any(link.artifact_id for link in selected_inputs):
            raise WorkMaterializationError(403, "agent_work_file_scope_not_supported")
        admission = queued_admission_error(run.workspace_id)
        if admission is not None:
            raise WorkMaterializationError(*admission)
        work = Session.objects.create(workspace_id=run.workspace_id, owner=run.user, agent=run.session.agent,
                                      origin="automation", title=args["objective"][:80])
        refs = []
        for link in selected_inputs:
            selected = SessionAssetLink.objects.create(workspace_id=run.workspace_id, session=work,
                userLibraryObject=link.userLibraryObject, sourceObject=link.sourceObject, attachedBy=run.user,
                capturedDisplayName=link.capturedDisplayName, capturedContentType=link.capturedContentType,
                capturedOwnerKind=link.capturedOwnerKind, capturedOwnerId=link.capturedOwnerId,
                capturedContentGeneration=link.capturedContentGeneration, capturedSizeBytes=link.capturedSizeBytes,
                capturedSha256=link.capturedSha256)
            refs.append(selected.id)
        child = AgentRun.objects.create(workspace_id=run.workspace_id, session=work, user=run.user,
            modelConfig=run.modelConfig, thinkingMode=run.thinkingMode, prompt=args["objective"],
            agent_instructions=run.agent_instructions if branch_for_agent(run.session.agent_id) else work.agent.instructions)
        create_agent_run_authorization(child, message_asset_refs=refs, image_digest=profile["imageDigest"])
        operation = accept_operation(run.user, run.workspace, "submitMessage", key, digest, work, child)
        binding = AgentWorkSession.objects.create(**_binding(result, run, operation))
        response = _response(binding)
    try:
        schedule_agent_run_lifecycle(child)
    except Exception:
        logger.exception("Agent work admission awaits existing lifecycle reconciliation")
        AgentRun.objects.filter(pk=child.pk, status="queued").update(transitionReason="agent_run_lifecycle_schedule_pending")
    return response, True
