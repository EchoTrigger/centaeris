use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ToolExecutionFact {
    ArtifactPublished(Value),
    CitationRecorded(Value),
    ExternalEvidenceRef(Value),
    FileFact(Value),
}

impl ToolExecutionFact {
    pub(crate) fn payload(&self) -> &Value {
        match self {
            Self::ArtifactPublished(payload)
            | Self::CitationRecorded(payload)
            | Self::ExternalEvidenceRef(payload)
            | Self::FileFact(payload) => payload,
        }
    }
}
