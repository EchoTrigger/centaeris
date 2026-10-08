"""Immutable hosted acceptance; coordinator admission follows the carrier commit."""
from django.db import DatabaseError, connection, transaction
from django.db.models import Max
import json

from .agent_input_contract import validate_input_body, validate_input_id
from .app_delegations import DelegationRejected, require_request_delegation, session_authority_is_current
from .agent_input_attachments import capture_attachments, validate_attachment_refs
from .assets import DeferredInputResolutionError
from .models import Agent, AgentCoordinationSession, AgentInput, Session, SessionEvent
from .workspace_access import locked_workspace_membership_for


class AgentInputError(ValueError):
    def __init__(self, status, code):
        super().__init__(code)
        self.status, self.code = status, code


def require_native_agent_delegation(request, scope, *, agent_id=None, session_id=None, lock=False):
    """Dedicated Agent intake/read routes accept exact native Agent grants only."""
    grant = require_request_delegation(request, scope, agent_id=agent_id, session_id=session_id, lock=lock)
    if grant is not None and grant.agent_id is None and getattr(request, "business_branch_id", None) is None:
        raise DelegationRejected("delegation_scope_forbidden")
    return grant


def owned_input_binding(user, agent_id, *, scope="events:read"):
    binding = AgentCoordinationSession.objects.select_related("agent", "session").filter(
        agent_id=agent_id, agent__owner=user, agent__status="active").first()
    if (binding is None or not user.is_active or binding.session.agent_id != agent_id
            or binding.session.owner_id != user.pk
            or binding.session.workspace_id != binding.agent.workspace_id
            or not session_authority_is_current(user.pk, binding.session_id, scope=scope)):
        raise AgentInputError(404, "coordination_session_not_found")
    return binding


def serialize_input(fact):
    from .agent_input_reads import input_read
    read = fact.read if hasattr(fact, "read") else input_read(fact)
    if isinstance(read, str):
        import json
        read = json.loads(read)
    return {"inputId": fact.input_id, "sequence": fact.sequence, "createdAtMs": fact.created_at_ms,
            "body": fact.body, "read": read, "attachments": [{key: item[key]
                for key in ("inputRef", "displayName", "contentType")} for item in fact.attachments]}


@transaction.atomic
def accept_agent_input(user, agent_id, input_id, body, attachment_refs=None, *, request=None):
    attachment_refs = [] if attachment_refs is None else attachment_refs
    validate_attachment_refs(attachment_refs)
    validate_input_id(input_id)
    validate_input_body(body, allow_empty=bool(attachment_refs))
    scope = Agent.objects.filter(pk=agent_id, owner=user, status="active").values("workspace_id").first()
    if scope is None or not user.is_active:
        raise AgentInputError(404, "agent_not_found")
    membership = locked_workspace_membership_for(user, scope["workspace_id"])
    if membership is None:
        raise AgentInputError(404, "agent_not_found")
    grant = require_native_agent_delegation(request, "messages:submit", agent_id=agent_id, lock=True) if request is not None else None
    agent = Agent.objects.select_for_update().get(pk=agent_id)
    binding = owned_input_binding(user, agent_id, scope="messages:submit")
    try:
        session = Session.objects.select_for_update(nowait=True).get(pk=binding.session_id)
    except DatabaseError as error:
        if getattr(error.__cause__, "sqlstate", None) != "55P03":
            raise
        raise AgentInputError(503, "agent_input_admission_busy") from error
    if not session_authority_is_current(user.pk, session.pk, scope="messages:submit"):
        raise AgentInputError(404, "coordination_session_not_found")
    binding.session = session
    existing = AgentInput.objects.filter(agent=agent, input_id=input_id).first()
    if existing is not None:
        if existing.session_id != binding.session_id or existing.membership_ref != membership.pk:
            raise AgentInputError(403, "agent_input_identity_revoked")
        origin = (grant.app_id, grant.pk) if grant is not None else (None, None)
        if (existing.acting_app_id, existing.app_delegation_id) != origin:
            raise AgentInputError(403, "agent_input_source_forbidden")
        if existing.body != body or [item["inputRef"] for item in existing.attachments] != attachment_refs:
            raise AgentInputError(409, "agent_input_conflict")
        _dispatch_after_commit(agent_id)
        return existing, False
    try:
        attachments = capture_attachments(user, session, attachment_refs)
    except DeferredInputResolutionError as error:
        raise AgentInputError(403, "agent_input_attachment_rejected") from error
    sequence = (AgentInput.objects.filter(agent=agent).aggregate(value=Max("sequence"))["value"] or 0) + 1
    # Runtime allocates SessionEvent sequences while holding this same Session
    # row lock. This immutable anchor orders acceptance against output commits.
    accepted_source_sequence = SessionEvent.objects.filter(session=session).aggregate(value=Max("sequence"))["value"] or 0
    with connection.cursor() as cursor:
        cursor.execute("SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint")
        created_at_ms = cursor.fetchone()[0]
    fact = AgentInput.objects.create(agent=agent, session=binding.session, input_id=input_id,
        membership_ref=membership.pk, sequence=sequence, accepted_source_sequence=accepted_source_sequence,
        body=body, attachments=attachments, created_at_ms=created_at_ms,
        acting_app_id=grant.app_id if grant else None, app_delegation=grant,
        credential_version=grant.credential_version if grant else None)
    _dispatch_after_commit(agent_id)
    return fact, True


def _dispatch_after_commit(agent_id):
    from .agent_input_delivery import try_dispatch_agent_inputs
    transaction.on_commit(lambda: try_dispatch_agent_inputs(agent_id))


def input_history(user, agent_id, after_sequence=0, limit=50):
    from types import SimpleNamespace
    from .agent_input_reads import input_read_lateral
    binding = owned_input_binding(user, agent_id)
    lateral = input_read_lateral()
    with connection.cursor() as cursor:
        cursor.execute("SELECT i.input_id,i.sequence,i.created_at_ms,i.body,input_read.value,i.attachments "
            "FROM app_core_agentinput i " + lateral +
            " WHERE i.agent_id=%s AND i.session_id=%s AND i.sequence>%s ORDER BY i.sequence LIMIT %s",
            [agent_id, binding.session_id, after_sequence, limit + 1])
        page = [SimpleNamespace(input_id=row[0], sequence=row[1], created_at_ms=row[2], body=row[3],
            read=row[4], attachments=row[5] if isinstance(row[5], list) else json.loads(row[5])) for row in cursor.fetchall()]
    return {"schema": "agent.inputs.v1", "agentId": agent_id, "sessionId": binding.session_id,
        "inputs": [serialize_input(fact) for fact in page[:limit]],
        "nextAfterSequence": page[limit - 1].sequence if len(page) > limit else None}
