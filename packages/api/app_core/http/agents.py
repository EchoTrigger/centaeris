import re
from typing import Literal

from django.db import transaction
from django.db.models import Q
from django.http import HttpResponse
from django.utils import timezone
from django.utils.dateparse import parse_datetime
from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.agent_avatars import normalize_avatar
from app_core.agent_identity import (
    normalize_agent_description,
    normalize_agent_instructions,
    normalize_agent_name,
)
from app_core.models import Agent, AgentDefinition, AgentRun, AgentCoordinationSession, Session
from app_core.agent_definitions import available_agent_definitions, publish_definition
from app_core.plugin_catalog import selected_plugin_activation, validate_plugin_names, plugin_activation_for_workspace
from app_core.agent_inputs import require_native_agent_delegation
from app_core.app_delegations import require_request_delegation
from app_core.business_agent_scope import branch_for_agent, BusinessAgentConfigurationUnavailable
from app_core.trash_retention import trash_is_restorable
from app_core.workspace_access import (
    WORKSPACE_ADMIN_ROLES,
    shared_submission_membership_for,
    locked_workspace_membership_for,
    workspace_membership_for,
)

from .query import valid_query_fields
from .response_schema import (
    AgentCoordinationSessionResponse,
    AgentWorkSessionsResponse,
    AgentEnvelope,
    AgentsEnvelope,
    COMMON_ERROR_RESPONSES,
    DeletedResponse,
    SessionTrashEnvelope,
)
from .schema import StrictSchema
from .security import session_auth, usage_auth
from .serialization import serialize_agent, serialize_session
from .trash_pagination import read_trash_cursor, trash_page


router = Router(tags=["agents"], by_alias=True)


@router.get("/agents/{agent_id}/work-sessions", auth=session_auth,
            response={200: AgentWorkSessionsResponse} | COMMON_ERROR_RESPONSES)
def list_work_sessions(request, agent_id: str, afterSessionId: str | None = None, limit: int = 50):
    from app_core.agent_work_presentation import verified_work
    from app_core.app_delegations import session_authority_is_current
    from app_core.models import AgentWorkSession
    if (not valid_query_fields(request.GET, {"afterSessionId", "limit"})
            or not 1 <= limit <= 100):
        return Status(400, {"error": "agent_work_sessions_cursor_invalid"})
    agent = _owned_agent(request.user, agent_id)
    if agent is None or agent.status != "active":
        return Status(404, {"error": "agent_not_found"})
    rows = AgentWorkSession.objects.select_related("session", "operation").filter(
        coordination_session_id__in=AgentCoordinationSession.objects.filter(agent=agent).values("session_id"),
        session__agent=agent, session__owner=request.user, session__workspace_id=agent.workspace_id,
        coordination_session__agent=agent, coordination_session__owner=request.user,
        coordination_session__workspace_id=agent.workspace_id,
        session__status="active", session__purgedAt__isnull=True).order_by("-session__createdAt", "-session_id")
    if afterSessionId is not None:
        boundary = rows.filter(session_id=afterSessionId).first()
        if boundary is None:
            return Status(400, {"error": "agent_work_sessions_cursor_invalid"})
        rows = rows.filter(Q(session__createdAt__lt=boundary.session.createdAt)
            | Q(session__createdAt=boundary.session.createdAt, session_id__lt=afterSessionId))
    page = list(rows[:limit + 1])
    sessions = []
    for binding in page[:limit]:
        child = verified_work(binding)
        if child is not None and session_authority_is_current(request.user.id, child.session_id, scope="sessions:read"):
            sessions.append(serialize_session(child.session))
    has_more = len(page) > limit
    return {"schema": "agent.work_sessions.v1", "agentId": agent.id, "workspaceId": agent.workspace_id,
            "sessions": sessions, "nextAfterSessionId": page[limit - 1].session_id if has_more else None,
            "hasMore": has_more}


class CreateCoordinationSessionRequest(StrictSchema):
    pass


class UpdateAgentModelSettings(StrictSchema):
    schema_id: Literal["agent.model_settings.update.v1"] = Field(alias="schema")
    model_config_ref: str | None
    thinking_mode: str | None


class AgentModelSettings(StrictSchema):
    schema_id: Literal["agent.model_settings.v1"] = Field(alias="schema")
    agent_id: str
    model_config_ref: str | None
    thinking_mode: str | None
    status: Literal["unconfigured", "configured", "unavailable"]


@router.get("/agents/{agent_id}/model-settings", auth=session_auth,
    response={200: AgentModelSettings} | COMMON_ERROR_RESPONSES)
def get_model_settings(request, agent_id: str):
    from app_core.agent_model_settings import serialize_model_settings
    if request.GET:
        return Status(400, {"error": "agent_model_settings_invalid"})
    agent = _owned_agent(request.user, agent_id)
    if agent is None or agent.status != "active":
        return Status(404, {"error": "agent_not_found"})
    try:
        return serialize_model_settings(agent)
    except BusinessAgentConfigurationUnavailable as error:
        return Status(409, {"error": str(error)})


@router.patch("/agents/{agent_id}/model-settings", auth=session_auth,
    response={200: AgentModelSettings} | COMMON_ERROR_RESPONSES)
def update_model_settings(request, agent_id: str, payload: UpdateAgentModelSettings):
    from app_core.agent_model_settings import AgentModelSettingsError, selected_model, serialize_model_settings
    if request.GET:
        return Status(400, {"error": "agent_model_settings_invalid"})
    with transaction.atomic():
        agent = _owned_agent(request.user, agent_id, lock=True)
        if agent is None or agent.status != "active":
            return Status(404, {"error": "agent_not_found"})
        if workspace_membership_for(request.user, agent.workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
            return Status(403, {"error": "agent_configuration_forbidden"})
        if branch_for_agent(agent.pk) is not None:
            return Status(409, {"error": "agent_business_configuration_managed"})
        business_root = agent.definition_id is None and agent.business_branches.exists()
        if agent.definition_id is None and not business_root:
            return Status(409, {"error": "agent_configuration_promotion_required"})
        definition = agent if business_root else AgentDefinition.objects.select_for_update().get(pk=agent.definition_id)
        if payload.model_config_ref is None:
            if payload.thinking_mode is not None:
                return Status(400, {"error": "agent_model_settings_invalid"})
            definition.model_config, definition.thinking_mode = None, ""
        else:
            try:
                definition.model_config, definition.thinking_mode = selected_model(payload.model_config_ref, payload.thinking_mode, workspace_id=agent.workspace_id)
            except AgentModelSettingsError as error:
                return Status(400, {"error": str(error)})
        definition.save(update_fields=["model_config", "thinking_mode", "updatedAt" if business_root else "updated_at"])
        if not business_root:
            try:
                publish_definition(definition, request.user)
            except ValueError as error:
                transaction.set_rollback(True)
                return Status(409, {"error": str(error)})
            agent.definition = definition
        return serialize_model_settings(agent)


def _coordination_response(binding):
    return {"schema": "agent.coordination_session.v1", "agentId": binding.agent_id,
            "sessionId": binding.session_id}


@router.post("/agents/{agent_id}/coordination-session", auth=session_auth,
             response={200: AgentCoordinationSessionResponse, 201: AgentCoordinationSessionResponse} | COMMON_ERROR_RESPONSES)
def create_coordination_session(request, agent_id: str, payload: CreateCoordinationSessionRequest):
    with transaction.atomic():
        agent = _owned_agent(request.user, agent_id, lock=True)
        if agent is None or agent.status != "active":
            return Status(404, {"error": "agent_not_found"})
        membership = workspace_membership_for(request.user, agent.workspace_id)
        if agent.definition_id and not available_agent_definitions(membership).filter(id=agent.definition_id).exists():
            return Status(404, {"error": "agent_not_found"})
        binding = AgentCoordinationSession.objects.select_related("session").filter(agent=agent).first()
        if binding is not None:
            if binding.session.status != "active":
                return Status(410, {"error": "coordination_session_deleted"})
            return _coordination_response(binding)
        session = Session.objects.create(workspace_id=agent.workspace_id, owner=request.user,
                                         agent=agent, origin="automation", title="Agent coordination")
        binding = AgentCoordinationSession.objects.create(agent=agent, session=session)
        return Status(201, _coordination_response(binding))


@router.get("/agents/{agent_id}/coordination-session", auth=usage_auth("assistant:use"),
            response={200: AgentCoordinationSessionResponse} | COMMON_ERROR_RESPONSES)
def get_coordination_session(request, agent_id: str):
    from app_core.app_delegations import session_authority_is_current
    require_native_agent_delegation(request, "assistant:use", agent_id=agent_id)
    agent = _owned_agent(request.user, agent_id)
    binding = AgentCoordinationSession.objects.filter(agent=agent).first() if agent is not None else None
    if binding is None or not session_authority_is_current(request.user.id, binding.session_id):
        return Status(404, {"error": "coordination_session_not_found"})
    return _coordination_response(binding)


class AgentProfileFields(StrictSchema):
    name: str
    description: str = ""
    instructions: str = ""
    avatar_kind: Literal["centaeris", "banana"] = Field('centaeris')

    @field_validator("name")
    @classmethod
    def validate_name(cls, value: str) -> str:
        return normalize_agent_name(value)

    @field_validator("description")
    @classmethod
    def validate_description(cls, value: str) -> str:
        return normalize_agent_description(value)

    @field_validator("instructions")
    @classmethod
    def validate_instructions(cls, value: str) -> str:
        return normalize_agent_instructions(value)


class AgentProfileUpdate(StrictSchema):
    name: str | None = None
    description: str | None = None
    instructions: str | None = None
    avatar_kind: Literal["centaeris", "banana"] | None = Field(None)

    @field_validator("name")
    @classmethod
    def validate_name(cls, value: str | None) -> str | None:
        return None if value is None else normalize_agent_name(value)

    @field_validator("description")
    @classmethod
    def validate_description(cls, value: str | None) -> str | None:
        return None if value is None else normalize_agent_description(value)

    @field_validator("instructions")
    @classmethod
    def validate_instructions(cls, value: str | None) -> str | None:
        return None if value is None else normalize_agent_instructions(value)


class CreateAgentRequest(AgentProfileFields):
    avatar_data_base64: str | None = None
    plugin_names: list[str] = Field(default_factory=list)


class UpdateAgentRequest(AgentProfileUpdate):
    avatar_data_base64: str | None = None
    plugin_names: list[str] | None = None


@router.get(
    "/workspaces/{workspace_id}/agents",
    auth=usage_auth("assistant:use"),
    response={200: AgentsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_agents(request, workspace_id: str):
    require_request_delegation(request, "assistant:use", workspace_id=workspace_id)
    if workspace_membership_for(request.user, workspace_id) is None:
        return Status(404, {"error": "workspace_not_found"})
    agents = Agent.objects.filter(
        workspace_id=workspace_id,
        owner=request.user,
        status="active",
    ).select_related("definition__published_version").defer(
        "avatar_image", "definition__avatar_image", "definition__published_version__avatar_image",
    ).order_by("createdAt", "id")
    if request.app_delegation is not None:
        agents = agents.filter(definition_id=request.app_delegation.definition_id)
        if request.app_delegation.agent_id is not None:
            agents = agents.filter(pk=request.business_branch.agent_id)
        else:
            agents = agents.filter(is_business_instance=False)
    else:
        membership = workspace_membership_for(request.user, workspace_id)
        for definition in available_agent_definitions(membership):
            version = definition.published_version
            Agent.objects.defer("avatar_image").get_or_create(workspace_id=workspace_id, owner=request.user, definition=definition,
                is_business_instance=False, defaults={"name": version.name, "description": version.description,
                    "instructions": version.instructions, "avatar_kind": version.avatar_kind})
        agents = agents.filter(business_branch__isnull=True).filter(
            Q(definition__isnull=True) | Q(definition_id__in=available_agent_definitions(membership).values("id")))
    return {"agents": [serialize_agent(agent) for agent in agents]}


@router.post(
    "/workspaces/{workspace_id}/agents",
    auth=session_auth,
    response={201: AgentEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_agent(request, workspace_id: str, payload: CreateAgentRequest):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        try:
            validate_plugin_names(sorted(payload.plugin_names))
            selected_plugin_activation(membership.workspace, sorted(payload.plugin_names))
            avatar_image, avatar_digest = normalize_avatar(payload.avatar_data_base64)
        except ValueError as error:
            return Status(400, {"error": str(error)})
        definition = AgentDefinition.objects.create(
            workspace=membership.workspace, created_by=request.user, name=payload.name,
            description=payload.description, instructions=payload.instructions,
            avatar_kind=payload.avatar_kind, avatar_image=avatar_image, avatar_digest=avatar_digest,
            plugin_names=sorted(payload.plugin_names), availability_scope="workspace",
        )
        publish_definition(definition, request.user)
        if membership.workspace.default_agent_definition_id is None:
            membership.workspace.default_agent_definition = definition
            membership.workspace.save(update_fields=["default_agent_definition", "updatedAt"])
        agent = Agent.objects.create(workspace=membership.workspace, owner=request.user,
            definition=definition, name=payload.name, description=payload.description,
            instructions=payload.instructions, avatar_kind=payload.avatar_kind)
    return Status(201, {"agent": serialize_agent(agent)})


class PromoteAgentRequest(StrictSchema):
    bind_agent_ids: list[str] = Field(default_factory=list)
    make_default: bool = True


@router.post("/agents/{agent_id}/promote", auth=session_auth,
    response={200: AgentEnvelope} | COMMON_ERROR_RESPONSES)
def promote_agent(request, agent_id: str, payload: PromoteAgentRequest):
    with transaction.atomic():
        agent = _owned_agent(request.user, agent_id, lock=True)
        if agent is None or agent.status != "active":
            return Status(404, {"error": "agent_not_found"})
        if workspace_membership_for(request.user, agent.workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
            return Status(403, {"error": "agent_configuration_forbidden"})
        if agent.definition_id is not None or agent.is_business_instance or branch_for_agent(agent.pk) is not None or agent.business_branches.exists():
            return Status(409, {"error": "agent_configuration_managed"})
        identities = set(payload.bind_agent_ids) | {agent.id}
        instances = list(Agent.objects.select_for_update(of=("self",)).filter(id__in=identities,
            workspace_id=agent.workspace_id, definition__isnull=True,
            is_business_instance=False, business_branch__isnull=True, business_branches__isnull=True))
        if {item.id for item in instances} != identities:
            return Status(400, {"error": "agent_promotion_instances_invalid"})
        activation = plugin_activation_for_workspace(agent.workspace)
        definition = AgentDefinition.objects.create(workspace=agent.workspace, created_by=request.user,
            name=agent.name, description=agent.description, instructions=agent.instructions,
            avatar_kind=agent.avatar_kind, avatar_image=agent.avatar_image, avatar_digest=agent.avatar_digest,
            model_config=agent.model_config, thinking_mode=agent.thinking_mode,
            plugin_names=sorted(item["name"] for item in activation["packages"]), availability_scope="workspace")
        publish_definition(definition, request.user)
        # The explicit bind list is the migration authority; no personal policy is inferred.
        if len({item.owner_id for item in instances}) != len(instances):
            transaction.set_rollback(True)
            return Status(409, {"error": "agent_promotion_owner_conflict"})
        # Ordinary model saves keep the binding immutable. This explicit promotion
        # is the sole forward transition for released personal instances.
        Agent.objects.filter(id__in=identities).update(definition=definition,
            avatar_image=None, avatar_digest="", updatedAt=timezone.now())
        if payload.make_default:
            agent.workspace.default_agent_definition = definition
            agent.workspace.save(update_fields=["default_agent_definition", "updatedAt"])
        agent.definition = definition
        agent.avatar_image, agent.avatar_digest = None, ""
    return {"agent": serialize_agent(agent)}


@router.get("/workspaces/{workspace_id}/default-agent", auth=session_auth,
    response={200: AgentEnvelope} | COMMON_ERROR_RESPONSES)
def get_default_agent(request, workspace_id: str):
    from app_core.agent_defaults import AgentPromotionRequired, ensure_default_agent
    current = workspace_membership_for(request.user, workspace_id)
    if current is None:
        return Status(404, {"error": "workspace_not_found"})
    pin = locked_workspace_membership_for if current.workspace.default_agent_definition_id is None else shared_submission_membership_for
    with transaction.atomic():
        membership = pin(request.user, workspace_id)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        try:
            agent, _ = ensure_default_agent(membership.workspace, request.user)
        except AgentPromotionRequired as error:
            return Status(409, {"error": str(error)})
    if agent.status == "deleted":
        return Status(410, {"error": "agent_deleted"})
    return {"agent": serialize_agent(agent)}


@router.get(
    "/agents/{agent_id}",
    auth=usage_auth("assistant:use"),
    response={200: AgentEnvelope} | COMMON_ERROR_RESPONSES,
)
def get_agent(request, agent_id: str):
    require_request_delegation(request, "assistant:use", agent_id=agent_id)
    agent = _owned_agent(request.user, agent_id)
    if agent is None or agent.status != "active" or (agent.definition_id and agent.definition.status != "active"):
        return Status(404, {"error": "agent_not_found"})
    return {"agent": serialize_agent(agent)}


@router.get("/agents/{agent_id}/avatar", auth=session_auth, response={200: None} | COMMON_ERROR_RESPONSES,
    openapi_extra={"responses": {200: {
        "description": "Private normalized Agent avatar",
        "content": {"image/png": {"schema": {"type": "string", "format": "binary"}}},
    }}})
def get_agent_avatar(request, agent_id: str):
    if not valid_query_fields(request.GET, {"v"}, required={"v"}):
        return Status(400, {"error": "agent_avatar_request_invalid"})
    version = request.GET["v"]
    if re.fullmatch(r"[0-9a-f]{64}", version) is None:
        return Status(400, {"error": "agent_avatar_request_invalid"})
    agent = _owned_agent(request.user, agent_id)
    avatar = agent.definition.published_version if agent is not None and agent.definition_id else agent
    if (agent is None or agent.status != "active" or agent.purgedAt is not None
            or avatar is None or agent.is_business_instance
            or not avatar.avatar_digest or version != avatar.avatar_digest):
        return Status(404, {"error": "agent_not_found"})
    response = HttpResponse(bytes(avatar.avatar_image), content_type="image/png")
    response["Cache-Control"] = "private, no-store"
    response["X-Content-Type-Options"] = "nosniff"
    return response


@router.get(
    "/agents/{agent_id}/trash/sessions",
    auth=session_auth,
    response={200: SessionTrashEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_trashed_agent_sessions(request, agent_id: str):
    agent = _owned_agent(request.user, agent_id)
    if agent is None:
        return Status(404, {"error": "agent_not_found"})
    if agent.status != "deleted":
        return Status(409, {"error": "agent_not_deleted"})
    if not trash_is_restorable(agent.deletedAt, agent.purgedAt):
        return Status(410, {"error": "agent_expired"})
    try:
        cursor = read_trash_cursor(
            request,
            f"agent:{agent_id}:sessions",
            {"isPinned", "updatedAt", "id"},
        )
        updated_at = parse_datetime(cursor["updatedAt"]) if cursor else None
        if cursor and (
            not isinstance(cursor["isPinned"], bool)
            or updated_at is None
            or not timezone.is_aware(updated_at)
            or not isinstance(cursor["id"], str)
            or not cursor["id"]
        ):
            raise ValueError("trash_cursor_invalid")
    except (TypeError, ValueError) as error:
        return Status(400, {"error": str(error)})
    sessions = agent.sessions.all()
    if cursor:
        same_pin_after = Q(isPinned=cursor["isPinned"]) & (
            Q(updatedAt__lt=updated_at)
            | Q(updatedAt=updated_at, id__gt=cursor["id"])
        )
        sessions = sessions.filter(
            same_pin_after | Q(isPinned=False)
            if cursor["isPinned"]
            else same_pin_after
        )
    sessions, next_cursor, has_more = trash_page(
        sessions.order_by("-isPinned", "-updatedAt", "id"),
        f"agent:{agent_id}:sessions",
        lambda session: {
            "isPinned": session.isPinned,
            "updatedAt": session.updatedAt.isoformat(),
            "id": session.id,
        },
    )
    running_session_ids = set(
        AgentRun.objects.filter(
            session__in=sessions,
            status__in={"queued", "running"},
        ).values_list("session_id", flat=True)
    )
    return {
        "sessions": [
            serialize_session(
                session,
                has_active_agent_run=session.id in running_session_ids,
            )
            for session in sessions
        ],
        "nextCursor": next_cursor,
        "hasMore": has_more,
    }


@router.patch(
    "/agents/{agent_id}",
    auth=session_auth,
    response={200: AgentEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_agent(request, agent_id: str, payload: UpdateAgentRequest):
    fields = payload.model_fields_set
    if not fields or any(getattr(payload, field) is None for field in fields if field != "avatar_data_base64"):
        return Status(400, {"error": "agent_invalid"})
    with transaction.atomic():
        agent = _owned_agent(request.user, agent_id, lock=True)
        if agent is None:
            return Status(404, {"error": "agent_not_found"})
        if agent.status == "deleted":
            return Status(410, {"error": "agent_deleted"})
        if agent.is_business_instance or branch_for_agent(agent.pk) is not None:
            return Status(409, {"error": "agent_business_configuration_managed"})
        if workspace_membership_for(request.user, agent.workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
            return Status(403, {"error": "agent_configuration_forbidden"})
        business_root = agent.definition_id is None and agent.business_branches.exists()
        if agent.definition_id is None and not business_root:
            return Status(409, {"error": "agent_configuration_promotion_required"})
        if business_root and "plugin_names" in fields:
            return Status(409, {"error": "agent_business_configuration_managed"})
        definition = agent if business_root else AgentDefinition.objects.select_for_update().get(pk=agent.definition_id)
        update_fields = []
        try:
            if "avatar_data_base64" in fields:
                definition.avatar_image, definition.avatar_digest = normalize_avatar(payload.avatar_data_base64)
                update_fields.extend(["avatar_image", "avatar_digest"])
            if "plugin_names" in fields:
                validate_plugin_names(sorted(payload.plugin_names))
                selected_plugin_activation(agent.workspace, sorted(payload.plugin_names))
            for field in fields - {"avatar_data_base64"}:
                setattr(definition, field, sorted(payload.plugin_names) if field == "plugin_names" else getattr(payload, field))
                update_fields.append(field)
            definition.save(update_fields=[*update_fields, "updatedAt" if business_root else "updated_at"])
            if not business_root:
                publish_definition(definition, request.user)
        except ValueError as error:
            transaction.set_rollback(True)
            return Status(400, {"error": str(error)})
        if not business_root:
            agent.definition = definition
    return {"agent": serialize_agent(agent)}


@router.delete(
    "/agents/{agent_id}",
    auth=session_auth,
    response={200: DeletedResponse} | COMMON_ERROR_RESPONSES,
)
def delete_agent(request, agent_id: str):
    with transaction.atomic():
        agent = _owned_agent(request.user, agent_id, lock=True)
        if agent is None:
            return Status(404, {"error": "agent_not_found"})
        if workspace_membership_for(request.user, agent.workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
            return Status(403, {"error": "agent_configuration_forbidden"})
        if agent.definition_id is not None:
            from app_core.agent_defaults import replace_retired_default
            definition = AgentDefinition.objects.select_for_update().get(pk=agent.definition_id)
            if definition.status == "disabled":
                return Status(410, {"error": "agent_deleted"})
            definition.status = "disabled"
            definition.save(update_fields=["status", "updated_at"])
            replace_retired_default(agent.workspace, request.user, definition)
            return {"deleted": True}
        if agent.status == "deleted":
            return Status(410, {"error": "agent_deleted"})
        if AgentRun.objects.filter(Q(session__agent=agent)
            | Q(session__agent__business_branch__root_agent=agent), status__in={"queued", "running"}).exists():
            return Status(409, {"error": "agent_has_active_agent_run"})
        agent.status = "deleted"
        agent.deletedAt = timezone.now()
        agent.deletedBy = request.user
        agent.save(update_fields=["status", "deletedAt", "deletedBy", "updatedAt"])
    return {"deleted": True}


@router.post(
    "/agents/{agent_id}/restore",
    auth=session_auth,
    response={200: AgentEnvelope} | COMMON_ERROR_RESPONSES,
)
def restore_agent(request, agent_id: str):
    with transaction.atomic():
        agent = _owned_agent(request.user, agent_id, lock=True)
        if agent is None:
            return Status(404, {"error": "agent_not_found"})
        if workspace_membership_for(request.user, agent.workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
            return Status(403, {"error": "agent_configuration_forbidden"})
        if agent.definition_id is not None:
            return Status(409, {"error": "agent_configuration_managed"})
        if agent.status != "deleted":
            return Status(409, {"error": "agent_not_deleted"})
        if not trash_is_restorable(agent.deletedAt, agent.purgedAt):
            return Status(410, {"error": "agent_expired"})
        agent.status = "active"
        agent.deletedAt = None
        agent.deletedBy = None
        agent.purgedAt = None
        agent.save(
            update_fields=["status", "deletedAt", "deletedBy", "purgedAt", "updatedAt"]
        )
    return {"agent": serialize_agent(agent)}


@router.delete(
    "/agents/{agent_id}/trash",
    auth=session_auth,
    response={200: DeletedResponse} | COMMON_ERROR_RESPONSES,
)
def permanently_delete_agent(request, agent_id: str):
    with transaction.atomic():
        agent = _owned_agent(request.user, agent_id, lock=True)
        if agent is None:
            return Status(404, {"error": "agent_not_found"})
        if workspace_membership_for(request.user, agent.workspace_id, allowed_roles=WORKSPACE_ADMIN_ROLES) is None:
            return Status(403, {"error": "agent_configuration_forbidden"})
        if agent.definition_id is not None:
            return Status(409, {"error": "agent_configuration_managed"})
        if agent.status != "deleted":
            return Status(409, {"error": "agent_not_deleted"})
        if agent.purgedAt is not None:
            return Status(410, {"error": "agent_purged"})
        agent.purgedAt = timezone.now()
        agent.avatar_image = None
        agent.avatar_digest = ""
        agent.save(update_fields=["purgedAt", "avatar_image", "avatar_digest", "updatedAt"])
    return {"deleted": True}


def _owned_agent(user, agent_id: str, *, lock: bool = False) -> Agent | None:
    agent_scope = Agent.objects.filter(id=agent_id, owner=user).values_list(
        "workspace_id",
        flat=True,
    ).first()
    if agent_scope is None:
        return None
    if lock:
        if locked_workspace_membership_for(user, agent_scope) is None:
            return None
        return Agent.objects.select_for_update().filter(
            id=agent_id,
            owner=user,
            workspace_id=agent_scope,
        ).first()
    if workspace_membership_for(user, agent_scope) is None:
        return None
    return Agent.objects.filter(
        id=agent_id,
        owner=user,
        workspace_id=agent_scope,
    ).first()
