from typing import Literal

from django.db import transaction
from ninja import Router, Status
from django.http import HttpResponse
from pydantic import Field, model_validator

from app_core.agent_definitions import available_agent_definitions, publish_definition
from app_core.agent_avatars import normalize_avatar
from app_core.agent_model_settings import selected_model, AgentModelSettingsError
from app_core.app_delegations import require_request_delegation
from app_core.models import Agent, AgentDefinition, AgentDefinitionMember, AgentDefinitionVersion, WorkspaceMembership
from app_core.plugin_catalog import selected_plugin_activation, validate_plugin_names
from app_core.workspace_access import WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for, workspace_membership_for

from .agents import AgentProfileFields, AgentProfileUpdate
from .response_schema import (
    AgentDefinitionEnvelope, AgentDefinitionsEnvelope, AgentDefinitionVersionEnvelope,
    AgentDefinitionVersionsEnvelope, AgentEnvelope, AvailableAgentDefinitionsEnvelope, COMMON_ERROR_RESPONSES,
)
from .schema import StrictSchema
from .security import session_auth, usage_auth
from .serialization import serialize_agent, serialize_agent_definition, serialize_agent_definition_version


router = Router(tags=["agent definitions"], by_alias=True)


class CreateAgentDefinitionRequest(AgentProfileFields):
    model_config_ref: str | None = None
    thinking_mode: str | None = None
    avatar_data_base64: str | None = None
    plugin_names: list[str] = Field(default_factory=list)

    @model_validator(mode="after")
    def validate_plugins(self):
        validate_plugin_names(sorted(self.plugin_names))
        self.plugin_names.sort()
        return self


class UpdateAgentDefinitionRequest(AgentProfileUpdate):
    model_config_ref: str | None = None
    thinking_mode: str | None = None
    avatar_data_base64: str | None = None
    status: Literal["active", "disabled"] | None = None
    plugin_names: list[str] | None = Field(default=None)

    @model_validator(mode="after")
    def validate_plugins(self):
        if self.plugin_names is not None:
            validate_plugin_names(sorted(self.plugin_names))
            self.plugin_names.sort()
        return self


class EmptyDefinitionRequest(StrictSchema):
    pass


class DefinitionAvailabilityRequest(StrictSchema):
    scope: Literal["none", "workspace", "members"]
    membership_ids: list[str] = Field(default_factory=list)

    @model_validator(mode="after")
    def validate_scope(self):
        if len(set(self.membership_ids)) != len(self.membership_ids):
            raise ValueError("duplicate membership identity")
        if self.scope != "members" and self.membership_ids:
            raise ValueError("member identities require members scope")
        return self


def _admin_definition(user, workspace_id, definition_id, *, lock=False):
    lookup = locked_workspace_membership_for if lock else workspace_membership_for
    if lookup(user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
        return None
    query = AgentDefinition.objects.select_related("published_version").filter(id=definition_id, workspace_id=workspace_id)
    if lock:
        query = query.select_for_update(of=("self",))
    return query.first()


@router.get("/workspaces/{workspace_id}/agent-definitions", auth=session_auth,
            response={200: AgentDefinitionsEnvelope} | COMMON_ERROR_RESPONSES)
def list_agent_definitions(request, workspace_id: str):
    if workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
        return Status(404, {"error": "workspace_not_found"})
    if request.GET:
        return Status(400, {"error": "agent_definition_invalid"})
    definitions = AgentDefinition.objects.filter(workspace_id=workspace_id).prefetch_related("member_grants").order_by("created_at", "id")
    return {"definitions": [serialize_agent_definition(item) for item in definitions]}


@router.post("/workspaces/{workspace_id}/agent-definitions", auth=session_auth,
             response={201: AgentDefinitionEnvelope} | COMMON_ERROR_RESPONSES)
def create_agent_definition(request, workspace_id: str, payload: CreateAgentDefinitionRequest):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        try:
            selected_plugin_activation(membership.workspace, payload.plugin_names)
        except ValueError:
            return Status(400, {"error": "agent_definition_plugin_not_enabled"})
        values = payload.model_dump(by_alias=False, exclude={"model_config_ref", "thinking_mode", "avatar_data_base64"})
        try:
            values["avatar_image"], values["avatar_digest"] = normalize_avatar(payload.avatar_data_base64)
            if payload.model_config_ref is not None:
                values["model_config"], values["thinking_mode"] = selected_model(payload.model_config_ref,
                    payload.thinking_mode, workspace_id=workspace_id)
            elif payload.thinking_mode is not None:
                return Status(400, {"error": "agent_model_settings_invalid"})
        except (ValueError, AgentModelSettingsError) as error:
            return Status(400, {"error": str(error)})
        definition = AgentDefinition.objects.create(workspace=membership.workspace, created_by=request.user, **values)
    return Status(201, {"definition": serialize_agent_definition(definition)})


@router.get("/workspaces/{workspace_id}/agent-definitions/{definition_id}", auth=session_auth,
            response={200: AgentDefinitionEnvelope} | COMMON_ERROR_RESPONSES)
def get_agent_definition(request, workspace_id: str, definition_id: str):
    definition = _admin_definition(request.user, workspace_id, definition_id)
    if definition is None:
        return Status(404, {"error": "agent_definition_not_found"})
    if request.GET:
        return Status(400, {"error": "agent_definition_invalid"})
    return {"definition": serialize_agent_definition(definition)}


@router.patch("/workspaces/{workspace_id}/agent-definitions/{definition_id}", auth=session_auth,
              response={200: AgentDefinitionEnvelope} | COMMON_ERROR_RESPONSES)
def update_agent_definition(request, workspace_id: str, definition_id: str, payload: UpdateAgentDefinitionRequest):
    fields = payload.model_fields_set
    if not fields or any(getattr(payload, field) is None for field in fields - {"model_config_ref", "thinking_mode", "avatar_data_base64"}):
        return Status(400, {"error": "agent_definition_invalid"})
    with transaction.atomic():
        definition = _admin_definition(request.user, workspace_id, definition_id, lock=True)
        if definition is None:
            return Status(404, {"error": "agent_definition_not_found"})
        if "plugin_names" in fields:
            try:
                selected_plugin_activation(definition.workspace, payload.plugin_names)
            except ValueError:
                return Status(400, {"error": "agent_definition_plugin_not_enabled"})
        update_fields = list(fields - {"model_config_ref", "thinking_mode", "avatar_data_base64"})
        try:
            if fields & {"model_config_ref", "thinking_mode"}:
                model_ref = payload.model_config_ref if "model_config_ref" in fields else definition.model_config_id
                if model_ref is None:
                    if payload.thinking_mode is not None:
                        return Status(400, {"error": "agent_model_settings_invalid"})
                    definition.model_config, definition.thinking_mode = None, ""
                else:
                    definition.model_config, definition.thinking_mode = selected_model(model_ref,
                        payload.thinking_mode, workspace_id=workspace_id)
                update_fields.extend(["model_config", "thinking_mode"])
            if "avatar_data_base64" in fields:
                definition.avatar_image, definition.avatar_digest = normalize_avatar(payload.avatar_data_base64)
                update_fields.extend(["avatar_image", "avatar_digest"])
        except (ValueError, AgentModelSettingsError) as error:
            return Status(400, {"error": str(error)})
        for field in fields - {"model_config_ref", "thinking_mode", "avatar_data_base64"}:
            setattr(definition, field, getattr(payload, field))
        definition.save(update_fields=[*update_fields, "updated_at"])
    return {"definition": serialize_agent_definition(definition)}


@router.get("/workspaces/{workspace_id}/agent-definitions/{definition_id}/versions", auth=session_auth,
            response={200: AgentDefinitionVersionsEnvelope} | COMMON_ERROR_RESPONSES)
def list_agent_definition_versions(request, workspace_id: str, definition_id: str):
    definition = _admin_definition(request.user, workspace_id, definition_id)
    if definition is None:
        return Status(404, {"error": "agent_definition_not_found"})
    if request.GET:
        return Status(400, {"error": "agent_definition_invalid"})
    return {"versions": [serialize_agent_definition_version(item) for item in definition.versions.order_by("version")]}


@router.post("/workspaces/{workspace_id}/agent-definitions/{definition_id}/versions", auth=session_auth,
             response={201: AgentDefinitionVersionEnvelope} | COMMON_ERROR_RESPONSES)
def publish_agent_definition(request, workspace_id: str, definition_id: str, payload: EmptyDefinitionRequest):
    with transaction.atomic():
        definition = _admin_definition(request.user, workspace_id, definition_id, lock=True)
        if definition is None:
            return Status(404, {"error": "agent_definition_not_found"})
        try:
            version = publish_definition(definition, request.user)
        except ValueError:
            return Status(409, {"error": "agent_definition_capability_unavailable"})
    return Status(201, {"version": serialize_agent_definition_version(version)})


@router.put("/workspaces/{workspace_id}/agent-definitions/{definition_id}/availability", auth=session_auth,
            response={200: AgentDefinitionEnvelope} | COMMON_ERROR_RESPONSES)
def set_agent_definition_availability(request, workspace_id: str, definition_id: str, payload: DefinitionAvailabilityRequest):
    with transaction.atomic():
        definition = _admin_definition(request.user, workspace_id, definition_id, lock=True)
        if definition is None:
            return Status(404, {"error": "agent_definition_not_found"})
        memberships = list(WorkspaceMembership.objects.select_for_update().filter(
            workspace_id=workspace_id, id__in=payload.membership_ids,
        ))
        if {item.id for item in memberships} != set(payload.membership_ids):
            return Status(400, {"error": "agent_definition_members_invalid"})
        definition.member_grants.all().delete()
        for membership in memberships:
            AgentDefinitionMember.objects.create(definition=definition, membership=membership)
        definition.availability_scope = payload.scope
        definition.save(update_fields=["availability_scope", "updated_at"])
    return {"definition": serialize_agent_definition(definition)}


@router.get("/workspaces/{workspace_id}/available-agent-definitions", auth=usage_auth("assistant:use"),
            response={200: AvailableAgentDefinitionsEnvelope} | COMMON_ERROR_RESPONSES)
def list_available_agent_definitions(request, workspace_id: str):
    membership = workspace_membership_for(request.user, workspace_id)
    if membership is None:
        return Status(404, {"error": "workspace_not_found"})
    if request.GET:
        return Status(400, {"error": "agent_definition_invalid"})
    definitions = available_agent_definitions(membership)
    if request.app_delegation is not None:
        definitions = definitions.filter(id=request.app_delegation.definition_id)
    return {"definitions": [serialize_agent_definition_version(item.published_version) for item in
                            definitions.order_by("created_at", "id")]}


@router.post("/workspaces/{workspace_id}/available-agent-definitions/{definition_id}/instance", auth=usage_auth("assistant:use"),
             response={200: AgentEnvelope, 201: AgentEnvelope} | COMMON_ERROR_RESPONSES)
def use_agent_definition(request, workspace_id: str, definition_id: str, payload: EmptyDefinitionRequest):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        require_request_delegation(request, "assistant:use", workspace_id=workspace_id, lock=True)
        definition = available_agent_definitions(membership).filter(id=definition_id).first()
        if definition is None:
            return Status(404, {"error": "agent_definition_not_available"})
        version = definition.published_version
        agent, created = Agent.objects.get_or_create(
            workspace=membership.workspace, owner=request.user, definition=definition, is_business_instance=False,
            defaults={"name": version.name, "description": version.description,
                      "instructions": version.instructions, "avatar_kind": version.avatar_kind},
        )
        if agent.status == "deleted":
            return Status(410, {"error": "agent_deleted"})
    return Status(201 if created else 200, {"agent": serialize_agent(agent)})


@router.get("/agent-definitions/{definition_id}/avatar", auth=session_auth,
    response={200: None} | COMMON_ERROR_RESPONSES)
def get_definition_avatar(request, definition_id: str):
    if set(request.GET) != {"v"}:
        return Status(400, {"error": "agent_avatar_request_invalid"})
    definition = AgentDefinition.objects.select_related("published_version").filter(pk=definition_id).first()
    membership = workspace_membership_for(request.user, definition.workspace_id) if definition else None
    if membership is None:
        return Status(404, {"error": "agent_definition_not_found"})
    avatar = definition.published_version
    if membership.role in WORKSPACE_ADMIN_ROLES and request.GET["v"] == definition.avatar_digest:
        avatar = definition
    elif not available_agent_definitions(membership).filter(pk=definition_id).exists():
        return Status(404, {"error": "agent_definition_not_found"})
    if avatar is None or not avatar.avatar_digest or request.GET["v"] != avatar.avatar_digest:
        return Status(404, {"error": "agent_definition_not_found"})
    response = HttpResponse(bytes(avatar.avatar_image), content_type="image/png")
    response["Cache-Control"] = "private, no-store"
    response["X-Content-Type-Options"] = "nosniff"
    return response
