from typing import Literal

from ninja.responses import codes_4xx, codes_5xx
from pydantic import Field, JsonValue

from .schema import ErrorResponse, ModelResponse, StrictSchema


COMMON_ERROR_RESPONSES = {
    codes_4xx: ErrorResponse,
    codes_5xx: ErrorResponse,
}


class AgentCoordinationSessionResponse(StrictSchema):
    schema_id: Literal["agent.coordination_session.v1"] = Field(alias="schema")
    agent_id: str
    session_id: str


class AgentMessageResponse(StrictSchema):
    id: str
    agent_run_id: str
    turn_id: str
    tool_call_id: str
    source_sequence: int
    created_at_ms: int
    body: str
    session_refs: list[str]
    file_refs: list[str]


class AgentMessagesResponse(StrictSchema):
    schema_id: Literal["agent.messages.v1"] = Field(alias="schema")
    agent_id: str
    session_id: str
    messages: list[AgentMessageResponse]
    next_after_sequence: int | None


class WorkspaceResponse(StrictSchema):
    id: str
    name: str
    description: str
    status: str
    role: Literal["owner", "admin", "member"]


class WorkspacesEnvelope(StrictSchema):
    workspaces: list[WorkspaceResponse]


class WorkspaceMemberResponse(StrictSchema):
    membership_id: str
    user_id: str
    email: str
    role: Literal["owner", "admin", "member"]
    created_at: str


class WorkspaceMembersEnvelope(StrictSchema):
    members: list[WorkspaceMemberResponse]


class WorkspaceMemberEnvelope(StrictSchema):
    member: WorkspaceMemberResponse


class WorkspaceGroupResponse(StrictSchema):
    id: str
    name: str
    kind: Literal["custom", "all_members"]
    created_at: str


class WorkspaceGroupEnvelope(StrictSchema):
    group: WorkspaceGroupResponse


class WorkspaceGroupsEnvelope(StrictSchema):
    groups: list[WorkspaceGroupResponse]


class WorkspaceGroupMembersEnvelope(StrictSchema):
    members: list[WorkspaceMemberResponse]


class WorkspaceOwnershipTransferredResponse(StrictSchema):
    owner: WorkspaceMemberResponse
    previous_owner: WorkspaceMemberResponse


class WorkspaceInvitationResponse(StrictSchema):
    id: str
    email: str
    role: Literal["admin", "member"]
    status: Literal["pending", "accepted", "revoked", "expired"]
    expires_at: str
    created_at: str


class WorkspaceInvitationEnvelope(StrictSchema):
    invitation: WorkspaceInvitationResponse
    invite_url: str


class WorkspaceInvitationsEnvelope(StrictSchema):
    invitations: list[WorkspaceInvitationResponse]


class WorkspaceInvitationPreviewResponse(StrictSchema):
    workspace_id: str
    workspace_name: str
    email: str
    role: Literal["admin", "member"]
    account_exists: bool
    expires_at: str


class WorkspaceInvitationAcceptedResponse(StrictSchema):
    workspace_id: str
    membership_id: str
    role: Literal["admin", "member"]
    user_created: bool


class PluginResourceResponse(StrictSchema):
    path: str
    digest: str


class WorkspaceMcpTransportResponse(StrictSchema):
    type: Literal["stdio", "streamableHttp"]
    endpoint: str | None


class WorkspaceMcpAuthResponse(StrictSchema):
    type: Literal["none", "bearer"]
    credential_ref: str | None
    credential_configured: bool | None


class WorkspaceMcpToolResponse(StrictSchema):
    source_name: str
    name: str
    description: str
    input_schema: dict[str, object]
    concurrency_safe: bool
    scopes: list[str]


class WorkspaceMcpServerResponse(StrictSchema):
    id: str
    model_contract_digest: str
    transport: WorkspaceMcpTransportResponse
    auth: WorkspaceMcpAuthResponse
    startup_timeout_ms: int
    tool_timeout_ms: int
    tools: list[WorkspaceMcpToolResponse]


class WorkspaceHookResponse(StrictSchema):
    id: str
    event: Literal[
        "UserPromptSubmit",
        "PreToolUse",
        "PermissionRequest",
        "PostToolUse",
        "PreCompact",
        "PostCompact",
        "SubagentStart",
        "SubagentStop",
    ]
    matcher: str | None
    timeout_ms: int


class WorkspacePluginResponse(StrictSchema):
    scope: Literal["workspace", "platform"] = "platform"
    writable: bool = False
    name: str
    display_name: str
    short_description: str
    capabilities: list[str]
    version: str
    package_digest: str
    enabled: bool
    skills: list[PluginResourceResponse]
    cli: list[PluginResourceResponse]
    mcp_servers: list[WorkspaceMcpServerResponse] | None
    mcp_credential_refs: list[str] | None
    hooks: list[WorkspaceHookResponse] | None
    errors: list[str]


class WorkspacePluginEnvelope(StrictSchema):
    plugin: WorkspacePluginResponse


class WorkspacePluginsEnvelope(StrictSchema):
    plugins: list[WorkspacePluginResponse]


class GlobalPluginResponse(StrictSchema):
    name: str
    display_name: str
    short_description: str
    capabilities: list[str]
    version: str
    enabled_workspace_count: int
    credential_count: int
    removable: bool
    errors: list[str]


class GlobalPluginEnvelope(StrictSchema):
    plugin: GlobalPluginResponse


class GlobalPluginsEnvelope(StrictSchema):
    plugins: list[GlobalPluginResponse]


class WorkspaceSkillResponse(StrictSchema):
    skill_id: str
    name: str
    description: str
    enabled: bool
    allow_implicit_invocation: bool
    allowed_tools: list[str]


class WorkspaceSkillsEnvelope(StrictSchema):
    schema_id: Literal["workspace.skill.catalog.result.v1"] = Field(alias="schema")
    skills: list[WorkspaceSkillResponse]


class WorkspaceSkillDetailEnvelope(StrictSchema):
    schema_id: Literal["workspace.skill.detail.result.v1"] = Field(alias="schema")
    skill: WorkspaceSkillResponse
    content: str


class AgentResponse(StrictSchema):
    id: str
    workspace_id: str
    definition_id: str | None
    definition_version_id: str | None
    is_business_root: bool
    plugin_names: list[str]
    name: str
    description: str
    instructions: str
    avatar_kind: Literal["centaeris", "banana"]
    avatar_url: str | None
    status: Literal["active", "deleted"]
    deleted_at: str | None
    created_at: str
    updated_at: str


class AgentEnvelope(StrictSchema):
    agent: AgentResponse


class AgentsEnvelope(StrictSchema):
    agents: list[AgentResponse]


class AgentDefinitionResponse(StrictSchema):
    id: str
    workspace_id: str
    name: str
    description: str
    instructions: str
    plugin_names: list[str]
    model_config_ref: str | None
    thinking_mode: str | None
    avatar_url: str | None
    avatar_kind: Literal["centaeris", "banana"]
    status: Literal["active", "disabled"]
    availability_scope: Literal["none", "workspace", "members"]
    membership_ids: list[str]
    published_version_id: str | None
    created_at: str
    updated_at: str


class AgentDefinitionVersionResponse(StrictSchema):
    id: str
    definition_id: str
    version: int
    name: str
    description: str
    instructions: str
    plugin_names: list[str]
    model_config_ref: str | None
    thinking_mode: str | None
    avatar_url: str | None
    avatar_kind: Literal["centaeris", "banana"]
    published_at: str


class AgentDefinitionEnvelope(StrictSchema):
    definition: AgentDefinitionResponse


class AgentDefinitionsEnvelope(StrictSchema):
    definitions: list[AgentDefinitionResponse]


class AgentDefinitionVersionEnvelope(StrictSchema):
    version: AgentDefinitionVersionResponse


class AgentDefinitionVersionsEnvelope(StrictSchema):
    versions: list[AgentDefinitionVersionResponse]


class AvailableAgentDefinitionsEnvelope(StrictSchema):
    definitions: list[AgentDefinitionVersionResponse]


class ContextMcpToolResponse(StrictSchema):
    provider_id: str
    name: str
    tokens: int


class ContextTokenBreakdownResponse(StrictSchema):
    system_prompt_tokens: int
    system_tool_tokens: int
    mcp_tool_tokens: int
    skills_tokens: int
    message_tokens: int
    auto_compact_buffer_tokens: int
    free_space_tokens: int
    mcp_tools: list[ContextMcpToolResponse]


class ContextUsageResponse(StrictSchema):
    used_tokens: int
    max_context_tokens: int
    used_percentage: int
    updated_at: int
    is_compacting: bool
    breakdown: ContextTokenBreakdownResponse


class SessionContextUsageEnvelope(StrictSchema):
    schema_id: Literal["session.context_usage.v1"] = Field(alias="schema")
    session_id: str
    context_usage: ContextUsageResponse | None


class TranscriptContentRangeResponse(StrictSchema):
    schema_id: Literal["transcript.content.range.v1"] = Field(alias="schema")
    session_id: str
    projection_version: Literal["transcript.projection.v1"]
    projection_generation: str
    ref_id: str
    revision: Literal["2"]
    byte_length: str
    start_offset: str
    end_offset: str
    content: str
    has_more: bool


class InitialInputOriginResponse(StrictSchema):
    message_id: str
    agent_id: str
    agent_name: str


class SessionResponse(StrictSchema):
    id: str
    workspace_id: str
    agent_id: str
    project_id: str | None
    current_environment_id: str
    execution_mode: Literal["perRun"]
    title: str
    origin: str
    initial_input_origin: InitialInputOriginResponse | None
    status: Literal["active", "deleted"]
    deleted_at: str | None
    is_pinned: bool
    is_unread: bool
    is_manually_unread: bool
    attention_revision: str = Field(pattern=r"^(0|[1-9][0-9]*)$")
    latest_attention_sequence: str = Field(pattern=r"^(0|[1-9][0-9]*)$")
    read_through_sequence: str = Field(pattern=r"^(0|[1-9][0-9]*)$")
    has_active_agent_run: bool
    updated_at: str


class SessionEnvelope(StrictSchema):
    session: SessionResponse


class AgentRunEnvironmentResponse(StrictSchema):
    session_id: str
    agent_run_id: str
    environment_id: str
    execution_mode: Literal["perRun"]


class SessionsEnvelope(StrictSchema):
    sessions: list[SessionResponse]


class AgentWorkSessionsResponse(StrictSchema):
    schema_id: Literal["agent.work_sessions.v1"] = Field(alias="schema")
    agent_id: str
    workspace_id: str
    sessions: list[SessionResponse]
    next_after_session_id: str | None
    has_more: bool


class SessionProjectResponse(StrictSchema):
    id: str
    workspace_id: str
    agent_id: str
    name: str
    created_at: str


class SessionProjectEnvelope(StrictSchema):
    project: SessionProjectResponse


class SessionProjectsEnvelope(StrictSchema):
    projects: list[SessionProjectResponse]


class SessionTrashEnvelope(SessionsEnvelope):
    next_cursor: str | None
    has_more: bool


class DeletedResponse(StrictSchema):
    deleted: bool


class SourceResponse(StrictSchema):
    id: str
    workspace_id: str
    source_type: str
    name: str
    status: str
    failure_reason: str
    access_level: Literal["read", "write", "control"]


class SourceEnvelope(StrictSchema):
    source: SourceResponse


class SourcesEnvelope(StrictSchema):
    sources: list[SourceResponse]


class SourceObjectResponse(StrictSchema):
    id: str
    source_id: str
    object_type: str
    display_path: str
    display_name: str
    content_type: str
    size_bytes: int | None
    sha256: str
    source_version: str
    status: str
    failure_reason: str


class SourceObjectEnvelope(StrictSchema):
    object: SourceObjectResponse


class SourceObjectsEnvelope(StrictSchema):
    objects: list[SourceObjectResponse]


class SourceGrantResponse(StrictSchema):
    id: str
    source_id: str
    workspace_group_id: str
    access_level: Literal["read", "write", "control"]


class SourceGrantEnvelope(StrictSchema):
    grant: SourceGrantResponse


class SourceGrantsEnvelope(StrictSchema):
    grants: list[SourceGrantResponse]


class LibraryObjectResponse(StrictSchema):
    id: str
    kind: str
    display_name: str
    object_kind: str
    content_type: str
    size_bytes: int | None
    sha256: str
    status: str
    failure_reason: str
    parent_folder_id: str | None
    deleted_at: str | None
    deletion_generation: int
    content_generation: int
    updated_at: str


class LibraryObjectEnvelope(StrictSchema):
    object: LibraryObjectResponse


class LibraryObjectsEnvelope(StrictSchema):
    objects: list[LibraryObjectResponse]


class TrashActorResponse(StrictSchema):
    user_id: str
    email: str


class TrashLocationResponse(StrictSchema):
    kind: Literal["workspace", "agent", "libraryRoot", "libraryFolder"]
    id: str | None
    label: str
    scope: Literal["workspace", "privateLibrary"]


class TrashItemResponse(StrictSchema):
    id: str
    kind: Literal["agent", "session", "source", "library"]
    title: str
    deleted_at: str
    expires_at: str
    scope: Literal["workspace", "privateLibrary"]
    location: TrashLocationResponse
    deleted_by: TrashActorResponse | None


class TrashFilterOptionsResponse(StrictSchema):
    deleted_by: list[TrashActorResponse]
    locations: list[TrashLocationResponse]


class TrashEnvelope(StrictSchema):
    items: list[TrashItemResponse]
    filter_options: TrashFilterOptionsResponse
    next_cursor: str | None
    has_more: bool


class LibraryNoteEnvelope(LibraryObjectEnvelope):
    markdown: str


class LibraryNoteSaveEnvelope(LibraryObjectEnvelope):
    save_id: str
    applied_generation: int


class LibraryNoteRevisionResponse(StrictSchema):
    generation: int
    display_name: str
    sha256: str
    size_bytes: int
    protected: bool
    created_at: str


class LibraryNoteHistoryEnvelope(StrictSchema):
    current_generation: int
    revisions: list[LibraryNoteRevisionResponse]


class ArtifactResponse(StrictSchema):
    id: str
    kind: str
    workspace_id: str
    session_id: str
    agent_run_id: str
    display_name: str
    content_type: str
    size_bytes: int
    sha256: str
    status: str
    download_url: str | None


class SessionAssetResponse(StrictSchema):
    id: str
    asset_kind: str
    display_name: str
    content_type: str
    asset: SourceObjectResponse | LibraryObjectResponse | ArtifactResponse


class SessionAssetEnvelope(StrictSchema):
    asset: SessionAssetResponse


class SessionAssetsEnvelope(StrictSchema):
    assets: list[SessionAssetResponse]


class SessionUploadEnvelope(StrictSchema):
    library_objects: list[LibraryObjectResponse]
    assets: list[SessionAssetResponse]


class ModelProviderResponse(StrictSchema):
    id: str
    display_name: str
    template_id: str | None
    api: str
    api_base: str
    enabled: bool
    credential_version: int
    updated_at: str


class ModelProviderEnvelope(StrictSchema):
    provider: ModelProviderResponse


class ModelProvidersEnvelope(StrictSchema):
    providers: list[ModelProviderResponse]


class ModelProviderTemplateModelResponse(StrictSchema):
    model_name: str
    display_name: str
    context_tokens: int
    max_output_tokens: int
    api_override: str | None = Field(default=None)
    thinking_mode: str | None = Field(default=None)
    thinking_modes: list[str] = Field(default_factory=list)


class ModelProviderTemplateResponse(StrictSchema):
    id: str
    display_name: str
    tier: str = "direct_api"
    logo_svg: str | None = Field(default=None)
    api: str
    api_base: str
    models: list[ModelProviderTemplateModelResponse]


class ModelProviderTemplatesEnvelope(StrictSchema):
    templates: list[ModelProviderTemplateResponse]


class ModelCatalogReconciliationEnvelope(StrictSchema):
    digest: str
    providers: list[dict]
    blocked: bool


class AdminModelResponse(ModelResponse):
    api: str
    api_override: str | None
    api_base: str
    enabled: bool
    revision: int
    updated_at: str


class AdminModelEnvelope(StrictSchema):
    model: AdminModelResponse


class AdminModelsEnvelope(StrictSchema):
    models: list[AdminModelResponse]


class RunIdsErrorResponse(ErrorResponse):
    agent_run_ids: list[str]


class ModelTestResponse(StrictSchema):
    ok: bool
    http_status: int | None
    latency_ms: int
    output_preview: str | None
    error_keyword: str | None


class RuntimeJobResultRefResponse(StrictSchema):
    kind: str
    id: str


class RuntimeJobResponse(StrictSchema):
    id: str
    status: str
    progress_topic: str
    result_refs: list[RuntimeJobResultRefResponse]
    error: str | None


class RuntimeJobEnvelope(StrictSchema):
    job: RuntimeJobResponse


class CitationLineLocatorResponse(StrictSchema):
    start_line: int = Field(gt=0)
    end_line: int = Field(gt=0)


class CitationPageLocatorResponse(StrictSchema):
    page_start: int = Field(gt=0)
    page_end: int = Field(gt=0)


class CitationTextSpanLocatorResponse(StrictSchema):
    kind: Literal["textSpan"]
    page_start: int | None = Field(default=None, gt=0)
    page_end: int | None = Field(default=None, gt=0)
    start_byte: int = Field(ge=0)
    end_byte: int = Field(gt=0)
    start_line: int = Field(gt=0)
    end_line: int = Field(gt=0)


class CitationPageRegionLocatorResponse(StrictSchema):
    kind: Literal["pageRegion"]
    page: int = Field(gt=0)
    bbox: tuple[int, int, int, int]


class CitationTableCellLocatorResponse(StrictSchema):
    kind: Literal["tableCell"]
    page: int = Field(gt=0)
    table_id: str
    start_row: int = Field(ge=0)
    end_row: int = Field(ge=0)
    start_column: int = Field(ge=0)
    end_column: int = Field(ge=0)


class CitationResponse(StrictSchema):
    citation_id: str
    input_ref: str
    display_name: str
    evidence_kind: str
    locator: (
        CitationLineLocatorResponse
        | CitationPageLocatorResponse
        | CitationTextSpanLocatorResponse
        | CitationPageRegionLocatorResponse
        | CitationTableCellLocatorResponse
    )
    source_url: str
    preview_url: str
    download_url: str
    origin_label: Literal["库"]


class CitationEnvelope(StrictSchema):
    citation: CitationResponse


class CitationSummaryResponse(StrictSchema):
    citation_id: str
    input_ref: str
    display_name: str
    source_tool_call_id: str
    source_url: str


class CitationSnapshotResponse(StrictSchema):
    schema_id: Literal["workspace.citations.v1"] = Field(alias="schema")
    agent_run_id: str
    session_id: str
    through_sequence: int = Field(ge=0)
    citations: list[CitationSummaryResponse]


class TranscriptCitationBinding(StrictSchema):
    source_sequence: str
    source_tool_call_id: str
    snapshot: CitationSnapshotResponse


class TranscriptCitationsResponse(StrictSchema):
    session_id: str
    bindings: list[TranscriptCitationBinding]


class HostedOperationResponse(StrictSchema):
    operation_id: str
    command: Literal["createSession", "submitMessage"]
    status: Literal["accepted"]
    session_id: str
    agent_run_id: str | None
    turn_id: str | None


class AgentRunCancellationResponse(StrictSchema):
    agent_run_id: str
    status: str
    disposition: str


class AgentRunSupplementResponse(StrictSchema):
    agent_run_id: str
    session_id: str
    supplement_id: str
    disposition: Literal["accepted", "duplicate"]
    queued_count: int = Field(ge=0, le=8)


class TranscriptPublishedArtifact(StrictSchema):
    artifact_ref: str
    filename: str
    download_url: str


class TranscriptTurnMetadata(StrictSchema):
    anchor_sequence: str
    started_at_ms: int
    completed_at_ms: int | None
    artifacts: list[TranscriptPublishedArtifact]


class TranscriptTurnMetadataResponse(StrictSchema):
    turns: list[TranscriptTurnMetadata]


class PresentationOperationResponse(StrictSchema):
    kind: Literal["call", "result"]
    call_id: str
    tool_name: str
    normalized_input: dict[str, JsonValue] | None
    result_state: str | None
    operations: list[JsonValue]
    preview_truncated: bool
    source_event_id: str
