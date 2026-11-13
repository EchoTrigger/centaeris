"""Recipient-scoped guide state and completion from authorized records."""
from typing import Literal

from django.db import transaction
from django.utils import timezone
from ninja import Router, Status

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


class GuideStep(StrictSchema):
    id: Literal["models", "materials", "plugins", "connections", "conversation", "issue"]
    completed: bool


class GuideResponse(StrictSchema):
    dismissed: bool
    steps: list[GuideStep]


class GuideEnvelope(StrictSchema):
    guide: GuideResponse


class GuidePatch(StrictSchema):
    dismissed: bool


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
    dismissed = UserWorkspaceGuideState.objects.filter(
        user=request.user, workspace_id=workspace_id, dismissedAt__isnull=False,
    ).exists()
    return {"guide": {"dismissed": dismissed, "steps": steps}}


@router.patch("/workspaces/{workspace_id}/guide", auth=session_auth,
    response={200: GuideEnvelope} | COMMON_ERROR_RESPONSES)
def update_guide(request, workspace_id: str, payload: GuidePatch):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        if request.GET:
            return Status(400, {"error": "workspace_guide_query_invalid"})
        steps = _steps(request.user, membership)
        if isinstance(steps, Status):
            return steps
        state, _ = UserWorkspaceGuideState.objects.get_or_create(user=request.user, workspace_id=workspace_id)
        if (state.dismissedAt is not None) != payload.dismissed:
            state.dismissedAt = timezone.now() if payload.dismissed else None
            state.save(update_fields=["dismissedAt"])
        return {"guide": {"dismissed": payload.dismissed, "steps": steps}}
