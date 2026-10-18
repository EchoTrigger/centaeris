"""Administrator-owned defaults for newly created execution environments."""
from typing import Literal

from django.db import transaction
from ninja import Router, Status

from app_core.workspace_access import WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for, workspace_membership_for
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["environment-settings"], by_alias=True)


class EnvironmentSettings(StrictSchema):
    network: Literal["disabled", "publicInternet"]


@router.get("/workspaces/{workspace_id}/environment-settings", auth=session_auth,
            response={200: EnvironmentSettings} | COMMON_ERROR_RESPONSES)
def read_environment_settings(request, workspace_id: str):
    membership = workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES)
    if membership is None:
        return Status(404, {"error": "workspace_not_found"})
    return {"network": membership.workspace.execution_network}


@router.patch("/workspaces/{workspace_id}/environment-settings", auth=session_auth,
              response={200: EnvironmentSettings} | COMMON_ERROR_RESPONSES)
def update_environment_settings(request, workspace_id: str, payload: EnvironmentSettings):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace = membership.workspace
        workspace.execution_network = payload.network
        workspace.save(update_fields=["execution_network", "updatedAt"])
    return {"network": workspace.execution_network}
