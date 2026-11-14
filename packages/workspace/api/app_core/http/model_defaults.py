"""Workspace-owned model selection for title generation only."""
from django.db import transaction
from ninja import Router, Status

from app_core.models import WorkspaceModelDefaults
from app_core.session_titles import current_title_model, title_mode_supported
from app_core.workspace_access import (
    WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for, workspace_membership_for,
)
from app_core.workspace_resources import models_for_workspace
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["model-defaults"], by_alias=True)


class ModelDefaults(StrictSchema):
    title_model_config_ref: str | None
    title_thinking_mode: str


class ModelDefaultsEnvelope(StrictSchema):
    defaults: ModelDefaults


def serialize_defaults(workspace_id):
    defaults = WorkspaceModelDefaults.objects.filter(workspace_id=workspace_id).first()
    anchor = defaults.titleModelConfig if defaults else None
    current = current_title_model(workspace_id, anchor, available=False) if anchor else None
    return {"defaults": {
        "titleModelConfigRef": current.pk if current else (anchor.pk if anchor else None),
        "titleThinkingMode": defaults.titleThinkingMode if defaults else "low",
    }}


@router.get("/workspaces/{workspace_id}/model-defaults", auth=session_auth,
            response={200: ModelDefaultsEnvelope} | COMMON_ERROR_RESPONSES)
def read_model_defaults(request, workspace_id: str):
    if workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
        return Status(404, {"error": "workspace_not_found"})
    return serialize_defaults(workspace_id)


@router.patch("/workspaces/{workspace_id}/model-defaults", auth=session_auth,
              response={200: ModelDefaultsEnvelope} | COMMON_ERROR_RESPONSES)
def update_model_defaults(request, workspace_id: str, payload: ModelDefaults):
    with transaction.atomic():
        if locked_workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
            return Status(404, {"error": "workspace_not_found"})
        model = None
        if payload.title_model_config_ref is not None:
            model = models_for_workspace(workspace_id).select_related("provider").filter(
                pk=payload.title_model_config_ref, enabled=True, isCurrent=True,
                provider__enabled=True, provider__archivedAt__isnull=True,
            ).first()
            if model is None:
                return Status(400, {"error": "model_not_found"})
        if (model and not title_mode_supported(model, payload.title_thinking_mode)) or (
            model is None and payload.title_thinking_mode != "low"
        ):
            return Status(400, {"error": "model_thinking_mode_unsupported"})
        WorkspaceModelDefaults.objects.update_or_create(workspace_id=workspace_id, defaults={
            "titleModelConfig": model, "titleThinkingMode": payload.title_thinking_mode,
        })
        return serialize_defaults(workspace_id)
