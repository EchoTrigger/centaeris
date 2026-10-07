use centaeris_core::execution::{
    ExecutionFileSystemError, ExecutionFileSystemErrorKind, ExecutionFileSystemOperation,
    ExecutionFileSystemOutput, ExecutionInputState, ExecutionWorkspaceGenerationV1,
};
use serde::{Deserialize, Serialize};

pub const SANDBOX_INPUT_INVENTORY_SCHEMA: &str = "sandbox.input_inventory.v1";
pub const SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA: &str = "workspace.snapshot.v1";
pub const SANDBOX_WORKSPACE_GENERATION_SCHEMA: &str = "workspace.generation.v1";
pub const SANDBOX_WORKSPACE_GENERATION_QUERY_LINE: &[u8] =
    b"{\"schema\":\"workspace.generation.query.v1\"}\n";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxFileSystemRequest {
    pub path: String,
    pub operation: ExecutionFileSystemOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxFileSystemResult {
    pub output: Option<ExecutionFileSystemOutput>,
    pub error: Option<ExecutionFileSystemError>,
}

impl SandboxFileSystemResult {
    pub fn into_result(self) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        match (self.output, self.error) {
            (Some(output), None) => Ok(output),
            (None, Some(error)) => Err(error),
            _ => Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::HostUnavailable,
                "sandbox filesystem helper returned an invalid result",
            )),
        }
    }
}

impl From<Result<ExecutionFileSystemOutput, ExecutionFileSystemError>> for SandboxFileSystemResult {
    fn from(value: Result<ExecutionFileSystemOutput, ExecutionFileSystemError>) -> Self {
        match value {
            Ok(output) => Self {
                output: Some(output),
                error: None,
            },
            Err(error) => Self {
                output: None,
                error: Some(error),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxMaterializedInput {
    pub input_ref: String,
    pub virtual_path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub source_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ExecutionInputState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxInputInventory {
    pub schema: String,
    pub inputs: Vec<SandboxMaterializedInput>,
}

impl Default for SandboxInputInventory {
    fn default() -> Self {
        Self {
            schema: SANDBOX_INPUT_INVENTORY_SCHEMA.to_string(),
            inputs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxWorkspaceGeneration {
    pub schema: String,
    pub generation: Option<ExecutionWorkspaceGenerationV1>,
    pub diagnostic: Option<String>,
}

impl SandboxWorkspaceGeneration {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SANDBOX_WORKSPACE_GENERATION_SCHEMA {
            return Err("sandbox workspace generation schema mismatch".to_string());
        }
        match (&self.generation, &self.diagnostic) {
            (Some(generation), None) => generation.validate(),
            (None, Some(diagnostic)) if !diagnostic.trim().is_empty() => Ok(()),
            _ => Err("sandbox workspace generation shape is invalid".to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxInputRevokeRequest {
    pub input_ref: String,
    pub virtual_path: String,
    pub state: ExecutionInputState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxArtifactRequest {
    pub path: String,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxArtifactMetadata {
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxWorkspaceSnapshotManifest {
    pub schema: String,
    pub files: Vec<SandboxWorkspaceSnapshotFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxWorkspaceSnapshotFile {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub executable: bool,
}
