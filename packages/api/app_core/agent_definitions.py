"""Hosted shared configuration; Agent and Session identities remain user-owned."""

from django.db.models import Max, Q

from .models import AgentDefinition, AgentDefinitionVersion
from .plugin_catalog import require_current_plugin_activation, selected_plugin_activation


class AgentDefinitionUnavailable(Exception):
    pass


def available_agent_definitions(membership):
    return AgentDefinition.objects.filter(
        workspace_id=membership.workspace_id,
        status="active",
        published_version__isnull=False,
    ).filter(
        Q(availability_scope="workspace")
        | Q(availability_scope="members", member_grants__membership=membership)
    ).select_related("published_version").defer("avatar_image", "published_version__avatar_image").distinct()


def published_version_for_agent(agent, membership):
    """Preflight availability, then recheck under the Run acceptance locks."""
    if agent.definition_id is None:
        return None
    definition = available_agent_definitions(membership).filter(id=agent.definition_id).first()
    if definition is None:
        raise AgentDefinitionUnavailable("agent_definition_not_available")
    try:
        require_current_plugin_activation(agent.workspace, definition.published_version.plugin_activation)
    except ValueError as error:
        raise AgentDefinitionUnavailable("agent_definition_capability_unavailable") from error
    return definition.published_version


def publish_definition(definition, user):
    """Caller owns the Workspace writer pin and the definition draft."""
    activation = selected_plugin_activation(definition.workspace, definition.plugin_names)
    latest = definition.versions.aggregate(latest=Max("version"))["latest"] or 0
    version = AgentDefinitionVersion.objects.create(
        definition=definition, version=latest + 1, published_by=user,
        name=definition.name, description=definition.description,
        instructions=definition.instructions, avatar_kind=definition.avatar_kind,
        avatar_image=definition.avatar_image, avatar_digest=definition.avatar_digest,
        model_config=definition.model_config, thinking_mode=definition.thinking_mode,
        plugin_activation=activation,
    )
    definition.published_version = version
    definition.save(update_fields=["published_version", "updated_at"])
    return version


def published_version_for_session(session, membership):
    """Resolve ordinary Session policy without replacing its historical Agent identity."""
    agent = session.agent
    retired = agent.status == "deleted" or (agent.definition_id is not None and agent.definition.status == "disabled")
    if not retired or agent.is_business_instance or agent.business_branches.exists():
        if agent.definition_id is None:
            raise AgentDefinitionUnavailable("agent_configuration_promotion_required")
        return published_version_for_agent(agent, membership)
    definition = available_agent_definitions(membership).filter(
        pk=membership.workspace.default_agent_definition_id, availability_scope="workspace").first()
    if definition is None:
        raise AgentDefinitionUnavailable("agent_definition_not_available")
    try:
        require_current_plugin_activation(membership.workspace, definition.published_version.plugin_activation)
    except ValueError as error:
        raise AgentDefinitionUnavailable("agent_definition_capability_unavailable") from error
    return definition.published_version
