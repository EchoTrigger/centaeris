"""Derive business isolation from persistent Agent identity, never tool arguments."""
from .app_delegations import session_authority_is_current
from .models import Agent, AgentCoordinationSession, AgentWorkSession, BusinessAgentBranch


class BusinessAgentConfigurationUnavailable(ValueError):
    pass


def branch_for_agent(agent_id):
    return BusinessAgentBranch.objects.filter(agent_id=agent_id).first()


def _require_root_configuration(agent, root):
    if (root.status != "active" or root.definition_id != agent.definition_id
            or root.owner_id != agent.owner_id or root.workspace_id != agent.workspace_id):
        raise BusinessAgentConfigurationUnavailable("agent_business_root_unavailable")
    return root


def agent_configuration(agent):
    """Read the maintained root policy; a missing root cannot revive a stale copy."""
    if hasattr(agent, "_business_agent_configuration"):
        return _require_root_configuration(agent, agent._business_agent_configuration)
    branch = branch_for_agent(agent.pk)
    if branch is None:
        return agent
    try:
        return _require_root_configuration(agent, branch.root_agent)
    except Agent.DoesNotExist as error:
        raise BusinessAgentConfigurationUnavailable("agent_business_root_unavailable") from error


def locked_agent_configuration(agent_id):
    """The caller holds Workspace/membership; lock root before its branch Agent."""
    branch = branch_for_agent(agent_id)
    try:
        root = Agent.objects.select_for_update().get(pk=branch.root_agent_id) if branch is not None else None
    except Agent.DoesNotExist as error:
        raise BusinessAgentConfigurationUnavailable("agent_business_root_unavailable") from error
    agent = Agent.objects.select_for_update().get(pk=agent_id)
    if root is not None:
        agent._business_agent_configuration = _require_root_configuration(agent, root)
    return agent


def run_session_ref_is_current(run, session_ref, scope="sessions:read"):
    if not session_authority_is_current(run.user_id, session_ref, run.app_delegation_id, scope):
        return False
    branch = branch_for_agent(run.session.agent_id)
    if branch is None:
        return True
    if session_ref == branch.session_id:
        return AgentCoordinationSession.objects.filter(agent_id=branch.agent_id, session_id=session_ref).exists()
    # Same Agent alone is insufficient: only its authoritative dispatch tree is
    # eligible. File references retain their separate signed Session asset ACL.
    return AgentWorkSession.objects.filter(session_id=session_ref,
        coordination_session_id=branch.session_id, source_run__session_id=branch.session_id,
        source_run__user_id=run.user_id, source_run__workspace_id=run.workspace_id,
        session__agent_id=branch.agent_id, session__owner_id=run.user_id,
        session__workspace_id=run.workspace_id).exists()
