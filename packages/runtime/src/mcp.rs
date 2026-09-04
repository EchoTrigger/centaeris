use crate::{atomic_file, plugins, user_data_layout};
use centaeris_core::extension::hooks::{
    LifecycleHookEngineV1, LifecycleHookHandlerV1, LocalLifecycleHookCommandRunnerV1,
};
use centaeris_core::extension::skills::{
    SkillSourceConfigV1, SkillSourceKindV1, SkillSourceScopeV1,
};
use centaeris_core::extension::{
    build_plugin_activation_snapshot, load_mcp_servers_file, load_plugin_registry_from_manifests,
    resolve_plugin_package, McpServerDeclarationV1, McpTransportV1, PluginDescriptorV1,
    PluginListRequestV1, PluginTrustPolicyV1,
};
use centaeris_core::runtime::contracts::current_timestamp_ms;
use centaeris_core::runtime::QueryLifecycleHookRuntime;
use centaeris_core::tool::layer::DynamicToolProvider;
use centaeris_core::tool::limits::ToolContractBudget;
use centaeris_core::tool::{DynamicToolContract, DynamicToolRegistry};
use centaeris_mcp::{
    connect_streamable_http_mcp_server, lazy_mcp_server_binding, valid_bearer_token,
    McpConnectError, McpServerConnector,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

const CATALOG_SCHEMA: &str = "native.mcp.catalog.v1";
const CREDENTIALS_SCHEMA: &str = "native.mcp.credentials.v1";
const MAX_PLUGIN_DIAGNOSTICS: usize = 128;
static MCP_CREDENTIAL_STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NativeMcpCatalogRequestV1 {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NativeMcpConfigureRequestV1 {
    plugin_name: String,
    server_id: String,
    bearer_token: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeMcpCatalogV1 {
    schema: String,
    servers: Vec<NativeMcpServerV1>,
    diagnostics: Vec<NativePluginDiagnosticV1>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativePluginDiagnosticV1 {
    code: &'static str,
    plugin_name: String,
    path: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeMcpServerV1 {
    plugin_name: String,
    plugin_display_name: String,
    server_id: String,
    plugin_enabled: bool,
    status: NativeMcpServerStatusV1,
    configurable: bool,
    configured: bool,
    transport: NativeMcpTransportV1,
    endpoint: Option<String>,
    tool_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum NativeMcpServerStatusV1 {
    Ready,
    NeedsConfiguration,
    Disabled,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum NativeMcpTransportV1 {
    Stdio,
    StreamableHttp,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpCredentialStoreV1 {
    schema: String,
    credentials: Vec<McpBearerCredentialV1>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct McpBearerCredentialV1 {
    plugin_name: String,
    credential_ref: String,
    bearer_token: String,
    updated_at_ms: i64,
}

pub(crate) struct NativePluginActivation {
    pub(crate) digest: String,
    pub(crate) dynamic_tool_registry: Arc<DynamicToolRegistry>,
    pub(crate) providers: Vec<Arc<dyn DynamicToolProvider + Send + Sync>>,
    pub(crate) skill_sources: Vec<SkillSourceConfigV1>,
    pub(crate) command_environment: HashMap<String, String>,
    pub(crate) lifecycle_hooks: QueryLifecycleHookRuntime,
}

struct NativeHttpMcpConnector {
    plugin_name: String,
    server: McpServerDeclarationV1,
}

impl McpServerConnector for NativeHttpMcpConnector {
    fn connect<'a>(
        &'a self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Arc<dyn DynamicToolProvider + Send + Sync>, McpConnectError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let bearer_token = match &self.server.transport {
                McpTransportV1::StreamableHttp {
                    bearer_credential_ref,
                    ..
                } => bearer_credential_ref
                    .as_deref()
                    .map(|credential_ref| {
                        let _guard = credential_store_guard().map_err(McpConnectError::Unavailable)?;
                        load_credentials(user_data_layout::mcp_credential_file_path().as_path())
                            .map_err(McpConnectError::Unavailable)?
                            .get(self.plugin_name.as_str(), credential_ref)
                            .map(ToOwned::to_owned)
                            .ok_or_else(|| {
                                McpConnectError::Unavailable(format!(
                                    "MCP credential is not configured: pluginName={} credentialRef={credential_ref}",
                                    self.plugin_name
                                ))
                            })
                    })
                    .transpose()?,
                McpTransportV1::Stdio { .. } => {
                    return Err(McpConnectError::Unavailable(format!(
                        "Native MCP does not support stdio transport: pluginName={} serverId={}",
                        self.plugin_name, self.server.id
                    )))
                }
            };
            connect_streamable_http_mcp_server(
                self.plugin_name.as_str(),
                self.server.clone(),
                bearer_token.as_deref(),
            )
            .await
        })
    }
}

pub(crate) fn catalog(_request: NativeMcpCatalogRequestV1) -> Result<NativeMcpCatalogV1, String> {
    let _guard = credential_store_guard()?;
    catalog_from(
        plugins::list(PluginListRequestV1::default())?,
        &load_credentials(user_data_layout::mcp_credential_file_path().as_path())?,
    )
}

pub(crate) async fn configure(
    request: NativeMcpConfigureRequestV1,
) -> Result<NativeMcpCatalogV1, String> {
    if !valid_bearer_token(request.bearer_token.as_str()) {
        return Err("MCP bearer token is invalid".to_string());
    }
    let descriptors = plugins::list(PluginListRequestV1::default())?;
    let descriptor = descriptors
        .iter()
        .find(|item| item.id == request.plugin_name)
        .ok_or_else(|| format!("MCP plugin not found: {}", request.plugin_name))?;
    if !descriptor.enabled {
        return Err(format!("MCP plugin is disabled: {}", descriptor.id));
    }
    require_valid_descriptor(descriptor)?;
    let package = resolve_plugin_package(Path::new(descriptor.path.as_str()))?;
    let mut declaration_budget = ToolContractBudget::default();
    let server = package
        .mcp_servers
        .iter()
        .map(|resource| {
            let declaration = load_mcp_servers_file(
                Path::new(descriptor.path.as_str())
                    .join(&resource.path)
                    .as_path(),
            )?;
            declaration_budget.add(&declaration)?;
            Ok(declaration)
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .flat_map(|file| file.servers)
        .find(|server| server.id == request.server_id)
        .ok_or_else(|| format!("MCP server not found: {}", request.server_id))?;
    let credential_ref = match &server.transport {
        McpTransportV1::StreamableHttp {
            bearer_credential_ref: Some(credential_ref),
            ..
        } => credential_ref.clone(),
        McpTransportV1::StreamableHttp {
            bearer_credential_ref: None,
            ..
        } => return Err("MCP server does not require configuration".to_string()),
        McpTransportV1::Stdio { .. } => {
            return Err("Native MCP does not support stdio transport".to_string())
        }
    };

    connect_streamable_http_mcp_server(
        package.name.as_str(),
        server,
        Some(request.bearer_token.as_str()),
    )
    .await
    .map_err(|error| error.to_string())?;

    let _guard = credential_store_guard()?;
    let path = user_data_layout::mcp_credential_file_path();
    let mut credentials = load_credentials(path.as_path())?;
    credentials.set(
        package.name,
        credential_ref,
        request.bearer_token,
        current_timestamp_ms(),
    );
    save_credentials(path.as_path(), &credentials)?;
    catalog_from(descriptors, &credentials)
}

pub(crate) async fn connect_enabled_plugins() -> Result<NativePluginActivation, String> {
    let descriptors = plugins::list(PluginListRequestV1::default())?;
    activation_from_descriptors(descriptors)
}

fn activation_from_descriptors(
    descriptors: Vec<PluginDescriptorV1>,
) -> Result<NativePluginActivation, String> {
    let mut enabled_roots = Vec::new();
    let mut contracts = Vec::<DynamicToolContract>::new();
    let mut declaration_budget = ToolContractBudget::default();
    let mut providers = Vec::new();
    let mut skill_sources = Vec::new();
    let mut cli_directories = Vec::<PathBuf>::new();
    let mut hook_handlers = Vec::<LifecycleHookHandlerV1>::new();
    let mut descriptor_ids = HashSet::new();
    let mut package_names = HashSet::new();

    for descriptor in descriptors.iter().filter(|descriptor| descriptor.enabled) {
        if !descriptor_ids.insert(descriptor.id.as_str()) {
            isolate_plugin(&descriptor.id, "duplicate plugin catalog id");
            continue;
        }
        let root = Path::new(descriptor.path.as_str());
        let staged = (|| {
            require_valid_descriptor(descriptor)?;
            let package = resolve_plugin_package(root)?;
            if package.name != descriptor.id {
                return Err(format!(
                    "plugin descriptor id does not match package name: descriptorId={} packageName={}",
                    descriptor.id, package.name
                ));
            }
            if package_names.contains(package.name.as_str()) {
                return Err(format!("duplicate activated plugin name: {}", package.name));
            }

            let mut staged_contracts = Vec::new();
            let mut staged_providers = Vec::new();
            let mut staged_skill_sources = Vec::new();
            let mut staged_cli_directories = Vec::new();
            let mut staged_hook_handlers = Vec::new();
            let mut staged_budget = declaration_budget.clone();
            for (index, resource) in package.skills.iter().enumerate() {
                let path = root
                    .join(resource.path.as_str())
                    .canonicalize()
                    .map_err(|error| {
                        format!(
                            "canonicalize plugin Skill failed {}: {error}",
                            root.join(resource.path.as_str()).display()
                        )
                    })?;
                staged_skill_sources.push(SkillSourceConfigV1 {
                    source_id: format!("plugin.{}.{index}", package.name),
                    scope: SkillSourceScopeV1::Plugin,
                    kind: if path.is_file() {
                        SkillSourceKindV1::SkillFile
                    } else {
                        SkillSourceKindV1::CatalogDirectory
                    },
                    path: path.to_string_lossy().to_string(),
                    workspace_root: None,
                    enabled: true,
                });
            }
            for resource in &package.cli {
                let path = root
                    .join(resource.path.as_str())
                    .canonicalize()
                    .map_err(|error| {
                        format!(
                            "canonicalize plugin CLI failed {}: {error}",
                            root.join(resource.path.as_str()).display()
                        )
                    })?;
                if !path.is_file() {
                    return Err(format!("plugin CLI is not a file: {}", path.display()));
                }
                let directory = path
                    .parent()
                    .ok_or_else(|| format!("plugin CLI has no parent: {}", path.display()))?
                    .to_path_buf();
                if !staged_cli_directories.contains(&directory) {
                    staged_cli_directories.push(directory);
                }
            }
            if !package.hooks.is_empty() {
                let manifest_path = root.join(".centaeris-plugin/plugin.json");
                let registry = load_plugin_registry_from_manifests(
                    &[manifest_path],
                    &PluginTrustPolicyV1 {
                        trusted_plugins: vec![package.name.clone()],
                    },
                )?;
                staged_hook_handlers.extend(registry.hook_handlers);
            }
            for resource in &package.mcp_servers {
                let declaration =
                    load_mcp_servers_file(root.join(resource.path.as_str()).as_path())?;
                staged_budget.add(&declaration)?;
                for server in declaration.servers {
                    if matches!(&server.transport, McpTransportV1::Stdio { .. }) {
                        return Err(format!(
                            "Native MCP does not support stdio transport: pluginName={} serverId={}",
                            package.name, server.id
                        ));
                    }
                    let binding = lazy_mcp_server_binding(
                        package.name.as_str(),
                        &server,
                        Arc::new(NativeHttpMcpConnector {
                            plugin_name: package.name.clone(),
                            server: server.clone(),
                        }),
                    )?;
                    staged_contracts.extend(binding.contracts);
                    staged_providers.push(binding.provider);
                }
            }
            let mut candidate_contracts = contracts.clone();
            candidate_contracts.extend(staged_contracts.iter().cloned());
            DynamicToolRegistry::from_contracts(candidate_contracts)?;
            Ok((
                package.name,
                staged_budget,
                staged_contracts,
                staged_providers,
                staged_skill_sources,
                staged_cli_directories,
                staged_hook_handlers,
            ))
        })();
        match staged {
            Ok((
                package_name,
                budget,
                new_contracts,
                new_providers,
                new_skill_sources,
                new_cli_directories,
                new_hook_handlers,
            )) => {
                package_names.insert(package_name);
                declaration_budget = budget;
                contracts.extend(new_contracts);
                providers.extend(new_providers);
                skill_sources.extend(new_skill_sources);
                for directory in new_cli_directories {
                    if !cli_directories.contains(&directory) {
                        cli_directories.push(directory);
                    }
                }
                hook_handlers.extend(new_hook_handlers);
                enabled_roots.push(root.to_path_buf());
            }
            Err(error) => isolate_plugin(&descriptor.id, error.as_str()),
        }
    }
    let snapshot = build_plugin_activation_snapshot(enabled_roots.as_slice())?;
    let command_environment = plugin_command_environment(cli_directories.as_slice())?;
    let hook_engine = LifecycleHookEngineV1::new(hook_handlers)?;
    let lifecycle_hooks = QueryLifecycleHookRuntime::new(
        hook_engine,
        Arc::new(
            LocalLifecycleHookCommandRunnerV1::with_environment_overrides(
                command_environment.clone(),
            ),
        ),
        None,
    );

    Ok(NativePluginActivation {
        digest: snapshot.digest,
        dynamic_tool_registry: Arc::new(DynamicToolRegistry::from_contracts(contracts)?),
        providers,
        skill_sources,
        command_environment,
        lifecycle_hooks,
    })
}

fn catalog_from(
    descriptors: Vec<PluginDescriptorV1>,
    credentials: &McpCredentialStoreV1,
) -> Result<NativeMcpCatalogV1, String> {
    let mut servers = Vec::new();
    let mut diagnostics = Vec::new();
    let mut declaration_budget = ToolContractBudget::default();
    let mut ids = HashSet::new();
    for descriptor in descriptors {
        if !ids.insert(descriptor.id.clone()) {
            isolate_plugin(&descriptor.id, "duplicate plugin catalog id");
            push_plugin_diagnostic(
                &mut diagnostics,
                "native_plugin_duplicate",
                &descriptor,
                "duplicate plugin catalog id",
            );
            continue;
        }
        let projected = (|| {
            require_valid_descriptor(&descriptor)?;
            let package = resolve_plugin_package(Path::new(descriptor.path.as_str()))?;
            let mut staged_budget = declaration_budget.clone();
            let mut staged_servers = Vec::new();
            for resource in package.mcp_servers {
                let declaration = load_mcp_servers_file(
                    Path::new(descriptor.path.as_str())
                        .join(resource.path)
                        .as_path(),
                )?;
                staged_budget.add(&declaration)?;
                staged_servers.extend(
                    declaration
                        .servers
                        .into_iter()
                        .map(|server| project_server(&descriptor, server, credentials)),
                );
            }
            Ok::<_, String>((staged_budget, staged_servers))
        })();
        match projected {
            Ok((budget, staged_servers)) => {
                declaration_budget = budget;
                servers.extend(staged_servers);
            }
            Err(error) => {
                isolate_plugin(&descriptor.id, error.as_str());
                push_plugin_diagnostic(
                    &mut diagnostics,
                    "native_plugin_invalid",
                    &descriptor,
                    error.as_str(),
                );
            }
        }
    }
    servers.sort_by(|left, right| {
        left.plugin_name
            .cmp(&right.plugin_name)
            .then(left.server_id.cmp(&right.server_id))
    });
    Ok(NativeMcpCatalogV1 {
        schema: CATALOG_SCHEMA.to_string(),
        servers,
        diagnostics,
    })
}

fn isolate_plugin(plugin_id: &str, error: &str) {
    eprintln!("native_plugin_isolated: pluginId={plugin_id} error={error}");
}

fn push_plugin_diagnostic(
    diagnostics: &mut Vec<NativePluginDiagnosticV1>,
    code: &'static str,
    descriptor: &PluginDescriptorV1,
    message: &str,
) {
    if diagnostics.len() >= MAX_PLUGIN_DIAGNOSTICS {
        return;
    }
    diagnostics.push(NativePluginDiagnosticV1 {
        code,
        plugin_name: descriptor.id.chars().take(128).collect(),
        path: descriptor.path.chars().take(1024).collect(),
        message: message.chars().take(1024).collect(),
    });
}

fn project_server(
    descriptor: &PluginDescriptorV1,
    server: McpServerDeclarationV1,
    credentials: &McpCredentialStoreV1,
) -> NativeMcpServerV1 {
    let (transport, endpoint, credential_ref) = match &server.transport {
        McpTransportV1::Stdio { .. } => (NativeMcpTransportV1::Stdio, None, None),
        McpTransportV1::StreamableHttp {
            url,
            bearer_credential_ref,
        } => (
            NativeMcpTransportV1::StreamableHttp,
            Some(url.clone()),
            bearer_credential_ref.as_deref(),
        ),
    };
    let configurable = credential_ref.is_some();
    let configured = credential_ref
        .map(|value| credentials.get(descriptor.id.as_str(), value).is_some())
        .unwrap_or(true);
    let status = if !descriptor.enabled {
        NativeMcpServerStatusV1::Disabled
    } else if matches!(server.transport, McpTransportV1::Stdio { .. }) {
        NativeMcpServerStatusV1::Unsupported
    } else if configured {
        NativeMcpServerStatusV1::Ready
    } else {
        NativeMcpServerStatusV1::NeedsConfiguration
    };
    NativeMcpServerV1 {
        plugin_name: descriptor.id.clone(),
        plugin_display_name: descriptor.name.clone(),
        server_id: server.id,
        plugin_enabled: descriptor.enabled,
        status,
        configurable,
        configured,
        transport,
        endpoint,
        tool_names: server.tools.into_iter().map(|tool| tool.name).collect(),
    }
}

fn require_valid_descriptor(descriptor: &PluginDescriptorV1) -> Result<(), String> {
    if descriptor.errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "plugin catalog entry is invalid: id={} errors={}",
            descriptor.id,
            descriptor.errors.join("; ")
        ))
    }
}

fn plugin_command_environment(
    cli_directories: &[PathBuf],
) -> Result<HashMap<String, String>, String> {
    if cli_directories.is_empty() {
        return Ok(HashMap::new());
    }
    let mut entries = cli_directories.to_vec();
    if let Some(existing) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(existing.as_os_str()));
    }
    let path = std::env::join_paths(entries.iter())
        .map_err(|error| format!("compose plugin CLI PATH failed: {error}"))?
        .into_string()
        .map_err(|_| "plugin CLI PATH must be UTF-8".to_string())?;
    Ok(HashMap::from([("PATH".to_string(), path)]))
}

impl McpCredentialStoreV1 {
    fn get(&self, plugin_name: &str, credential_ref: &str) -> Option<&str> {
        self.credentials
            .iter()
            .find(|credential| {
                credential.plugin_name == plugin_name && credential.credential_ref == credential_ref
            })
            .map(|credential| credential.bearer_token.as_str())
    }

    fn set(
        &mut self,
        plugin_name: String,
        credential_ref: String,
        bearer_token: String,
        updated_at_ms: i64,
    ) {
        if let Some(existing) = self.credentials.iter_mut().find(|credential| {
            credential.plugin_name == plugin_name && credential.credential_ref == credential_ref
        }) {
            existing.bearer_token = bearer_token;
            existing.updated_at_ms = updated_at_ms;
        } else {
            self.credentials.push(McpBearerCredentialV1 {
                plugin_name,
                credential_ref,
                bearer_token,
                updated_at_ms,
            });
        }
        self.credentials.sort_by(|left, right| {
            left.plugin_name
                .cmp(&right.plugin_name)
                .then(left.credential_ref.cmp(&right.credential_ref))
        });
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != CREDENTIALS_SCHEMA {
            return Err("Native MCP credential schema mismatch".to_string());
        }
        let mut keys = HashSet::new();
        for credential in &self.credentials {
            validate_lower_kebab("MCP credential pluginName", credential.plugin_name.as_str())?;
            validate_lower_kebab(
                "MCP credential credentialRef",
                credential.credential_ref.as_str(),
            )?;
            if !valid_bearer_token(credential.bearer_token.as_str()) {
                return Err("Native MCP bearer token is invalid".to_string());
            }
            if credential.updated_at_ms < 0 {
                return Err("Native MCP credential updatedAtMs is invalid".to_string());
            }
            if !keys.insert((
                credential.plugin_name.as_str(),
                credential.credential_ref.as_str(),
            )) {
                return Err("duplicate Native MCP credential binding".to_string());
            }
        }
        Ok(())
    }
}

fn load_credentials(path: &Path) -> Result<McpCredentialStoreV1, String> {
    if !path.exists() {
        return Ok(McpCredentialStoreV1 {
            schema: CREDENTIALS_SCHEMA.to_string(),
            credentials: Vec::new(),
        });
    }
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("read Native MCP credentials failed: {error}"))?;
    let credentials = serde_json::from_str::<McpCredentialStoreV1>(raw.as_str())
        .map_err(|error| format!("parse Native MCP credentials failed: {error}"))?;
    credentials.validate()?;
    Ok(credentials)
}

fn save_credentials(path: &Path, credentials: &McpCredentialStoreV1) -> Result<(), String> {
    credentials.validate()?;
    let mut encoded = serde_json::to_vec_pretty(credentials)
        .map_err(|error| format!("serialize Native MCP credentials failed: {error}"))?;
    encoded.push(b'\n');
    atomic_file::write_file_atomically(path, encoded.as_slice(), "Native MCP credentials")
}

fn credential_store_guard() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    MCP_CREDENTIAL_STORE_LOCK
        .lock()
        .map_err(|_| "Native MCP credential store lock poisoned".to_string())
}

fn validate_lower_kebab(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('-')
        || value.ends_with('-')
        || value.contains("--")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!("{label} must be canonical lower-kebab-case"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::extension::{
        mcp_model_contract_digest, McpLifecycleV1, McpServersFileV1, McpToolDeclarationV1,
        MCP_SERVERS_SCHEMA_V1,
    };
    use serde_json::json;

    fn test_descriptor(root: &Path) -> PluginDescriptorV1 {
        PluginDescriptorV1 {
            id: "demo-plugin".to_string(),
            name: "Demo Plugin".to_string(),
            description: String::new(),
            source: "local".to_string(),
            enabled: true,
            path: root.to_string_lossy().to_string(),
            manifest_path: Some(
                root.join(".centaeris-plugin/plugin.json")
                    .to_string_lossy()
                    .to_string(),
            ),
            errors: Vec::new(),
            version: Some("1.0.0".to_string()),
            tools: Vec::new(),
            scopes: Vec::new(),
            activation_status: "enabled".to_string(),
            policy_source: "user".to_string(),
        }
    }

    #[test]
    fn catalog_projects_declared_status_without_connecting() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-native-mcp-catalog-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        fs::create_dir_all(root.join(".centaeris-plugin")).expect("plugin manifest dir");
        fs::create_dir_all(root.join("mcp")).expect("plugin MCP dir");
        fs::write(
            root.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"demo-plugin","version":"1.0.0","paths":{"mcpServers":["mcp/server.json"]},"interface":{"displayName":"Demo Plugin"}}"#,
        )
        .expect("plugin manifest");
        let tools = vec![McpToolDeclarationV1 {
            source_name: "search".to_string(),
            name: "demo_search".to_string(),
            description: "Search demos.".to_string(),
            input_schema: json!({"type": "object"}),
            concurrency_safe: true,
            scopes: Vec::new(),
        }];
        fs::write(
            root.join("mcp/server.json"),
            serde_json::to_vec_pretty(&McpServersFileV1 {
                schema: MCP_SERVERS_SCHEMA_V1.to_string(),
                servers: vec![McpServerDeclarationV1 {
                    id: "demo-server".to_string(),
                    model_contract_digest: mcp_model_contract_digest(
                        "demo-server",
                        tools.as_slice(),
                    )
                    .expect("model contract digest"),
                    transport: McpTransportV1::StreamableHttp {
                        url: "https://example.com/mcp".to_string(),
                        bearer_credential_ref: Some("demo-token".to_string()),
                    },
                    lifecycle: McpLifecycleV1::Auto,
                    startup_timeout_ms: 1_000,
                    tool_timeout_ms: 1_000,
                    tools,
                }],
            })
            .expect("MCP declaration"),
        )
        .expect("MCP declaration");
        let descriptor = test_descriptor(root.as_path());
        let mut credentials = McpCredentialStoreV1 {
            schema: CREDENTIALS_SCHEMA.to_string(),
            credentials: Vec::new(),
        };
        let missing = serde_json::to_value(
            catalog_from(vec![descriptor.clone()], &credentials).expect("missing catalog"),
        )
        .expect("serialize missing catalog");
        assert_eq!(missing["servers"][0]["status"], "needsConfiguration");
        credentials.set(
            "demo-plugin".to_string(),
            "demo-token".to_string(),
            "secret".to_string(),
            1,
        );
        let ready = serde_json::to_value(
            catalog_from(vec![descriptor], &credentials).expect("ready catalog"),
        )
        .expect("serialize ready catalog");
        assert_eq!(ready["servers"][0]["status"], "ready");
        fs::remove_dir_all(root).expect("remove temp root");
    }

    #[test]
    fn activation_exposes_cli_path_and_trusted_plugin_hooks() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-native-plugin-cli-hooks-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        fs::create_dir_all(root.join(".centaeris-plugin")).expect("plugin manifest dir");
        fs::create_dir_all(root.join("bin")).expect("plugin CLI dir");
        fs::create_dir_all(root.join("hooks")).expect("plugin hooks dir");
        fs::write(
            root.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"demo-plugin","version":"1.0.0","paths":{"cli":["bin/demo-cli"],"hooks":["hooks/hooks.json"]},"interface":{"displayName":"Demo Plugin"}}"#,
        )
        .expect("plugin manifest");
        fs::write(root.join("bin/demo-cli"), "demo").expect("plugin CLI");
        fs::write(
            root.join("hooks/hooks.json"),
            r#"{"schema":"plugin_hooks_v1","handlers":[{"id":"check","event":"UserPromptSubmit","program":"demo-cli"}]}"#,
        )
        .expect("plugin hooks");

        let activation = activation_from_descriptors(vec![test_descriptor(root.as_path())])
            .expect("activate CLI and hooks");
        let path = activation
            .command_environment
            .get("PATH")
            .expect("plugin PATH");
        let first = std::env::split_paths(std::ffi::OsStr::new(path))
            .next()
            .expect("first PATH entry");
        assert_eq!(
            first,
            root.join("bin").canonicalize().expect("CLI directory")
        );
        assert!(activation.lifecycle_hooks.has_handlers());
        assert!(!activation.digest.is_empty());
        fs::remove_dir_all(root).expect("remove temp root");
    }

    #[test]
    fn credential_store_round_trips_and_rejects_unknown_fields() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-native-mcp-credentials-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        fs::create_dir_all(&root).expect("temp root");
        let path = root.join("credentials.json");
        let mut credentials = load_credentials(&path).expect("empty credentials");
        credentials.set(
            "demo-plugin".to_string(),
            "demo-token".to_string(),
            "secret".to_string(),
            1,
        );
        save_credentials(&path, &credentials).expect("save credentials");
        assert_eq!(
            load_credentials(&path)
                .expect("load credentials")
                .get("demo-plugin", "demo-token"),
            Some("secret")
        );
        fs::write(
            &path,
            r#"{"schema":"native.mcp.credentials.v1","credentials":[],"banana":true}"#,
        )
        .expect("write invalid credentials");
        assert!(load_credentials(&path).is_err());
        fs::remove_dir_all(root).expect("remove temp root");
    }

    #[test]
    fn catalog_isolates_invalid_plugin_without_hiding_healthy_plugin() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-native-mcp-isolation-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        fs::create_dir_all(root.join(".centaeris-plugin")).expect("plugin manifest dir");
        fs::create_dir_all(root.join("mcp")).expect("plugin MCP dir");
        fs::write(
            root.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"demo-plugin","version":"1.0.0","paths":{"mcpServers":["mcp/server.json"]},"interface":{"displayName":"Demo Plugin"}}"#,
        )
        .expect("plugin manifest");
        let tools = vec![McpToolDeclarationV1 {
            source_name: "search".to_string(),
            name: "demo_search".to_string(),
            description: "Search demos.".to_string(),
            input_schema: json!({"type": "object"}),
            concurrency_safe: true,
            scopes: Vec::new(),
        }];
        fs::write(
            root.join("mcp/server.json"),
            serde_json::to_vec_pretty(&McpServersFileV1 {
                schema: MCP_SERVERS_SCHEMA_V1.to_string(),
                servers: vec![McpServerDeclarationV1 {
                    id: "demo-server".to_string(),
                    model_contract_digest: mcp_model_contract_digest(
                        "demo-server",
                        tools.as_slice(),
                    )
                    .expect("model contract digest"),
                    transport: McpTransportV1::StreamableHttp {
                        url: "https://example.com/mcp".to_string(),
                        bearer_credential_ref: None,
                    },
                    lifecycle: McpLifecycleV1::Auto,
                    startup_timeout_ms: 1_000,
                    tool_timeout_ms: 1_000,
                    tools,
                }],
            })
            .expect("MCP declaration"),
        )
        .expect("MCP declaration");
        let healthy = test_descriptor(root.as_path());
        let mut invalid = healthy.clone();
        invalid.id = "broken-plugin".to_string();
        invalid.errors = vec!["future manifest field".to_string()];

        let catalog = catalog_from(
            vec![invalid, healthy],
            &McpCredentialStoreV1 {
                schema: CREDENTIALS_SCHEMA.to_string(),
                credentials: Vec::new(),
            },
        )
        .expect("healthy plugin remains available");
        assert_eq!(catalog.servers.len(), 1);
        assert_eq!(catalog.servers[0].plugin_name, "demo-plugin");
        assert_eq!(catalog.diagnostics.len(), 1);
        assert_eq!(catalog.diagnostics[0].code, "native_plugin_invalid");
        assert_eq!(catalog.diagnostics[0].plugin_name, "broken-plugin");
        fs::remove_dir_all(root).expect("remove temp root");
    }
}
