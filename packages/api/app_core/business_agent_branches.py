"""Persistent business identity, separated from user authority and credentials."""
import unicodedata

from django.db import transaction

from .app_delegations import DelegationRejected, require_current_delegation
from .models import Agent, AgentCoordinationSession, BusinessAgentBranch, Session
from .workspace_access import locked_workspace_membership_for


BRANCH_HEADER = "X-Centaeris-Business-Branch-Id"


def validate_business_user_id(value):
    if (not isinstance(value, str) or not 1 <= len(value) <= 256 or not value.strip()
            or any(unicodedata.category(char) in {"Cc", "Cs"} for char in value)):
        raise ValueError("business_branch_request_invalid")
    return value


def require_business_branch(grant, branch_id, *, lock=False, require_active=True):
    if grant.agent_id is None:
        raise DelegationRejected("delegation_scope_forbidden")
    query = BusinessAgentBranch.objects.select_related("agent", "session", "root_agent")
    if lock:
        query = query.select_for_update(of=("self",))
    branch = query.filter(pk=branch_id, app_id=grant.app_id, root_agent_id=grant.agent_id).first()
    if (branch is None or branch.agent.definition_id is not None
            or branch.agent.owner_id != grant.user_id or branch.agent.workspace_id != grant.workspace_id
            or branch.session.agent_id != branch.agent_id or branch.session.owner_id != grant.user_id
            or branch.session.workspace_id != grant.workspace_id
            or not AgentCoordinationSession.objects.filter(agent_id=branch.agent_id, session_id=branch.session_id).exists()
            or (require_active and (branch.agent.status != "active" or branch.session.status != "active"))):
        raise DelegationRejected("business_branch_not_found", 404)
    return branch


def bind_request_business_branch(request, grant):
    """Resolve once at authentication; later admission never reselects headers."""
    request.business_branch = None
    request.business_branch_id = None
    if grant.agent_id is None:
        return
    resolving = request.method == "POST" and request.resolver_match.url_name == "resolve_business_agent_branch"
    if resolving:
        if BRANCH_HEADER in request.headers:
            raise DelegationRejected("business_branch_header_invalid", 400)
        if request.resolver_match.kwargs.get("root_agent_id") != grant.agent_id:
            raise DelegationRejected("business_branch_not_found", 404)
        return
    branch_id = request.headers.get(BRANCH_HEADER)
    if not branch_id:
        raise DelegationRejected("business_branch_required", 400)
    branch = require_business_branch(grant, branch_id)
    request.business_branch = branch
    request.business_branch_id = branch.pk
    grant.business_branch = branch


@transaction.atomic
def resolve_business_branch(request, root_agent_id, business_user_id):
    validate_business_user_id(business_user_id)
    grant = request.app_delegation
    if grant is None or grant.agent_id is None:
        raise DelegationRejected("delegation_scope_forbidden")
    membership = locked_workspace_membership_for(request.user, grant.workspace_id)
    if membership is None:
        raise DelegationRejected("delegation_not_available")
    grant = require_current_delegation(grant.pk, "assistant:use", lock=True,
        expected_credential_version=request.app_delegation_credential_version)
    if root_agent_id != grant.agent_id:
        raise DelegationRejected("business_branch_not_found", 404)
    root = Agent.objects.select_for_update().get(pk=root_agent_id)
    existing = BusinessAgentBranch.objects.filter(app_id=grant.app_id, root_agent=root,
        business_user_id=business_user_id).first()
    if existing is not None:
        branch = require_business_branch(grant, existing.pk, require_active=False)
        if branch.agent.status != "active" or branch.session.status != "active":
            raise DelegationRejected("business_branch_deleted", 410)
        return branch, False
    agent = Agent.objects.create(workspace=root.workspace, owner=root.owner, name=root.name,
        description=root.description, instructions=root.instructions, avatar_kind=root.avatar_kind,
        model_config=root.model_config, thinking_mode=root.thinking_mode)
    session = Session.objects.create(workspace=root.workspace, owner=root.owner, agent=agent,
        origin="automation", title="Agent coordination")
    AgentCoordinationSession.objects.create(agent=agent, session=session)
    branch = BusinessAgentBranch.objects.create(app=grant.app, root_agent=root,
        business_user_id=business_user_id, agent=agent, session=session)
    return branch, True
