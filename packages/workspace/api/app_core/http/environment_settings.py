"""Administrator-owned defaults for newly created execution environments."""
from typing import Literal

from django.db import transaction
from ninja import Router, Status
from pydantic import ConfigDict, Field, model_validator

from app_core.environment_resources import MAX_RESOURCE_COUNT, MAX_SETTINGS_BYTES
from app_core.workspace_access import WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for, workspace_membership_for
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import ErrorResponse, StrictSchema
from .security import session_auth

router = Router(tags=["environment-settings"], by_alias=True)


class EnvironmentSettings(StrictSchema):
    network: Literal["disabled", "publicInternet"]
    cpu_milli: int = Field(gt=0, le=MAX_RESOURCE_COUNT)
    memory_bytes: int = Field(gt=0, le=MAX_SETTINGS_BYTES)
    workspace_bytes: int = Field(gt=0, le=MAX_SETTINGS_BYTES)
    revision: int = Field(gt=0)


class UpdateEnvironmentSettings(StrictSchema):
    model_config = ConfigDict(json_schema_extra={"minProperties": 1})

    # Omission leaves a field unset; its declared type rejects explicit null.
    network: Literal["disabled", "publicInternet"] = None
    cpu_milli: int = Field(default=None, gt=0, le=MAX_RESOURCE_COUNT)
    memory_bytes: int = Field(default=None, gt=0, le=MAX_SETTINGS_BYTES)
    workspace_bytes: int = Field(default=None, gt=0, le=MAX_SETTINGS_BYTES)

    @model_validator(mode="after")
    def require_change(self):
        if not self.model_fields_set:
            raise ValueError("environment_settings_update_empty")
        return self


class EnvironmentSettingsValidationError(ErrorResponse):
    fields: list[str]


def _settings(workspace):
    return {"network": workspace.execution_network,
        **{field: workspace.execution_resources[field] for field in ("cpuMilli", "memoryBytes", "workspaceBytes")},
        "revision": workspace.execution_settings_revision}


@router.get("/workspaces/{workspace_id}/environment-settings", auth=session_auth,
            response={200: EnvironmentSettings} | COMMON_ERROR_RESPONSES)
def read_environment_settings(request, workspace_id: str):
    membership = workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES)
    if membership is None:
        return Status(404, {"error": "workspace_not_found"})
    return _settings(membership.workspace)


@router.patch("/workspaces/{workspace_id}/environment-settings", auth=session_auth,
              response=COMMON_ERROR_RESPONSES | {
                  200: EnvironmentSettings, 400: ErrorResponse | EnvironmentSettingsValidationError})
def update_environment_settings(request, workspace_id: str, payload: UpdateEnvironmentSettings):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace = membership.workspace
        changes = payload.model_dump(exclude_unset=True, by_alias=True)
        network = changes.pop("network", workspace.execution_network)
        if "workspaceBytes" in changes:
            changes["storageBytes"] = changes["workspaceBytes"]
        resources = workspace.execution_resources | changes
        if network != workspace.execution_network or resources != workspace.execution_resources:
            workspace.execution_network = network
            workspace.execution_resources = resources
            workspace.execution_settings_revision += 1
            workspace.save(update_fields=["execution_network", "execution_resources",
                "execution_settings_revision", "updatedAt"])
    return _settings(workspace)
