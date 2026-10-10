"""First admission of one immutable work return to its native coordinator."""
import json
import logging
import time

from django.core.exceptions import ObjectDoesNotExist
from django.db import connection, transaction
from django.db.models import Max, Q

from .agent_messages import _digest
from .agent_model_settings import AgentModelSettingsError, configured_agent_model
from .agent_run_authorization_factory import create_agent_run_authorization
from .agent_work_query import _signed_native_run
from .agent_work_returns import _bound_work, _payload, WorkReturnError
from .app_delegations import session_authority_is_current
from .business_agent_scope import agent_configuration, locked_agent_configuration
from .execution_admission import queued_admission_error
from .hosted_operations import accept_operation, replay_operation, serialize_operation
from .models import AgentCoordinationSession, AgentInputQueue, AgentRun, AgentWorkConsumeAttempt, AgentWorkReturn, Session
from .runtime_client import request_execution_profile, request_host_event_input, schedule_agent_run_lifecycle
from .workspace_access import agent_run_membership_is_current, locked_workspace_membership_for


logger = logging.getLogger(__name__)


def automatic_consume_operation_id(notice_id):
    return "agent-work-consume:" + _digest("workspace.agent_work.automatic_consume.v1",
        {"noticeId": notice_id}).removeprefix("sha256:")


@transaction.atomic
def discover_pending_work_returns(after, through, limit):
    """Page committed notices; cursors and operation IDs grant no authority."""
    with connection.cursor() as cursor:
        cursor.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
    rows = AgentWorkReturn.objects.filter(Q(first_consume__isnull=True) | Q(first_consume__coordinator_run__isnull=True)).order_by("id")
    if through is None:
        through = rows.values_list("id", flat=True).last()
    if through is None:
        return {"schema": "workspace.agent_work.consume_discovered.v1", "entries": [], "through": None, "next": None}
    rows = rows.filter(id__lte=through)
    if after is not None:
        rows = rows.filter(id__gt=after)
    page = list(rows.values_list("id", flat=True)[:limit + 1])
    return {"schema": "workspace.agent_work.consume_discovered.v1", "through": through,
        "next": page[limit-1] if len(page) > limit else None,
        "entries": [{"cursor": notice_id, "noticeId": notice_id,
                     "operationId": automatic_consume_operation_id(notice_id)} for notice_id in page[:limit]]}


def _locked_notice(notice_id):
    try:
        notice = AgentWorkReturn.objects.select_related("work__source_run__user", "work__source_run__session").get(pk=notice_id)
        source = notice.work.source_run
        membership = locked_workspace_membership_for(source.user, source.workspace_id)
        agent = locked_agent_configuration(source.session.agent_id)
        session = Session.objects.select_for_update().get(pk=source.session_id)
        work, child = _bound_work(notice.child_run_id)
        source = work.source_run
        # Current membership and work identity.
        if (membership is None or work.pk != notice.work_id or not source.user.is_active
                or not agent_run_membership_is_current(source) or not agent_run_membership_is_current(child)
                or source.authorization.payload["thinkingMode"] != (source.thinkingMode or None)
                or child.authorization.payload["thinkingMode"] != (child.thinkingMode or None)):
            raise WorkReturnError(403, "agent_work_consume_authority_rejected")
        # Active owned resources.
        if (agent.status != "active" or session.status != "active"
                or agent.owner_id != source.user_id or agent.workspace_id != source.workspace_id
                or session.owner_id != source.user_id or session.workspace_id != source.workspace_id):
            raise WorkReturnError(403, "agent_work_consume_authority_rejected")
        # Native coordination binding.
        if (agent.definition_id or source.definition_version_id or child.definition_version_id
                or not AgentCoordinationSession.objects.filter(agent=agent, session=session).exists()):
            raise WorkReturnError(403, "agent_work_consume_authority_rejected")
        # Current source submit and child read access.
        if (not session_authority_is_current(source.user_id, session.id, scope="messages:submit")
                or not session_authority_is_current(source.user_id, child.session_id, scope="events:read")):
            raise WorkReturnError(403, "agent_work_consume_authority_rejected")
        # Immutable delivered content and fact identity.
        if (notice.payload != _payload(work, child, notice.payload["terminal"])
                or notice.fact_kind != notice.payload["terminal"]["kind"]
                or notice.fact_ref != notice.payload["terminal"]["factRef"]):
            raise WorkReturnError(403, "agent_work_consume_authority_rejected")
        return notice, source, agent, session
    except WorkReturnError:
        raise
    except (ObjectDoesNotExist, ValueError, TypeError, KeyError, AttributeError) as error:
        raise WorkReturnError(403, "agent_work_consume_authority_rejected") from error


def bound_host_input(attempt, run):
    """A lifecycle record reference rehydrates only its committed admission binding."""
    binding = attempt.input_binding
    expected = {"schema", "agentRunId", "turnId", "sessionId", "authorizationDigest", "initialInput"}
    initial = binding.get("initialInput", {})
    # Binding structure precedes field access.
    if set(binding) != expected or binding["schema"] != "workspace.agent_work.consume_binding.v1":
        raise ValueError("agent_work_consume_binding_rejected")
    # Accepted Run identity.
    if (binding["agentRunId"] != run.id or binding["turnId"] != run.turn_id
            or binding["sessionId"] != run.session_id or binding["authorizationDigest"] != run.authorization.digest):
        raise ValueError("agent_work_consume_binding_rejected")
    # The coordinator keeps its own admitted authorization/model snapshot.
    # Neither the source Run nor a later Agent setting can replace that snapshot.
    if (_signed_native_run(run) != binding["authorizationDigest"]
            or run.authorization.payload["thinkingMode"] != (run.thinkingMode or None)):
        raise ValueError("agent_work_consume_binding_rejected")
    # Initial input structure and admission identity.
    if (set(initial) != {"type", "attemptId", "noticeId", "input"}
            or initial["type"] != "hostEvent" or initial["attemptId"] != attempt.pk
            or initial["noticeId"] != attempt.notice_id):
        raise ValueError("agent_work_consume_binding_rejected")
    # Native input structure and exact notice content.
    if (set(initial["input"]) != {"inputId", "messageId", "source", "content"}
            or initial["input"]["inputId"] != attempt.pk
            or initial["input"]["source"] != attempt.notice.payload["source"]
            or json.loads(initial["input"]["content"]) != attempt.notice.payload):
        raise ValueError("agent_work_consume_binding_rejected")
    # Hosted operation ownership and result.
    if (attempt.operation.command != "consumeWorkReturn"
            or (attempt.operation.agentRunId is not None and
                (attempt.operation.agentRunId != run.id or attempt.operation.turnId != run.turn_id))
            or attempt.operation.sessionId != run.session_id
            or attempt.operation.user_id != run.user_id or attempt.operation.workspace_id != run.workspace_id
            or attempt.operation.acting_app_id or attempt.operation.app_delegation_id):
        raise ValueError("agent_work_consume_binding_rejected")
    # Original coordinator and accepted objective.
    if (attempt.notice.work.coordination_session_id != run.session_id
            or run.tailPolicy != "append"):
        raise ValueError("agent_work_consume_binding_rejected")
    return initial


def bound_initial_input(attempt, run):
    initial = bound_host_input(attempt, run)
    if not AgentInputQueue.objects.filter(agent_run=run, initial_host=attempt, initial_input__isnull=True).exists():
        raise ValueError("agent_work_consume_initial_owner_rejected")
    if run.prompt != attempt.notice.work.source_run.prompt:
        raise ValueError("agent_work_consume_initial_objective_rejected")
    return initial


def _response(attempt):
    return {"schema": "workspace.agent_work.consumed.v1", "disposition": "accepted",
            "noticeId": attempt.notice_id, "attemptId": attempt.pk,
            "operation": serialize_operation(attempt.operation)}


def _existing(notice, source, operation_id, digest):
    receipt = replay_operation(source.user, source.workspace_id, "consumeWorkReturn", operation_id, digest)
    attempt = AgentWorkConsumeAttempt.objects.select_related("coordinator_run__authorization", "notice__work", "operation").filter(notice=notice).first()
    if receipt is not None and (attempt is None or attempt.operation_id != receipt.pk):
        raise WorkReturnError(409, "agent_work_consume_conflict")
    if attempt is not None:
        if attempt.coordinator_run_id is None:
            binding = attempt.input_binding
            initial = binding["initialInput"]
            expected = {"schema": "workspace.agent_work.consume_binding.v1", "agentRunId": None,
                "turnId": None, "sessionId": source.session_id, "authorizationDigest": None,
                "initialInput": initial}
            native = initial["input"]
            if (binding != expected or set(initial) != {"type", "attemptId", "noticeId", "input"}
                    or initial["type"] != "hostEvent" or initial["attemptId"] != attempt.pk
                    or initial["noticeId"] != notice.pk
                    or set(native) != {"inputId", "messageId", "source", "content"}
                    or native["inputId"] != attempt.pk or native["source"] != notice.payload["source"]
                    or json.loads(native["content"]) != notice.payload
                    or attempt.operation.agentRunId is not None or attempt.operation.turnId is not None
                    or attempt.operation.command != "consumeWorkReturn"
                    or attempt.operation.sessionId != source.session_id
                    or attempt.operation.user_id != source.user_id
                    or attempt.operation.workspace_id != source.workspace_id
                    or attempt.operation.requestDigest != digest
                    or attempt.operation.acting_app_id or attempt.operation.app_delegation_id):
                raise ValueError("agent_work_consume_binding_rejected")
            return attempt
        if not agent_run_membership_is_current(attempt.coordinator_run):
            raise WorkReturnError(403, "agent_work_consume_authority_rejected")
        if AgentInputQueue.objects.filter(agent_run_id=attempt.coordinator_run_id, initial_host=attempt).exists():
            bound_initial_input(attempt, attempt.coordinator_run)
        else:
            bound_host_input(attempt, attempt.coordinator_run)
        return _response(attempt)
    return None


def _coordinator_model(agent):
    try:
        return configured_agent_model(agent)
    except AgentModelSettingsError as error:
        code = "agent_model_not_configured" if str(error) == "agent_model_not_configured" else "agent_model_not_available"
        raise WorkReturnError(409, code) from error



def _active(session):
    from .session_event import project_committed_agent_run
    runs = list(session.agent_runs.filter(status__in=["queued", "running"]).order_by("createdAt", "id"))
    for run in runs:
        project_committed_agent_run(run)
        run.refresh_from_db()
    runs = [run for run in runs if run.status in {"queued", "running"}]
    if len(runs) > 1:
        raise WorkReturnError(409, "agent_input_coordinator_fork_rejected")
    return runs[0] if runs else None


def _queue(run):
    from .runtime_client import _validate_agent_run_binding
    auth = _validate_agent_run_binding(run)
    if not agent_run_membership_is_current(run):
        raise WorkReturnError(403, "agent_work_consume_authority_rejected")
    queue, _ = AgentInputQueue.objects.get_or_create(agent_run=run,
        defaults={"authorization_digest": auth.digest})
    if queue.authorization_digest != auth.digest:
        raise WorkReturnError(403, "agent_input_queue_authorization_rejected")
    return queue


def _has_capacity(queue):
    native = AgentWorkConsumeAttempt.objects.filter(coordinator_run_id=queue.pk,
        acknowledged_at_ms__isnull=True).count()
    return native + queue.deliveries.filter(acknowledged_at_ms__isnull=True).count() < 256


def _bind(attempt, run):
    if attempt.coordinator_run_id is not None:
        raise ValueError("agent_work_consume_owner_already_bound")
    from .models import AgentInputDelivery
    if AgentInputDelivery.objects.filter(queue_id=run.pk, input__input_id=attempt.pk).exists():
        raise ValueError("agent_input_identity_collision")
    binding = {**attempt.input_binding, "agentRunId": run.pk, "turnId": run.turn_id,
               "authorizationDigest": run.authorization.digest}
    # Acceptance content and receipt never change. Only the previously absent
    # Run ownership is assigned, under the same Session/queue admission fence.
    changed = AgentWorkConsumeAttempt.objects.filter(pk=attempt.pk, coordinator_run__isnull=True).update(
        coordinator_run=run, input_binding=binding)
    if changed != 1:
        raise ValueError("agent_work_consume_owner_conflict")
    attempt.coordinator_run, attempt.input_binding = run, binding
    bound_host_input(attempt, run)


def _initial_objective(session, source):
    pending = AgentWorkConsumeAttempt.objects.filter(coordinator_run__isnull=True,
        notice__work__coordination_session=session).order_by("delivery_sequence")[:256]
    for waiting in pending:
        try:
            _, original, _, _ = _locked_notice(waiting.notice_id)
            return original.prompt
        except WorkReturnError as error:
            if error.status != 403:
                raise
    return source.prompt


def consume_work_return(notice_id, operation_id):
    if connection.in_atomic_block:
        raise WorkReturnError(409, "agent_work_consume_requires_committed_notice")
    digest = _digest("workspace.agent_work.consume.v1", {"noticeId": notice_id}).removeprefix("sha256:")
    with transaction.atomic():
        notice, source, agent, session = _locked_notice(notice_id)
        existing = _existing(notice, source, operation_id, digest)
        if isinstance(existing, dict):
            return existing, 200
        active = _active(session)
        need_profile = active is None
    # Pending accepted notices reuse the immutable input; no factory call or
    # new acceptance is needed for handoff. Idle admission uses current policy.
    profile = request_execution_profile() if need_profile else None
    candidate = existing if existing is not None else AgentWorkConsumeAttempt()
    if existing is None:
        content = json.dumps(notice.payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        native_input = request_host_event_input(session.id, candidate.id, notice.payload["source"], content)
    scheduled = None
    with transaction.atomic():
        notice, source, agent, session = _locked_notice(notice_id)
        current = _existing(notice, source, operation_id, digest)
        if isinstance(current, dict):
            return current, 200
        if current is not None:
            candidate = current
        active = _active(session)
        run = active
        initial = False
        # Retain the completed work's pending carrier and acceptance when no
        # new Run fits. A later binding reuses this same receipt and attempt.
        if (active is None and profile is not None
                and queued_admission_error(source.workspace_id) is None):
            # User carriers win idle admission too, using the existing binder.
            from .models import AgentInput
            pending_users = list(AgentInput.objects.filter(agent=agent, session=session,
                membership_ref=source.membership_ref, delivery__isnull=True).order_by("sequence")[:256])
            model, mode = _coordinator_model(agent)
            run = AgentRun.objects.create(workspace_id=source.workspace_id, user=source.user, session=session,
                modelConfig=model, thinkingMode=mode, prompt=pending_users[0].body if pending_users else _initial_objective(session, source),
                agent_instructions=agent_configuration(agent).instructions)
            create_agent_run_authorization(run, image_digest=profile["imageDigest"])
            if pending_users:
                from .agent_input_delivery import _bind_pending
                _bind_pending(run, pending_users, initial=pending_users[0])
            else:
                _queue(run)
                initial = True
            scheduled = run
        queue = _queue(run) if run is not None else None
        bindable = queue is not None and queue.accepting and _has_capacity(queue)
        if current is None:
            receipt = accept_operation(source.user, source.workspace, "consumeWorkReturn", operation_id, digest,
                session, run if bindable else None)
            candidate.notice, candidate.operation = notice, receipt
            candidate.coordinator_run = run if bindable else None
            sequence = AgentWorkConsumeAttempt.objects.filter(notice__work__coordination_session=session).aggregate(
                last=Max("delivery_sequence"))["last"] or 0
            candidate.delivery_sequence, candidate.created_at_ms = sequence + 1, time.time_ns() // 1_000_000
            candidate.input_binding = {"schema": "workspace.agent_work.consume_binding.v1",
                "agentRunId": run.pk if bindable else None, "turnId": run.turn_id if bindable else None,
                "sessionId": session.pk, "authorizationDigest": run.authorization.digest if bindable else None,
                "initialInput": {"type": "hostEvent", "attemptId": candidate.pk, "noticeId": notice_id, "input": native_input}}
            candidate.save()
        elif bindable:
            _bind(candidate, run)
        if queue is not None and queue.accepting:
            pending = AgentWorkConsumeAttempt.objects.filter(coordinator_run__isnull=True,
                notice__work__coordination_session=session).order_by("delivery_sequence")[:256]
            for waiting in pending:
                if not _has_capacity(queue):
                    break
                try:
                    _, waiting_source, _, _ = _locked_notice(waiting.notice_id)
                except WorkReturnError as error:
                    if error.status != 403:
                        raise
                    continue
                waiting_digest = _digest("workspace.agent_work.consume.v1",
                    {"noticeId": waiting.notice_id}).removeprefix("sha256:")
                _existing(waiting.notice, waiting_source, waiting.operation.operationId, waiting_digest)
                _bind(waiting, run)
                if waiting.pk == candidate.pk:
                    candidate = waiting
        if initial and candidate.coordinator_run_id == run.pk:
            first = AgentWorkConsumeAttempt.objects.filter(coordinator_run=run).order_by("delivery_sequence").first()
            AgentInputQueue.objects.filter(pk=run.pk, initial_input__isnull=True, initial_host__isnull=True).update(initial_host=first)
        if candidate.coordinator_run_id is not None:
            bound_host_input(candidate, candidate.coordinator_run)
        result = _response(candidate)
    if scheduled is not None:
        try:
            schedule_agent_run_lifecycle(scheduled)
        except Exception:
            logger.exception("Accepted work consumption awaits lifecycle reconciliation: %s", scheduled.pk)
            AgentRun.objects.filter(pk=scheduled.pk, status="queued").update(transitionReason="agent_run_schedule_pending")
    return result, 200 if current is not None else 201
