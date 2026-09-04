#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeOperationKind {
    Read,
    DesiredStateWrite,
    IdentityMutation,
    Creation,
    OneShotAction,
}

impl RuntimeOperationKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::DesiredStateWrite => "desiredStateWrite",
            Self::IdentityMutation => "identityMutation",
            Self::Creation => "creation",
            Self::OneShotAction => "oneShotAction",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeRetryPolicy {
    SafeRetry,
    SameOperationId,
    NoAutomaticRetry,
}

impl RuntimeRetryPolicy {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SafeRetry => "safeRetry",
            Self::SameOperationId => "sameOperationId",
            Self::NoAutomaticRetry => "noAutomaticRetry",
        }
    }
}

macro_rules! runtime_commands {
    ($consumer:ident) => {
        $consumer! {
            AgentContextUsageGet, "agent_context_usage_get", SharedRuntime, Read, SafeRetry, None;
            AgentContextCompact, "_centaeris/session/compact_context", SharedRuntime, OneShotAction, NoAutomaticRetry, None;
            AgentDeadLetterDismiss, "agent_dead_letter_dismiss", SharedRuntime, IdentityMutation, NoAutomaticRetry, Some("agent_dead_letter_get");
            AgentDeadLetterGet, "agent_dead_letter_get", SharedRuntime, Read, SafeRetry, None;
            AgentDeadLetterList, "agent_dead_letter_list", SharedRuntime, Read, SafeRetry, None;
            AgentDeadLetterReplay, "agent_dead_letter_replay", SharedRuntime, IdentityMutation, NoAutomaticRetry, Some("agent_dead_letter_get");
            SessionActivate, "_centaeris/session/activate", SharedRuntime, DesiredStateWrite, NoAutomaticRetry, Some("workspace_get");
            AgentAnswerNow, "_centaeris/session/answer_now", SharedRuntime, OneShotAction, NoAutomaticRetry, None;
            AgentQuestionAnswer, "_centaeris/session/answer_question", SharedRuntime, OneShotAction, NoAutomaticRetry, None;
            SessionDelete, "_centaeris/session/delete", SharedRuntime, IdentityMutation, NoAutomaticRetry, Some("session/list");
            SessionDiagnostics, "_centaeris/session/diagnostics", SharedRuntime, Read, SafeRetry, None;
            SessionProjectionGet, "_centaeris/session/project", SharedRuntime, Read, SafeRetry, None;
            SessionReorder, "_centaeris/session/reorder", SharedRuntime, DesiredStateWrite, NoAutomaticRetry, Some("session/list");
            AgentRunList, "_centaeris/session/agent-runs", SharedRuntime, Read, SafeRetry, None;
            AgentRunStreamReplay, "_centaeris/session/agent-runs/replay", SharedRuntime, Read, SafeRetry, None;
            AgentRunAttach, "_centaeris/session/agent-runs/attach", SharedRuntime, IdentityMutation, NoAutomaticRetry, None;
            AgentRunDetach, "_centaeris/session/agent-runs/detach", SharedRuntime, IdentityMutation, NoAutomaticRetry, None;
            AgentRunDetachViewer, "_centaeris/session/agent-runs/detach-viewer", SharedRuntime, IdentityMutation, NoAutomaticRetry, None;
            AgentRunCancel, "_centaeris/session/agent-runs/cancel", SharedRuntime, IdentityMutation, NoAutomaticRetry, Some("_centaeris/session/agent-runs");
            AgentSupplement, "_centaeris/session/supplement", SharedRuntime, OneShotAction, NoAutomaticRetry, None;
            SessionUpdate, "_centaeris/session/update_metadata", SharedRuntime, DesiredStateWrite, NoAutomaticRetry, Some("session/load");
            AgentRuntimeConfigGet, "agent_runtime_config_get", SharedRuntime, Read, SafeRetry, None;
            AgentRuntimeConfigReset, "agent_runtime_config_reset", SharedRuntime, DesiredStateWrite, NoAutomaticRetry, Some("agent_runtime_config_get");
            AgentRuntimeConfigSet, "agent_runtime_config_set", SharedRuntime, DesiredStateWrite, NoAutomaticRetry, Some("agent_runtime_config_get");
            AgentRuntimeModelTest, "agent_runtime_model_test", SharedRuntime, OneShotAction, NoAutomaticRetry, None;
            McpCatalog, "mcp/catalog", SharedRuntime, Read, SafeRetry, None;
            McpConfigure, "mcp/configure", SharedRuntime, DesiredStateWrite, NoAutomaticRetry, Some("mcp/catalog");
            AgentRuntimeGarbageCollect, "agent_runtime_garbage_collect", SharedRuntime, OneShotAction, NoAutomaticRetry, None;
            AgentRuntimeJobGet, "agent_runtime_job_get", SharedRuntime, Read, SafeRetry, None;
            AgentRuntimeJobList, "agent_runtime_job_list", SharedRuntime, Read, SafeRetry, None;
            AgentStateGet, "agent_state_get", SharedRuntime, Read, SafeRetry, None;
            TranscriptProjection, "transcript/project", SharedRuntime, Read, SafeRetry, None;
            PluginCatalogState, "plugin/catalog_state", SharedRuntime, Read, SafeRetry, None;
            SkillSourceList, "skill/source/list", SharedRuntime, Read, SafeRetry, None;
            SkillSourceAdd, "skill/source/add", SharedRuntime, Creation, NoAutomaticRetry, Some("skill/source/list");
            SkillSourceRemove, "skill/source/remove", SharedRuntime, IdentityMutation, NoAutomaticRetry, Some("skill/source/list");
            SkillSourceSetEnabled, "skill/source/set_enabled", SharedRuntime, DesiredStateWrite, NoAutomaticRetry, Some("skill/source/list");
            SkillSourceRef, "skill/source/ref", SharedRuntime, Read, SafeRetry, None;
            SkillCatalog, "skill/catalog", SharedRuntime, Read, SafeRetry, None;
            SkillDetail, "skill/detail", SharedRuntime, Read, SafeRetry, None;
            SkillSetEnabled, "skill/set_enabled", SharedRuntime, DesiredStateWrite, NoAutomaticRetry, Some("skill/detail");
            SkillReload, "skill/reload", SharedRuntime, OneShotAction, NoAutomaticRetry, Some("skill/catalog");
            PluginDetail, "plugin/detail", SharedRuntime, Read, SafeRetry, None;
            PluginList, "plugin/list", SharedRuntime, Read, SafeRetry, None;
            PluginReload, "plugin/reload", SharedRuntime, OneShotAction, NoAutomaticRetry, Some("plugin/catalog_state");
            PluginSetEnabled, "plugin/set_enabled", SharedRuntime, DesiredStateWrite, NoAutomaticRetry, Some("plugin/detail");
            PluginSourceRef, "plugin/source_ref", SharedRuntime, Read, SafeRetry, None;
            SessionList, "session/list", SharedRuntime, Read, SafeRetry, None;
            SessionGet, "session/load", SharedRuntime, Read, SafeRetry, None;
            SessionCreate, "session/new", SharedRuntime, Creation, SameOperationId, Some("session/new");
            AgentInput, "session/prompt", SharedRuntime, Creation, SameOperationId, Some("session/prompt");

            ProcessCapture, "process_capture", ExecutionHost, OneShotAction, NoAutomaticRetry, None;
            SidecarList, "sidecar_list", ExecutionHost, Read, SafeRetry, None;
            SidecarStart, "sidecar_start", ExecutionHost, Creation, NoAutomaticRetry, Some("sidecar_list");
            SidecarStop, "sidecar_stop", ExecutionHost, IdentityMutation, NoAutomaticRetry, Some("sidecar_list");
            WorkspaceFileTree, "workspace_file_tree", ExecutionHost, Read, SafeRetry, None;
            WorkspaceReadFile, "workspace_read_file", ExecutionHost, Read, SafeRetry, None;

            AppExit, "app_exit", HostSurface, OneShotAction, NoAutomaticRetry, None;
            DesktopFilePreviewRead, "desktop_file_preview_read", HostSurface, Read, SafeRetry, None;
            Initialize, "initialize", HostSurface, DesiredStateWrite, NoAutomaticRetry, None;
            PluginInstall, "plugin/install", HostSurface, Creation, NoAutomaticRetry, Some("plugin/list");
            PluginRemove, "plugin/remove", HostSurface, IdentityMutation, NoAutomaticRetry, Some("plugin/list");
            WorkspaceActivate, "workspace_activate", HostSurface, DesiredStateWrite, NoAutomaticRetry, Some("workspace_get");
            WorkspaceGet, "workspace_get", HostSurface, Read, SafeRetry, None;
            WorkspaceGitDiffGet, "workspace_git_diff_get", HostSurface, Read, SafeRetry, None;
            WorkspaceGitFileDiffGet, "workspace_git_file_diff_get", HostSurface, Read, SafeRetry, None;
            WorkspaceGitHubCliStatusGet, "workspace_git_github_cli_status_get", HostSurface, Read, SafeRetry, None;
            WorkspaceGitStatusGet, "workspace_git_status_get", HostSurface, Read, SafeRetry, None;
            WorkspaceOpenFolder, "workspace_open_folder", HostSurface, OneShotAction, NoAutomaticRetry, None;
            WorkspaceRemove, "workspace_remove", HostSurface, IdentityMutation, NoAutomaticRetry, Some("workspace_get");
            WorkspaceReset, "workspace_reset", HostSurface, DesiredStateWrite, NoAutomaticRetry, Some("workspace_get");
            WorkspaceRename, "workspace_rename", HostSurface, DesiredStateWrite, NoAutomaticRetry, Some("workspace_get");
            WorkspaceRevealFolder, "workspace_reveal_folder", HostSurface, OneShotAction, NoAutomaticRetry, None;
        }
    };
}

pub(crate) use runtime_commands;
