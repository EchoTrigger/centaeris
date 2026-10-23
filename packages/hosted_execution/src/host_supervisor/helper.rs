//! Fixed host-owned helper capabilities carried by the shared task supervisor.
use centaeris_core::execution::{
    EnvironmentTaskFence, EnvironmentTaskScope, MAX_EXECUTION_INPUT_BYTES,
    MAX_PUBLISHED_ARTIFACT_BYTES,
};
use serde::{Deserialize, Serialize};

const JSON_LIMIT: u64 = 1024 * 1024;
pub const HELPER_STREAM_LIMIT: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TrustedHelperKind {
    PrepareWorkspace,
    PrepareManagedWorkspace,
    FileSystem,
    InputInventory,
    MaterializeInput,
    RevokeInput,
    ReadArtifact,
    SnapshotCollect,
    SnapshotCapture,
    SnapshotRestore,
    WorkspaceGeneration,
    WorkspaceGenerationRpc,
    WorkspaceWatch,
    AgentProcessState,
    QuiesceAgentProcesses,
}

impl TrustedHelperKind {
    pub fn entrypoint(self) -> &'static str {
        match self {
            Self::PrepareWorkspace => "prepare-workspace",
            Self::PrepareManagedWorkspace => "prepare-managed-workspace",
            Self::FileSystem => "filesystem-once",
            Self::InputInventory => "input-inventory",
            Self::MaterializeInput => "materialize-input",
            Self::RevokeInput => "revoke-input",
            Self::ReadArtifact => "read-artifact",
            Self::SnapshotCollect => "snapshot-collect",
            Self::SnapshotCapture => "snapshot-capture",
            Self::SnapshotRestore => "snapshot-restore",
            Self::WorkspaceGeneration => "workspace-generation",
            Self::WorkspaceGenerationRpc => "workspace-generation-rpc",
            Self::WorkspaceWatch => "workspace-watch",
            Self::AgentProcessState => "agent-process-state",
            Self::QuiesceAgentProcesses => "quiesce-agent-processes",
        }
    }

    pub fn input_limit(self) -> u64 {
        match self {
            Self::PrepareWorkspace
            | Self::PrepareManagedWorkspace
            | Self::InputInventory
            | Self::SnapshotCollect
            | Self::WorkspaceGeneration
            | Self::WorkspaceWatch
            | Self::AgentProcessState
            | Self::QuiesceAgentProcesses => 0,
            Self::FileSystem | Self::MaterializeInput => MAX_EXECUTION_INPUT_BYTES + JSON_LIMIT + 4,
            Self::SnapshotRestore => HELPER_STREAM_LIMIT,
            Self::SnapshotCapture => JSON_LIMIT + 1024,
            Self::RevokeInput | Self::ReadArtifact | Self::WorkspaceGenerationRpc => JSON_LIMIT,
        }
    }

    pub fn output_limit(self) -> u64 {
        match self {
            Self::FileSystem | Self::ReadArtifact => MAX_PUBLISHED_ARTIFACT_BYTES + JSON_LIMIT + 4,
            Self::SnapshotCollect | Self::SnapshotCapture => HELPER_STREAM_LIMIT,
            _ => JSON_LIMIT,
        }
    }

    pub fn timeout_ms(self) -> u64 {
        match self {
            Self::WorkspaceWatch => 0,
            Self::SnapshotCollect | Self::SnapshotCapture | Self::SnapshotRestore => 300_000,
            _ => 60_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostHelperRequest {
    pub fence: EnvironmentTaskFence,
    pub operation_id: String,
    pub scope: EnvironmentTaskScope,
    pub kind: TrustedHelperKind,
    pub input_digest: String,
    pub input_length: u64,
}

impl HostHelperRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.fence.validate_operation_id(&self.operation_id)?;
        self.fence.authorize_start(&self.scope)?;
        if self.input_length > self.kind.input_limit()
            || !self.input_digest.strip_prefix("sha256:").is_some_and(|v| {
                v.len() == 64
                    && v.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err("host_helper_request_invalid".into());
        }
        Ok(())
    }
}
