from typing import Literal

from django.db import transaction
from django.db.models import Q
from django.utils import timezone
from django.utils.dateparse import parse_datetime
from ninja import Router, Status
from pydantic import Field, field_validator

from app_core.agent_identity import (
    normalize_agent_description,
    normalize_agent_instructions,
    normalize_agent_name,
)
from app_core.models import Agent, AgentRun, AgentCoordinationSession, Session
from app_core.agent_definitions import available_agent_definitions
from app_core.agent_inputs import require_native_agent_delegation
from app_core.app_delegations import require_request_delegation
from app_core.business_agent_scope import branch_for_agent, BusinessAgentConfigurationUnavailable
from app_core.trash_retention import trash_is_restorable
from app_core.workspace_access import (
    locked_workspace_membership_for,
    workspace_membership_for,
)

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
    if (set(request.GET) - {"afterSessionId", "limit"}
            or any(len(request.GET.getlist(field)) != 1 for field in request.GET)
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
    model_config_ref: str | None = Field(alias="modelConfigRef")
    thinking_mode: str | None = Field(alias="thinkingMode")


class AgentModelSettings(StrictSchema):
    schema_id: Literal["agent.model_settings.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    model_config_ref: str | None = Field(alias="modelConfigRef")
    thinking_mode: str | None = Field(alias="thinkingMode")
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
        if branch_for_agent(agent.pk) is not None:
            return Status(409, {"error": "agent_business_configuration_managed"})
        if payload.model_config_ref is None:
            if payload.thinking_mode is not None:
                return Status(400, {"error": "agent_model_settings_invalid"})
            agent.model_config, agent.thinking_mode = None, ""
        else:
            try:
                agent.model_config, agent.thinking_mode = selected_model(payload.model_config_ref, payload.thinking_mode)
            except AgentModelSettingsError as error:
                return Status(400, {"error": str(error)})
        agent.save(update_fields=["model_config", "thinking_mode", "updatedAt"])
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


class CreateAgentRequest(StrictSchema):
    name: str
    description: str = ""
    instructions: str = ""
    avatar_kind: Literal["centaeris", "banana"] = Field(
        "centaeris",
        alias="avatarKind",
    )

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


class UpdateAgentRequest(StrictSchema):
    name: str | None = None
    description: str | None = None
    instructions: str | None = None
    avatar_kind: Literal["centaeris", "banana"] | None = Field(
        None,
        alias="avatarKind",
    )

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
    ).select_related("definition__published_version").order_by("createdAt", "id")
    if request.app_delegation is not None:
        agents = agents.filter(definition_id=request.app_delegation.definition_id)
        if request.app_delegation.agent_id is not None:
            agents = agents.filter(pk=request.business_branch.agent_id)
    else:
        agents = agents.filter(business_branch__isnull=True)
    return {"agents": [serialize_agent(agent) for agent in agents]}


@router.post(
    "/workspaces/{workspace_id}/agents",
    auth=session_auth,
    response={201: AgentEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_agent(request, workspace_id: str, payload: CreateAgentRequest):
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        agent = Agent.objects.create(
            workspace=membership.workspace,
            owner=request.user,
            name=payload.name,
            description=payload.description,
            instructions=payload.instructions,
            avatar_kind=payload.avatar_kind,
        )
    return Status(201, {"agent": serialize_agent(agent)})


@router.get(
    "/agents/{agent_id}",
    auth=usage_auth("assistant:use"),
    response={200: AgentEnvelope} | COMMON_ERROR_RESPONSES,
)
def get_agent(request, agent_id: str):
    require_request_delegation(request, "assistant:use", agent_id=agent_id)
    agent = _owned_agent(request.user, agent_id)
    if agent is None or agent.status != "active":
        return Status(404, {"error": "agent_not_found"})
    return {"agent": serialize_agent(agent)}


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
    if not fields or any(getattr(payload, field) is None for field in fields):
        return Status(400, {"error": "agent_invalid"})
    with transaction.atomic():
        agent = _owned_agent(request.user, agent_id, lock=True)
        if agent is None:
            return Status(404, {"error": "agent_not_found"})
        if agent.status == "deleted":
            return Status(410, {"error": "agent_deleted"})
        if branch_for_agent(agent.pk) is not None:
            return Status(409, {"error": "agent_business_configuration_managed"})
        if agent.definition_id is not None:
            return Status(409, {"error": "agent_configuration_managed"})
        if all(getattr(agent, field) == getattr(payload, field) for field in fields):
            return Status(409, {"error": "agent_unchanged"})
        update_fields = []
        if "name" in fields:
            agent.name = payload.name
            update_fields.append("name")
        if "description" in fields:
            agent.description = payload.description
            update_fields.append("description")
        if "instructions" in fields:
            agent.instructions = payload.instructions
            update_fields.append("instructions")
        if "avatar_kind" in fields:
            agent.avatar_kind = payload.avatar_kind
            update_fields.append("avatar_kind")
        agent.save(update_fields=[*update_fields, "updatedAt"])
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
        if agent.status != "deleted":
            return Status(409, {"error": "agent_not_deleted"})
        if agent.purgedAt is not None:
            return Status(410, {"error": "agent_purged"})
        agent.purgedAt = timezone.now()
        agent.save(update_fields=["purgedAt", "updatedAt"])
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
