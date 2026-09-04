mod catalog;
mod config;
mod prompt;
mod types;

pub use catalog::SkillIndex;
pub use config::{
    add_skill_source, remove_skill_source, set_skill_enabled, set_skill_source_enabled,
    validate_skill_sources_config, SkillSourceConfigStore,
};
pub use prompt::render_available_skills;
pub use types::{
    SkillCapabilityMetadata, SkillCatalogItemV1, SkillCatalogLoadConfig, SkillCatalogRequest,
    SkillCatalogSnapshotV1, SkillDetailRequest, SkillDetailV1, SkillDiagnosticV1, SkillEntryV1,
    SkillPolicyV1, SkillSetEnabledRequest, SkillSourceAddRequest, SkillSourceConfigV1,
    SkillSourceIdRequest, SkillSourceKindV1, SkillSourceListRequest, SkillSourceRefV1,
    SkillSourceScopeV1, SkillSourceSetEnabledRequest, SkillSourceStatusV1, SkillSourcesConfigV1,
    SKILL_CATALOG_SNAPSHOT_SCHEMA_V1, SKILL_SOURCES_CONFIG_SCHEMA_V1,
};
