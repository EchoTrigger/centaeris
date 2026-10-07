from typing import Literal

from ninja.responses import codes_4xx, codes_5xx
from pydantic import Field

from .schema import ErrorResponse, ModelResponse, StrictSchema


COMMON_ERROR_RESPONSES = {
    codes_4xx: ErrorResponse,
    codes_5xx: ErrorResponse,
}


class AgentCoordinationSessionResponse(StrictSchema):
    schema_id: Literal["agent.coordination_session.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")


class AgentMessageResponse(StrictSchema):
    id: str
    agent_run_id: str = Field(alias="agentRunId")
    turn_id: str = Field(alias="turnId")
    tool_call_id: str = Field(alias="toolCallId")
    source_sequence: int = Field(alias="sourceSequence")
    created_at_ms: int = Field(alias="createdAtMs")
    body: str
    session_refs: list[str] = Field(alias="sessionRefs")
    file_refs: list[str] = Field(alias="fileRefs")


class AgentMessagesResponse(StrictSchema):
    schema_id: Literal["agent.messages.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    session_id: str = Field(alias="sessionId")
    messages: list[AgentMessageResponse]
    next_after_sequence: int | None = Field(alias="nextAfterSequence")


class WorkspaceResponse(StrictSchema):
    id: str
    name: str
    description: str
    status: str
    role: Literal["owner", "admin", "member"]


class WorkspacesEnvelope(StrictSchema):
    workspaces: list[WorkspaceResponse]


class WorkspaceMemberResponse(StrictSchema):
    membership_id: str = Field(alias="membershipId")
    user_id: str = Field(alias="userId")
    email: str
    role: Literal["owner", "admin", "member"]
    created_at: str = Field(alias="createdAt")


class WorkspaceMembersEnvelope(StrictSchema):
    members: list[WorkspaceMemberResponse]


class WorkspaceMemberEnvelope(StrictSchema):
    member: WorkspaceMemberResponse


class WorkspaceGroupResponse(StrictSchema):
    id: str
    name: str
    kind: Literal["custom", "all_members"]
    created_at: str = Field(alias="createdAt")


class WorkspaceGroupEnvelope(StrictSchema):
    group: WorkspaceGroupResponse


class WorkspaceGroupsEnvelope(StrictSchema):
    groups: list[WorkspaceGroupResponse]


class WorkspaceGroupMembersEnvelope(StrictSchema):
    members: list[WorkspaceMemberResponse]


class WorkspaceOwnershipTransferredResponse(StrictSchema):
    owner: WorkspaceMemberResponse
    previous_owner: WorkspaceMemberResponse = Field(alias="previousOwner")


class WorkspaceInvitationResponse(StrictSchema):
    id: str
    email: str
    role: Literal["admin", "member"]
    status: Literal["pending", "accepted", "revoked", "expired"]
    expires_at: str = Field(alias="expiresAt")
    created_at: str = Field(alias="createdAt")


class WorkspaceInvitationEnvelope(StrictSchema):
    invitation: WorkspaceInvitationResponse
    invite_url: str = Field(alias="inviteUrl")


class WorkspaceInvitationsEnvelope(StrictSchema):
    invitations: list[WorkspaceInvitationResponse]


class WorkspaceInvitationPreviewResponse(StrictSchema):
    workspace_id: str = Field(alias="workspaceId")
    workspace_name: str = Field(alias="workspaceName")
    email: str
    role: Literal["admin", "member"]
    account_exists: bool = Field(alias="accountExists")
    expires_at: str = Field(alias="expiresAt")


class WorkspaceInvitationAcceptedResponse(StrictSchema):
    workspace_id: str = Field(alias="workspaceId")
    membership_id: str = Field(alias="membershipId")
    role: Literal["admin", "member"]
    user_created: bool = Field(alias="userCreated")


class PluginResourceResponse(StrictSchema):
    path: str
    digest: str


class WorkspaceMcpTransportResponse(StrictSchema):
    type: Literal["stdio", "streamableHttp"]
    endpoint: str | None


class WorkspaceMcpAuthResponse(StrictSchema):
    type: Literal["none", "bearer"]
    credential_ref: str | None = Field(alias="credentialRef")
    credential_configured: bool | None = Field(alias="credentialConfigured")


class WorkspaceMcpToolResponse(StrictSchema):
    source_name: str = Field(alias="sourceName")
    name: str
    description: str
    input_schema: dict[str, object] = Field(alias="inputSchema")
    concurrency_safe: bool = Field(alias="concurrencySafe")
    scopes: list[str]


class WorkspaceMcpServerResponse(StrictSchema):
    id: str
    model_contract_digest: str = Field(alias="modelContractDigest")
    transport: WorkspaceMcpTransportResponse
    auth: WorkspaceMcpAuthResponse
    startup_timeout_ms: int = Field(alias="startupTimeoutMs")
    tool_timeout_ms: int = Field(alias="toolTimeoutMs")
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
    timeout_ms: int = Field(alias="timeoutMs")


class WorkspacePluginResponse(StrictSchema):
    name: str
    display_name: str = Field(alias="displayName")
    short_description: str = Field(alias="shortDescription")
    capabilities: list[str]
    version: str
    package_digest: str = Field(alias="packageDigest")
    enabled: bool
    skills: list[PluginResourceResponse]
    cli: list[PluginResourceResponse]
    mcp_servers: list[WorkspaceMcpServerResponse] | None = Field(alias="mcpServers")
    mcp_credential_refs: list[str] | None = Field(alias="mcpCredentialRefs")
    hooks: list[WorkspaceHookResponse] | None
    errors: list[str]


class WorkspacePluginEnvelope(StrictSchema):
    plugin: WorkspacePluginResponse


class WorkspacePluginsEnvelope(StrictSchema):
    plugins: list[WorkspacePluginResponse]


class GlobalPluginResponse(StrictSchema):
    name: str
    display_name: str = Field(alias="displayName")
    short_description: str = Field(alias="shortDescription")
    capabilities: list[str]
    version: str
    enabled_workspace_count: int = Field(alias="enabledWorkspaceCount")
    credential_count: int = Field(alias="credentialCount")
    removable: bool
    errors: list[str]


class GlobalPluginEnvelope(StrictSchema):
    plugin: GlobalPluginResponse


class GlobalPluginsEnvelope(StrictSchema):
    plugins: list[GlobalPluginResponse]


class WorkspaceSkillResponse(StrictSchema):
    skill_id: str = Field(alias="skillId")
    name: str
    description: str
    enabled: bool
    allow_implicit_invocation: bool = Field(alias="allowImplicitInvocation")
    allowed_tools: list[str] = Field(alias="allowedTools")


class WorkspaceSkillsEnvelope(StrictSchema):
    schema_id: Literal["workspace.skill.catalog.result.v1"] = Field(alias="schema")
    skills: list[WorkspaceSkillResponse]


class WorkspaceSkillDetailEnvelope(StrictSchema):
    schema_id: Literal["workspace.skill.detail.result.v1"] = Field(alias="schema")
    skill: WorkspaceSkillResponse
    content: str


class AgentResponse(StrictSchema):
    id: str
    workspace_id: str = Field(alias="workspaceId")
    definition_id: str | None = Field(alias="definitionId")
    definition_version_id: str | None = Field(alias="definitionVersionId")
    name: str
    description: str
    instructions: str
    avatar_kind: Literal["centaeris", "banana"] = Field(alias="avatarKind")
    status: Literal["active", "deleted"]
    deleted_at: str | None = Field(alias="deletedAt")
    created_at: str = Field(alias="createdAt")
    updated_at: str = Field(alias="updatedAt")


class AgentEnvelope(StrictSchema):
    agent: AgentResponse


class AgentsEnvelope(StrictSchema):
    agents: list[AgentResponse]


class AgentDefinitionResponse(StrictSchema):
    id: str
    workspace_id: str = Field(alias="workspaceId")
    name: str
    description: str
    instructions: str
    plugin_names: list[str] = Field(alias="pluginNames")
    avatar_kind: Literal["centaeris", "banana"] = Field(alias="avatarKind")
    status: Literal["active", "disabled"]
    availability_scope: Literal["none", "workspace", "members"] = Field(alias="availabilityScope")
    membership_ids: list[str] = Field(alias="membershipIds")
    published_version_id: str | None = Field(alias="publishedVersionId")
    created_at: str = Field(alias="createdAt")
    updated_at: str = Field(alias="updatedAt")


class AgentDefinitionVersionResponse(StrictSchema):
    id: str
    definition_id: str = Field(alias="definitionId")
    version: int
    name: str
    description: str
    instructions: str
    plugin_names: list[str] = Field(alias="pluginNames")
    avatar_kind: Literal["centaeris", "banana"] = Field(alias="avatarKind")
    published_at: str = Field(alias="publishedAt")


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
    provider_id: str = Field(alias="providerId")
    name: str
    tokens: int


class ContextTokenBreakdownResponse(StrictSchema):
    system_prompt_tokens: int = Field(alias="systemPromptTokens")
    system_tool_tokens: int = Field(alias="systemToolTokens")
    mcp_tool_tokens: int = Field(alias="mcpToolTokens")
    skills_tokens: int = Field(alias="skillsTokens")
    message_tokens: int = Field(alias="messageTokens")
    auto_compact_buffer_tokens: int = Field(alias="autoCompactBufferTokens")
    free_space_tokens: int = Field(alias="freeSpaceTokens")
    mcp_tools: list[ContextMcpToolResponse] = Field(alias="mcpTools")


class ContextUsageResponse(StrictSchema):
    used_tokens: int = Field(alias="usedTokens")
    max_context_tokens: int = Field(alias="maxContextTokens")
    used_percentage: int = Field(alias="usedPercentage")
    updated_at: int = Field(alias="updatedAt")
    is_compacting: bool = Field(alias="isCompacting")
    breakdown: ContextTokenBreakdownResponse


class SessionContextUsageEnvelope(StrictSchema):
    schema_id: Literal["session.context_usage.v1"] = Field(alias="schema")
    session_id: str = Field(alias="sessionId")
    context_usage: ContextUsageResponse | None = Field(alias="contextUsage")


class TranscriptContentRangeResponse(StrictSchema):
    schema_id: Literal["transcript.content.range.v1"] = Field(alias="schema")
    session_id: str = Field(alias="sessionId")
    projection_version: Literal["transcript.projection.v1"] = Field(
        alias="projectionVersion"
    )
    projection_generation: str = Field(alias="projectionGeneration")
    ref_id: str = Field(alias="refId")
    revision: Literal["2"]
    byte_length: str = Field(alias="byteLength")
    start_offset: str = Field(alias="startOffset")
    end_offset: str = Field(alias="endOffset")
    content: str
    has_more: bool = Field(alias="hasMore")


class InitialInputOriginResponse(StrictSchema):
    message_id: str = Field(alias="messageId")
    agent_id: str = Field(alias="agentId")
    agent_name: str = Field(alias="agentName")


class SessionResponse(StrictSchema):
    id: str
    workspace_id: str = Field(alias="workspaceId")
    agent_id: str = Field(alias="agentId")
    project_id: str | None = Field(alias="projectId")
    title: str
    origin: str
    initial_input_origin: InitialInputOriginResponse | None = Field(alias="initialInputOrigin")
    status: Literal["active", "deleted"]
    deleted_at: str | None = Field(alias="deletedAt")
    is_pinned: bool = Field(alias="isPinned")
    is_unread: bool = Field(alias="isUnread")
    has_active_agent_run: bool = Field(alias="hasActiveAgentRun")
    updated_at: str = Field(alias="updatedAt")


class SessionEnvelope(StrictSchema):
    session: SessionResponse


class SessionsEnvelope(StrictSchema):
    sessions: list[SessionResponse]


class AgentWorkSessionsResponse(StrictSchema):
    schema_id: Literal["agent.work_sessions.v1"] = Field(alias="schema")
    agent_id: str = Field(alias="agentId")
    workspace_id: str = Field(alias="workspaceId")
    sessions: list[SessionResponse]
    next_after_session_id: str | None = Field(alias="nextAfterSessionId")
    has_more: bool = Field(alias="hasMore")


class SessionProjectResponse(StrictSchema):
    id: str
    workspace_id: str = Field(alias="workspaceId")
    agent_id: str = Field(alias="agentId")
    name: str
    created_at: str = Field(alias="createdAt")


class SessionProjectEnvelope(StrictSchema):
    project: SessionProjectResponse


class SessionProjectsEnvelope(StrictSchema):
    projects: list[SessionProjectResponse]


class SessionTrashEnvelope(SessionsEnvelope):
    next_cursor: str | None = Field(alias="nextCursor")
    has_more: bool = Field(alias="hasMore")


class DeletedResponse(StrictSchema):
    deleted: bool


class SourceResponse(StrictSchema):
    id: str
    workspace_id: str = Field(alias="workspaceId")
    source_type: str = Field(alias="sourceType")
    name: str
    status: str
    failure_reason: str = Field(alias="failureReason")
    access_level: Literal["read", "write", "control"] = Field(alias="accessLevel")


class SourceEnvelope(StrictSchema):
    source: SourceResponse


class SourcesEnvelope(StrictSchema):
    sources: list[SourceResponse]


class SourceObjectResponse(StrictSchema):
    id: str
    source_id: str = Field(alias="sourceId")
    object_type: str = Field(alias="objectType")
    display_path: str = Field(alias="displayPath")
    display_name: str = Field(alias="displayName")
    content_type: str = Field(alias="contentType")
    size_bytes: int | None = Field(alias="sizeBytes")
    sha256: str
    source_version: str = Field(alias="sourceVersion")
    status: str
    failure_reason: str = Field(alias="failureReason")


class SourceObjectEnvelope(StrictSchema):
    object: SourceObjectResponse


class SourceObjectsEnvelope(StrictSchema):
    objects: list[SourceObjectResponse]


class SourceGrantResponse(StrictSchema):
    id: str
    source_id: str = Field(alias="sourceId")
    workspace_group_id: str = Field(alias="workspaceGroupId")
    access_level: Literal["read", "write", "control"] = Field(alias="accessLevel")


class SourceGrantEnvelope(StrictSchema):
    grant: SourceGrantResponse


class SourceGrantsEnvelope(StrictSchema):
    grants: list[SourceGrantResponse]


class LibraryObjectResponse(StrictSchema):
    id: str
    kind: str
    display_name: str = Field(alias="displayName")
    object_kind: str = Field(alias="objectKind")
    content_type: str = Field(alias="contentType")
    size_bytes: int | None = Field(alias="sizeBytes")
    sha256: str
    status: str
    failure_reason: str = Field(alias="failureReason")
    parent_folder_id: str | None = Field(alias="parentFolderId")
    deleted_at: str | None = Field(alias="deletedAt")
    deletion_generation: int = Field(alias="deletionGeneration")
    updated_at: str = Field(alias="updatedAt")


class LibraryObjectEnvelope(StrictSchema):
    object: LibraryObjectResponse


class LibraryObjectsEnvelope(StrictSchema):
    objects: list[LibraryObjectResponse]


class TrashActorResponse(StrictSchema):
    user_id: str = Field(alias="userId")
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
    deleted_at: str = Field(alias="deletedAt")
    expires_at: str = Field(alias="expiresAt")
    scope: Literal["workspace", "privateLibrary"]
    location: TrashLocationResponse
    deleted_by: TrashActorResponse | None = Field(alias="deletedBy")


class TrashFilterOptionsResponse(StrictSchema):
    deleted_by: list[TrashActorResponse] = Field(alias="deletedBy")
    locations: list[TrashLocationResponse]


class TrashEnvelope(StrictSchema):
    items: list[TrashItemResponse]
    filter_options: TrashFilterOptionsResponse = Field(alias="filterOptions")
    next_cursor: str | None = Field(alias="nextCursor")
    has_more: bool = Field(alias="hasMore")


class LibraryNoteEnvelope(LibraryObjectEnvelope):
    markdown: str


class ArtifactResponse(StrictSchema):
    id: str
    kind: str
    workspace_id: str = Field(alias="workspaceId")
    session_id: str = Field(alias="sessionId")
    agent_run_id: str = Field(alias="agentRunId")
    display_name: str = Field(alias="displayName")
    content_type: str = Field(alias="contentType")
    size_bytes: int = Field(alias="sizeBytes")
    sha256: str
    status: str
    download_url: str | None = Field(alias="downloadUrl")


class SessionAssetResponse(StrictSchema):
    id: str
    asset_kind: str = Field(alias="assetKind")
    display_name: str = Field(alias="displayName")
    content_type: str = Field(alias="contentType")
    asset: SourceObjectResponse | LibraryObjectResponse | ArtifactResponse


class SessionAssetEnvelope(StrictSchema):
    asset: SessionAssetResponse


class SessionAssetsEnvelope(StrictSchema):
    assets: list[SessionAssetResponse]


class SessionUploadEnvelope(StrictSchema):
    library_objects: list[LibraryObjectResponse] = Field(alias="libraryObjects")
    assets: list[SessionAssetResponse]


class ModelProviderResponse(StrictSchema):
    id: str
    display_name: str = Field(alias="displayName")
    template_id: str | None = Field(alias="templateId")
    api: str
    api_base: str = Field(alias="apiBase")
    enabled: bool
    credential_version: int = Field(alias="credentialVersion")
    updated_at: str = Field(alias="updatedAt")


class ModelProviderEnvelope(StrictSchema):
    provider: ModelProviderResponse


class ModelProvidersEnvelope(StrictSchema):
    providers: list[ModelProviderResponse]


class ModelProviderTemplateModelResponse(StrictSchema):
    model_name: str = Field(alias="modelName")
    display_name: str = Field(alias="displayName")
    context_tokens: int = Field(alias="contextTokens")
    max_output_tokens: int = Field(alias="maxOutputTokens")
    api_override: str | None = Field(default=None, alias="apiOverride")
    thinking_mode: str | None = Field(default=None, alias="thinkingMode")
    thinking_modes: list[str] = Field(default_factory=list, alias="thinkingModes")


class ModelProviderTemplateResponse(StrictSchema):
    id: str
    display_name: str = Field(alias="displayName")
    tier: str = "direct_api"
    logo_svg: str | None = Field(default=None, alias="logoSvg")
    api: str
    api_base: str = Field(alias="apiBase")
    models: list[ModelProviderTemplateModelResponse]


class ModelProviderTemplatesEnvelope(StrictSchema):
    templates: list[ModelProviderTemplateResponse]


class ModelCatalogReconciliationEnvelope(StrictSchema):
    digest: str
    providers: list[dict]
    blocked: bool


class AdminModelResponse(ModelResponse):
    api: str
    api_override: str | None = Field(alias="apiOverride")
    api_base: str = Field(alias="apiBase")
    enabled: bool
    revision: int
    updated_at: str = Field(alias="updatedAt")


class AdminModelEnvelope(StrictSchema):
    model: AdminModelResponse


class AdminModelsEnvelope(StrictSchema):
    models: list[AdminModelResponse]


class RunIdsErrorResponse(ErrorResponse):
    agent_run_ids: list[str] = Field(alias="agentRunIds")


class ModelTestResponse(StrictSchema):
    ok: bool
    http_status: int | None = Field(alias="httpStatus")
    latency_ms: int = Field(alias="latencyMs")
    output_preview: str | None = Field(alias="outputPreview")
    error_keyword: str | None = Field(alias="errorKeyword")


class RuntimeJobResultRefResponse(StrictSchema):
    kind: str
    id: str


class RuntimeJobResponse(StrictSchema):
    id: str
    status: str
    progress_topic: str = Field(alias="progressTopic")
    result_refs: list[RuntimeJobResultRefResponse] = Field(alias="resultRefs")
    error: str | None


class RuntimeJobEnvelope(StrictSchema):
    job: RuntimeJobResponse


class CitationLineLocatorResponse(StrictSchema):
    start_line: int = Field(alias="startLine", gt=0)
    end_line: int = Field(alias="endLine", gt=0)


class CitationPageLocatorResponse(StrictSchema):
    page_start: int = Field(alias="pageStart", gt=0)
    page_end: int = Field(alias="pageEnd", gt=0)


class CitationTextSpanLocatorResponse(StrictSchema):
    kind: Literal["textSpan"]
    page_start: int | None = Field(default=None, alias="pageStart", gt=0)
    page_end: int | None = Field(default=None, alias="pageEnd", gt=0)
    start_byte: int = Field(alias="startByte", ge=0)
    end_byte: int = Field(alias="endByte", gt=0)
    start_line: int = Field(alias="startLine", gt=0)
    end_line: int = Field(alias="endLine", gt=0)


class CitationPageRegionLocatorResponse(StrictSchema):
    kind: Literal["pageRegion"]
    page: int = Field(gt=0)
    bbox: tuple[int, int, int, int]


class CitationTableCellLocatorResponse(StrictSchema):
    kind: Literal["tableCell"]
    page: int = Field(gt=0)
    table_id: str = Field(alias="tableId")
    start_row: int = Field(alias="startRow", ge=0)
    end_row: int = Field(alias="endRow", ge=0)
    start_column: int = Field(alias="startColumn", ge=0)
    end_column: int = Field(alias="endColumn", ge=0)


class CitationResponse(StrictSchema):
    citation_id: str = Field(alias="citationId")
    input_ref: str = Field(alias="inputRef")
    display_name: str = Field(alias="displayName")
    evidence_kind: str = Field(alias="evidenceKind")
    locator: (
        CitationLineLocatorResponse
        | CitationPageLocatorResponse
        | CitationTextSpanLocatorResponse
        | CitationPageRegionLocatorResponse
        | CitationTableCellLocatorResponse
    )
    source_url: str = Field(alias="sourceUrl")
    preview_url: str = Field(alias="previewUrl")
    download_url: str = Field(alias="downloadUrl")
    origin_label: Literal["库"] = Field(alias="originLabel")


class CitationEnvelope(StrictSchema):
    citation: CitationResponse


class CitationSummaryResponse(StrictSchema):
    citation_id: str = Field(alias="citationId")
    input_ref: str = Field(alias="inputRef")
    display_name: str = Field(alias="displayName")
    source_tool_call_id: str = Field(alias="sourceToolCallId")
    source_url: str = Field(alias="sourceUrl")


class CitationSnapshotResponse(StrictSchema):
    schema_id: Literal["workspace.citations.v1"] = Field(alias="schema")
    agent_run_id: str = Field(alias="agentRunId")
    session_id: str = Field(alias="sessionId")
    through_sequence: int = Field(alias="throughSequence", ge=0)
    citations: list[CitationSummaryResponse]


class TranscriptCitationBinding(StrictSchema):
    source_sequence: str = Field(alias="sourceSequence")
    source_tool_call_id: str = Field(alias="sourceToolCallId")
    snapshot: CitationSnapshotResponse


class TranscriptCitationsResponse(StrictSchema):
    session_id: str = Field(alias="sessionId")
    bindings: list[TranscriptCitationBinding]


class HostedOperationResponse(StrictSchema):
    operation_id: str = Field(alias="operationId")
    command: Literal["createSession", "submitMessage"]
    status: Literal["accepted"]
    session_id: str = Field(alias="sessionId")
    agent_run_id: str | None = Field(alias="agentRunId")
    turn_id: str | None = Field(alias="turnId")


class AgentRunCancellationResponse(StrictSchema):
    agent_run_id: str = Field(alias="agentRunId")
    status: str
    disposition: str


class AgentRunSupplementResponse(StrictSchema):
    agent_run_id: str = Field(alias="agentRunId")
    session_id: str = Field(alias="sessionId")
    supplement_id: str = Field(alias="supplementId")
    disposition: Literal["accepted", "duplicate"]
    queued_count: int = Field(alias="queuedCount", ge=0, le=8)


class TranscriptPublishedArtifact(StrictSchema):
    artifact_ref: str = Field(alias="artifactRef")
    filename: str
    download_url: str = Field(alias="downloadUrl")


class TranscriptTurnMetadata(StrictSchema):
    anchor_sequence: str = Field(alias="anchorSequence")
    started_at_ms: int = Field(alias="startedAtMs")
    completed_at_ms: int | None = Field(alias="completedAtMs")
    artifacts: list[TranscriptPublishedArtifact]


class TranscriptTurnMetadataResponse(StrictSchema):
    turns: list[TranscriptTurnMetadata]
