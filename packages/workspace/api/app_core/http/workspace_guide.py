"""Recipient-scoped guide state and completion from authorized records."""
from typing import Literal

from django.db import transaction
from django.utils import timezone
from ninja import Router, Status
from pydantic import Field, model_validator

from app_core.models import AgentRun, Issue, McpBearerCredential, SourceObject, UserLibraryObject, UserWorkspaceGuideState
from app_core.plugin_catalog import load_plugin_connector_resources, load_plugin_interfaces
from app_core.workspace_access import (
    WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for,
    source_grants_for_membership, workspace_membership_for,
)
from app_core.workspace_plugins import workspace_plugin_catalog
from app_core.workspace_resources import models_for_workspace
from .response_schema import COMMON_ERROR_RESPONSES
from .schema import StrictSchema
from .security import session_auth

router = Router(tags=["workspace guide"], by_alias=True)


GuideStepId = Literal["models", "materials", "plugins", "connections", "conversation", "issue"]


class GuideStep(StrictSchema):
    id: GuideStepId
    completed: bool


class GuideResponse(StrictSchema):
    dismissed: bool
    steps: list[GuideStep]


class GuideEnvelope(StrictSchema):
    guide: GuideResponse


class GuidePatch(StrictSchema):
    dismissed: bool = False
    manual_steps: dict[GuideStepId, bool] = Field(default_factory=dict)

    @model_validator(mode="after")
    def require_change(self):
        if "dismissed" not in self.model_fields_set and not self.manual_steps:
            raise ValueError("A guide change is required")
        return self


def _guide(steps, state):
    overrides = state.manualSteps if state else {}
    return {"guide": {
        "dismissed": state is not None and state.dismissedAt is not None,
        "steps": [{**step, "completed": overrides.get(step["id"], step["completed"])} for step in steps],
    }}


def _steps(user, membership):
    workspace = membership.workspace
    completed = {}
    if membership.role in WORKSPACE_ADMIN_ROLES:
        completed["models"] = models_for_workspace(workspace.pk).filter(
            enabled=True, isCurrent=True, provider__workspace=workspace,
            provider__enabled=True, provider__archivedAt__isnull=True,
            provider__credential__isnull=False,
        ).exclude(provider__credential__encryptedSecret="").exists()
        try:
            catalog = workspace_plugin_catalog(workspace)
            enabled = set(workspace.pluginEnablements.values_list("pluginName", flat=True))
            if enabled - {package["name"] for package in catalog["packages"]}:
                return Status(409, {"error": "workspace_plugin_enablement_invalid"})
            load_plugin_interfaces(catalog)
            configured = set(McpBearerCredential.objects.filter(workspace=workspace)
                .exclude(encrypted_secret="").values_list("plugin_name", "credential_ref"))
            connectors = [(package["name"], load_plugin_connector_resources(package))
                for package in catalog["packages"]]
        except (OSError, ValueError):
            return Status(503, {"error": "plugin_catalog_invalid"})
        completed["plugins"] = any(package["name"] in enabled for package in catalog["packages"])
        completed["connections"] = any(
            connector["credentialRef"] is None or (name, connector["credentialRef"]) in configured
            for name, resources in connectors for connector in resources
        )
    materials = SourceObject.objects.filter(
        workspace=workspace, source__status="ready", objectType="file",
        deletedAt__isnull=True,
    ).exclude(status="deleted")
    if membership.role not in WORKSPACE_ADMIN_ROLES:
        materials = materials.filter(source_id__in=source_grants_for_membership(membership).values("source_id"))
    completed["materials"] = UserLibraryObject.objects.filter(
        owner=user, objectKind__in=["file", "image"], deletedAt__isnull=True,
    ).exclude(status="deleted").exists() or materials.exists()
    completed["conversation"] = AgentRun.objects.filter(
        workspace=workspace, user=user, session__owner=user,
        session__purpose="interactive", session__status="active", acting_app__isnull=True,
    ).exists()
    completed["issue"] = Issue.objects.filter(workspace=workspace, creator=user).exists()
    return [{"id": step, "completed": completed[step]}
        for step in ("models", "materials", "plugins", "connections", "conversation", "issue")
        if step in completed]


@router.get("/workspaces/{workspace_id}/guide", auth=session_auth,
    response={200: GuideEnvelope} | COMMON_ERROR_RESPONSES)
def get_guide(request, workspace_id: str):
    membership = workspace_membership_for(request.user, workspace_id)
    if membership is None:
        return Status(404, {"error": "workspace_not_found"})
    if request.GET:
        return Status(400, {"error": "workspace_guide_query_invalid"})
    steps = _steps(request.user, membership)
    if isinstance(steps, Status):
        return steps
    state = UserWorkspaceGuideState.objects.filter(user=request.user, workspace_id=workspace_id).first()
    return _guide(steps, state)


@router.patch("/workspaces/{workspace_id}/guide", auth=session_auth,
    response={200: GuideEnvelope} | COMMON_ERROR_RESPONSES)
def update_guide(request, workspace_id: str, payload: GuidePatch):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        if request.GET:
            return Status(400, {"error": "workspace_guide_query_invalid"})
        if membership.role not in WORKSPACE_ADMIN_ROLES and payload.manual_steps.keys() & {"models", "plugins", "connections"}:
            return Status(403, {"error": "workspace_guide_step_forbidden"})
        steps = _steps(request.user, membership)
        if isinstance(steps, Status):
            return steps
        state, _ = UserWorkspaceGuideState.objects.get_or_create(user=request.user, workspace_id=workspace_id)
        changed = []
        if "dismissed" in payload.model_fields_set and (state.dismissedAt is not None) != payload.dismissed:
            state.dismissedAt = timezone.now() if payload.dismissed else None
            changed.append("dismissedAt")
        if payload.manual_steps:
            state.manualSteps = {**state.manualSteps, **payload.manual_steps}
            changed.append("manualSteps")
        if changed:
            state.save(update_fields=changed)
        return _guide(steps, state)
