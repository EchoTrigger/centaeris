"""Owner-set Agent model policy; existing Runs retain their authorized snapshots."""
from .models import ModelConfig, validate_thinking_mode
from .business_agent_scope import agent_configuration, BusinessAgentConfigurationUnavailable
from .runtime_contract import require_opaque_ref


class AgentModelSettingsError(ValueError):
    pass


def selected_model(model_config_ref, thinking_mode, *, workspace_id=None):
    try:
        require_opaque_ref("modelConfigRef", model_config_ref)
        if "\0" in model_config_ref:
            raise ValueError("modelConfigRef must not contain NUL")
    except ValueError as error:
        raise AgentModelSettingsError("agent_model_not_available") from error
    try:
        from .workspace_resources import models_for_workspace
        model = models_for_workspace(workspace_id).get(pk=model_config_ref, enabled=True, isCurrent=True)
    except ModelConfig.DoesNotExist as error:
        raise AgentModelSettingsError("agent_model_not_available") from error
    mode = model.thinkingMode
    if thinking_mode is not None:
        try:
            validate_thinking_mode(thinking_mode)
        except ValueError as error:
            raise AgentModelSettingsError("model_thinking_mode_unsupported") from error
        if thinking_mode not in model.thinkingModes:
            raise AgentModelSettingsError("model_thinking_mode_unsupported")
        mode = thinking_mode
    return model, mode


def configured_agent_model(agent, *, definition_version=None):
    """Call under the Agent admission lock, then snapshot on the new Run."""
    business_instance = agent.is_business_instance or agent.business_branches.exists()
    try:
        agent = agent_configuration(agent)
    except BusinessAgentConfigurationUnavailable as error:
        raise AgentModelSettingsError(str(error)) from error
    workspace_id = agent.workspace_id
    if definition_version is not None:
        agent = definition_version
    elif agent.definition_id is not None:
        agent = agent.definition.published_version
        if agent is None:
            raise AgentModelSettingsError("agent_definition_not_available")
    elif not business_instance:
        raise AgentModelSettingsError("agent_configuration_promotion_required")
    if agent.model_config_id is None:
        raise AgentModelSettingsError("agent_model_not_configured")
    mode = agent.thinking_mode or None
    model, resolved = selected_model(agent.model_config_id, mode, workspace_id=workspace_id)
    # A saved empty effort is a pinned value, not a request to resolve a changed
    # catalog default. ModelConfig revisions must not mutate their meaning.
    if resolved != agent.thinking_mode:
        raise AgentModelSettingsError("agent_model_not_available")
    return model, agent.thinking_mode


def serialize_model_settings(agent):
    configuration = agent_configuration(agent)
    if agent.definition_id is not None:
        configuration = agent.definition.published_version
        if configuration is None:
            raise AgentModelSettingsError("agent_definition_not_available")
    status = "unconfigured"
    if configuration.model_config_id is not None:
        try:
            configured_agent_model(agent)
            status = "configured"
        except AgentModelSettingsError:
            status = "unavailable"
    return {"schema": "agent.model_settings.v1", "agentId": agent.pk,
        "modelConfigRef": configuration.model_config_id, "thinkingMode": configuration.thinking_mode or None,
        "status": status}
