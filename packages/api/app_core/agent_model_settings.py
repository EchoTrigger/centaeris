"""Owner-set Agent model policy; existing Runs retain their authorized snapshots."""
from .models import ModelConfig, validate_thinking_mode
from .business_agent_scope import agent_configuration, BusinessAgentConfigurationUnavailable
from .runtime_contract import require_opaque_ref


class AgentModelSettingsError(ValueError):
    pass


def selected_model(model_config_ref, thinking_mode):
    try:
        require_opaque_ref("modelConfigRef", model_config_ref)
        if "\0" in model_config_ref:
            raise ValueError("modelConfigRef must not contain NUL")
    except ValueError as error:
        raise AgentModelSettingsError("agent_model_not_available") from error
    try:
        model = ModelConfig.objects.get(pk=model_config_ref, enabled=True, isCurrent=True)
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


def configured_agent_model(agent):
    """Call under the Agent admission lock, then snapshot on the new Run."""
    try:
        agent = agent_configuration(agent)
    except BusinessAgentConfigurationUnavailable as error:
        raise AgentModelSettingsError(str(error)) from error
    if agent.model_config_id is None:
        raise AgentModelSettingsError("agent_model_not_configured")
    mode = agent.thinking_mode or None
    model, resolved = selected_model(agent.model_config_id, mode)
    # A saved empty effort is a pinned value, not a request to resolve a changed
    # catalog default. ModelConfig revisions must not mutate their meaning.
    if resolved != agent.thinking_mode:
        raise AgentModelSettingsError("agent_model_not_available")
    return model, agent.thinking_mode


def serialize_model_settings(agent):
    configuration = agent_configuration(agent)
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
