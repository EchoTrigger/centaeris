from typing import Literal

from django.db import transaction
from django.db.models import Max
from ninja import Router, Status
from pydantic import Field, model_validator

from app_core.agent_definitions import available_agent_definitions
from app_core.app_delegations import require_request_delegation
from app_core.models import Agent, AgentDefinition, AgentDefinitionMember, AgentDefinitionVersion, WorkspaceMembership
from app_core.plugin_catalog import selected_plugin_activation, validate_plugin_names
from app_core.workspace_access import WORKSPACE_ADMIN_ROLES, locked_workspace_membership_for, workspace_membership_for

from .agents import CreateAgentRequest, UpdateAgentRequest
from .response_schema import (
    AgentDefinitionEnvelope, AgentDefinitionsEnvelope, AgentDefinitionVersionEnvelope,
    AgentDefinitionVersionsEnvelope, AgentEnvelope, AvailableAgentDefinitionsEnvelope, COMMON_ERROR_RESPONSES,
)
from .schema import StrictSchema
from .security import session_auth, usage_auth
from .serialization import serialize_agent, serialize_agent_definition, serialize_agent_definition_version


router = Router(tags=["agent definitions"], by_alias=True)


class CreateAgentDefinitionRequest(CreateAgentRequest):
    plugin_names: list[str] = Field(default_factory=list, alias="pluginNames")

    @model_validator(mode="after")
    def validate_plugins(self):
        validate_plugin_names(sorted(self.plugin_names))
        self.plugin_names.sort()
        return self


class UpdateAgentDefinitionRequest(UpdateAgentRequest):
    status: Literal["active", "disabled"] | None = None
    plugin_names: list[str] | None = Field(default=None, alias="pluginNames")

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
    membership_ids: list[str] = Field(default_factory=list, alias="membershipIds")

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
        definition = AgentDefinition.objects.create(workspace=membership.workspace, created_by=request.user,
                                                     **payload.model_dump(by_alias=False))
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
    if not fields or any(getattr(payload, field) is None for field in fields):
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
        for field in fields:
            setattr(definition, field, getattr(payload, field))
        definition.save(update_fields=[*fields, "updated_at"])
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
            activation = selected_plugin_activation(definition.workspace, definition.plugin_names)
        except ValueError:
            return Status(409, {"error": "agent_definition_capability_unavailable"})
        latest = definition.versions.aggregate(latest=Max("version"))["latest"] or 0
        version = AgentDefinitionVersion.objects.create(
            definition=definition, version=latest + 1, published_by=request.user,
            name=definition.name, description=definition.description,
            instructions=definition.instructions, avatar_kind=definition.avatar_kind,
            plugin_activation=activation,
        )
        definition.published_version = version
        definition.save(update_fields=["published_version", "updated_at"])
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
            workspace=membership.workspace, owner=request.user, definition=definition,
            defaults={"name": version.name, "description": version.description,
                      "instructions": version.instructions, "avatar_kind": version.avatar_kind},
        )
        if agent.status == "deleted":
            return Status(410, {"error": "agent_deleted"})
    return Status(201 if created else 200, {"agent": serialize_agent(agent)})
