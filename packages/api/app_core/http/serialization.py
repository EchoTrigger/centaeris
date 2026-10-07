from app_core.models import (
    Agent,
    Artifact,
    Session,
    SessionProject,
    ModelConfig,
    ModelProvider,
    Source,
    SourceObject,
    UserLibraryObject,
    Workspace,
)


def serialize_workspace(workspace: Workspace, role: str) -> dict:
    return {
        "id": workspace.id,
        "name": workspace.name,
        "description": workspace.description,
        "status": workspace.status,
        "role": role,
    }


def serialize_agent(agent: Agent) -> dict:
    from app_core.business_agent_scope import agent_configuration
    configuration = agent_configuration(agent)
    version = agent.definition.published_version if agent.definition_id else None
    return {
        "id": agent.id,
        "workspaceId": agent.workspace_id,
        "definitionId": agent.definition_id,
        "definitionVersionId": version.id if version else None,
        "name": version.name if version else configuration.name,
        "description": version.description if version else configuration.description,
        "instructions": version.instructions if version else configuration.instructions,
        "avatarKind": version.avatar_kind if version else configuration.avatar_kind,
        "status": agent.status,
        "deletedAt": agent.deletedAt.isoformat() if agent.deletedAt else None,
        "createdAt": agent.createdAt.isoformat(),
        "updatedAt": agent.updatedAt.isoformat(),
    }


def serialize_agent_definition(definition) -> dict:
    return {
        "id": definition.id, "workspaceId": definition.workspace_id,
        "name": definition.name, "description": definition.description,
        "instructions": definition.instructions, "avatarKind": definition.avatar_kind,
        "pluginNames": definition.plugin_names,
        "status": definition.status, "availabilityScope": definition.availability_scope,
        "membershipIds": sorted(item.membership_id for item in definition.member_grants.all()),
        "publishedVersionId": definition.published_version_id,
        "createdAt": definition.created_at.isoformat(), "updatedAt": definition.updated_at.isoformat(),
    }


def serialize_agent_definition_version(version) -> dict:
    return {
        "id": version.id, "definitionId": version.definition_id, "version": version.version,
        "name": version.name, "description": version.description,
        "instructions": version.instructions, "avatarKind": version.avatar_kind,
        "pluginNames": [package["name"] for package in version.plugin_activation["packages"]],
        "publishedAt": version.published_at.isoformat(),
    }


def serialize_session(session: Session, has_active_agent_run: bool | None = None) -> dict:
    from app_core.agent_work_presentation import initial_input_origin
    if has_active_agent_run is None:
        has_active_agent_run = session.agent_runs.filter(
            status__in=["queued", "running"]
        ).exists()
    return {
        "id": session.id,
        "workspaceId": session.workspace_id,
        "agentId": session.agent_id,
        "projectId": session.project_id,
        "title": session.title,
        "origin": session.origin,
        "initialInputOrigin": initial_input_origin(session),
        "status": session.status,
        "deletedAt": session.deletedAt.isoformat() if session.deletedAt else None,
        "isPinned": session.isPinned,
        "isUnread": session.isUnread,
        "hasActiveAgentRun": has_active_agent_run,
        "updatedAt": session.updatedAt.isoformat(),
    }


def serialize_model(model: ModelConfig) -> dict:
    return {
        "id": model.id,
        "displayName": model.displayName or model.modelName,
        "providerId": model.provider_id,
        "providerDisplayName": model.provider.displayName if model.provider_id else None,
        "modelName": model.modelName,
        "contextTokens": model.contextTokens,
        "maxOutputTokens": model.maxOutputTokens,
        "thinkingMode": model.thinkingMode or None,
        "thinkingModes": model.thinkingModes,
    }


def serialize_session_project(project: SessionProject) -> dict:
    return {
        "id": project.id,
        "workspaceId": project.workspace_id,
        "agentId": project.agent_id,
        "name": project.name,
        "createdAt": project.created_at.isoformat(),
    }


def serialize_admin_model(model: ModelConfig) -> dict:
    return serialize_model(model) | {
        "displayName": model.displayName,
        "api": model.resolvedApi,
        "apiOverride": model.apiOverride,
        "apiBase": model.resolvedApiBase,
        "enabled": model.enabled,
        "revision": model.revision,
        "updatedAt": model.updatedAt.isoformat(),
    }


def serialize_user(user) -> dict:
    return {
        "id": str(user.id),
        "email": user.email or user.username,
        "isStaff": user.is_staff,
        "isSuperuser": user.is_superuser,
    }


def serialize_model_provider(provider: ModelProvider) -> dict:
    credential = provider.credential
    return {
        "id": provider.id,
        "displayName": provider.displayName,
        "templateId": provider.template_id,
        "api": provider.api,
        "apiBase": provider.apiBase,
        "enabled": provider.enabled,
        "credentialVersion": credential.version,
        "updatedAt": provider.updatedAt.isoformat(),
    }


def serialize_source(source: Source, access_level: str) -> dict:
    return {
        "id": source.id,
        "workspaceId": source.workspace_id,
        "sourceType": source.sourceType,
        "name": source.name,
        "status": source.status,
        "failureReason": source.failureReason,
        "accessLevel": access_level,
    }


def serialize_source_object(item: SourceObject) -> dict:
    return {
        "id": item.id,
        "sourceId": item.source_id,
        "objectType": item.objectType,
        "displayPath": item.displayPath,
        "displayName": item.displayName,
        "contentType": item.contentType,
        "sizeBytes": item.sizeBytes,
        "sha256": item.sha256,
        "sourceVersion": item.sourceVersion,
        "status": item.status,
        "failureReason": safe_asset_failure_reason(item.status, item.failureReason),
    }


def serialize_library_object(item: UserLibraryObject) -> dict:
    return {
        "id": item.id,
        "kind": "userLibraryObject",
        "displayName": item.displayName,
        "objectKind": item.objectKind,
        "contentType": item.contentType,
        "sizeBytes": item.sizeBytes,
        "sha256": item.sha256,
        "status": item.status,
        "failureReason": safe_asset_failure_reason(item.status, item.failureReason),
        "parentFolderId": item.parentFolder_id,
        "deletedAt": item.deletedAt.isoformat() if item.deletedAt else None,
        "deletionGeneration": item.deletionGeneration,
        "updatedAt": item.updatedAt.isoformat(),
    }


def safe_asset_failure_reason(status: str, reason: str) -> str:
    if status != "failed":
        return ""
    if 0 < len(reason) <= 160 and all(
        character.isascii()
        and (character.islower() or character.isdigit() or character in "_-.: ")
        for character in reason
    ):
        return reason
    return "asset_processing_failed"


def serialize_artifact(item: Artifact) -> dict:
    return {
        "id": item.id,
        "kind": "artifact",
        "workspaceId": item.workspace_id,
        "sessionId": item.session_id,
        "agentRunId": item.agent_run_id,
        "displayName": item.displayName,
        "contentType": item.contentType,
        "sizeBytes": item.sizeBytes,
        "sha256": item.sha256,
        "status": item.status,
        "downloadUrl": (
            f"/api/artifacts/{item.id}/download" if item.status == "published" else None
        ),
    }
