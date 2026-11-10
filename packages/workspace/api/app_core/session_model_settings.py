"""Explicit Workspace catalog selection for a Session Run."""
from .models import ModelConfig, validate_thinking_mode
from .runtime_contract import require_opaque_ref
from .workspace_resources import models_for_workspace

class SessionModelSettingsError(ValueError):
    pass

def selected_session_model(workspace_id, model_config_ref, thinking_mode):
    try:
        require_opaque_ref("modelConfigRef", model_config_ref)
        model = models_for_workspace(workspace_id).get(pk=model_config_ref, enabled=True, isCurrent=True)
    except (ValueError, ModelConfig.DoesNotExist) as error:
        raise SessionModelSettingsError("model_not_found") from error
    mode = model.thinkingMode
    if thinking_mode is not None:
        try:
            validate_thinking_mode(thinking_mode)
        except ValueError as error:
            raise SessionModelSettingsError("model_thinking_mode_unsupported") from error
        if thinking_mode not in model.thinkingModes:
            raise SessionModelSettingsError("model_thinking_mode_unsupported")
        mode = thinking_mode
    return model, mode
