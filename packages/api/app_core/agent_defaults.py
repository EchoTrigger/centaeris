"""Workspace default configuration with private member Agent identities."""
from django.db import transaction

from .agent_definitions import publish_definition
from .models import Agent, AgentDefinition, Workspace

DEFAULT_AGENT_NAME = "Centaeris"
DEFAULT_AGENT_DESCRIPTION = "默认 Agent"


class AgentPromotionRequired(ValueError):
    pass


def ensure_default_agent(workspace, user) -> tuple[Agent, bool]:
    # Callers pin fresh Workspace authority. Existing defaults need no writer;
    # a first bootstrap rechecks under the exclusive configuration boundary.
    with transaction.atomic():
        if workspace.default_agent_definition_id is None:
            workspace = Workspace.objects.select_for_update().get(pk=workspace.pk)
            if workspace.default_agent_definition_id is None:
                if Agent.objects.filter(workspace=workspace).exists():
                    raise AgentPromotionRequired("agent_configuration_promotion_required")
                definition = AgentDefinition.objects.create(
                    workspace=workspace, created_by=user, name=DEFAULT_AGENT_NAME,
                    description=DEFAULT_AGENT_DESCRIPTION, availability_scope="workspace",
                )
                publish_definition(definition, user)
                workspace.default_agent_definition = definition
                workspace.save(update_fields=["default_agent_definition", "updatedAt"])
        definition = workspace.default_agent_definition
        if (definition.status != "active" or definition.availability_scope != "workspace"
                or definition.published_version_id is None):
            raise AgentPromotionRequired("agent_definition_not_available")
        version = definition.published_version
        return Agent.objects.get_or_create(
            workspace=workspace, owner=user, definition=definition, is_business_instance=False,
            defaults={"name": version.name, "description": version.description,
                      "instructions": version.instructions, "avatar_kind": version.avatar_kind},
        )


def replace_retired_default(workspace, user, retired):
    """Retain only the authoritative workspace model selection in its base profile."""
    if workspace.default_agent_definition_id != retired.pk:
        return
    version = retired.published_version
    definition = AgentDefinition.objects.create(workspace=workspace, created_by=user,
        name=DEFAULT_AGENT_NAME, description=DEFAULT_AGENT_DESCRIPTION, availability_scope="workspace",
        model_config=version.model_config if version else None,
        thinking_mode=version.thinking_mode if version else "")
    publish_definition(definition, user)
    workspace.default_agent_definition = definition
    workspace.save(update_fields=["default_agent_definition", "updatedAt"])
