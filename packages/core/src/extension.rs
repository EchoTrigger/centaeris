mod activation;
mod catalog;
pub mod composition;
mod config;
pub mod hooks;
mod manifest;
mod mcp;
pub mod skills;
mod types;

pub use activation::{
    build_plugin_activation_snapshot, plugin_catalog_state, resolve_plugin_package,
    validate_plugin_activation_snapshot, PLUGIN_ACTIVATION_SNAPSHOT_SCHEMA_V1,
};
pub use catalog::{
    list_plugins, plugin_detail, plugin_source_ref, reload_plugins, set_plugin_enabled,
    PluginCatalogRoots,
};
pub use config::PluginConfigStore;
pub use manifest::{
    load_plugin_manifest_file, load_plugin_registry_from_manifests, PluginHookDeclarationV1,
    PluginHookHandlerConfigV1, PluginHooksFileV1, PluginInterfaceV1, PluginManifestPathsV1,
    PluginManifestV1, PluginRegistryV1, PluginTrustPolicyV1,
};
pub use mcp::{
    load_mcp_servers_file, mcp_model_contract_digest, mcp_provider_id, McpLifecycleV1,
    McpServerDeclarationV1, McpServersFileV1, McpToolDeclarationV1, McpTransportV1,
    MCP_SERVERS_SCHEMA_V1,
};
pub use types::{
    ActivatedPluginPackageV1, PluginActivationSnapshotV1, PluginCapabilitiesV1,
    PluginCatalogStateV1, PluginDescriptorV1, PluginDetailRequestV1, PluginDetailV1,
    PluginListRequestV1, PluginResourceDigestV1, PluginSetEnabledRequestV1, PluginSourceRefV1,
};
