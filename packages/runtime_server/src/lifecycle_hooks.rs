use std::path::Path;
use std::sync::Arc;

use centaeris_core::extension::hooks::{
    LifecycleHookAuditSink, LifecycleHookEngineV1, LifecycleHookEventNameV1,
    LifecycleHookRunStatusV1, LifecycleHookRunV1, LifecycleHookSourceKindV1,
};
use centaeris_core::extension::{
    load_plugin_registry_from_manifests, PluginActivationSnapshotV1, PluginRegistryV1,
    PluginTrustPolicyV1,
};
use centaeris_core::runtime::contracts::{EventVisibility, RuntimeEvent};
use centaeris_core::runtime::QueryLifecycleHookRuntime;
use centaeris_core::session::store::{RuntimeStore, RuntimeStoreActor};
use serde::Serialize;

use crate::docker_execution_host::DockerExecutionHostRunner;
use crate::skill_projection::{verified_plugin_package_roots_at, PLUGIN_CATALOG_ROOT};

const WORKSPACE_HOOK_CATALOG_RESULT_SCHEMA: &str = "workspace.hook.catalog.result.v1";

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceHookCatalogResult {
    schema: String,
    plugins: Vec<WorkspacePluginHooks>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkspacePluginHooks {
    plugin_name: String,
    hooks: Vec<WorkspaceHookSummary>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct WorkspaceHookSummary {
    id: String,
    event: LifecycleHookEventNameV1,
    matcher: Option<String>,
    timeout_ms: u64,
}

struct WorkspaceHookAuditSink {
    store: Arc<RuntimeStoreActor>,
    session_id: String,
    agent_run_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceHookRunAudit<'a> {
    hook_run_id: &'a str,
    plugin: &'a str,
    handler: &'a str,
    event: LifecycleHookEventNameV1,
    status: &'a LifecycleHookRunStatusV1,
    duration_ms: u128,
    exit_code: Option<i32>,
    diagnostic: Option<&'a str>,
}

impl LifecycleHookAuditSink for WorkspaceHookAuditSink {
    fn record_hook_runs(&self, runs: &[LifecycleHookRunV1]) -> Result<(), String> {
        for run in runs {
            RuntimeStore::append_event_idempotent(
                self.store.as_ref(),
                workspace_hook_run_event(
                    self.session_id.as_str(),
                    self.agent_run_id.as_str(),
                    run,
                )?,
            )
            .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

pub(crate) fn workspace_lifecycle_hook_runtime(
    activation: &PluginActivationSnapshotV1,
    docker: Arc<DockerExecutionHostRunner>,
    store: Arc<RuntimeStoreActor>,
    session_id: String,
    agent_run_id: String,
) -> Result<QueryLifecycleHookRuntime, String> {
    let registry = workspace_plugin_registry_at(activation, Path::new(PLUGIN_CATALOG_ROOT))?;
    Ok(QueryLifecycleHookRuntime::new(
        LifecycleHookEngineV1::new(registry.hook_handlers)?,
        docker,
        Some(Arc::new(WorkspaceHookAuditSink {
            store,
            session_id,
            agent_run_id,
        })),
    ))
}

pub(crate) fn workspace_hook_catalog(
    activation: &PluginActivationSnapshotV1,
) -> Result<WorkspaceHookCatalogResult, String> {
    workspace_hook_catalog_at(activation, Path::new(PLUGIN_CATALOG_ROOT))
}

fn workspace_hook_catalog_at(
    activation: &PluginActivationSnapshotV1,
    catalog_root: &Path,
) -> Result<WorkspaceHookCatalogResult, String> {
    let registry = workspace_plugin_registry_at(activation, catalog_root)?;
    let mut plugins = Vec::with_capacity(activation.packages.len());
    for package in &activation.packages {
        let handler_prefix = format!("{}:", package.name);
        let hooks = registry
            .hook_handlers
            .iter()
            .filter(|handler| handler.source.name == package.name)
            .map(|handler| {
                let id = handler
                    .id
                    .strip_prefix(handler_prefix.as_str())
                    .ok_or_else(|| "workspace Hook handler identity mismatch".to_string())?;
                Ok(WorkspaceHookSummary {
                    id: id.to_string(),
                    event: handler.event,
                    matcher: handler.matcher.clone(),
                    timeout_ms: handler.timeout_ms,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        plugins.push(WorkspacePluginHooks {
            plugin_name: package.name.clone(),
            hooks,
        });
    }
    Ok(WorkspaceHookCatalogResult {
        schema: WORKSPACE_HOOK_CATALOG_RESULT_SCHEMA.to_string(),
        plugins,
    })
}

fn workspace_plugin_registry_at(
    activation: &PluginActivationSnapshotV1,
    catalog_root: &Path,
) -> Result<PluginRegistryV1, String> {
    let package_roots = verified_plugin_package_roots_at(activation, catalog_root)?;
    let manifest_paths = package_roots
        .iter()
        .map(|root| root.join(".centaeris-plugin/plugin.json"))
        .collect::<Vec<_>>();
    load_plugin_registry_from_manifests(
        manifest_paths.as_slice(),
        &PluginTrustPolicyV1 {
            trusted_plugins: activation
                .packages
                .iter()
                .map(|package| package.name.clone())
                .collect(),
        },
    )
}

fn workspace_hook_run_event(
    session_id: &str,
    agent_run_id: &str,
    run: &LifecycleHookRunV1,
) -> Result<RuntimeEvent, String> {
    if run.source.kind != LifecycleHookSourceKindV1::Plugin {
        return Err("Workspace Hook audit source must be Plugin".to_string());
    }
    let handler = run
        .handler_id
        .strip_prefix(format!("{}:", run.source.name).as_str())
        .ok_or_else(|| "Workspace Hook audit handler identity mismatch".to_string())?;
    let payload = WorkspaceHookRunAudit {
        hook_run_id: run.hook_run_id.as_str(),
        plugin: run.source.name.as_str(),
        handler,
        event: run.event,
        status: &run.status,
        duration_ms: run.completed_at_ms.saturating_sub(run.started_at_ms),
        exit_code: run.exit_code,
        diagnostic: run.diagnostic.as_deref(),
    };
    Ok(RuntimeEvent {
        event_id: format!("lifecycle_hook_run:{agent_run_id}:{}", run.hook_run_id),
        session_id: session_id.to_string(),
        task_id: Some(agent_run_id.to_string()),
        event_type: "lifecycle_hook_run_v1".to_string(),
        at_ms: i64::try_from(run.completed_at_ms).unwrap_or(i64::MAX),
        visibility: EventVisibility::Internal,
        payload_json: serde_json::to_string(&payload)
            .map_err(|error| format!("serialize Workspace Hook audit failed: {error}"))?,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use centaeris_core::extension::build_plugin_activation_snapshot;
    use serde_json::json;

    use super::*;

    #[test]
    fn hook_catalog_projects_only_safe_static_fields() {
        let catalog_root = std::env::temp_dir().join(format!(
            "centaeris-workspace-hook-catalog-{}-{}",
            std::process::id(),
            centaeris_core::runtime::contracts::current_timestamp_ms()
        ));
        let package_root = catalog_root.join("wiki");
        fs::create_dir_all(package_root.join(".centaeris-plugin")).unwrap();
        fs::create_dir_all(package_root.join("hooks")).unwrap();
        fs::write(
            package_root.join(".centaeris-plugin/plugin.json"),
            r#"{"name":"wiki","version":"1.0.0","paths":{"hooks":["hooks/hooks.json"]}}"#,
        )
        .unwrap();
        fs::write(
            package_root.join("hooks/hooks.json"),
            r#"{"schema":"plugin_hooks_v1","handlers":[{"id":"guard_write","event":"PreToolUse","matcher":"write","program":"node","args":["hooks/guard-write.mjs"],"timeoutMs":5000}]}"#,
        )
        .unwrap();
        fs::write(package_root.join("hooks/guard-write.mjs"), "").unwrap();
        let activation =
            build_plugin_activation_snapshot(std::slice::from_ref(&package_root)).unwrap();

        let catalog = workspace_hook_catalog_at(&activation, catalog_root.as_path()).unwrap();

        assert_eq!(
            serde_json::to_value(catalog).unwrap(),
            json!({
                "schema": "workspace.hook.catalog.result.v1",
                "plugins": [{
                    "pluginName": "wiki",
                    "hooks": [{
                        "id": "guard_write",
                        "event": "PreToolUse",
                        "matcher": "write",
                        "timeoutMs": 5000
                    }]
                }]
            })
        );
        let _ = fs::remove_dir_all(catalog_root);
    }

    #[test]
    fn hook_audit_event_contains_no_model_or_process_payload() {
        let event = workspace_hook_run_event(
            "session_1",
            "agent_run_1",
            &LifecycleHookRunV1 {
                hook_run_id: "hook_run_1".to_string(),
                handler_id: "wiki:guard_write".to_string(),
                event: LifecycleHookEventNameV1::PreToolUse,
                source: centaeris_core::extension::hooks::LifecycleHookSourceV1 {
                    kind: LifecycleHookSourceKindV1::Plugin,
                    name: "wiki".to_string(),
                },
                status: LifecycleHookRunStatusV1::Succeeded,
                started_at_ms: 10,
                completed_at_ms: 15,
                exit_code: Some(0),
                diagnostic: Some("ok".to_string()),
            },
        )
        .unwrap();

        assert_eq!(event.visibility, EventVisibility::Internal);
        assert_eq!(event.task_id.as_deref(), Some("agent_run_1"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(event.payload_json.as_str()).unwrap(),
            json!({
                "hookRunId": "hook_run_1",
                "plugin": "wiki",
                "handler": "guard_write",
                "event": "PreToolUse",
                "status": "Succeeded",
                "durationMs": 5,
                "exitCode": 0,
                "diagnostic": "ok"
            })
        );
        assert!(!event.payload_json.contains("toolInput"));
        assert!(!event.payload_json.contains("stdout"));
        assert!(!event.payload_json.contains("path"));
    }
}
