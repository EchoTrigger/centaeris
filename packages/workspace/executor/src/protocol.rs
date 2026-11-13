use centaeris_core::execution::{
    ExecutionFileSystemError, ExecutionFileSystemErrorKind, ExecutionFileSystemOperation,
    ExecutionFileSystemOutput, ExecutionInputState, ExecutionWorkspaceGenerationV1,
};
use serde::{Deserialize, Serialize};

mod filesystem_frame;
pub use filesystem_frame::{
    read_filesystem_request, read_filesystem_result, write_filesystem_request,
    write_filesystem_result, FileSystemFrameMetadata, FileSystemResponseFrameMetadata,
    FILESYSTEM_FRAME_HEADER_LIMIT,
};

pub const SANDBOX_INPUT_INVENTORY_SCHEMA: &str = "sandbox.input_inventory.v1";
pub const SANDBOX_ARTIFACT_STREAM_SCHEMA: &str = "sandbox.artifact.stream.v1";
pub const SANDBOX_ARTIFACT_STREAM_HEADER_LIMIT: usize = 1024 * 1024;
pub const SANDBOX_ARTIFACT_STREAM_DIGEST_BYTES: usize = 32;
pub const SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA_V1: &str = "workspace.snapshot.v1";
pub const SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA: &str = "workspace.snapshot.v2";
pub const SANDBOX_SNAPSHOT_CAPTURE_SCHEMA: &str = "workspace.snapshot.capture.v1";
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

/// The artifact bytes and raw SHA-256 digest follow this length-prefixed header.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxArtifactStreamHeader {
    pub schema: String,
    pub filename: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxArtifactMetadata {
    pub filename: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxWorkspaceSnapshotManifest {
    pub schema: String,
    pub files: Vec<SandboxWorkspaceSnapshotFile>,
    pub directories: Vec<String>,
    pub symlinks: Vec<SandboxWorkspaceSnapshotSymlink>,
}

impl SandboxWorkspaceSnapshotManifest {
    pub fn node_count(&self) -> usize {
        self.files.len() + self.directories.len() + self.symlinks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.node_count() == 0
    }
}

impl Serialize for SandboxWorkspaceSnapshotManifest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeStruct};

        let legacy = match self.schema.as_str() {
            SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA_V1
                if self.directories.is_empty() && self.symlinks.is_empty() =>
            {
                true
            }
            SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA => false,
            _ => return Err(S::Error::custom("unsupported workspace snapshot schema")),
        };
        // Released v1 manifests retain their canonical bytes and stored identities.
        let mut state = serializer.serialize_struct(
            "SandboxWorkspaceSnapshotManifest",
            if legacy { 2 } else { 4 },
        )?;
        state.serialize_field("schema", &self.schema)?;
        state.serialize_field("files", &self.files)?;
        if !legacy {
            state.serialize_field("directories", &self.directories)?;
            state.serialize_field("symlinks", &self.symlinks)?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for SandboxWorkspaceSnapshotManifest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "schema", deny_unknown_fields)]
        enum Version {
            #[serde(rename = "workspace.snapshot.v1")]
            V1 {
                files: Vec<SandboxWorkspaceSnapshotFile>,
            },
            #[serde(rename = "workspace.snapshot.v2")]
            V2 {
                files: Vec<SandboxWorkspaceSnapshotFile>,
                directories: Vec<String>,
                symlinks: Vec<SandboxWorkspaceSnapshotSymlink>,
            },
        }
        Ok(match Version::deserialize(deserializer)? {
            Version::V1 { files } => Self {
                schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA_V1.into(),
                files,
                directories: Vec::new(),
                symlinks: Vec::new(),
            },
            Version::V2 {
                files,
                directories,
                symlinks,
            } => Self {
                schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
                files,
                directories,
                symlinks,
            },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxWorkspaceSnapshotSymlink {
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxWorkspaceSnapshotFile {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxSnapshotCaptureRequest {
    pub schema: String,
    pub capture_id: String,
    pub baseline: Option<SandboxSnapshotBaseline>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxSnapshotBaseline {
    pub snapshot_sha256: String,
    pub manifest: SandboxWorkspaceSnapshotManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxSnapshotCaptureResponse {
    pub schema: String,
    pub capture_id: String,
    pub disposition: SnapshotCaptureDisposition,
    pub baseline_snapshot_sha256: Option<String>,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotCaptureDisposition {
    Snapshot,
    Unchanged,
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn legacy_snapshot_upgrade_retains_original_canonical_bytes() {
        let original = br#"{"schema":"workspace.snapshot.v1","files":[{"path":"result.txt","sizeBytes":0,"sha256":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","executable":false}]}"#;
        let decoded: SandboxWorkspaceSnapshotManifest = serde_json::from_slice(original).unwrap();
        assert!(decoded.directories.is_empty());
        assert!(decoded.symlinks.is_empty());
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), original);
        let upgraded = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            ..decoded
        };
        let value = serde_json::to_value(upgraded).unwrap();
        assert_eq!(value["schema"], SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA);
        assert_eq!(value["directories"], serde_json::json!([]));
        assert_eq!(value["symlinks"], serde_json::json!([]));
    }

    #[test]
    fn snapshot_versions_reject_unknown_or_missing_fields() {
        for json in [
            r#"{"schema":"workspace.snapshot.v1","files":[],"directories":[]}"#,
            r#"{"schema":"workspace.snapshot.v2","files":[],"directories":[]}"#,
            r#"{"schema":"workspace.snapshot.v2","files":[],"directories":[],"symlinks":[],"unknown":0}"#,
            r#"{"schema":"workspace.snapshot.v3","files":[],"directories":[],"symlinks":[]}"#,
            r#"{"schema":"workspace.snapshot.v2","files":[],"directories":[],"symlinks":[{"path":"link","target":"/etc/passwd","unknown":0}]}"#,
        ] {
            assert!(
                serde_json::from_str::<SandboxWorkspaceSnapshotManifest>(json).is_err(),
                "accepted {json}"
            );
        }
    }
}
