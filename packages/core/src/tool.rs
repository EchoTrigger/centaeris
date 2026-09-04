mod catalog;
pub(crate) mod concurrency;
mod dynamic;
mod error;
pub mod inputs;
pub mod knowledge;
pub mod layer;
pub mod limits;
pub mod permission;
mod policy;
mod projection;
mod types;

pub use catalog::{canonicalize_tool_name, list_tool_contracts, BUILTIN_TOOL_PROVIDER_ID};
pub(crate) use catalog::{
    EDIT_MAX_ARGS_BYTES, EDIT_MAX_ITEMS, EDIT_MAX_NEW_TEXT_BYTES, EDIT_MAX_OLD_TEXT_BYTES,
    READ_MAX_BYTES, READ_MAX_LINES, WORKSPACE_MUTATION_MAX_BYTES,
};
pub use dynamic::{list_tool_contracts_with_dynamic, DynamicToolContract, DynamicToolRegistry};
pub use error::{ToolErrorInfo, ToolFailureKind};
pub use permission::RiskLevel;
pub use policy::is_tool_concurrency_safe;
pub use projection::{
    build_model_tool_definitions, build_model_tool_definitions_for_names,
    build_model_tool_definitions_for_names_with_dynamic,
};
pub use types::{ModelToolChoice, ModelToolDefinition, ToolContract, ToolTurnBehavior};
