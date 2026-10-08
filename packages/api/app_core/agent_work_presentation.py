"""Read-only presentation of authoritative hosted dispatch bindings."""
from django.core.exceptions import ObjectDoesNotExist

from .agent_work_returns import _bound_work, WorkReturnError
from .models import AgentCoordinationSession, AgentWorkSession, SessionEvent


def verified_work(binding, *, source_event=...):
    """Never turn an arbitrary automation Session or a text ID into hosted work."""
    try:
        work, child = _bound_work(binding.operation.agentRunId, source_event=source_event)
        if work is None or work.pk != binding.pk or child.session_id != binding.session_id:
            return None
        if not AgentCoordinationSession.objects.filter(agent_id=child.session.agent_id,
                session_id=work.coordination_session_id).exists():
            return None
        from .workspace_access import agent_run_membership_is_current
        from .business_agent_scope import run_session_ref_is_current
        return child if (agent_run_membership_is_current(child) and agent_run_membership_is_current(work.source_run)
            and run_session_ref_is_current(work.source_run, child.session_id)) else None
    except (ObjectDoesNotExist, WorkReturnError, ValueError, TypeError, KeyError):
        return None


def initial_input_origin(session):
    if session._meta.get_field("work_binding").is_cached(session):
        try:
            binding = session.work_binding
        except ObjectDoesNotExist:
            return None
    else:
        binding = AgentWorkSession.objects.select_related("operation").filter(session=session).first()
    if binding is None:
        return None
    child = verified_work(binding)
    if child is None:
        return None
    records = list(SessionEvent.objects.filter(session=session, agent_run=child,
        session_level=False, projects_to_agent_run_stream=True, payload__type="user_message",
        payload__turnId=binding.operation.turnId).order_by("sequence")[:2])
    if len(records) != 1:
        return None
    event = records[0]
    wire, payload = event.payload, event.payload.get("payload", {})
    message_id = payload.get("messageId")
    if (wire.get("schemaVersion") != "session.event.v1" or wire.get("eventVersion") != 1
            or wire.get("eventId") != event.eventId or wire.get("sequence") != event.sequence
            or wire.get("sessionId") != session.id or wire.get("agentRunId") != child.id
            or wire.get("createdAtMs") != event.createdAtMs or payload.get("text") != child.prompt
            or not isinstance(message_id, str) or not message_id):
        return None
    return {"messageId": message_id, "agentId": child.session.agent_id,
            "agentName": child.session.agent.name}


def work_refs_for_message(binding, result, *, snapshot=None):
    """Attach dispatch to its first verified public reply, never body IDs."""
    from .agent_messages import _trusted_message
    from .app_delegations import session_authority_is_current
    if snapshot is not None:
        return _snapshot_work_refs(binding, result, snapshot)
    refs = []
    sources = SessionEvent.objects.filter(session_id=binding.session_id, agent_run_id=result.agent_run.id,
        sequence__lt=result.sequence).values("eventId")
    works = AgentWorkSession.objects.select_related("operation", "session").filter(
        coordination_session_id=binding.session_id, source_run_id=result.agent_run.id,
        source_event_id__in=sources, session__status="active", session__purgedAt__isnull=True,
        session__owner_id=binding.agent.owner_id, session__workspace_id=binding.agent.workspace_id,
        session__agent_id=binding.agent_id).order_by("created_at", "session_id")[:8]
    for work in works:
        child = verified_work(work)
        if child is None or not session_authority_is_current(binding.agent.owner_id, child.session_id, scope="sessions:read"):
            continue
        origin = SessionEvent.objects.get(pk=work.source_event_id)
        earlier = list(SessionEvent.objects.filter(session_id=binding.session_id, workspace_id=binding.agent.workspace_id,
            agent_run_id=result.agent_run.id, projects_to_agent_run_stream=True,
            sequence__gt=origin.sequence, sequence__lt=result.sequence,
            payload__type="tool_result", payload__payload__toolName="send_message",
            payload__payload__resultState="successWithOutput").select_related("agent_run").order_by("sequence")[:101])
        # Bounded uncertainty fails closed; work remains available in its own list.
        if len(earlier) > 100 or any(_trusted_message(binding, prior, attach_work=False) is not None for prior in earlier):
            continue
        refs.append(child.session_id)
    return refs


def _snapshot_work_refs(binding, result, rows):
    """History captures dispatch, prior replies and their calls in its one SQL snapshot."""
    from types import SimpleNamespace
    from .agent_messages import _trusted_message
    from .app_delegations import session_authority_is_current
    refs = []
    for row in rows:
        work = AgentWorkSession.objects.select_related("operation").filter(session_id=row["sessionId"],
            coordination_session_id=binding.session_id, source_run_id=result.agent_run.id,
            source_event_id=row["source"]["eventId"]).first()
        if work is None:
            continue
        source = SimpleNamespace(**row["source"])
        child = verified_work(work, source_event=source)
        if (child is None or child.session.status != "active" or child.session.purgedAt is not None
                or not session_authority_is_current(binding.agent.owner_id, child.session_id, scope="sessions:read")):
            continue
        earlier = row["earlier"]
        if len(earlier) > 100:
            continue
        found = False
        for prior in earlier:
            carrier = SimpleNamespace(**prior["result"], agent_run=result.agent_run)
            call = SimpleNamespace(**prior["call"]) if prior["call"] is not None else None
            if _trusted_message(binding, carrier, call=call, attach_work=False) is not None:
                found = True
                break
        if not found:
            refs.append(child.session_id)
    return refs
