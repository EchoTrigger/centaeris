use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SKILL_SOURCES_CONFIG_SCHEMA_V1: &str = "skill.sources.v1";
pub const SKILL_CATALOG_SNAPSHOT_SCHEMA_V1: &str = "skill_catalog_snapshot_v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum SkillSourceScopeV1 {
    Workspace,
    User,
    System,
    Plugin,
}

impl SkillSourceScopeV1 {
    pub(crate) fn priority(self) -> u8 {
        match self {
            Self::Workspace => 0,
            Self::User => 1,
            Self::System => 2,
            Self::Plugin => 3,
        }
    }

    pub(crate) fn identity_key(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::User => "user",
            Self::System => "system",
            Self::Plugin => "plugin",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SkillSourceKindV1 {
    CatalogDirectory,
    SkillFile,
}

impl SkillSourceKindV1 {
    pub(crate) fn identity_key(self) -> &'static str {
        match self {
            Self::CatalogDirectory => "catalogDirectory",
            Self::SkillFile => "skillFile",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceConfigV1 {
    pub source_id: String,
    pub scope: SkillSourceScopeV1,
    pub kind: SkillSourceKindV1,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillPolicyV1 {
    pub source_id: String,
    pub skill_name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourcesConfigV1 {
    pub schema_version: String,
    pub sources: Vec<SkillSourceConfigV1>,
    pub skill_policies: Vec<SkillPolicyV1>,
}

impl Default for SkillSourcesConfigV1 {
    fn default() -> Self {
        Self {
            schema_version: SKILL_SOURCES_CONFIG_SCHEMA_V1.to_string(),
            sources: Vec::new(),
            skill_policies: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkillCatalogLoadConfig {
    pub cwd: Option<PathBuf>,
    pub sources_config: SkillSourcesConfigV1,
    pub max_skills: usize,
}

impl Default for SkillCatalogLoadConfig {
    fn default() -> Self {
        Self {
            cwd: None,
            sources_config: SkillSourcesConfigV1::default(),
            max_skills: 256,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SkillCapabilityMetadata {
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntryV1 {
    pub skill_id: String,
    pub source_id: String,
    pub scope: SkillSourceScopeV1,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub allow_implicit_invocation: bool,
    pub capability_metadata: SkillCapabilityMetadata,
    pub skill_md_path: String,
    pub root_path: String,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadowed_by: Option<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillSourceStatusV1 {
    pub source_id: String,
    pub scope: SkillSourceScopeV1,
    pub kind: SkillSourceKindV1,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    pub enabled: bool,
}

impl From<&SkillSourceConfigV1> for SkillSourceStatusV1 {
    fn from(value: &SkillSourceConfigV1) -> Self {
        Self {
            source_id: value.source_id.clone(),
            scope: value.scope,
            kind: value.kind,
            path: value.path.clone(),
            workspace_root: value.workspace_root.clone(),
            enabled: value.enabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillDiagnosticV1 {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalogSnapshotV1 {
    pub schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub catalog_hash: String,
    pub sources: Vec<SkillSourceStatusV1>,
    pub skills: Vec<SkillEntryV1>,
    pub diagnostics: Vec<SkillDiagnosticV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalogItemV1 {
    pub name: String,
    pub description: String,
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillDetailV1 {
    pub skill: SkillEntryV1,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillCatalogRequest {
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillDetailRequest {
    pub cwd: Option<String>,
    pub skill_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSetEnabledRequest {
    pub cwd: Option<String>,
    pub skill_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceAddRequest {
    pub scope: SkillSourceScopeV1,
    pub kind: SkillSourceKindV1,
    pub path: String,
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceListRequest {}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceIdRequest {
    pub source_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSourceSetEnabledRequest {
    pub source_id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkillSourceRefV1 {
    pub kind: String,
    pub path: String,
}
