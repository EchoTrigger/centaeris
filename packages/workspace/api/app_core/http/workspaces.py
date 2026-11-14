import json
import logging
import re
from app_core.transcript_output import read_output_range
from typing import Literal

from django.db import transaction
from django.http import HttpResponse, JsonResponse
from django.db.models import Max
from django.utils import timezone
from ninja import Router, Status
from ninja.responses import codes_4xx
from pydantic import Field, ValidationError, field_validator, model_validator

from app_core.assets import captured_input_fields
from app_core.app_delegations import require_delegated_session, require_request_delegation
from app_core.hosted_operations import (
    OPERATION_ID_PATTERN,
    HostedOperationError,
    accept_operation,
    lock_operation_identity,
    replay_operation,
    request_digest,
    serialize_operation,
)
from app_core.models import (
    Artifact,
    Session,
    SessionProject,
    ModelConfig,
    SessionAssetLink,
    SessionEvent,
    AgentRun,
    McpBearerCredential,
    Workspace,
    WorkspacePluginEnablement,
    validate_thinking_mode,
)
from app_core.plugin_catalog import (
    activation_digest,
    load_plugin_bearer_credential_refs,
    load_plugin_interfaces,
    plugin_lifecycle_lock,
    plugin_activation_for_workspace,
)
from app_core.agent_run_authorization_factory import create_agent_run_authorization
from app_core.session_model_settings import selected_session_model, SessionModelSettingsError
from app_core.session_deletion import purge_session_runtime, record_session_purge_intent
from app_core.agent_run_stream import encode_stream_cursor
from app_core.presentation_activity import pending_agent_run_supplements
from app_core.runtime_client import (
    TranscriptRuntimeError,
    request_agent_run_cancellation,
    request_agent_run_supplement,
    request_agent_run_question_response,
    request_workspace_skill_catalog,
    request_workspace_skill_detail,
    request_workspace_hook_catalog,
    request_workspace_mcp_catalog,
    request_execution_profile,
    request_transcript_page,
    request_transcript_patches,
    request_transcript_content,
    schedule_agent_run_lifecycle,
)
from app_core.session_event import (
    committed_session_terminal_state,
    project_committed_agent_run,
)
from app_core.trash_retention import trash_is_restorable
from app_core.workspace_access import (
    WORKSPACE_ADMIN_ROLES,
    agent_run_membership_is_current,
    locked_workspace_membership_for,
    shared_submission_membership_for,
    workspace_membership_for,
)

from .query import valid_query_fields
from .response_schema import (
    COMMON_ERROR_RESPONSES,
    DeletedResponse,
    SessionEnvelope,
    SessionProjectEnvelope,
    SessionProjectsEnvelope,
    SessionContextUsageEnvelope,
    TranscriptContentRangeResponse,
    TranscriptTurnMetadataResponse,
    HostedOperationResponse,
    AgentRunCancellationResponse,
    AgentRunEnvironmentResponse,
    AgentRunSupplementResponse,
    ActiveTranscriptAgentRunEnvelope,
    SessionsEnvelope,
    SessionSearchEnvelope,
    WorkspacesEnvelope,
    WorkspacePluginEnvelope,
    WorkspacePluginResponse,
    WorkspacePluginsEnvelope,
    WorkspaceSkillDetailEnvelope,
    WorkspaceSkillsEnvelope,
)
from .library import (
    _create_uploaded_library_objects,
    _delete_stored_upload_batch,
    _require_uploads,
    _store_upload_batch,
)
from .schema import ErrorResponse, StrictSchema
from .security import session_auth, usage_auth
from .serialization import (
    serialize_model,
    serialize_session,
    serialize_session_project,
    serialize_workspace,
)


from app_core.workspace_plugins import workspace_plugin_catalog

logger = logging.getLogger(__name__)
router = Router(tags=["workspaces-sessions"], by_alias=True)
TRANSCRIPT_PROJECTION_VERSION = "transcript.projection.v1"
TRANSCRIPT_CONTENT_RANGE_BYTES = 64 * 1024
CONTEXT_COMPACTION_HEADROOM_TOKENS = 32_768


class AgentSessionCreationError(Exception):
    def __init__(self, status: int, code: str):
        super().__init__(code)
        self.status = status
        self.code = code


class AcceptedOperationReplay(Exception):
    """Leave the acceptance transaction before cleaning up losing upload objects."""

    def __init__(self, receipt):
        super().__init__("operation_already_accepted")
        self.receipt = receipt


class RewriteLastUserTailRequest(StrictSchema):
    type: Literal["rewriteLastUser"]
    target_message_id: str = Field(min_length=1, max_length=160)
    expected_tail_message_id: str = Field(min_length=1, max_length=160)


class SessionMessageRequest(StrictSchema):
    operation_id: str = Field(pattern=OPERATION_ID_PATTERN)
    text: str = ""
    project_id: str | None = Field(default=None)
    model_config_ref: str = Field(min_length=1)
    thinking_mode: str | None = Field(default=None)
    attachment_refs: list[str] = Field(default_factory=list)
    tail_action: RewriteLastUserTailRequest | None = Field(default=None)


class SessionCreateRequest(StrictSchema):
    operation_id: str = Field(pattern=OPERATION_ID_PATTERN)
    project_id: str | None = Field(default=None)


class SessionProjectCreateRequest(StrictSchema):
    name: str

    @field_validator("name")
    @classmethod
    def normalize_name(cls, value: str) -> str:
        value = value.strip()
        if not value or len(value) > 100 or not value.isprintable():
            raise ValueError("session_project_name_invalid")
        return value


class AgentRunSupplementRequest(StrictSchema):
    supplement_id: str = Field(min_length=1, max_length=64)
    message: str = Field(min_length=1)

    @field_validator("supplement_id")
    @classmethod
    def validate_supplement_id(cls, value: str) -> str:
        if (
            value != value.strip()
            or any(
                ord(character) < 32 or 127 <= ord(character) <= 159
                for character in value
            )
            or len(value.encode()) > 64
        ):
            raise ValueError("turn_supplement_id_invalid")
        return value

    @field_validator("message")
    @classmethod
    def validate_message(cls, value: str) -> str:
        value = value.strip()
        if not value:
            raise ValueError("turn_supplement_message_required")
        if len(value.encode()) > 65_536:
            raise ValueError("turn_supplement_message_too_large")
        return value


class SessionReadRequest(StrictSchema):
    through_sequence: str = Field(pattern=r"^(0|[1-9][0-9]*)$", max_length=19)

    @field_validator("through_sequence")
    @classmethod
    def validate_sequence(cls, value):
        from app_core.session_attention import MAX_SEQUENCE
        if int(value) > MAX_SEQUENCE:
            raise ValueError("session_read_sequence_invalid")
        return value


class QuestionResponseRequest(StrictSchema):
    operation_id: str = Field(pattern=OPERATION_ID_PATTERN, min_length=1, max_length=128)
    continuation_id: str = Field(min_length=1, max_length=256)
    action: Literal["answer", "skip"]
    answer: str | None = Field(default=None, max_length=65536)

    @model_validator(mode="after")
    def validate_response(self):
        if not self.continuation_id.strip():
            raise ValueError("question_continuation_required")
        if self.action == "answer":
            if self.answer is None or not self.answer.strip() or len(self.answer.encode()) > 65536:
                raise ValueError("question_answer_required")
        elif "answer" in self.model_fields_set:
            raise ValueError("question_skip_has_no_answer")
        return self


class SessionMetadataRequest(StrictSchema):
    title: str | None = None
    is_pinned: bool | None = Field(default=None)
    is_unread: bool | None = Field(default=None)


class PluginEnablementRequest(StrictSchema):
    enabled: bool


@router.get(
    "/workspaces",
    auth=session_auth,
    response={200: WorkspacesEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_workspaces(request):
    workspaces = [
        serialize_workspace(membership.workspace, membership.role)
        for membership in request.user.workspace_memberships.select_related(
            "workspace"
        ).filter(workspace__status="active")
    ]
    return {"workspaces": workspaces}


@router.get(
    "/workspaces/{workspace_id}/plugins",
    auth=session_auth,
    response={200: WorkspacePluginsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_workspace_plugins(request, workspace_id: str):
    workspace = _plugin_workspace_for_user(request.user, workspace_id)
    if workspace is None:
        return Status(404, {"error": "workspace_not_found"})
    try:
        catalog = workspace_plugin_catalog(workspace, require_packages=False)
    except ValueError:
        logger.exception("Release Plugin catalog is invalid")
        return Status(503, {"error": "plugin_catalog_invalid"})
    enabled = set(
        workspace.pluginEnablements.values_list("pluginName", flat=True)
    )
    catalog_names = {package["name"] for package in catalog["packages"]}
    if enabled - catalog_names:
        return Status(409, {"error": "workspace_plugin_enablement_invalid"})
    return {
        "plugins": [
            _workspace_plugin(package, package["name"] in enabled, workspace=workspace)
            for package in catalog["packages"]
        ]
    }


@router.get(
    "/workspaces/{workspace_id}/plugins/{plugin_name}",
    auth=session_auth,
    response={200: WorkspacePluginEnvelope} | COMMON_ERROR_RESPONSES,
)
def get_workspace_plugin(request, workspace_id: str, plugin_name: str):
    workspace = _plugin_workspace_for_user(request.user, workspace_id)
    if workspace is None:
        return Status(404, {"error": "workspace_not_found"})
    try:
        catalog = workspace_plugin_catalog(workspace, require_packages=False)
    except ValueError:
        return Status(503, {"error": "plugin_catalog_invalid"})
    package = next((item for item in catalog["packages"] if item["name"] == plugin_name), None)
    if package is None:
        return Status(404, {"error": "plugin_not_found"})
    enabled = workspace.pluginEnablements.filter(pluginName=plugin_name).exists()
    return {"plugin": _workspace_plugin(package, enabled, inspect=True, workspace=workspace)}


@router.patch(
    "/workspaces/{workspace_id}/plugins/{plugin_name}",
    auth=session_auth,
    response={200: WorkspacePluginEnvelope} | COMMON_ERROR_RESPONSES,
)
@transaction.atomic
def set_workspace_plugin_enabled(
    request,
    workspace_id: str,
    plugin_name: str,
    payload: PluginEnablementRequest,
):
    workspace = _plugin_workspace_for_user(
        request.user,
        workspace_id,
        allowed_roles=WORKSPACE_ADMIN_ROLES,
    )
    if workspace is None:
        return Status(404, {"error": "workspace_not_found"})
    try:
        catalog = workspace_plugin_catalog(workspace, require_packages=False)
        package = next(
            (
                item
                for item in catalog["packages"]
                if item["name"] == plugin_name
            ),
            None,
        )
    except ValueError:
        logger.exception("Release Plugin catalog is invalid")
        return Status(503, {"error": "plugin_catalog_invalid"})
    if package is None:
        return Status(404, {"error": "plugin_not_found"})
    plugin = _workspace_plugin(package, payload.enabled, inspect=payload.enabled, workspace=workspace)
    if payload.enabled and plugin["errors"]:
        return Status(409, {"error": "workspace_plugin_unavailable"})
    with plugin_lifecycle_lock():
        try:
            current = next((
                item for item in workspace_plugin_catalog(workspace, require_packages=False)["packages"]
                if item["name"] == plugin_name
            ), None)
        except ValueError:
            logger.exception("Release Plugin catalog is invalid")
            return Status(503, {"error": "plugin_catalog_invalid"})
        if current is None:
            return Status(404, {"error": "plugin_not_found"})
        if current != package:
            return Status(409, {"error": "workspace_plugin_package_changed"})
        with transaction.atomic():
            if payload.enabled:
                WorkspacePluginEnablement.objects.get_or_create(
                    workspace=workspace,
                    pluginName=plugin_name,
                )
            else:
                WorkspacePluginEnablement.objects.filter(
                    workspace=workspace,
                    pluginName=plugin_name,
                ).delete()
    return {"plugin": plugin}


@router.get(
    "/workspaces/{workspace_id}/skills",
    auth=session_auth,
    response={200: WorkspaceSkillsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_workspace_skills(request, workspace_id: str):
    workspace = _workspace_for_user(request.user, workspace_id)
    if workspace is None:
        return Status(404, {"error": "workspace_not_found"})
    try:
        return request_workspace_skill_catalog(plugin_activation_for_workspace(workspace))
    except (RuntimeError, ValueError):
        logger.exception("Workspace Skill catalog is unavailable")
        return Status(503, {"error": "workspace_skill_catalog_unavailable"})


@router.get(
    "/workspaces/{workspace_id}/skills/{skill_id}",
    auth=session_auth,
    response={200: WorkspaceSkillDetailEnvelope} | COMMON_ERROR_RESPONSES,
)
def get_workspace_skill(request, workspace_id: str, skill_id: str):
    workspace = _workspace_for_user(request.user, workspace_id)
    if workspace is None:
        return Status(404, {"error": "workspace_not_found"})
    try:
        return request_workspace_skill_detail(
            plugin_activation_for_workspace(workspace), skill_id
        )
    except LookupError:
        return Status(404, {"error": "skill_not_found"})
    except (RuntimeError, ValueError):
        logger.exception("Workspace Skill detail is unavailable")
        return Status(503, {"error": "workspace_skill_detail_unavailable"})


@router.get(
    "/workspaces/{workspace_id}/session-projects",
    auth=session_auth,
    response={200: SessionProjectsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_session_projects(request, workspace_id: str):
    workspace = _workspace_for_user(request.user, workspace_id)
    if workspace is None:
        return Status(404, {"error": "workspace_not_found"})
    if request.GET:
        return Status(400, {"error": "session_project_filter_unsupported"})
    projects = SessionProject.objects.filter(
        workspace=workspace,
        owner=request.user,
    ).order_by("created_at", "id")
    return {"projects": [serialize_session_project(project) for project in projects]}


@router.post(
    "/workspaces/{workspace_id}/session-projects",
    auth=session_auth,
    response={201: SessionProjectEnvelope} | COMMON_ERROR_RESPONSES,
)
def create_session_project(
    request, workspace_id: str, payload: SessionProjectCreateRequest
):
    workspace = _workspace_for_user(request.user, workspace_id)
    if workspace is None:
        return Status(404, {"error": "workspace_not_found"})
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace = membership.workspace
        project = SessionProject.objects.create(
            workspace=workspace,
            owner=request.user,
            name=payload.name,
        )
    return Status(201, {"project": serialize_session_project(project)})


def _authorized_workspace_sessions(request, workspace_id: str):
    workspace = _workspace_for_user(request.user, workspace_id)
    if workspace is None:
        return Status(404, {"error": "workspace_not_found"})
    require_request_delegation(request, "sessions:read", workspace_id=workspace_id)
    return Session.objects.filter(workspace=workspace, owner=request.user, status="active", purpose="interactive")


def prepare_workspace_session_list(request, workspace_id: str):
    """Authorize and scope navigation without loading or projecting its rows."""
    sessions = _authorized_workspace_sessions(request, workspace_id)
    if isinstance(sessions, Status):
        return sessions
    if request.GET:
        return Status(400, {"error": "session_filter_unsupported"})
    return sessions


@router.get(
    "/workspaces/{workspace_id}/sessions/search",
    auth=usage_auth("sessions:read"),
    response={200: SessionSearchEnvelope} | COMMON_ERROR_RESPONSES,
)
def search_workspace_sessions(request, workspace_id: str):
    sessions = _authorized_workspace_sessions(request, workspace_id)
    if isinstance(sessions, Status):
        return sessions
    if not valid_query_fields(request.GET, {"query", "cursor"}, required={"query"}):
        return Status(400, {"error": "session_search_query_invalid"})
    query = request.GET["query"].strip()
    cursor = request.GET.get("cursor")
    if not query or len(query) > 256 or "\0" in query or (cursor is not None and (not cursor or len(cursor) > 4096)):
        return Status(400, {"error": "session_search_query_invalid"})
    from app_core.session_search import search_sessions
    try:
        return search_sessions(sessions.filter(archivedAt__isnull=True), user_id=request.user.pk,
            workspace_id=workspace_id, query=query, cursor=cursor)
    except ValueError as error:
        if str(error) != "session_search_cursor_invalid":
            raise
        return Status(400, {"error": str(error)})


@router.get(
    "/workspaces/{workspace_id}/sessions",
    auth=usage_auth("sessions:read"),
    response={200: SessionsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_workspace_sessions(request, workspace_id: str):
    sessions_query = prepare_workspace_session_list(request, workspace_id)
    if isinstance(sessions_query, Status):
        return sessions_query
    sessions = list(sessions_query.filter(archivedAt__isnull=True).order_by("-isPinned", "-updatedAt"))
    from app_core.session_attention import refresh_session_attention_many
    refresh_session_attention_many(sessions)
    running_session_ids = set(
        AgentRun.objects.filter(
            session__in=sessions, status__in=["queued", "running"]
        ).values_list("session_id", flat=True)
    )
    return {
        "sessions": [
            serialize_session(session, has_active_agent_run=session.id in running_session_ids, attention_is_current=True)
            for session in sessions
        ]
    }


@router.get(
    "/workspaces/{workspace_id}/archived-sessions",
    auth=session_auth,
    response={200: SessionsEnvelope} | COMMON_ERROR_RESPONSES,
)
def list_archived_sessions(request, workspace_id: str):
    sessions_query = prepare_workspace_session_list(request, workspace_id)
    if isinstance(sessions_query, Status):
        return sessions_query
    sessions = list(sessions_query.filter(archivedAt__isnull=False).order_by("-archivedAt", "-id"))
    from app_core.session_attention import refresh_session_attention_many
    refresh_session_attention_many(sessions)
    return {
        "sessions": [
            serialize_session(session, has_active_agent_run=False, attention_is_current=True)
            for session in sessions
        ],
    }


@router.post(
    "/workspaces/{workspace_id}/sessions",
    auth=usage_auth("sessions:create"),
    response={201: HostedOperationResponse} | COMMON_ERROR_RESPONSES,
)
def create_workspace_session(request, workspace_id: str, payload: SessionCreateRequest):
    workspace = _workspace_for_user(request.user, workspace_id)
    if workspace is None:
        return Status(404, {"error": "workspace_not_found"})
    with transaction.atomic():
        membership = locked_workspace_membership_for(request.user, workspace_id)
        if membership is None:
            return Status(404, {"error": "workspace_not_found"})
        workspace = membership.workspace
        grant = require_request_delegation(request, "sessions:create", workspace_id=workspace_id,
                                            lock=True, require_active=False)
        digest = request_digest(payload)
        try:
            receipt = replay_operation(request.user, workspace_id, "createSession", payload.operation_id, digest,
                                       app_delegation=grant)
        except HostedOperationError as error:
            return Status(error.status, {"error": error.code})
        if receipt is not None:
            return Status(201, serialize_operation(receipt))
        project = None
        if payload.project_id is not None:
            project = SessionProject.objects.filter(
                id=payload.project_id,
                workspace=workspace,
                owner=request.user,
            ).first()
            if project is None:
                return Status(404, {"error": "session_project_not_found"})
        session = Session.objects.create(
            workspace=workspace,
            owner=request.user,
            project=project,
        )
        receipt = accept_operation(request.user, workspace, "createSession", payload.operation_id, digest, session,
                                   app_delegation=grant)
    return Status(201, serialize_operation(receipt))


@router.get(
    "/workspaces/{workspace_id}/operations/{command}/{operation_id}",
    auth=usage_auth("sessions:read"),
    response={200: HostedOperationResponse} | COMMON_ERROR_RESPONSES,
)
def get_hosted_operation(request, response: HttpResponse, workspace_id: str,
                         command: Literal["createSession", "submitMessage", "respondQuestion"],
                         operation_id: str):
    response["Cache-Control"] = "no-store"
    if request.GET or re.fullmatch(OPERATION_ID_PATTERN, operation_id) is None:
        return Status(400, {"error": "request_invalid"})
    try:
        with transaction.atomic():
            if locked_workspace_membership_for(request.user, workspace_id) is None:
                return Status(404, {"error": "operation_not_found"})
            grant = require_request_delegation(request, "sessions:read", workspace_id=workspace_id, lock=True)
            receipt = replay_operation(request.user, workspace_id, command, operation_id, app_delegation=grant)
            if receipt is None:
                return Status(404, {"error": "operation_not_found"})
            return serialize_operation(receipt)
    except HostedOperationError as error:
        return Status(error.status, {"error": error.code})


@router.get(
    "/sessions/{session_id}",
    auth=usage_auth("sessions:read"),
    response={200: SessionEnvelope} | COMMON_ERROR_RESPONSES,
)
def get_session(request, session_id: str):
    session = _authorized_transcript_session(request.user, session_id)
    if session is None:
        return Status(404, {"error": "session_not_found"})
    if session.archivedAt is not None:
        return Status(409, {"error": "session_archived"})
    return {"session": serialize_session(session)}


@router.get(
    "/sessions/{session_id}/agent-runs/{agent_run_id}/environment",
    auth=usage_auth("sessions:read"),
    response={200: AgentRunEnvironmentResponse} | COMMON_ERROR_RESPONSES,
)
def get_agent_run_environment(request, response: HttpResponse, session_id: str, agent_run_id: str):
    response["Cache-Control"] = "no-store"
    if request.GET:
        return Status(400, {"error": "request_invalid"})
    session = _session_for_update(request.user, session_id, lock=False)
    if session is None:
        return Status(404, {"error": "session_not_found"})
    require_request_delegation(request, "sessions:read", session_id=session_id)
    run = AgentRun.objects.select_related("environment").filter(
        pk=agent_run_id, session=session, user=request.user, workspace_id=session.workspace_id,
    ).first()
    if run is None:
        return Status(404, {"error": "agent_run_not_found"})
    from app_core.session_environments import agent_run_environment_matches
    if not agent_run_environment_matches(run, session=session):
        return Status(409, {"error": "agent_run_environment_binding_invalid"})
    return {"sessionId": session.id, "agentRunId": run.id,
            "environmentId": run.environment_id, "executionMode": "perRun"}


@router.patch(
    "/sessions/{session_id}",
    auth=session_auth,
    response={200: SessionEnvelope} | COMMON_ERROR_RESPONSES,
)
def update_session_metadata(
    request, session_id: str, payload: SessionMetadataRequest
):
    fields = payload.model_fields_set
    if not fields or any(getattr(payload, field) is None for field in fields):
        return Status(400, {"error": "session_metadata_invalid"})
    with transaction.atomic():
        session = _session_for_update(request.user, session_id, lock=True)
        if session is None:
            return Status(404, {"error": "session_not_found"})
        update_fields = []
        if "title" in fields:
            title = payload.title.strip()
            if not title or len(title) > 200:
                return Status(400, {"error": "session_title_invalid"})
            session.title = title
            session.titleRevision += 1
            update_fields.extend(["title", "titleRevision"])
        if "is_pinned" in fields:
            session.isPinned = payload.is_pinned
            update_fields.append("isPinned")
        if "is_unread" in fields:
            from app_core.session_attention import set_manual_session_unread
            set_manual_session_unread(session, payload.is_unread)
        if update_fields:
            session.save(update_fields=update_fields)
    return {"session": serialize_session(session)}


@router.post(
    "/sessions/{session_id}/read",
    auth=session_auth,
    response={200: SessionEnvelope} | COMMON_ERROR_RESPONSES,
)
def acknowledge_session_notifications(request, response: HttpResponse, session_id: str, payload: SessionReadRequest):
    from app_core.session_attention import acknowledge_session_read
    response["Cache-Control"] = "no-store"
    if request.GET:
        return Status(400, {"error": "session_read_request_invalid"})
    with transaction.atomic():
        session = _session_for_update(request.user, session_id, lock=True)
        if session is None:
            return Status(404, {"error": "session_not_found"})
        try:
            acknowledge_session_read(session, int(payload.through_sequence))
        except ValueError as error:
            return Status(400, {"error": str(error)})
        return {"session": serialize_session(session)}


@router.delete(
    "/sessions/{session_id}",
    auth=session_auth,
    response={200: DeletedResponse} | COMMON_ERROR_RESPONSES,
)
def delete_session(request, session_id: str):
    with transaction.atomic():
        agent, session = _locked_owned_session(request.user, session_id)
        if session is None:
            return Status(404, {"error": "session_not_found"})
        try:
            record_session_purge_intent(session)
        except RuntimeError:
            return Status(503, {"error": "session_delete_purge_unavailable"})
        active_agent_runs = list(
            session.agent_runs.select_for_update(of=("self",))
            .select_related("authorization", "modelConfig", "session")
            .filter(status__in={"queued", "running"})
        )
        for agent_run in active_agent_runs:
            try:
                cancellation = request_agent_run_cancellation(agent_run)
            except RuntimeError:
                logger.exception(
                    "Session deletion could not stop AgentRun",
                    extra={"agentRunId": agent_run.id, "sessionId": session.id},
                )
                return Status(503, {"error": "session_delete_cancel_unavailable"})
            if cancellation["disposition"] == "terminal":
                projected = project_committed_agent_run(
                    agent_run, cancellation["terminalState"]
                )
                if projected.status in {"queued", "running"}:
                    return Status(503, {"error": "session_delete_cancel_unavailable"})
            else:
                AgentRun.objects.filter(
                    id=agent_run.id, status__in={"queued", "running"}
                ).update(transitionReason="agent_run_cancel_requested")
        if session.agent_runs.filter(status__in={"queued", "running"}).exists():
            return Status(503, {"error": "session_delete_cancel_pending"})
    # Runtime revalidates the owner/workspace and commits tree erasure and its
    # tombstone atomically. Release the Session lock before crossing that boundary.
    try:
        purge_session_runtime(session)
    except RuntimeError:
        logger.exception("Failed to purge Runtime tree for session %s", session.id)
        return Status(503, {"error": "session_delete_purge_unavailable"})
    return {"deleted": True}


def _set_session_archived(user, session_id: str, *, archived: bool):
    with transaction.atomic():
        _, session = _locked_owned_session(user, session_id)
        if session is None or session.status != "active":
            return Status(404, {"error": "session_not_found"})
        if archived and session.agent_runs.filter(status__in={"queued", "running"}).exists():
            return Status(409, {"error": "session_has_active_agent_run"})
        if archived != (session.archivedAt is not None):
            session.archivedAt = timezone.now() if archived else None
            # The existing Session update trigger publishes the navigation change.
            session.save(update_fields=["archivedAt", "updatedAt"])
    return {"session": serialize_session(session)}


@router.put(
    "/sessions/{session_id}/archive",
    auth=session_auth,
    response={200: SessionEnvelope} | COMMON_ERROR_RESPONSES,
)
def archive_session(request, session_id: str):
    return _set_session_archived(request.user, session_id, archived=True)


@router.delete(
    "/sessions/{session_id}/archive",
    auth=session_auth,
    response={200: SessionEnvelope} | COMMON_ERROR_RESPONSES,
)
def unarchive_session(request, session_id: str):
    return _set_session_archived(request.user, session_id, archived=False)


@router.post(
    "/sessions/{session_id}/restore",
    auth=session_auth,
    response={200: SessionEnvelope} | COMMON_ERROR_RESPONSES,
)
def restore_session(request, session_id: str):
    with transaction.atomic():
        agent, session = _locked_owned_session(request.user, session_id)
        if session is None:
            return Status(404, {"error": "session_not_found"})
        if session.purgeRequestedAt is not None:
            return Status(410, {"error": "session_expired"})
        if session.status != "deleted":
            return Status(409, {"error": "session_not_deleted"})
        if not trash_is_restorable(session.deletedAt, session.purgedAt):
            return Status(410, {"error": "session_expired"})
        if session.agent_runs.filter(status__in={"queued", "running"}).exists():
            return Status(409, {"error": "session_stop_pending"})
        session.status = "active"
        session.deletedAt = None
        session.deletedBy = None
        session.purgedAt = None
        session.save(
            update_fields=["status", "deletedAt", "deletedBy", "purgedAt", "updatedAt"]
        )
    return {"session": serialize_session(session, has_active_agent_run=False)}


@router.delete(
    "/sessions/{session_id}/trash",
    auth=session_auth,
    response={200: DeletedResponse} | COMMON_ERROR_RESPONSES,
)
def permanently_delete_session(request, session_id: str):
    with transaction.atomic():
        _agent, session = _locked_owned_session(request.user, session_id)
        if session is None:
            return Status(404, {"error": "session_not_found"})
        if session.status != "deleted":
            return Status(409, {"error": "session_not_deleted"})
        if session.purgedAt is not None:
            return Status(410, {"error": "session_purged"})
        try:
            record_session_purge_intent(session)
        except RuntimeError:
            return Status(503, {"error": "session_delete_purge_unavailable"})
    try:
        purge_session_runtime(session)
    except RuntimeError:
        logger.exception("Failed to purge Runtime tree for session %s", session.id)
        return Status(503, {"error": "session_delete_purge_unavailable"})
    return {"deleted": True}


@router.get(
    "/sessions/{session_id}/transcript",
    auth=usage_auth("sessions:read"),
    response=None,
)
def session_transcript(request, session_id: str):
    session = _authorized_transcript_session(request.user, session_id)
    if session is None:
        return _transcript_json_response({"error": "session_not_found"}, status=404)
    fields = set(request.GET.keys())
    if not valid_query_fields(request.GET, fields):
        return _transcript_json_response(
            {"error": "transcript_page_query_invalid"}, status=400
        )
    anchor_event = None
    if "anchorEventId" in fields:
        if fields not in ({"anchorEventId"}, {"anchorEventId", "sourceHighWater", "projectionGeneration"}):
            return _transcript_json_response({"error": "transcript_page_query_invalid"}, status=400)
        event_id = request.GET["anchorEventId"]
        if not event_id or len(event_id) > 160:
            return _transcript_json_response({"error": "transcript_page_query_invalid"}, status=400)
        anchor_event = SessionEvent.objects.filter(session=session, eventId=event_id).values(
            "sequence", "payload__type", "payload__payload__stage").first()
        if anchor_event is None or not (
            anchor_event["payload__type"] in {"user_message", "assistant_message"}
            or (anchor_event["payload__type"] == "phase_event"
                and anchor_event["payload__payload__stage"] == "model_process_summary")
        ):
            return _transcript_json_response({"error": "transcript_target_not_found"}, status=404)
        fields = fields - {"anchorEventId"}
    if not fields:
        source_high_water = _session_source_high_water(session.id)
        generation = None
        older_cursor = None
    elif fields in (
        {"sourceHighWater", "projectionGeneration"},
        {"sourceHighWater", "projectionGeneration", "olderCursor"},
    ):
        source_high_water = request.GET["sourceHighWater"]
        generation = request.GET["projectionGeneration"]
        older_cursor = request.GET.get("olderCursor")
        current_high_water = _session_source_high_water(session.id)
        if (
            not _canonical_waterline(source_high_water)
            or int(source_high_water) > current_high_water
            or not generation
            or len(generation) > 160
            or (older_cursor is not None and (not older_cursor or len(older_cursor) > 4096))
        ):
            return _transcript_json_response(
                {"error": "transcript_page_cursor_invalid"}, status=400
            )
    else:
        return _transcript_json_response(
            {"error": "transcript_page_query_invalid"}, status=400
        )
    body = {
        "schema": "runtime.transcript.page.read.v1",
        "sessionId": session.id,
        "projectionVersion": TRANSCRIPT_PROJECTION_VERSION,
        "projectionGeneration": generation,
        "sourceHighWater": str(source_high_water),
        "olderCursor": older_cursor,
    }
    if anchor_event is not None:
        if anchor_event["sequence"] > int(source_high_water):
            return _transcript_json_response({"error": "transcript_page_cursor_invalid"}, status=400)
        body["anchorSequence"] = str(anchor_event["sequence"])
    try:
        page = request_transcript_page(body)
    except TranscriptRuntimeError as error:
        return _transcript_json_response(error.payload, status=409)
    except RuntimeError:
        logger.exception("Workspace transcript page request failed", extra={"sessionId": session.id})
        return _transcript_json_response(
            {"error": "transcript_page_unavailable"}, status=503
        )
    if anchor_event is not None:
        block = next((block for block in page["blocks"] if block["orderKey"]["sourceSequence"] == str(anchor_event["sequence"])), None)
        if block is None:
            return _transcript_json_response({"error": "transcript_target_not_found"}, status=404)
        return _transcript_json_response({"schema": "workspace.transcript.target.v1", "targetBlockId": block["blockId"], "page": page})
    return _transcript_json_response(page)


@router.get(
    "/sessions/{session_id}/transcript/patches",
    auth=usage_auth("sessions:read"),
    response=None,
)
def session_transcript_patches(request, session_id: str):
    session = _authorized_transcript_session(request.user, session_id)
    if session is None:
        return _transcript_json_response({"error": "session_not_found"}, status=404)
    fields = set(request.GET.keys())
    if not valid_query_fields(request.GET, fields) or fields not in (
        {"afterSourceHighWater", "projectionGeneration"},
        {"afterSourceHighWater", "throughSourceHighWater", "projectionGeneration"},
    ):
        return _transcript_json_response(
            {"error": "transcript_patch_query_invalid"}, status=400
        )
    after = request.GET["afterSourceHighWater"]
    generation = request.GET["projectionGeneration"]
    current_high_water = _session_source_high_water(session.id)
    through = request.GET.get("throughSourceHighWater", str(current_high_water))
    if (
        not _canonical_waterline(after)
        or not _canonical_waterline(through)
        or int(after) > int(through)
        or int(through) > current_high_water
        or not generation
        or len(generation) > 160
    ):
        return _transcript_json_response(
            {"error": "transcript_patch_cursor_invalid"}, status=400
        )
    body = {
        "schema": "runtime.transcript.patch.read.v1",
        "sessionId": session.id,
        "projectionVersion": TRANSCRIPT_PROJECTION_VERSION,
        "projectionGeneration": generation,
        "afterSourceHighWater": after,
        "throughSourceHighWater": through,
    }
    try:
        result = request_transcript_patches(body)
    except TranscriptRuntimeError as error:
        return _transcript_json_response(error.payload, status=409)
    except RuntimeError:
        logger.exception("Workspace transcript patch request failed", extra={"sessionId": session.id})
        return _transcript_json_response(
            {"error": "transcript_patch_unavailable"}, status=503
        )
    return _transcript_json_response(result)


@router.get(
    "/sessions/{session_id}/transcript/content",
    auth=usage_auth("sessions:read"),
    response={200: TranscriptContentRangeResponse} | COMMON_ERROR_RESPONSES,
)
def session_transcript_content(request, session_id: str):
    session = _authorized_transcript_session(request.user, session_id)
    if session is None:
        return _transcript_json_response({"error": "session_not_found"}, status=404)
    fields = {
        "projectionGeneration",
        "refId",
        "revision",
        "byteLength",
        "offset",
    }
    if not valid_query_fields(request.GET, fields, required=fields):
        return _transcript_json_response(
            {"error": "transcript_content_query_invalid"}, status=400
        )
    generation = request.GET["projectionGeneration"]
    ref_id = request.GET["refId"]
    revision = request.GET["revision"]
    byte_length_raw = request.GET["byteLength"]
    offset_raw = request.GET["offset"]
    if (
        not generation
        or len(generation) > 160
        or not ref_id.startswith(("tool-output:", "session-event:"))
        or len(ref_id) > 320
        or not _canonical_waterline(revision)
        or not _canonical_waterline(byte_length_raw)
        or byte_length_raw == "0"
        or not _canonical_waterline(offset_raw)
        or int(offset_raw) > int(byte_length_raw)
    ):
        return _transcript_json_response(
            {"error": "transcript_content_query_invalid"}, status=400
        )
    if ref_id.startswith("session-event:"):
        try:
            return _transcript_json_response(request_transcript_content({
                "schema": "transcript.content.range.read.v1",
                "sessionId": session.id,
                "projectionVersion": TRANSCRIPT_PROJECTION_VERSION,
                "projectionGeneration": generation,
                "refId": ref_id,
                "revision": revision,
                "byteLength": byte_length_raw,
                "offset": offset_raw,
                "maxBytes": 64 * 1024,
            }))
        except TranscriptRuntimeError as error:
            return _transcript_json_response(error.payload, status=409)
        except RuntimeError:
            return _transcript_json_response({"error": "transcript_content_unavailable"}, status=503)
    call_id = ref_id.removeprefix("tool-output:")
    if not call_id or revision != "2":
        return _transcript_json_response({"error": "transcript_content_query_invalid"}, status=400)
    candidates = list(
        SessionEvent.objects.filter(
            session=session,
        )
        .extra(
            where=[
                "app_core_sessionevent.payload ->> 'type' = 'tool_result'",
                "app_core_sessionevent.payload #>> '{payload,callId}' = %s",
            ],
            params=[call_id],
        )
        .only("payload")[:2]
    )
    if len(candidates) != 1:
        return _transcript_json_response(
            {"error": "transcript_content_unavailable"}, status=409
        )
    payload = candidates[0].payload.get("payload")
    byte_length = int(byte_length_raw)
    if (
        not isinstance(payload, dict)
        or payload.get("outputComplete") is not True
        or payload.get("outputByteLength") != byte_length
    ):
        return _transcript_json_response(
            {"error": "transcript_content_reference_stale"}, status=409
        )
    try:
        if payload.get("fullOutputPath") is not None or payload.get("outputStartByte") is not None:
            content, end_offset = read_output_range(candidates[0], int(offset_raw), byte_length)
        else:
            inline_content = payload.get("modelContent")
            if (not isinstance(inline_content, str)
                    or len(inline_content.encode("utf-8")) != byte_length):
                raise ValueError("transcript inline content is invalid")
            content, end_offset = _transcript_utf8_range(inline_content, int(offset_raw))
    except ValueError:
        logger.exception(
            "Workspace transcript content range failed", extra={"sessionId": session.id}
        )
        return _transcript_json_response(
            {"error": "transcript_content_unavailable"}, status=409
        )
    return _transcript_json_response(
        {
            "schema": "transcript.content.range.v1",
            "sessionId": session.id,
            "projectionVersion": TRANSCRIPT_PROJECTION_VERSION,
            "projectionGeneration": generation,
            "refId": ref_id,
            "revision": revision,
            "byteLength": byte_length_raw,
            "startOffset": offset_raw,
            "endOffset": str(end_offset),
            "content": content,
            "hasMore": end_offset < byte_length,
        }
    )


@router.get("/sessions/{session_id}/transcript/turn-metadata", auth=usage_auth("sessions:read"),
    response={200: TranscriptTurnMetadataResponse, **COMMON_ERROR_RESPONSES})
def session_transcript_turn_metadata(request, session_id: str):
    session = _authorized_transcript_session(request.user, session_id)
    if session is None:
        return _transcript_json_response({"error": "session_not_found"}, status=404)
    values = request.GET.getlist("sequence")
    if (set(request.GET) != {"sequence"} or not 1 <= len(values) <= 128
            or not all(_canonical_waterline(value) for value in values)):
        return _transcript_json_response({"error": "transcript_turn_metadata_query_invalid"}, status=400)
    anchors = dict(SessionEvent.objects.filter(session=session,
        sequence__in=[int(value) for value in values]).values_list("sequence", "agent_run_id"))
    run_ids = set(anchors.values())
    starts = dict(SessionEvent.objects.filter(session=session, agent_run_id__in=run_ids,
        payload__type="user_message").values_list("agent_run_id", "createdAtMs"))
    terminals = dict(SessionEvent.objects.filter(session=session, agent_run_id__in=run_ids,
        payload__type__in={"agent_run_completed", "agent_run_failed", "agent_run_interrupted"},
    ).values_list("agent_run_id", "createdAtMs"))
    artifacts = {}
    for artifact in Artifact.objects.filter(session=session, agent_run_id__in=run_ids,
            status="published", deletedAt__isnull=True).order_by("publishedAt", "id"):
        artifacts.setdefault(artifact.agent_run_id, []).append({
            "artifactRef": f"artifact:{artifact.id}", "filename": artifact.safeFilename,
            "downloadUrl": f"/api/artifacts/{artifact.id}/download",
        })
    times = [{"anchorSequence": str(sequence), "startedAtMs": starts[run_id],
        "completedAtMs": terminals.get(run_id), "artifacts": artifacts.get(run_id, [])}
        for sequence, run_id in sorted(anchors.items()) if run_id in starts]
    return _transcript_json_response({"turns": times})


@router.get(
    "/sessions/{session_id}/transcript/active-agent-run",
    auth=usage_auth("sessions:read"),
    response={200: ActiveTranscriptAgentRunEnvelope} | COMMON_ERROR_RESPONSES,
)
def session_transcript_active_agent_run(request, session_id: str):
    session = _authorized_transcript_session(request.user, session_id)
    if session is None:
        return _transcript_json_response({"error": "session_not_found"}, status=404)
    source_high_water = request.GET.get("sourceHighWater")
    if (
        not valid_query_fields(request.GET, {"sourceHighWater"}, required={"sourceHighWater"})
        or not _canonical_waterline(source_high_water)
        or int(source_high_water) > _session_source_high_water(session.id)
    ):
        return _transcript_json_response(
            {"error": "transcript_active_agent_run_query_invalid"}, status=400
        )
    candidates = list(
        session.agent_runs.select_related("authorization").filter(status__in={"queued", "running"})
        .order_by("-createdAt", "-id")[:2]
    )
    active_runs = []
    for agent_run in candidates:
        terminal_state = committed_session_terminal_state(agent_run)
        if terminal_state is None:
            active_runs.append(agent_run)
        else:
            project_committed_agent_run(agent_run, terminal_state)
    if len(active_runs) > 1:
        return _transcript_json_response(
            {"error": "transcript_active_agent_run_conflict"}, status=409
        )
    active_run = active_runs[0] if active_runs else None
    return _transcript_json_response(
        {
            "schema": "workspace.transcript.active_agent_run.v1",
            "sessionId": session.id,
            "pendingSupplements": pending_agent_run_supplements(active_run),
            "agentRun": (
                {
                    "agentRunId": active_run.id,
                    "status": active_run.status,
                    "streamCursor": encode_stream_cursor(
                        active_run.id, int(source_high_water)
                    ),
                }
                if active_run is not None
                else None
            ),
        }
    )


@router.get(
    "/sessions/{session_id}/context-usage",
    auth=usage_auth("sessions:read"),
    response={200: SessionContextUsageEnvelope} | COMMON_ERROR_RESPONSES,
)
def session_context_usage(request, session_id: str):
    session = _session_for_update(request.user, session_id, lock=False)
    if session is None:
        return Status(404, {"error": "session_not_found"})
    boundaries = (
        SessionEvent.objects.filter(
            session=session,
            payload__type="model_request_started",
        )
        .select_related("agent_run__modelConfig")
    )
    event = boundaries.filter(payload__payload__purpose="main").order_by("-sequence").first()
    latest_boundary = boundaries.order_by("-sequence").first()
    try:
        is_compacting = bool(
            latest_boundary
            and latest_boundary.payload["payload"]["purpose"] == "compaction"
            and latest_boundary.agent_run.status == "running"
        )
        context_usage = None if event is None else _context_usage(event, is_compacting)
    except (KeyError, TypeError, ValueError):
        logger.exception("Session context usage is invalid", extra={"sessionId": session_id})
        return Status(409, {"error": "session_context_usage_invalid"})
    return {
        "schema": "session.context_usage.v1",
        "sessionId": session.id,
        "contextUsage": context_usage,
    }


def _authorized_transcript_session(user, session_id: str) -> Session | None:
    try:
        session = Session.objects.select_related("workspace").get(
            id=session_id,
            owner=user,
            purpose="interactive",
            purgedAt__isnull=True,
        )
    except Session.DoesNotExist:
        return None
    if workspace_membership_for(user, session.workspace_id) is None:
        return None
    return session


def _transcript_json_response(payload: dict, *, status: int = 200) -> JsonResponse:
    response = JsonResponse(payload, status=status)
    response["Cache-Control"] = "no-store"
    return response


def _session_source_high_water(session_id: str) -> int:
    return SessionEvent.objects.filter(session_id=session_id).aggregate(
        value=Max("sequence")
    )["value"] or 0


def _canonical_waterline(value: str) -> bool:
    return (
        isinstance(value, str)
        and value.isascii()
        and value.isdigit()
        and (value == "0" or not value.startswith("0"))
        and len(value) <= 20
        and int(value) <= 18_446_744_073_709_551_615
    )


def _accept_session_message(request, workspace, session, payload, prompt, attachment_refs,
                            execution_profile, digest, *, grant=None, initial_title=None):
    """Accept a message in the caller's transaction after authority/session locks."""
    try:
        model, thinking_mode = selected_session_model(workspace.id, payload.model_config_ref, payload.thinking_mode)
    except SessionModelSettingsError as error:
        raise AgentSessionCreationError(400, str(error)) from error
    agent_run = AgentRun.objects.create(
        workspace=workspace,
        session=session,
        user=request.user,
        acting_app=grant.app if grant else None,
        app_delegation=grant,
        modelConfig=model,
        thinkingMode=thinking_mode,
        prompt=prompt,
        agent_instructions="",
        tailPolicy=("rewriteLastUser" if payload.tail_action else "append"),
        rewriteTargetMessageId=(payload.tail_action.target_message_id if payload.tail_action else ""),
        rewriteExpectedTailMessageId=(payload.tail_action.expected_tail_message_id if payload.tail_action else ""),
    )
    create_agent_run_authorization(
        agent_run,
        message_asset_refs=attachment_refs,
        image_digest=execution_profile["imageDigest"],
        personal_library=True,
    )
    from app_core.material_files import check_original_budget
    from app_core.material_contract import KnowledgeError
    try:
        check_original_budget(agent_run)
    except KnowledgeError as error:
        raise AgentSessionCreationError(error.status, error.code) from error
    if initial_title is not None:
        session.title = initial_title
    elif session.title == "New chat" and session.titleRevision == 0:
        session.title = prompt[:80]
    session.updatedAt = timezone.now()
    session.save(update_fields=["title", "updatedAt"])
    receipt = accept_operation(request.user, workspace, "submitMessage", payload.operation_id,
                               digest, session, agent_run, app_delegation=grant)
    if grant is None:
        from app_core.session_titles import enqueue_session_title
        enqueue_session_title(session, agent_run)
    return agent_run, receipt


def _schedule_accepted_agent_run(agent_run):
    try:
        schedule_agent_run_lifecycle(agent_run)
    except Exception:
        logger.exception(
            "Workspace AgentRun lifecycle scheduling is pending reconciliation",
            extra={"agentRunId": agent_run.id},
        )
        AgentRun.objects.filter(id=agent_run.id, status="queued").update(
            transitionReason="agent_run_lifecycle_schedule_pending"
        )


@router.post(
    "/workspaces/{workspace_id}/sessions/{session_id}/messages",
    auth=usage_auth("messages:submit"),
    response={
        202: HostedOperationResponse,
        codes_4xx: ErrorResponse,
        503: ErrorResponse,
    },
)
def create_session_message(
    request,
    workspace_id: str,
    session_id: str,
):
    workspace = _workspace_for_user(request.user, workspace_id)
    if workspace is None:
        return Status(404, {"error": "session_not_found"})
    payload, uploads, request_error = _parse_session_message_request(request)
    if request_error:
        return Status(400, {"error": request_error})
    prompt = payload.text.strip()
    if not prompt:
        return Status(400, {"error": "empty_message"})
    attachment_refs = payload.attachment_refs
    if (
        not isinstance(attachment_refs, list)
        or any(
            not isinstance(item, str) or not item.strip() for item in attachment_refs
        )
        or attachment_refs != sorted(set(attachment_refs))
    ):
        return Status(400, {"error": "invalid_attachment_refs"})
    try:
        digest = request_digest(payload, session_id=session_id, uploads=uploads)
        with transaction.atomic():
            membership = shared_submission_membership_for(request.user, workspace_id)
            if membership is None:
                return Status(404, {"error": "session_not_found"})
            grant = require_request_delegation(request, "messages:submit", workspace_id=workspace_id,
                session_id=session_id,
                additional_scopes=(["sessions:create"] if session_id == "new" else [])
                    + (["attachments:write"] if uploads else []), lock=True, require_active=False)
            lock_operation_identity(request.user, workspace_id, "submitMessage", payload.operation_id)
            receipt = replay_operation(request.user, workspace_id, "submitMessage", payload.operation_id, digest,
                                       app_delegation=grant)
            if receipt is not None:
                return Status(202, serialize_operation(receipt))
            if grant is not None:
                if session_id != "new":
                    require_delegated_session(grant, session_id)
    except ValueError as error:
        return Status(400, {"error": str(error)})
    except HostedOperationError as error:
        return Status(error.status, {"error": error.code})
    if session_id != "new" and uploads:
        return Status(400, {"error": "message_upload_requires_new_session"})
    if session_id != "new" and payload.project_id is not None:
        return Status(400, {"error": "session_project_requires_new_session"})
    if session_id == "new" and attachment_refs:
        return Status(403, {"error": "attachment_not_accessible"})
    if session_id == "new" and payload.tail_action is not None:
        return Status(400, {"error": "rewrite_requires_existing_session"})
    if session_id == "new":
        if payload.project_id is not None and not SessionProject.objects.filter(
            id=payload.project_id, workspace=workspace, owner=request.user,
        ).exists():
            return Status(404, {"error": "session_project_not_found"})
    else:
        selected_session = Session.objects.filter(id=session_id, workspace=workspace, owner=request.user,
            status="active", archivedAt__isnull=True, purpose="interactive").only("purgeRequestedAt").first()
        if selected_session is None:
            return Status(404, {"error": "session_not_found"})
        if selected_session.purgeRequestedAt is not None:
            return Status(409, {"error": "session_delete_pending"})
    try:
        selected_session_model(workspace.id, payload.model_config_ref, payload.thinking_mode)
    except SessionModelSettingsError as error:
        return Status(400, {"error": str(error)})
    try:
        execution_profile = request_execution_profile()
    except RuntimeError:
        logger.exception("Runtime execution profile is unavailable")
        return Status(503, {"error": "runtime_execution_profile_unavailable"})
    stored = []
    if uploads:
        try:
            stored = _store_upload_batch(uploads, f"users/{request.user.id}/library")
        except ValueError as error:
            return Status(400, {"error": str(error)})
    try:
        with transaction.atomic():
            membership = shared_submission_membership_for(request.user, workspace_id)
            if membership is None:
                raise AgentSessionCreationError(404, "session_not_found")
            workspace = membership.workspace
            grant = require_request_delegation(request, "messages:submit", workspace_id=workspace_id,
                session_id=session_id,
                additional_scopes=(["sessions:create"] if session_id == "new" else [])
                    + (["attachments:write"] if uploads else []), lock=True, require_active=False)
            lock_operation_identity(request.user, workspace_id, "submitMessage", payload.operation_id)
            receipt = replay_operation(request.user, workspace_id, "submitMessage", payload.operation_id, digest,
                                       app_delegation=grant)
            if receipt is not None:
                raise AcceptedOperationReplay(receipt)
            if grant is not None:
                if session_id != "new":
                    require_delegated_session(grant, session_id)
            session = None
            if session_id != "new":
                session = Session.objects.select_for_update(of=("self",)).filter(
                    id=session_id, workspace=workspace, owner=request.user, status="active", archivedAt__isnull=True, purpose="interactive",
                ).first()
            if session_id == "new":
                project = None
                if payload.project_id is not None:
                    project = SessionProject.objects.filter(
                        id=payload.project_id,
                        workspace=workspace,
                        owner=request.user,
                    ).first()
                    if project is None:
                        raise AgentSessionCreationError(
                            404, "session_project_not_found"
                        )
                session = Session.objects.create(
                    workspace=workspace,
                    owner=request.user,
                    project=project,
                )
                library_objects = _create_uploaded_library_objects(request.user, stored)
                asset_links = []
                for library_object in library_objects:
                    asset_link, _created = SessionAssetLink.objects.get_or_create(
                        workspace=workspace,
                        session=session,
                        userLibraryObject=library_object,
                        attachedBy=request.user,
                        **captured_input_fields(library_object),
                        defaults={
                            "capturedDisplayName": library_object.displayName,
                            "capturedContentType": library_object.contentType,
                        },
                    )
                    asset_links.append(asset_link)
                attachment_refs = sorted(link.id for link in asset_links)
            else:
                if session is None:
                    return Status(404, {"error": "session_not_found"})
                if session.purgeRequestedAt is not None:
                    return Status(409, {"error": "session_delete_pending"})
                if session.agent_runs.filter(status__in={"queued", "running"}).exists():
                    return Status(409, {"error": "session_has_active_agent_run"})
                # Cleanup publishes the final workspace under this same Session
                # lock. Do not freeze the next Run's baseline before its release.
                pending_preservation = session.agent_runs.filter(
                    environment_id=session.current_environment_id,
                    status__in={"completed", "failed", "cancelled"},
                    environment_release__isnull=True,
                )
                if pending_preservation.filter(transitionReason="workspace_preservation_blocked").exists():
                    return Status(409, {"error": "session_workspace_preservation_blocked"})
                if pending_preservation.exists():
                    return Status(409, {"error": "session_workspace_preservation_pending"})
                linked_attachment_refs = set(
                    session.assetLinks.filter(id__in=attachment_refs).values_list(
                        "id",
                        flat=True,
                    )
                )
                if linked_attachment_refs != set(attachment_refs):
                    return Status(403, {"error": "attachment_not_accessible"})
            agent_run, receipt = _accept_session_message(
                request, workspace, session, payload, prompt, attachment_refs,
                execution_profile, digest, grant=grant,
            )
    except AcceptedOperationReplay as replay:
        _delete_stored_upload_batch(stored)
        return Status(202, serialize_operation(replay.receipt))
    except (AgentSessionCreationError, HostedOperationError) as database_error:
        try:
            _delete_stored_upload_batch(stored)
        except RuntimeError as cleanup_error:
            raise cleanup_error from database_error
        if database_error.status in {429, 503}:
            response = JsonResponse({"error": database_error.code}, status=database_error.status)
            response["Retry-After"] = "5"
            return response
        return Status(database_error.status, {"error": database_error.code})
    except Exception as database_error:
        try:
            _delete_stored_upload_batch(stored)
        except RuntimeError as cleanup_error:
            raise cleanup_error from database_error
        raise
    _schedule_accepted_agent_run(agent_run)
    return Status(
        202,
        serialize_operation(receipt),
    )


def _parse_session_message_request(request):
    uploads = []
    if request.content_type == "application/json":
        try:
            value = json.loads(request.body)
        except (UnicodeDecodeError, json.JSONDecodeError):
            return None, uploads, "invalid_json"
    elif request.content_type == "multipart/form-data":
        try:
            uploads = _require_uploads(
                request,
                {"operationId", "text", "modelConfigRef", "projectId", "thinkingMode"},
            )
        except ValueError as error:
            return None, uploads, str(error)
        if (
            not {"operationId", "text", "modelConfigRef"}.issubset(request.POST)
            or not set(request.POST).issubset(
                {"operationId", "text", "modelConfigRef", "projectId", "thinkingMode"}
            )
            or len(request.POST.getlist("operationId")) != 1
            or len(request.POST.getlist("text")) != 1
            or len(request.POST.getlist("modelConfigRef")) != 1
            or len(request.POST.getlist("projectId")) > 1
            or len(request.POST.getlist("thinkingMode")) > 1
        ):
            return None, uploads, "message_fields_invalid"
        value = {
            "operationId": request.POST["operationId"],
            "text": request.POST["text"],
            "modelConfigRef": request.POST["modelConfigRef"],
        }
        if "thinkingMode" in request.POST:
            value["thinkingMode"] = request.POST["thinkingMode"]
        if "projectId" in request.POST:
            value["projectId"] = request.POST["projectId"]
    else:
        return None, uploads, "message_content_type_unsupported"
    try:
        return SessionMessageRequest.model_validate(value), uploads, None
    except ValidationError as error:
        code = (
            "invalid_attachment_refs"
            if any("attachmentRefs" in item["loc"] for item in error.errors())
            else "request_invalid"
        )
        return None, uploads, code


@router.post(
    "/sessions/{session_id}/agent-runs/{agent_run_id}/questions/{question_id}/responses",
    auth=usage_auth("messages:submit"),
    response={202: HostedOperationResponse} | COMMON_ERROR_RESPONSES,
)
def respond_question(request, session_id: str, agent_run_id: str, question_id: str, payload: QuestionResponseRequest):
    session = Session.objects.filter(id=session_id, owner=request.user, status="active", purpose="interactive").first()
    if session is None:
        return Status(404, {"error": "session_not_found"})
    digest = request_digest(payload, target={"sessionId": session_id, "agentRunId": agent_run_id, "questionId": question_id})
    try:
        with transaction.atomic():
            membership = shared_submission_membership_for(request.user, session.workspace_id)
            if membership is None:
                return Status(404, {"error": "session_not_found"})
            grant = require_request_delegation(request, "messages:submit", workspace_id=session.workspace_id,
                session_id=session_id, lock=True)
            lock_operation_identity(request.user, session.workspace_id, "respondQuestion", payload.operation_id)
            receipt = replay_operation(request.user, session.workspace_id, "respondQuestion", payload.operation_id,
                digest, app_delegation=grant)
            if receipt is not None:
                return Status(202, serialize_operation(receipt))
            session = Session.objects.select_for_update().filter(id=session_id, owner=request.user, status="active", purpose="interactive").first()
            if session is None:
                return Status(404, {"error": "session_not_found"})
            run = AgentRun.objects.select_related("authorization", "modelConfig", "session").filter(
                id=agent_run_id, session=session, workspace_id=session.workspace_id, user=request.user).first()
            if run is None or not agent_run_membership_is_current(run):
                return Status(404, {"error": "agent_run_not_found"})
            # Runtime atomically binds and records the response before waking the
            # original lifecycle Job. Its receipt also recovers an uncertain HTTP
            # result after the checkpoint has already been consumed.
            request_agent_run_question_response(run, operation_id=payload.operation_id,
                continuation_id=payload.continuation_id, question_id=question_id,
                action=payload.action, answer=payload.answer)
            receipt = accept_operation(request.user, membership.workspace, "respondQuestion", payload.operation_id,
                digest, session, run, app_delegation=grant)
        return Status(202, serialize_operation(receipt))
    except HostedOperationError as error:
        return Status(error.status, {"error": error.code})
    except ValueError as error:
        return Status(409, {"error": str(error)})
    except RuntimeError:
        logger.exception("Question response admission failed", extra={"agentRunId": agent_run_id})
        return Status(503, {"error": "agent_run_question_response_unavailable"})


@router.post(
    "/sessions/{session_id}/agent-runs/{agent_run_id}/supplements",
    auth=usage_auth("messages:submit"),
    response={
        202: AgentRunSupplementResponse,
        codes_4xx: ErrorResponse,
    },
)
def supplement_agent_run(
    request,
    session_id: str,
    agent_run_id: str,
    payload: AgentRunSupplementRequest,
):
    try:
        agent_run = AgentRun.objects.select_related(
            "authorization", "modelConfig", "session"
        ).get(
            id=agent_run_id,
            session_id=session_id,
            status__in={"queued", "running"},
            session__owner=request.user,
            session__purpose="interactive",
        )
    except AgentRun.DoesNotExist:
        return Status(404, {"error": "active_agent_run_not_found"})
    if not agent_run_membership_is_current(agent_run):
        return Status(404, {"error": "active_agent_run_not_found"})
    require_request_delegation(request, "messages:submit", session_id=session_id)
    try:
        result = request_agent_run_supplement(agent_run, payload.supplement_id, payload.message)
    except ValueError as error:
        return Status(409, {"error": str(error)})
    except RuntimeError:
        logger.exception(
            "Workspace AgentRun supplement request failed", extra={"agentRunId": agent_run.id}
        )
        return Status(503, {"error": "agent_run_supplement_unavailable"})
    return Status(
        202,
        {
            "agentRunId": agent_run.id,
            "sessionId": agent_run.session_id,
            "supplementId": payload.supplement_id,
            "disposition": result["disposition"],
            "queuedCount": result["queuedCount"],
        },
    )


@router.post(
    "/sessions/{session_id}/agent-runs/{agent_run_id}/cancel",
    auth=usage_auth("runs:cancel"),
    response={
        200: AgentRunCancellationResponse,
        202: AgentRunCancellationResponse,
        503: ErrorResponse,
        codes_4xx: ErrorResponse,
    },
)
def cancel_agent_run(request, session_id: str, agent_run_id: str):
    try:
        agent_run = AgentRun.objects.select_related(
            "authorization", "modelConfig", "session"
        ).get(
            id=agent_run_id,
            session_id=session_id,
            session__owner=request.user,
            session__purpose="interactive",
        )
    except AgentRun.DoesNotExist:
        return Status(404, {"error": "agent_run_not_found"})
    if not agent_run_membership_is_current(agent_run):
        return Status(404, {"error": "agent_run_not_found"})
    require_request_delegation(request, "runs:cancel", session_id=session_id)
    # Runtime also consumes any remaining continuation for a terminal owner.
    try:
        cancellation = request_agent_run_cancellation(agent_run)
    except RuntimeError:
        logger.exception(
            "Workspace AgentRun cancellation request failed", extra={"agentRunId": agent_run.id}
        )
        return Status(503, {"error": "agent_run_cancel_unavailable"})
    if cancellation["disposition"] == "terminal":
        projected = project_committed_agent_run(agent_run, cancellation["terminalState"])
        return {
            "agentRunId": projected.id,
            "status": projected.status,
            "disposition": "terminal",
        }
    AgentRun.objects.filter(id=agent_run.id, status__in={"queued", "running"}).update(
        transitionReason="agent_run_cancel_requested"
    )
    agent_run.refresh_from_db(fields=["status"])
    return Status(
        202,
        {
            "agentRunId": agent_run.id,
            "status": agent_run.status,
            "disposition": "requested",
        },
    )


def _plugin_workspace_for_user(user, workspace_id, *, allowed_roles=None):
    # This explicit resource boundary does not alter generic membership lookup.
    if allowed_roles == WORKSPACE_ADMIN_ROLES:
        from app_core.workspace_resources import workspace_resource_admin
        try:
            return workspace_resource_admin(user, workspace_id)
        except PermissionError:
            return None
    if user.is_authenticated and user.is_superuser:
        return Workspace.objects.filter(pk=workspace_id, status="active").first()
    return _workspace_for_user(user, workspace_id)


def _workspace_for_user(
    user,
    workspace_id: str,
    *,
    allowed_roles=None,
) -> Workspace | None:
    options = {} if allowed_roles is None else {"allowed_roles": allowed_roles}
    membership = workspace_membership_for(user, workspace_id, **options)
    return membership.workspace if membership else None


def _workspace_mcp_servers(catalog: dict) -> dict[str, list[dict]]:
    result = request_workspace_mcp_catalog(catalog)
    plugins = result.get("plugins")
    if not isinstance(plugins, list):
        raise ValueError("workspace_mcp_catalog_plugins_invalid")
    by_name = {}
    for plugin in plugins:
        if (
            not isinstance(plugin, dict)
            or set(plugin) != {"pluginName", "servers"}
            or not isinstance(plugin["pluginName"], str)
            or not isinstance(plugin["servers"], list)
            or plugin["pluginName"] in by_name
        ):
            raise ValueError("workspace_mcp_catalog_plugin_invalid")
        by_name[plugin["pluginName"]] = plugin["servers"]
    expected = {package["name"] for package in catalog["packages"]}
    if set(by_name) != expected:
        raise ValueError("workspace_mcp_catalog_plugin_mismatch")
    return by_name


def _workspace_hooks(catalog: dict) -> dict[str, list[dict]]:
    result = request_workspace_hook_catalog(catalog)
    plugins = result.get("plugins")
    if not isinstance(plugins, list):
        raise ValueError("workspace_hook_catalog_plugins_invalid")
    by_name = {}
    supported_events = {
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "PreCompact",
        "PostCompact",
        "SubagentStart",
        "SubagentStop",
    }
    for plugin in plugins:
        if (
            not isinstance(plugin, dict)
            or set(plugin) != {"pluginName", "hooks"}
            or not isinstance(plugin["pluginName"], str)
            or not isinstance(plugin["hooks"], list)
            or plugin["pluginName"] in by_name
        ):
            raise ValueError("workspace_hook_catalog_plugin_invalid")
        hook_ids = set()
        for hook in plugin["hooks"]:
            if (
                not isinstance(hook, dict)
                or set(hook) != {"id", "event", "matcher", "timeoutMs"}
                or not isinstance(hook["id"], str)
                or not hook["id"]
                or hook["id"] in hook_ids
                or hook["event"] not in supported_events
                or (hook["matcher"] is not None and not isinstance(hook["matcher"], str))
                or type(hook["timeoutMs"]) is not int
                or not 1 <= hook["timeoutMs"] <= 10_000
            ):
                raise ValueError("workspace_hook_catalog_hook_invalid")
            hook_ids.add(hook["id"])
        by_name[plugin["pluginName"]] = plugin["hooks"]
    expected = {package["name"] for package in catalog["packages"]}
    if set(by_name) != expected:
        raise ValueError("workspace_hook_catalog_plugin_mismatch")
    return by_name


def _workspace_plugin(package: dict, enabled: bool, *, inspect: bool = False, workspace=None) -> dict:
    catalog = {
        "schema": "plugin_activation_snapshot_v1",
        "digest": activation_digest([package]),
        "packages": [package],
    }
    errors = []
    try:
        interface = load_plugin_interfaces(catalog)[package["name"]]
    except (OSError, ValueError):
        logger.exception("Plugin manifest is unavailable: %s", package["name"])
        interface = {"displayName": package["name"], "shortDescription": "", "capabilities": []}
        errors.append("plugin_manifest_invalid")
    plugin = _serialize_workspace_plugin(package, interface, enabled, None, None, set())
    plugin["errors"] = errors
    if not errors:
        try:
            plugin["mcpCredentialRefs"] = load_plugin_bearer_credential_refs(package)
        except (OSError, ValueError):
            logger.exception("Plugin credential metadata is unavailable: %s", package["name"])
            errors.append("plugin_credentials_unavailable")
    if inspect and "plugin_manifest_invalid" not in errors:
        configured = set(McpBearerCredential.objects.filter(workspace=workspace).values_list("plugin_name", "credential_ref"))
        for field, contribution, loader, code in (
            (
                "mcpServers",
                "mcpServers",
                _workspace_mcp_servers,
                "workspace_mcp_catalog_unavailable",
            ),
            ("hooks", "hooks", _workspace_hooks, "workspace_hook_catalog_unavailable"),
        ):
            if not package[contribution]:
                plugin[field] = []
                continue
            try:
                values = loader(catalog)[package["name"]]
                projected = _serialize_workspace_plugin(
                    package, interface, enabled,
                    values if field == "mcpServers" else None,
                    values if field == "hooks" else None,
                    configured,
                )
                WorkspacePluginResponse.model_validate(projected)
                plugin[field] = projected[field]
            except (OSError, RuntimeError, ValueError, KeyError, TypeError):
                logger.exception("Plugin inspection failed: %s %s", package["name"], field)
                errors.append(code)
    plugin.update(scope="workspace", writable=True)
    return plugin


def _serialize_workspace_plugin(
    package: dict,
    interface: dict,
    enabled: bool,
    mcp_servers: list[dict] | None,
    hooks: list[dict] | None,
    configured_credentials: set[tuple[str, str]],
) -> dict:
    projected_servers = []
    for server in mcp_servers or []:
        auth = server["auth"]
        credential_ref = auth["credentialRef"]
        projected_servers.append(
            {
                **server,
                "auth": {
                    **auth,
                    "credentialConfigured": (
                        None
                        if auth["type"] == "none"
                        else (package["name"], credential_ref)
                        in configured_credentials
                    ),
                },
            }
        )
    return {
        "name": package["name"],
        **interface,
        "version": package["version"],
        "packageDigest": package["packageDigest"],
        "enabled": enabled,
        "skills": package["skills"],
        "cli": package["cli"],
        "mcpServers": None if mcp_servers is None else projected_servers,
        "mcpCredentialRefs": None,
        "hooks": hooks,
        "errors": [],
    }


def _context_usage(event: SessionEvent, is_compacting: bool) -> dict:
    payload = event.payload["payload"]
    purpose = payload["purpose"]
    estimate = payload["contextTokenEstimate"]
    breakdown = payload["contextTokenBreakdown"]
    expected_breakdown_fields = {
        "systemPromptTokens",
        "systemToolTokens",
        "mcpToolTokens",
        "skillsTokens",
        "messageTokens",
        "mcpTools",
    }
    if (
        purpose != "main"
        or type(estimate) is not int
        or estimate < 0
        or not isinstance(breakdown, dict)
        or set(breakdown) != expected_breakdown_fields
    ):
        raise ValueError("session_context_usage_payload_invalid")
    token_fields = expected_breakdown_fields - {"mcpTools"}
    if any(type(breakdown[field]) is not int or breakdown[field] < 0 for field in token_fields):
        raise ValueError("session_context_usage_breakdown_invalid")
    if sum(breakdown[field] for field in token_fields) != estimate:
        raise ValueError("session_context_usage_total_mismatch")
    mcp_tools = breakdown["mcpTools"]
    if not isinstance(mcp_tools, list) or any(
        not isinstance(tool, dict)
        or set(tool) != {"providerId", "name", "tokens"}
        or not isinstance(tool["providerId"], str)
        or not tool["providerId"].strip()
        or not isinstance(tool["name"], str)
        or not tool["name"].strip()
        or type(tool["tokens"]) is not int
        or tool["tokens"] < 0
        for tool in mcp_tools
    ):
        raise ValueError("session_context_usage_mcp_tools_invalid")
    if len({(tool["providerId"], tool["name"]) for tool in mcp_tools}) != len(mcp_tools):
        raise ValueError("session_context_usage_mcp_tool_identity_duplicate")
    if sum(tool["tokens"] for tool in mcp_tools) != breakdown["mcpToolTokens"]:
        raise ValueError("session_context_usage_mcp_total_mismatch")
    max_context_tokens = event.agent_run.modelConfig.contextTokens
    auto_compact_buffer_tokens = min(
        max(max_context_tokens - estimate, 0),
        CONTEXT_COMPACTION_HEADROOM_TOKENS,
    )
    used_tokens = min(estimate + auto_compact_buffer_tokens, max_context_tokens)
    return {
        "usedTokens": used_tokens,
        "maxContextTokens": max_context_tokens,
        "usedPercentage": min(
            (used_tokens * 100 + max_context_tokens // 2) // max_context_tokens,
            100,
        ),
        "updatedAt": event.createdAtMs,
        "isCompacting": is_compacting,
        "breakdown": {
            **breakdown,
            "autoCompactBufferTokens": auto_compact_buffer_tokens,
            "freeSpaceTokens": max(max_context_tokens - used_tokens, 0),
        },
    }


def _transcript_utf8_range(content: str, offset: int) -> tuple[str, int]:
    encoded = content.encode("utf-8")
    if offset > len(encoded):
        raise ValueError("transcript content offset exceeds byteLength")
    return _decode_transcript_range(
        encoded[offset : offset + TRANSCRIPT_CONTENT_RANGE_BYTES], offset
    )


def _decode_transcript_range(encoded: bytes, offset: int) -> tuple[str, int]:
    for trim in range(4):
        candidate = encoded if trim == 0 else encoded[:-trim]
        try:
            content = candidate.decode("utf-8")
            return content, offset + len(candidate)
        except UnicodeDecodeError:
            continue
    raise UnicodeDecodeError("utf-8", encoded, 0, len(encoded), "invalid range boundary")


def _session_for_update(user, session_id: str, *, lock: bool):
    query = Session.objects
    if lock:
        query = query.select_for_update()
    try:
        session = query.select_related("workspace").get(
            id=session_id,
            owner=user,
            purpose="interactive",
            status="active",
            archivedAt__isnull=True,
        )
    except Session.DoesNotExist:
        return None
    return session if workspace_membership_for(user, session.workspace_id) else None


def _locked_owned_session(user, session_id: str):
    workspace_id = Session.objects.filter(id=session_id, owner=user, purpose="interactive").values_list("workspace_id", flat=True).first()
    if workspace_id is None or locked_workspace_membership_for(user, workspace_id) is None:
        return None, None
    session = Session.objects.select_for_update().filter(id=session_id, workspace_id=workspace_id, owner=user, purpose="interactive").first()
    return None, session
