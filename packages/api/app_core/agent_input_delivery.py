"""Admission preserves the carrier even when profile or lifecycle RPCs fail."""
import logging

from django.db import transaction

from . import runtime_client
from .agent_definitions import published_version_for_agent
from .agent_inputs import AgentInputError, owned_input_binding
from .agent_model_settings import AgentModelSettingsError, configured_agent_model
from .agent_run_authorization_factory import create_agent_run_authorization
from .business_agent_scope import agent_configuration, locked_agent_configuration
from .execution_admission import queued_admission_error
from .models import Agent, AgentInput, AgentInputDelivery, AgentInputQueue, AgentRun, Session
from .session_event import project_committed_agent_run
from .workspace_access import locked_workspace_membership_for


logger = logging.getLogger(__name__)
MAX_PENDING_AGENT_INPUTS = 256


def _locked_pending(agent_id):
    scope = Agent.objects.select_related("owner").get(pk=agent_id, status="active")
    membership = locked_workspace_membership_for(scope.owner, scope.workspace_id)
    if membership is None:
        raise AgentInputError(403, "agent_input_identity_revoked")
    agent = locked_agent_configuration(agent_id)
    binding = owned_input_binding(scope.owner, agent_id, scope="messages:submit")
    session = Session.objects.select_for_update().get(pk=binding.session_id)
    pending = list(AgentInput.objects.filter(agent=agent, session=session,
        membership_ref=membership.pk, delivery__isnull=True).order_by("sequence")[:MAX_PENDING_AGENT_INPUTS])
    active = list(session.agent_runs.filter(status__in=["queued", "running"]).order_by("createdAt", "id"))
    for run in active:
        project_committed_agent_run(run)
        run.refresh_from_db()
    active = [run for run in active if run.status in ["queued", "running"]]
    if len(active) > 1:
        raise AgentInputError(409, "agent_input_coordinator_fork_rejected")
    return agent, membership, session, pending, active[0] if active else None


def _bind_pending(run, pending, *, initial=None):
    authorization = runtime_client._validate_agent_run_binding(run)
    queue, _ = AgentInputQueue.objects.get_or_create(agent_run=run,
        defaults={"authorization_digest": authorization.digest, "initial_input": initial})
    if queue.authorization_digest != authorization.digest:
        raise AgentInputError(403, "agent_input_queue_authorization_rejected")
    if not queue.accepting:
        return False
    if any(fact.membership_ref != run.membership_ref for fact in pending):
        raise AgentInputError(403, "agent_input_identity_revoked")
    from .models import AgentWorkConsumeAttempt
    native = AgentWorkConsumeAttempt.objects.filter(coordinator_run=run,
        acknowledged_at_ms__isnull=True).count()
    capacity = MAX_PENDING_AGENT_INPUTS - queue.deliveries.filter(acknowledged_at_ms__isnull=True).count() - native
    captured = {item["inputRef"]: item for item in authorization.payload["assetRefs"]}
    for delivered in queue.deliveries.select_related("input"):
        for item in delivered.input.attachments:
            if item["inputRef"] in captured and captured[item["inputRef"]] != item:
                raise AgentInputError(409, "agent_input_attachment_identity_conflict")
            captured[item["inputRef"]] = item
    bound = 0
    for fact in pending[:max(0, capacity)]:
        proposed = dict(captured)
        for item in fact.attachments:
            if item["inputRef"] in proposed and proposed[item["inputRef"]] != item:
                raise AgentInputError(409, "agent_input_attachment_identity_conflict")
            proposed[item["inputRef"]] = item
        from .runtime_contract import MAX_DECLARED_INPUTS
        if (len(proposed) > MAX_DECLARED_INPUTS or sum(item["sizeBytes"] for item in proposed.values())
                > authorization.payload["resources"]["dataTmpfsBytes"] // 2):
            # The accepted carrier remains pending for the next serialized Run;
            # admitting it here would exceed the existing sandbox resource grant.
            break
        if AgentWorkConsumeAttempt.objects.filter(coordinator_run=run, pk=fact.input_id).exists():
            raise AgentInputError(409, "agent_input_identity_collision")
        AgentInputDelivery.objects.create(input=fact, queue=queue)
        captured, bound = proposed, bound + 1
    return bool(bound)


def dispatch_agent_inputs(agent_id):
    """A second locked admission rechecks policy and the owner after profile I/O."""
    if transaction.get_connection().in_atomic_block:
        raise AgentInputError(409, "agent_input_dispatch_requires_committed_fact")
    with transaction.atomic():
        agent, membership, session, pending, active = _locked_pending(agent_id)
        if not pending:
            return None
        if active is not None:
            run = active if _bind_pending(active, pending) else None
        else:
            configured_agent_model(agent)
            run = None
    if active is None:
        profile = runtime_client.request_execution_profile()
        with transaction.atomic():
            agent, membership, session, pending, active = _locked_pending(agent_id)
            if not pending:
                return None
            if active is not None:
                run = active if _bind_pending(active, pending) else None
            else:
                model, mode = configured_agent_model(agent)
                admission = queued_admission_error(agent.workspace_id)
                if admission is not None:
                    raise AgentInputError(*admission)
                version = published_version_for_agent(agent, membership)
                initial = pending[0]
                run = AgentRun.objects.create(workspace=agent.workspace, user=agent.owner,
                    session=session, modelConfig=model, thinkingMode=mode, prompt=initial.body,
                    definition_version=version,
                    agent_instructions=version.instructions if version else agent_configuration(agent).instructions)
                create_agent_run_authorization(run, [item["inputRef"] for item in initial.attachments],
                    image_digest=profile["imageDigest"], declared_inputs=initial.attachments)
                _bind_pending(run, pending, initial=initial)
    if run is not None:
        try:
            runtime_client.schedule_agent_run_lifecycle(run)
        except Exception:
            logger.exception("Committed Agent input admission awaits lifecycle reconciliation")
            AgentRun.objects.filter(pk=run.pk, status="queued").update(transitionReason="agent_run_schedule_pending")
    return run


def try_dispatch_agent_inputs(agent_id):
    try:
        return dispatch_agent_inputs(agent_id)
    except AgentModelSettingsError:
        # An unconfigured owner policy leaves the accepted carrier pending.
        return None
    except Exception:
        logger.exception("Retained Agent inputs await coordinator admission", extra={"agentId": agent_id})
        return None


def initial_agent_input(run):
    queue = AgentInputQueue.objects.select_related("initial_input").filter(agent_run=run).first()
    if queue is None or queue.initial_input_id is None:
        return None
    fact = queue.initial_input
    if (queue.authorization_digest != run.authorization.digest or fact.session_id != run.session_id
            or fact.membership_ref != run.membership_ref or fact.body != run.prompt
            or not AgentInputDelivery.objects.filter(input=fact, queue=queue).exists()):
        raise ValueError("agent_input_initial_binding_rejected")
    return {"type": "userInput", "inputId": fact.input_id, "message": fact.body,
        "attachmentRefs": [item["inputRef"] for item in fact.attachments]}
