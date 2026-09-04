use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::execution::sandbox::{FileSystemSandboxPolicy, NetworkSandboxPolicy, SandboxPolicy};
use crate::execution::{
    classify_execution_host_failure, ExecutionCancellationProbe, ExecutionFileSystemErrorKind,
    ExecutionFileSystemOperation, ExecutionFileSystemOutput, ExecutionHostBinding,
    ExecutionHostFailureKind, ExecutionHostKind, ExecutionHostMode,
};
use crate::extension::skills::{SkillCatalogLoadConfig, SkillIndex};
use crate::model::prepared_prompt::{
    inspect_model_input_image, ExecutionModelInputImageRefV1, MODEL_INPUT_IMAGE_MAX_BYTES,
};
use crate::session::external_context::ExternalContextStorePort;
use crate::session::reliability::ResourceClaimStorePort;
use crate::tool::inputs::{ResolvedInput, ResolvedInputState};
use crate::tool::{
    canonicalize_tool_name, is_tool_concurrency_safe, list_tool_contracts, DynamicToolRegistry,
    ToolContract, ToolErrorInfo, ToolFailureKind, ToolTurnBehavior,
};

mod edit;
mod facts;
mod local_handlers;
mod mutation;
mod outcome;
mod providers;
mod read;
mod result_capture;
mod result_state;
mod write;

use edit::EditToolHandler;
pub use facts::ToolExecutionFact;
use local_handlers::BashToolHandler;
use providers::wrap_dynamic_tool_output;
pub use providers::{
    extract_dynamic_tool_pending_poll, DynamicToolPendingPoll, DynamicToolPollingSpec,
    DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse,
};
use read::ReadToolHandler;
pub(crate) use result_capture::{tool_result_capture, MODEL_TOOL_RESULT_MAX_BYTES};
pub use result_state::ToolResultState;
use write::WriteToolHandler;

#[derive(Debug, Clone)]
pub struct ToolInvocationRequest {
    pub tool_call_id: String,
    pub tool_name: String,
    pub args_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileMutationCommitRequest {
    pub schema: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub operation: String,
    pub path: String,
    pub target_path: Option<String>,
    pub previous_file_hash: Option<String>,
    pub read_snapshot_hash: Option<String>,
    pub file_hash: Option<String>,
    pub bytes_written: Option<usize>,
    pub added_lines: Option<usize>,
    pub removed_lines: Option<usize>,
    pub session_id: Option<String>,
    pub execution_owner: String,
}

pub trait FileMutationCommitPort {
    fn commit_file_mutation(&self, request: FileMutationCommitRequest) -> Result<(), String>;
}

#[derive(Debug, Clone)]
pub struct ResolvedInputReadRequest {
    pub inputs: Vec<ResolvedInput>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    pub poll_args: Value,
    pub tool_call_id: String,
}

pub trait ResolvedInputReaderPort {
    fn read(&self, request: ResolvedInputReadRequest) -> Result<LocalToolOutput, String>;
}

#[derive(Clone)]
pub struct ToolRuntimeContext {
    execution_host_binding: Option<Arc<ExecutionHostBinding>>,
    execution_cancellation_probe: Option<Arc<ExecutionCancellationProbe>>,
    pub session_id: Option<String>,
    pub execution_owner: Option<String>,
    pub current_tool_call_id: Option<String>,
    pub current_tool_name: Option<String>,
    file_read_snapshots: Arc<Mutex<HashMap<String, String>>>,
    pub resource_claim_store: Option<Arc<dyn ResourceClaimStorePort + Send + Sync>>,
    pub resource_claim_ttl_ms: u64,
    pub file_mutation_commit_port: Option<Arc<dyn FileMutationCommitPort + Send + Sync>>,
    pub resolved_input_reader: Option<Arc<dyn ResolvedInputReaderPort + Send + Sync>>,
    pub external_context_store: Option<Arc<dyn ExternalContextStorePort + Send + Sync>>,
    pub sandbox_policy: SandboxPolicy,
    pub resolved_input_manifest: Option<Arc<ResolvedInputState>>,
    pub resolved_input_root: Option<PathBuf>,
}

impl Default for ToolRuntimeContext {
    fn default() -> Self {
        Self {
            execution_host_binding: None,
            execution_cancellation_probe: None,
            session_id: None,
            execution_owner: None,
            current_tool_call_id: None,
            current_tool_name: None,
            file_read_snapshots: Arc::new(Mutex::new(HashMap::new())),
            resource_claim_store: None,
            resource_claim_ttl_ms: DEFAULT_RESOURCE_CLAIM_TTL_MS,
            file_mutation_commit_port: None,
            resolved_input_reader: None,
            external_context_store: None,
            sandbox_policy: SandboxPolicy::workspace_write_no_network("/workspace"),
            resolved_input_manifest: None,
            resolved_input_root: None,
        }
    }
}

impl std::fmt::Debug for ToolRuntimeContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRuntimeContext")
            .field(
                "cwd",
                &self
                    .execution_host_binding
                    .as_ref()
                    .map(|binding| binding.cwd()),
            )
            .field(
                "execution_host_binding_configured",
                &self.execution_host_binding.is_some(),
            )
            .field(
                "execution_cancellation_probe_configured",
                &self.execution_cancellation_probe.is_some(),
            )
            .field("session_id", &self.session_id)
            .field("execution_owner", &self.execution_owner)
            .field("current_tool_call_id", &self.current_tool_call_id)
            .field("current_tool_name", &self.current_tool_name)
            .field(
                "resource_claim_store_configured",
                &self.resource_claim_store.is_some(),
            )
            .field("resource_claim_ttl_ms", &self.resource_claim_ttl_ms)
            .field(
                "file_mutation_commit_port_configured",
                &self.file_mutation_commit_port.is_some(),
            )
            .field(
                "resolved_input_reader_configured",
                &self.resolved_input_reader.is_some(),
            )
            .field(
                "external_context_store_configured",
                &self.external_context_store.is_some(),
            )
            .field("sandbox_policy", &self.sandbox_policy)
            .field(
                "resolved_input_manifest_configured",
                &self.resolved_input_manifest.is_some(),
            )
            .field("resolved_input_root", &self.resolved_input_root)
            .finish()
    }
}

impl ToolRuntimeContext {
    pub fn with_cwd(cwd: PathBuf) -> Result<Self, String> {
        Self::default().replace_cwd(cwd)
    }

    pub fn with_execution_host_binding(
        mut self,
        execution_host_binding: Arc<ExecutionHostBinding>,
    ) -> Self {
        self.sandbox_policy = execution_host_binding.policy().clone();
        self.execution_host_binding = Some(execution_host_binding);
        self
    }

    fn replace_cwd(mut self, cwd: PathBuf) -> Result<Self, String> {
        let binding = match self.execution_host_binding.as_ref() {
            Some(binding) => binding.with_cwd(cwd, self.sandbox_policy.clone())?,
            None => {
                #[cfg(test)]
                {
                    ExecutionHostBinding::new_test_local(cwd, self.sandbox_policy.clone())?
                }
                #[cfg(not(test))]
                {
                    return Err("execution host binding is required before setting cwd".to_string());
                }
            }
        };
        let canonical = binding.cwd().to_path_buf();
        let previous_root = self.sandbox_policy.filesystem.workspace_root.clone();
        remap_sandbox_policy_root(
            &mut self.sandbox_policy.filesystem,
            previous_root.as_path(),
            canonical.as_path(),
        );
        self.file_read_snapshots = Arc::new(Mutex::new(HashMap::new()));
        let policy = self.execution_host_policy(binding.mode());
        self.execution_host_binding = Some(Arc::new(binding.with_policy(policy)));
        Ok(self)
    }

    fn execution_host_policy(&self, mode: ExecutionHostMode) -> SandboxPolicy {
        let mut policy = self.sandbox_policy.clone();
        result_capture::expose_local_capture_root(&mut policy, mode, self.session_id.as_deref());
        policy
    }

    fn refresh_execution_host_policy(&mut self) {
        let Some(mode) = self
            .execution_host_binding
            .as_ref()
            .map(|binding| binding.mode())
        else {
            return;
        };
        let policy = self.execution_host_policy(mode);
        if let Some(binding) = self.execution_host_binding.as_ref() {
            self.execution_host_binding = Some(Arc::new(binding.with_policy(policy)));
        }
    }

    pub(super) fn execution_host_binding(&self) -> Result<Arc<ExecutionHostBinding>, String> {
        self.execution_host_binding.clone().ok_or_else(|| {
            "execution host binding is required for filesystem and command tools".to_string()
        })
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        let session_id = session_id.into();
        if !session_id.trim().is_empty() {
            self.session_id = Some(session_id);
            self.refresh_execution_host_policy();
        }
        self
    }

    pub fn with_execution_owner(mut self, execution_owner: impl Into<String>) -> Self {
        let owner = execution_owner.into();
        self.execution_owner = Some(owner);
        self
    }

    pub fn with_execution_cancellation_probe(
        mut self,
        cancellation_probe: Arc<ExecutionCancellationProbe>,
    ) -> Self {
        self.execution_cancellation_probe = Some(cancellation_probe.clone());
        if let Some(binding) = self.execution_host_binding.as_ref() {
            self.execution_host_binding = Some(Arc::new(
                binding
                    .as_ref()
                    .clone()
                    .with_cancellation_probe(Some(cancellation_probe)),
            ));
        }
        self
    }

    pub fn with_tool_invocation(
        mut self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        self.current_tool_call_id = Some(tool_call_id.into());
        self.current_tool_name = Some(tool_name.into());
        if let Some(binding) = self.execution_host_binding.as_ref() {
            self.execution_host_binding = Some(Arc::new(
                binding.with_operation_scope(self.current_tool_call_id.clone()),
            ));
        }
        self
    }

    pub fn write_lease_owner(&self) -> &str {
        self.execution_owner
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("anonymous-tool-runtime")
    }

    pub(super) fn record_file_read_snapshot(
        &self,
        path_identity: &str,
        file_hash: impl Into<String>,
    ) -> Result<(), String> {
        self.file_read_snapshots
            .lock()
            .map_err(|_| "file read snapshot registry lock poisoned".to_string())?
            .insert(path_identity.to_string(), file_hash.into());
        Ok(())
    }

    pub(super) fn require_file_read_snapshot(
        &self,
        path_identity: &str,
        path_label: &str,
    ) -> Result<String, String> {
        self.file_read_snapshots
            .lock()
            .map_err(|_| "file read snapshot registry lock poisoned".to_string())?
            .get(path_identity)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "file mutation rejected: read snapshot is required before modifying existing file: {path_label}"
                )
            })
    }

    pub fn with_resource_claim_store(
        mut self,
        store: Arc<dyn ResourceClaimStorePort + Send + Sync>,
    ) -> Self {
        self.resource_claim_store = Some(store);
        if self.resource_claim_ttl_ms == 0 {
            self.resource_claim_ttl_ms = DEFAULT_RESOURCE_CLAIM_TTL_MS;
        }
        self
    }

    pub fn with_file_mutation_commit_port(
        mut self,
        port: Arc<dyn FileMutationCommitPort + Send + Sync>,
    ) -> Self {
        self.file_mutation_commit_port = Some(port);
        self
    }

    pub fn with_resolved_input_reader(
        mut self,
        port: Arc<dyn ResolvedInputReaderPort + Send + Sync>,
    ) -> Self {
        self.resolved_input_reader = Some(port);
        self
    }

    pub fn with_external_context_store(
        mut self,
        store: Arc<dyn ExternalContextStorePort + Send + Sync>,
    ) -> Self {
        self.external_context_store = Some(store);
        self
    }

    pub fn with_workspace_write_allowed(mut self, allowed: bool) -> Self {
        let cwd = self.sandbox_policy.filesystem.workspace_root.clone();
        self.sandbox_policy
            .filesystem
            .writable_roots
            .retain(|root| root != &cwd);
        if allowed {
            self.sandbox_policy.filesystem.writable_roots.push(cwd);
        }
        self.refresh_execution_host_policy();
        self
    }

    pub fn with_network_policy(mut self, network_policy: NetworkSandboxPolicy) -> Self {
        self.sandbox_policy.network = network_policy;
        self.refresh_execution_host_policy();
        self
    }

    pub fn with_sandbox_filesystem_paths(
        mut self,
        additional_writable_roots: Vec<PathBuf>,
        denied_read_paths: Vec<PathBuf>,
    ) -> Self {
        for root in additional_writable_roots {
            if !self
                .sandbox_policy
                .filesystem
                .writable_roots
                .contains(&root)
            {
                self.sandbox_policy.filesystem.writable_roots.push(root);
            }
        }
        for path in denied_read_paths {
            if !self
                .sandbox_policy
                .filesystem
                .denied_read_paths
                .contains(&path)
            {
                self.sandbox_policy
                    .filesystem
                    .denied_read_paths
                    .push(path.clone());
            }
            if !self
                .sandbox_policy
                .filesystem
                .denied_write_paths
                .contains(&path)
            {
                self.sandbox_policy.filesystem.denied_write_paths.push(path);
            }
        }
        self.refresh_execution_host_policy();
        self
    }

    pub fn with_resolved_input_manifest(mut self, manifest: Arc<ResolvedInputState>) -> Self {
        self.resolved_input_manifest = Some(manifest);
        self
    }

    pub fn with_resolved_input_root(mut self, root: PathBuf) -> Result<Self, String> {
        let canonical = root
            .canonicalize()
            .map_err(|error| format!("canonicalize resolved input root failed: {error}"))?;
        if !canonical.is_dir() {
            return Err("resolved input root is not a directory".to_string());
        }
        self.resolved_input_root = Some(canonical);
        Ok(self)
    }

    pub fn resource_claim_store(&self) -> Option<Arc<dyn ResourceClaimStorePort + Send + Sync>> {
        self.resource_claim_store.clone()
    }

    pub fn sandbox_policy(&self) -> &SandboxPolicy {
        &self.sandbox_policy
    }

    pub fn resource_claim_ttl_ms(&self) -> u64 {
        self.resource_claim_ttl_ms
            .max(DEFAULT_RESOURCE_CLAIM_TTL_MS)
    }

    pub fn commit_file_mutation(
        &self,
        mut request: FileMutationCommitRequest,
    ) -> Result<(), String> {
        let Some(port) = self.file_mutation_commit_port.as_ref() else {
            return Err(
                "FileMutationCommitPort is required before Write/Edit can apply filesystem mutation"
                    .to_string(),
            );
        };
        request.session_id = request.session_id.or_else(|| self.session_id.clone());
        if request.execution_owner.trim().is_empty() {
            request.execution_owner = self.write_lease_owner().to_string();
        }
        port.commit_file_mutation(request)
    }

    pub fn current_tool_call_id(&self) -> Result<String, String> {
        self.current_tool_call_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| {
                "ToolRuntimeContext.currentToolCallId is required for file mutation commit"
                    .to_string()
            })
    }

    pub fn current_tool_name(&self) -> Result<String, String> {
        self.current_tool_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| {
                "ToolRuntimeContext.currentToolName is required for file mutation commit"
                    .to_string()
            })
    }
}

fn remap_sandbox_policy_root(
    policy: &mut FileSystemSandboxPolicy,
    previous_root: &Path,
    cwd: &Path,
) {
    fn remap(path: &Path, previous_root: &Path, cwd: &Path) -> PathBuf {
        path.strip_prefix(previous_root)
            .map(|relative| cwd.join(relative))
            .unwrap_or_else(|_| path.to_path_buf())
    }

    policy.workspace_root = cwd.to_path_buf();
    for path in policy
        .read_only_roots
        .iter_mut()
        .chain(policy.writable_roots.iter_mut())
        .chain(policy.denied_read_paths.iter_mut())
        .chain(policy.denied_write_paths.iter_mut())
    {
        *path = remap(path.as_path(), previous_root, cwd);
    }
}

const DEFAULT_RESOURCE_CLAIM_TTL_MS: u64 = 5 * 60 * 1000;
pub const EXECUTION_CANCELLATION_INDETERMINATE: &str = "execution_cancellation_indeterminate";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub tool_call_id: String,
    pub tool_name: String,
    pub status: String,
    /// The complete, bounded tool result returned to the model. Runtime code
    /// transports this value verbatim and must not reclassify or rewrite it.
    pub content: String,
    /// Structured execution data for events, UI, audit, and checkpoints. Host
    /// diagnostics belong here and never need to be recovered from `content`.
    pub details: Value,
    /// Validated semantic facts produced by the executor. Hosts persist these
    /// through Core and must not infer them from display details.
    #[serde(default)]
    pub facts: Vec<ToolExecutionFact>,
    /// Structured runtime error state for events, UI, audit, and checkpoints.
    /// The model receives the complete executor-owned `content` instead.
    pub error: Option<ToolErrorInfo>,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub latency_ms: i64,
    pub parallel_group: Option<String>,
    pub transition_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LocalToolOutput {
    pub content: String,
    pub details: Value,
    pub facts: Vec<ToolExecutionFact>,
    pub status: String,
    pub error: Option<ToolErrorInfo>,
    pub transition_reason: String,
}

impl LocalToolOutput {
    pub fn success(content: impl Into<String>, details: Value) -> Self {
        Self {
            content: content.into(),
            details,
            facts: Vec::new(),
            status: "ok".to_string(),
            error: None,
            transition_reason: "local_tool_exec".to_string(),
        }
    }

    pub fn failure(content: impl Into<String>, details: Value, error: ToolErrorInfo) -> Self {
        Self {
            content: content.into(),
            details,
            facts: Vec::new(),
            status: "error".to_string(),
            error: Some(error),
            transition_reason: "local_tool_exec_error".to_string(),
        }
    }

    pub fn with_facts(mut self, facts: Vec<ToolExecutionFact>) -> Self {
        self.facts = facts;
        self
    }
}

#[derive(Debug, Clone)]
pub struct LocalToolError {
    pub content: String,
    pub details: Value,
    pub error: Box<ToolErrorInfo>,
}

impl LocalToolError {
    pub fn new(content: impl Into<String>, details: Value, error: ToolErrorInfo) -> Self {
        Self {
            content: content.into(),
            details,
            error: Box::new(error),
        }
    }

    pub fn from_message(message: impl Into<String>) -> Self {
        let message = message.into();
        Self::new(
            message.clone(),
            json!({ "message": message }),
            ToolErrorInfo::from_unstructured_error(message),
        )
    }
}

impl From<String> for LocalToolError {
    fn from(message: String) -> Self {
        Self::from_message(message)
    }
}

pub trait LocalToolHandler {
    fn name(&self) -> &'static str;
    fn invoke(
        &self,
        args_json: &str,
        runtime_context: &ToolRuntimeContext,
    ) -> Result<LocalToolOutput, LocalToolError>;
}

#[derive(Clone)]
pub struct ToolLayer {
    handlers: HashMap<String, Arc<dyn LocalToolHandler + Send + Sync>>,
    dynamic_tool_providers:
        HashMap<(String, String, String), Arc<dyn DynamicToolProvider + Send + Sync>>,
    skill_index: Arc<SkillIndex>,
    dynamic_tool_registry: Arc<DynamicToolRegistry>,
    runtime_context: ToolRuntimeContext,
}

impl std::fmt::Debug for ToolLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolLayer")
            .field("handlers_len", &self.handlers.len())
            .field("dynamic_provider_count", &self.dynamic_tool_providers.len())
            .field("skill_count", &self.skill_index.entries().len())
            .field("dynamic_tool_count", &self.dynamic_tool_registry.len())
            .field("runtime_context", &self.runtime_context)
            .finish()
    }
}

#[cfg(test)]
impl Default for ToolLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolLayer {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::new_with_skill_catalog_config(SkillCatalogLoadConfig::default())
    }

    #[cfg(test)]
    pub fn new_with_dynamic_tool_registry(dynamic_tool_registry: Arc<DynamicToolRegistry>) -> Self {
        Self::new_with_skill_catalog_config_and_dynamic_tool_registry(
            SkillCatalogLoadConfig::default(),
            dynamic_tool_registry,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_skill_catalog_config(
        skill_catalog_config: SkillCatalogLoadConfig,
    ) -> Self {
        Self::new_with_skill_catalog_config_and_dynamic_tool_registry(
            skill_catalog_config,
            Arc::new(DynamicToolRegistry::empty()),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_skill_catalog_config_and_dynamic_tool_registry(
        skill_catalog_config: SkillCatalogLoadConfig,
        dynamic_tool_registry: Arc<DynamicToolRegistry>,
    ) -> Self {
        let skill_index = Arc::new(
            SkillIndex::load(skill_catalog_config).expect("load skill index for ToolLayer failed"),
        );
        Self::new_with_loaded_skill_index_and_dynamic_tool_registry(
            skill_index,
            dynamic_tool_registry,
            ToolRuntimeContext::default(),
        )
    }

    pub fn try_new_with_skill_catalog_config_dynamic_tool_registry_and_execution_host_binding(
        skill_catalog_config: SkillCatalogLoadConfig,
        dynamic_tool_registry: Arc<DynamicToolRegistry>,
        execution_host_binding: Arc<ExecutionHostBinding>,
    ) -> Result<Self, String> {
        let skill_index = SkillIndex::load(skill_catalog_config)?;
        Ok(Self::new_with_loaded_skill_index_and_dynamic_tool_registry(
            Arc::new(skill_index),
            dynamic_tool_registry,
            ToolRuntimeContext::default().with_execution_host_binding(execution_host_binding),
        ))
    }

    pub fn try_new_with_skill_catalog_config_and_execution_host_binding(
        skill_catalog_config: SkillCatalogLoadConfig,
        execution_host_binding: Arc<ExecutionHostBinding>,
    ) -> Result<Self, String> {
        Self::try_new_with_skill_catalog_config_dynamic_tool_registry_and_execution_host_binding(
            skill_catalog_config,
            Arc::new(DynamicToolRegistry::empty()),
            execution_host_binding,
        )
    }

    fn new_with_loaded_skill_index_and_dynamic_tool_registry(
        shared_skill_index: Arc<SkillIndex>,
        dynamic_tool_registry: Arc<DynamicToolRegistry>,
        runtime_context: ToolRuntimeContext,
    ) -> Self {
        let mut layer = Self {
            handlers: HashMap::new(),
            dynamic_tool_providers: HashMap::new(),
            skill_index: shared_skill_index.clone(),
            dynamic_tool_registry: dynamic_tool_registry.clone(),
            runtime_context,
        };
        layer.register(Arc::new(ReadToolHandler));
        layer.register(Arc::new(BashToolHandler::new()));
        layer.register(Arc::new(WriteToolHandler));
        layer.register(Arc::new(EditToolHandler));
        layer
    }

    pub fn with_cwd(mut self, cwd: PathBuf) -> Result<Self, String> {
        self.runtime_context = self.runtime_context.replace_cwd(cwd)?;
        Ok(self)
    }

    pub fn with_network_policy(mut self, network_policy: NetworkSandboxPolicy) -> Self {
        self.runtime_context = self.runtime_context.with_network_policy(network_policy);
        self
    }

    pub fn with_execution_cancellation_probe(
        mut self,
        cancellation_probe: Arc<ExecutionCancellationProbe>,
    ) -> Self {
        self.runtime_context = self
            .runtime_context
            .with_execution_cancellation_probe(cancellation_probe);
        self
    }

    pub fn with_sandbox_filesystem_paths(
        mut self,
        additional_writable_roots: Vec<PathBuf>,
        denied_read_paths: Vec<PathBuf>,
    ) -> Self {
        self.runtime_context = self
            .runtime_context
            .with_sandbox_filesystem_paths(additional_writable_roots, denied_read_paths);
        self
    }

    pub fn with_execution_owner(mut self, execution_owner: impl Into<String>) -> Self {
        self.runtime_context = self.runtime_context.with_execution_owner(execution_owner);
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.runtime_context = self.runtime_context.with_session_id(session_id);
        self
    }

    pub fn with_resource_claim_store(
        mut self,
        store: Arc<dyn ResourceClaimStorePort + Send + Sync>,
    ) -> Self {
        self.runtime_context = self.runtime_context.with_resource_claim_store(store);
        self
    }

    pub fn with_file_mutation_commit_port(
        mut self,
        port: Arc<dyn FileMutationCommitPort + Send + Sync>,
    ) -> Self {
        self.runtime_context = self.runtime_context.with_file_mutation_commit_port(port);
        self
    }

    pub fn with_resolved_input_reader(
        mut self,
        port: Arc<dyn ResolvedInputReaderPort + Send + Sync>,
    ) -> Self {
        self.runtime_context = self.runtime_context.with_resolved_input_reader(port);
        self
    }

    pub fn with_external_context_store(
        mut self,
        store: Arc<dyn ExternalContextStorePort + Send + Sync>,
    ) -> Self {
        self.runtime_context = self.runtime_context.with_external_context_store(store);
        self
    }

    pub fn with_resolved_input_manifest(mut self, manifest: Arc<ResolvedInputState>) -> Self {
        self.runtime_context = self.runtime_context.with_resolved_input_manifest(manifest);
        self
    }

    pub fn with_resolved_input_root(mut self, root: PathBuf) -> Result<Self, String> {
        self.runtime_context = self.runtime_context.with_resolved_input_root(root)?;
        Ok(self)
    }

    pub fn with_dynamic_tool_registry(mut self, registry: Arc<DynamicToolRegistry>) -> Self {
        self.dynamic_tool_registry = registry;
        self.dynamic_tool_providers.clear();
        self
    }

    pub fn with_workspace_write_allowed(mut self, allowed: bool) -> Self {
        self.runtime_context = self.runtime_context.with_workspace_write_allowed(allowed);
        self
    }

    pub fn register(&mut self, handler: Arc<dyn LocalToolHandler + Send + Sync>) {
        assert!(
            canonicalize_tool_name(handler.name()).is_some(),
            "local tool handler is absent from the fixed catalog: {}",
            handler.name()
        );
        self.handlers.insert(handler.name().to_string(), handler);
    }

    pub fn register_dynamic_tool_provider(
        &mut self,
        provider: Arc<dyn DynamicToolProvider + Send + Sync>,
    ) -> Result<(), String> {
        let provider_id = provider.provider_id();
        if provider_id.trim().is_empty() || provider_id != provider_id.trim() {
            return Err("dynamic tool providerId must be exact and non-empty".to_string());
        }
        let keys = self
            .dynamic_tool_registry
            .list_contracts()
            .into_iter()
            .filter(|contract| contract.provider_id.as_deref() == Some(provider_id))
            .map(|contract| {
                let schema_hash = contract
                    .schema_hash
                    .ok_or_else(|| "dynamic tool contract schemaHash is required".to_string())?;
                Ok((provider_id.to_string(), contract.name, schema_hash))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if keys.is_empty() {
            return Err(format!(
                "dynamic tool provider has no authorized contract: {provider_id}"
            ));
        }
        if keys
            .iter()
            .any(|key| self.dynamic_tool_providers.contains_key(key))
        {
            return Err(format!(
                "duplicate dynamic tool provider binding: {provider_id}"
            ));
        }
        for key in keys {
            self.dynamic_tool_providers.insert(key, provider.clone());
        }
        Ok(())
    }

    pub fn can_handle(&self, tool_name: &str) -> bool {
        let canonical = canonicalize_tool_name(tool_name).unwrap_or(tool_name);
        self.handlers.contains_key(canonical)
            || self
                .dynamic_tool_registry
                .find_contract(canonical)
                .is_some()
    }

    pub fn is_concurrency_safe(&self, tool_name: &str) -> bool {
        let canonical = canonicalize_tool_name(tool_name).unwrap_or(tool_name);
        if let Some(contract) = self.dynamic_tool_registry.find_contract(canonical) {
            return contract.concurrency_safe;
        }
        is_tool_concurrency_safe(canonical)
    }

    pub fn tool_contract(&self, tool_name: &str) -> Result<ToolContract, String> {
        let canonical = canonicalize_tool_name(tool_name).unwrap_or(tool_name);
        if let Some(contract) = self.dynamic_tool_registry.find_contract(canonical) {
            return Ok(contract);
        }
        list_tool_contracts()
            .into_iter()
            .find(|contract| contract.name == canonical)
            .ok_or_else(|| format!("tool contract not found: {canonical}"))
    }

    pub fn tool_turn_behavior(&self, tool_name: &str) -> Result<ToolTurnBehavior, String> {
        self.tool_contract(tool_name)
            .map(|contract| contract.turn_behavior)
    }

    pub fn dynamic_tool_registry(&self) -> &DynamicToolRegistry {
        self.dynamic_tool_registry.as_ref()
    }

    pub fn skill_index(&self) -> &SkillIndex {
        self.skill_index.as_ref()
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.runtime_context
            .execution_host_binding
            .as_ref()
            .map(|binding| binding.cwd())
    }

    pub fn resolve_execution_model_input_image(
        &self,
        reference: &ExecutionModelInputImageRefV1,
    ) -> Result<Vec<u8>, String> {
        reference.validate()?;
        let binding = self.runtime_context.execution_host_binding()?;
        let output = binding
            .run_file_system_operation(
                reference.path.as_str(),
                ExecutionFileSystemOperation::ReadFile {
                    max_bytes: MODEL_INPUT_IMAGE_MAX_BYTES,
                },
            )
            .map_err(|error| format!("resolve execution model input image failed: {error}"))?;
        let ExecutionFileSystemOutput::ReadFile(output) = output else {
            return Err(
                "resolve execution model input image returned an unexpected result".to_string(),
            );
        };
        if output.file_hash != reference.sha256 {
            return Err("execution_model_input_image_hash_mismatch".to_string());
        }
        let (content_type, width_px, height_px) = inspect_model_input_image(&output.bytes)?;
        if content_type != reference.content_type
            || output.bytes.len() as u64 != reference.byte_length
            || width_px != reference.width_px
            || height_px != reference.height_px
        {
            return Err("execution_model_input_image_metadata_mismatch".to_string());
        }
        Ok(output.bytes)
    }

    pub fn execution_host_kind(&self) -> Option<ExecutionHostKind> {
        self.runtime_context
            .execution_host_binding
            .as_ref()
            .map(|binding| binding.kind())
    }

    pub fn read_agents_instructions(&self) -> Result<Option<(String, String)>, String> {
        const AGENTS_MAX_BYTES: usize = 32_768 * 4;
        const AGENTS_MAX_CHARS: usize = 32_768;

        if self.runtime_context.execution_host_binding.is_none() {
            return Ok(None);
        }
        let binding = self
            .runtime_context
            .execution_host_binding()
            .map_err(|error| format!("read AGENTS.md failed: {error}"))?;
        let output = match binding.run_file_system_operation(
            "AGENTS.md",
            ExecutionFileSystemOperation::ReadFile {
                max_bytes: AGENTS_MAX_BYTES,
            },
        ) {
            Ok(output) => output,
            Err(error) if error.kind == ExecutionFileSystemErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("read AGENTS.md failed: {error}")),
        };
        let ExecutionFileSystemOutput::ReadFile(read) = output else {
            return Err("read AGENTS.md returned an unexpected filesystem output".to_string());
        };
        let content = String::from_utf8(read.bytes)
            .map_err(|error| format!("AGENTS.md must be valid UTF-8: {error}"))?;
        let char_count = content.chars().count();
        if char_count > AGENTS_MAX_CHARS {
            return Err(format!(
                "AGENTS.md exceeds character limit: actualChars={char_count} maxChars={AGENTS_MAX_CHARS}"
            ));
        }
        Ok(Some((content, read.file_hash)))
    }

    pub fn bash_description(&self) -> Result<&'static str, String> {
        self.runtime_context
            .execution_host_binding
            .as_ref()
            .map(|binding| binding.bash_description())
            .ok_or_else(|| "execution host binding is required for bash".to_string())
    }

    #[cfg(test)]
    pub fn execute(&self, req: ToolInvocationRequest) -> ToolExecutionResult {
        let args_json = req.args_json.clone();
        result_capture::seal_tool_result(
            self.execute_unsealed(req),
            args_json.as_str(),
            self.runtime_context.session_id.as_deref(),
            self.runtime_context.execution_host_binding.as_deref(),
        )
    }

    #[cfg(test)]
    fn execute_unsealed(&self, req: ToolInvocationRequest) -> ToolExecutionResult {
        let started_at_ms = now_ms();
        let canonical_name = canonicalize_tool_name(req.tool_name.as_str())
            .unwrap_or(req.tool_name.as_str())
            .to_string();
        if self
            .dynamic_tool_registry
            .find_contract(canonical_name.as_str())
            .is_some()
        {
            return unsupported_tool_result(req, canonical_name, started_at_ms);
        }
        if let Some(handler) = self.handlers.get(canonical_name.as_str()) {
            let invocation_context = self
                .runtime_context
                .clone()
                .with_tool_invocation(req.tool_call_id.clone(), canonical_name.clone());
            return match handler.invoke(req.args_json.as_str(), &invocation_context) {
                Ok(output) => local_tool_execution_result(
                    req.tool_call_id,
                    canonical_name,
                    output,
                    started_at_ms,
                ),
                Err(failure) => local_tool_failure_result(
                    req.tool_call_id,
                    canonical_name,
                    failure,
                    started_at_ms,
                ),
            };
        }

        ToolExecutionResult {
            tool_call_id: req.tool_call_id,
            tool_name: canonical_name.clone(),
            status: "error".to_string(),
            content: format!("unsupported local tool: {canonical_name}"),
            details: json!({ "toolName": canonical_name }),
            facts: Vec::new(),
            error: Some(ToolErrorInfo::from_unstructured_error(format!(
                "unsupported local tool: {canonical_name}"
            ))),
            started_at_ms,
            completed_at_ms: now_ms(),
            latency_ms: now_ms().saturating_sub(started_at_ms),
            parallel_group: None,
            transition_reason: Some("local_tool_unsupported".to_string()),
        }
    }

    pub async fn execute_async(&self, req: ToolInvocationRequest) -> ToolExecutionResult {
        let args_json = req.args_json.clone();
        result_capture::seal_tool_result(
            self.execute_async_unsealed(req).await,
            args_json.as_str(),
            self.runtime_context.session_id.as_deref(),
            self.runtime_context.execution_host_binding.as_deref(),
        )
    }

    async fn execute_async_unsealed(&self, req: ToolInvocationRequest) -> ToolExecutionResult {
        let started_at_ms = now_ms();
        let canonical_name = canonicalize_tool_name(req.tool_name.as_str())
            .unwrap_or(req.tool_name.as_str())
            .to_string();
        if let Some(contract) = self
            .dynamic_tool_registry
            .find_contract(canonical_name.as_str())
        {
            return self
                .execute_dynamic_tool_async(req, canonical_name, contract, started_at_ms)
                .await;
        }
        if let Some(handler) = self.handlers.get(canonical_name.as_str()).cloned() {
            let invocation_context = self
                .runtime_context
                .clone()
                .with_tool_invocation(req.tool_call_id.clone(), canonical_name.clone());
            let tool_call_id = req.tool_call_id;
            let args_json = req.args_json;
            let tool_name = canonical_name;
            return match tokio::task::spawn_blocking(move || {
                handler.invoke(args_json.as_str(), &invocation_context)
            })
            .await
            {
                Ok(Ok(output)) => {
                    local_tool_execution_result(tool_call_id, tool_name, output, started_at_ms)
                }
                Ok(Err(failure)) => {
                    local_tool_failure_result(tool_call_id, tool_name, failure, started_at_ms)
                }
                Err(err) => ToolExecutionResult {
                    tool_call_id,
                    tool_name,
                    status: "error".to_string(),
                    content:
                        "Tool execution failed because its runtime task terminated unexpectedly."
                            .to_string(),
                    details: json!({ "joinError": err.to_string() }),
                    facts: Vec::new(),
                    error: Some(ToolErrorInfo::new(
                        ToolFailureKind::HostUnavailable,
                        "Tool execution failed because its runtime task terminated unexpectedly",
                        "Tool execution failed",
                    )),
                    started_at_ms,
                    completed_at_ms: now_ms(),
                    latency_ms: now_ms().saturating_sub(started_at_ms),
                    parallel_group: None,
                    transition_reason: Some("local_tool_blocking_join_failed".to_string()),
                },
            };
        }

        unsupported_tool_result(req, canonical_name, started_at_ms)
    }

    async fn execute_dynamic_tool_async(
        &self,
        req: ToolInvocationRequest,
        canonical_name: String,
        contract: ToolContract,
        started_at_ms: i64,
    ) -> ToolExecutionResult {
        let provider_id = contract.provider_id.clone().unwrap_or_default();
        let schema_hash = contract.schema_hash.clone().unwrap_or_default();
        let provider_key = (
            provider_id.clone(),
            canonical_name.clone(),
            schema_hash.clone(),
        );
        let Some(provider) = self.dynamic_tool_providers.get(&provider_key) else {
            let completed_at_ms = now_ms();
            return ToolExecutionResult {
                tool_call_id: req.tool_call_id,
                tool_name: canonical_name,
                status: "error".to_string(),
                content: format!("dynamic tool provider not configured: {provider_id}"),
                details: json!({
                    "dynamicTool": true,
                    "toolName": contract.name,
                    "providerId": provider_id,
                    "schemaHash": schema_hash,
                    "error": "dynamic tool provider not configured",
                }),
                facts: Vec::new(),
                error: Some(ToolErrorInfo::from_unstructured_error(format!(
                    "dynamic tool provider not configured: {provider_id}"
                ))),
                started_at_ms,
                completed_at_ms,
                latency_ms: completed_at_ms.saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("dynamic_tool_provider_missing".to_string()),
            };
        };

        let cancellation_probe = self.runtime_context.execution_cancellation_probe.clone();
        let provider_request = DynamicToolProviderRequest {
            tool_call_id: req.tool_call_id.clone(),
            tool_name: canonical_name.clone(),
            args_json: req.args_json,
            contract: contract.clone(),
            cancellation_probe: cancellation_probe.clone(),
        };
        let provider_result = provider.execute_with_error_info(provider_request).await;
        if let Some(reason) = provider_result.is_err().then_some(()).and_then(|()| {
            cancellation_probe
                .as_deref()
                .and_then(|probe| probe().ok().flatten())
        }) {
            let completed_at_ms = now_ms();
            return ToolExecutionResult {
                tool_call_id: req.tool_call_id,
                tool_name: canonical_name,
                status: "cancelled".to_string(),
                content: default_model_message_for_tool_failure(&ToolFailureKind::Cancelled)
                    .to_string(),
                details: json!({
                    "dynamicTool": true,
                    "toolName": contract.name,
                    "providerId": provider_id,
                    "schemaHash": schema_hash,
                    "status": "cancelled",
                    "reason": reason,
                }),
                facts: Vec::new(),
                error: Some(ToolErrorInfo::new(
                    ToolFailureKind::Cancelled,
                    default_model_message_for_tool_failure(&ToolFailureKind::Cancelled),
                    default_user_message_for_tool_failure(&ToolFailureKind::Cancelled),
                )),
                started_at_ms,
                completed_at_ms,
                latency_ms: completed_at_ms.saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("dynamic_tool_provider_cancelled".to_string()),
            };
        }
        match provider_result {
            Ok(response) => {
                let completed_at_ms = now_ms();
                let error = response.is_error.then(|| {
                    ToolErrorInfo::new(
                        ToolFailureKind::ProviderError,
                        response.content.clone(),
                        default_user_message_for_tool_failure(&ToolFailureKind::ProviderError),
                    )
                });
                ToolExecutionResult {
                    tool_call_id: req.tool_call_id,
                    tool_name: canonical_name,
                    status: if response.is_error { "error" } else { "ok" }.to_string(),
                    content: response.content,
                    details: wrap_dynamic_tool_output(&contract, response.details),
                    facts: response.facts,
                    error,
                    started_at_ms,
                    completed_at_ms,
                    latency_ms: completed_at_ms.saturating_sub(started_at_ms),
                    parallel_group: None,
                    transition_reason: Some(
                        response
                            .transition_reason
                            .unwrap_or_else(|| "dynamic_tool_provider_exec".to_string()),
                    ),
                }
            }
            Err(error) => {
                let completed_at_ms = now_ms();
                let model_message = error.model_message.clone();
                ToolExecutionResult {
                    tool_call_id: req.tool_call_id,
                    tool_name: canonical_name,
                    status: "error".to_string(),
                    content: model_message.to_string(),
                    details: json!({
                    "dynamicTool": true,
                    "toolName": contract.name,
                    "providerId": provider_id,
                    "schemaHash": schema_hash,
                        "error": model_message,
                        "errorKind": error.kind.as_str(),
                        "retryable": error.retryable,
                    }),
                    facts: Vec::new(),
                    error: Some(error),
                    started_at_ms,
                    completed_at_ms,
                    latency_ms: completed_at_ms.saturating_sub(started_at_ms),
                    parallel_group: None,
                    transition_reason: Some("dynamic_tool_provider_exec_error".to_string()),
                }
            }
        }
    }
}

fn unsupported_tool_result(
    req: ToolInvocationRequest,
    canonical_name: String,
    started_at_ms: i64,
) -> ToolExecutionResult {
    ToolExecutionResult {
        tool_call_id: req.tool_call_id,
        tool_name: canonical_name.clone(),
        status: "error".to_string(),
        content: format!("unsupported local tool: {canonical_name}"),
        details: json!({ "toolName": canonical_name }),
        facts: Vec::new(),
        error: Some(ToolErrorInfo::from_unstructured_error(format!(
            "unsupported local tool: {canonical_name}"
        ))),
        started_at_ms,
        completed_at_ms: now_ms(),
        latency_ms: now_ms().saturating_sub(started_at_ms),
        parallel_group: None,
        transition_reason: Some("local_tool_unsupported".to_string()),
    }
}

fn local_tool_execution_result(
    tool_call_id: String,
    tool_name: String,
    output: LocalToolOutput,
    started_at_ms: i64,
) -> ToolExecutionResult {
    let completed_at_ms = now_ms();
    ToolExecutionResult {
        tool_call_id,
        tool_name,
        status: output.status,
        content: output.content,
        details: output.details,
        facts: output.facts,
        error: output.error,
        started_at_ms,
        completed_at_ms,
        latency_ms: completed_at_ms.saturating_sub(started_at_ms),
        parallel_group: None,
        transition_reason: Some(output.transition_reason),
    }
}

fn local_tool_failure_result(
    tool_call_id: String,
    tool_name: String,
    failure: LocalToolError,
    started_at_ms: i64,
) -> ToolExecutionResult {
    let completed_at_ms = now_ms();
    ToolExecutionResult {
        tool_call_id,
        tool_name,
        status: "error".to_string(),
        content: failure.content,
        details: failure.details,
        facts: Vec::new(),
        error: Some(*failure.error),
        started_at_ms,
        completed_at_ms,
        latency_ms: completed_at_ms.saturating_sub(started_at_ms),
        parallel_group: None,
        transition_reason: Some("local_tool_exec_error".to_string()),
    }
}

struct LocalToolOutputStatus {
    status: String,
    error: Option<ToolErrorInfo>,
    transition_reason: String,
}

fn bash_local_tool_output(details: Value) -> LocalToolOutput {
    let status = bash_tool_output_status(&details);
    let content = bash_tool_content(&details, status.error.as_ref());
    let mut details = details;
    if let Some(details) = details.as_object_mut() {
        details.remove("stdout");
        details.remove("stderr");
    }
    LocalToolOutput {
        content,
        details,
        facts: Vec::new(),
        status: status.status,
        error: status.error,
        transition_reason: status.transition_reason,
    }
}

fn bash_tool_output_status(parsed: &Value) -> LocalToolOutputStatus {
    if parsed.get("schema").and_then(Value::as_str) != Some("bash_result_v1") {
        return LocalToolOutputStatus {
            status: "error".to_string(),
            error: Some(ToolErrorInfo::new(
                ToolFailureKind::Unknown,
                "Bash returned an invalid result payload",
                "Bash result payload invalid",
            )),
            transition_reason: "local_tool_invalid_result".to_string(),
        };
    }
    let executed = parsed
        .get("executed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let timed_out = parsed
        .get("timedOut")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exit_code = parsed.get("exitCode").and_then(Value::as_i64);
    if executed && !timed_out && exit_code == Some(0) {
        return LocalToolOutputStatus {
            status: "ok".to_string(),
            error: None,
            transition_reason: "local_tool_exec".to_string(),
        };
    }

    let explicit_failure_kind = parsed
        .pointer("/executionHost/failureKind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.eq_ignore_ascii_case("none"));
    let mut error = if let Some(error) = tool_error_from_bash_payload(parsed) {
        error
    } else if let Some(failure_kind) = explicit_failure_kind {
        ToolErrorInfo::from_execution_host_failure(
            failure_kind,
            exit_code.map(|code| code as i32),
            timed_out,
        )
    } else {
        // Classify the failure using structured types. The Bash handler has
        // already separated bounded model content from structured details.
        let failure_kind = classify_execution_host_failure(
            exit_code.map(|code| code as i32),
            timed_out,
            parsed
                .get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            parsed
                .get("stderr")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        tool_error_from_execution_host_failure(failure_kind, exit_code.map(|code| code as i32))
    };
    if let Some(diagnostic_id) = parsed
        .pointer("/runtimeDiagnostics/0/diagnosticId")
        .and_then(Value::as_str)
    {
        error = error.with_diagnostic(diagnostic_id);
    }
    LocalToolOutputStatus {
        status: "error".to_string(),
        error: Some(error),
        transition_reason: "local_tool_exec_error".to_string(),
    }
}

fn bash_tool_content(details: &Value, error: Option<&ToolErrorInfo>) -> String {
    if details.get("schema").and_then(Value::as_str) != Some("bash_result_v1") {
        return error
            .map(|value| value.model_message.clone())
            .unwrap_or_else(|| "Bash returned an invalid result payload".to_string());
    }

    let executed = details
        .get("executed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !executed {
        return error
            .map(|value| value.model_message.clone())
            .unwrap_or_else(|| "Bash failed before command execution".to_string());
    }

    let exit_code = details.get("exitCode").and_then(Value::as_i64);
    let timed_out = details
        .get("timedOut")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let timeout_ms = details.get("timeoutMs").and_then(Value::as_u64);
    let stdout = details
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_matches(['\r', '\n']);
    let stderr = details
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_matches(['\r', '\n']);

    let mut sections = Vec::new();
    if timed_out {
        sections.push(match timeout_ms {
            Some(timeout_ms) => format!("Command timed out after {timeout_ms} ms."),
            None => "Command timed out.".to_string(),
        });
    } else if let Some(exit_code) = exit_code {
        if exit_code == 0 {
            sections.push("Command completed successfully with exit code 0.".to_string());
        } else {
            sections.push(format!("Command failed with exit code {exit_code}."));
        }
    } else if let Some(error) = error {
        sections.push(error.model_message.clone());
    } else {
        sections.push("Command completed without an exit code.".to_string());
    }

    if !stdout.is_empty() {
        sections.push(format!("stdout:\n{stdout}"));
    }
    if !stderr.is_empty() {
        sections.push(format!("stderr:\n{stderr}"));
    }
    if stdout.is_empty() && stderr.is_empty() {
        if let Some(error) = error {
            if !sections
                .iter()
                .any(|section| section == &error.model_message)
            {
                sections.push(format!("error: {}", error.model_message));
            }
        } else {
            sections.push("No stdout or stderr was produced.".to_string());
        }
    }
    sections.extend(network_diagnostic_model_messages(details));
    sections.extend(input_state_change_model_messages(details));

    sections.join("\n")
}

fn input_state_change_model_messages(details: &Value) -> Vec<String> {
    details
        .get("inputStateChanges")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|change| {
            let input_ref = change.get("inputRef")?.as_str()?;
            let state = change.get("state")?.as_str()?;
            matches!(
                state,
                "asset_removed" | "access_revoked" | "source_deleted" | "stale_generation"
            )
            .then(|| format!("Input `{input_ref}` is no longer available: {state}."))
        })
        .collect()
}

fn network_diagnostic_model_messages(details: &Value) -> Vec<String> {
    let Some(diagnostics) = details.get("runtimeDiagnostics").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut messages = Vec::new();
    for diagnostic in diagnostics {
        let message = match diagnostic.get("code").and_then(Value::as_str) {
            Some("network_policy_denied") => {
                Some("ExecutionHost network policy denied an outbound connection.")
            }
            Some("network_dns_failed") => {
                Some("ExecutionHost reported an outbound DNS resolution failure.")
            }
            Some("network_unreachable") => {
                Some("ExecutionHost reported that an outbound network target was unreachable.")
            }
            _ => None,
        };
        if let Some(message) = message {
            if !messages.iter().any(|existing| existing == message) {
                messages.push(message.to_string());
            }
        }
    }
    messages
}

fn tool_error_from_bash_payload(parsed: &Value) -> Option<ToolErrorInfo> {
    let kind = parsed
        .pointer("/toolError/kind")
        .and_then(Value::as_str)
        .and_then(tool_failure_kind_from_str)?;
    let model_message = parsed
        .pointer("/toolError/modelMessage")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| default_model_message_for_tool_failure(&kind));
    let user_message = parsed
        .pointer("/toolError/userMessage")
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| default_user_message_for_tool_failure(&kind));
    let mut error = ToolErrorInfo::new(kind, model_message, user_message);
    if let Some(retryable) = parsed
        .pointer("/toolError/retryable")
        .and_then(Value::as_bool)
    {
        error = error.with_retryable(retryable);
    }
    if let Some(diagnostic_id) = parsed
        .pointer("/toolError/diagnosticId")
        .and_then(Value::as_str)
    {
        error = error.with_diagnostic(diagnostic_id);
    }
    Some(error)
}

fn tool_failure_kind_from_str(value: &str) -> Option<ToolFailureKind> {
    match value {
        "command_failed" => Some(ToolFailureKind::CommandFailed),
        "timed_out" => Some(ToolFailureKind::TimedOut),
        "sandbox_unavailable" => Some(ToolFailureKind::SandboxUnavailable),
        "host_unavailable" => Some(ToolFailureKind::HostUnavailable),
        "provider_error" => Some(ToolFailureKind::ProviderError),
        "permission_denied" => Some(ToolFailureKind::PermissionDenied),
        "invalid_input" => Some(ToolFailureKind::InvalidInput),
        "cancelled" => Some(ToolFailureKind::Cancelled),
        "unknown" => Some(ToolFailureKind::Unknown),
        _ => None,
    }
}

fn default_model_message_for_tool_failure(kind: &ToolFailureKind) -> &'static str {
    match kind {
        ToolFailureKind::CommandFailed => "command did not execute successfully",
        ToolFailureKind::TimedOut => "command timed out before completing",
        ToolFailureKind::SandboxUnavailable => {
            "sandbox runtime is unavailable; check sandbox configuration"
        }
        ToolFailureKind::HostUnavailable => {
            "execution host is unavailable; the sandbox runtime may need restarting"
        }
        ToolFailureKind::ProviderError => "dynamic tool provider returned an error",
        ToolFailureKind::PermissionDenied => {
            "tool execution was denied by policy or permission requirements"
        }
        ToolFailureKind::InvalidInput => {
            "tool input is invalid; revise the tool arguments and retry"
        }
        ToolFailureKind::Cancelled => "tool execution was cancelled before completion",
        ToolFailureKind::Unknown => "tool execution encountered an unexpected error",
    }
}

fn default_user_message_for_tool_failure(kind: &ToolFailureKind) -> &'static str {
    match kind {
        ToolFailureKind::CommandFailed => "Command execution failed",
        ToolFailureKind::TimedOut => "Command timed out",
        ToolFailureKind::SandboxUnavailable => "Sandbox unavailable",
        ToolFailureKind::HostUnavailable => "Execution host unavailable",
        ToolFailureKind::ProviderError => "Dynamic tool execution failed",
        ToolFailureKind::PermissionDenied => "Tool execution denied",
        ToolFailureKind::InvalidInput => "Invalid tool input",
        ToolFailureKind::Cancelled => "Tool execution cancelled",
        ToolFailureKind::Unknown => "Tool execution error",
    }
}

fn tool_error_from_execution_host_failure(
    failure_kind: ExecutionHostFailureKind,
    exit_code: Option<i32>,
) -> ToolErrorInfo {
    match failure_kind {
        ExecutionHostFailureKind::TimedOut => ToolErrorInfo::new(
            ToolFailureKind::TimedOut,
            "command timed out before completing",
            "Command timed out",
        )
        .with_retryable(true),
        ExecutionHostFailureKind::CommandFailed => ToolErrorInfo::new(
            ToolFailureKind::CommandFailed,
            format!("command failed with exit code {}", exit_code.unwrap_or(-1)),
            "Command execution failed",
        ),
        ExecutionHostFailureKind::Cancelled => ToolErrorInfo::new(
            ToolFailureKind::Cancelled,
            "command was cancelled at an execution safe point",
            "Command cancelled",
        ),
        ExecutionHostFailureKind::HostUnavailable => ToolErrorInfo::new(
            ToolFailureKind::HostUnavailable,
            "execution host is unavailable; the sandbox runtime may need restarting",
            "Execution host unavailable",
        )
        .with_retryable(true),
        ExecutionHostFailureKind::SandboxUnavailable => ToolErrorInfo::new(
            ToolFailureKind::SandboxUnavailable,
            "sandbox runtime is unavailable; check sandbox configuration",
            "Sandbox unavailable",
        )
        .with_retryable(true),
        ExecutionHostFailureKind::None => ToolErrorInfo::new(
            ToolFailureKind::Unknown,
            "command did not execute successfully",
            "Command failed",
        ),
    }
}

fn parse_tool_args(tool_name: &str, args_json: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(args_json)
        .map_err(|err| format!("parse {tool_name} args JSON failed: {err}"))
}

fn extract_string_arg(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        args.get(*key)
            .and_then(Value::as_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn now_ms() -> i64 {
    crate::runtime::contracts::current_timestamp_ms()
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use super::{FileMutationCommitPort, FileMutationCommitRequest, ToolRuntimeContext};

    #[derive(Debug, Default)]
    pub(crate) struct RecordingFileMutationCommitPort {
        requests: Mutex<Vec<FileMutationCommitRequest>>,
    }

    impl RecordingFileMutationCommitPort {
        pub(crate) fn shared() -> Arc<Self> {
            Arc::new(Self::default())
        }

        pub(crate) fn requests(&self) -> Vec<FileMutationCommitRequest> {
            self.requests.lock().expect("requests lock").clone()
        }
    }

    impl FileMutationCommitPort for RecordingFileMutationCommitPort {
        fn commit_file_mutation(&self, request: FileMutationCommitRequest) -> Result<(), String> {
            let mut guard = self.requests.lock().expect("requests lock");
            guard.push(request);
            Ok(())
        }
    }

    #[derive(Debug)]
    pub(crate) struct FailingFileMutationCommitPort;

    impl FileMutationCommitPort for FailingFileMutationCommitPort {
        fn commit_file_mutation(&self, _request: FileMutationCommitRequest) -> Result<(), String> {
            Err("test file mutation commit failed".to_string())
        }
    }

    pub(crate) fn context_with_file_mutation_commit(
        context: ToolRuntimeContext,
        tool_call_id: &str,
        tool_name: &str,
    ) -> ToolRuntimeContext {
        context
            .with_tool_invocation(tool_call_id, tool_name)
            .with_file_mutation_commit_port(RecordingFileMutationCommitPort::shared())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::tool::{DynamicToolContract, DynamicToolRegistry, ToolTurnBehavior};
    use crate::tool::{ToolErrorInfo, ToolFailureKind};
    use serde_json::{json, Value};

    use super::{
        bash_local_tool_output, DynamicToolProvider, DynamicToolProviderRequest,
        DynamicToolProviderResponse, LocalToolError, LocalToolHandler, LocalToolOutput,
        ToolExecutionResult, ToolInvocationRequest, ToolLayer, ToolResultState, ToolRuntimeContext,
    };

    fn project_file_path_for_tests() -> String {
        for candidate in ["core/src/lib.rs", "src/lib.rs"] {
            if std::path::Path::new(candidate).exists() {
                return candidate.to_string();
            }
        }
        "src/lib.rs".to_string()
    }

    fn tool_layer_with_test_workspace_root(layer: ToolLayer) -> ToolLayer {
        layer
            .with_cwd(std::env::current_dir().expect("read current dir"))
            .expect("set test workspace root")
            .with_file_mutation_commit_port(
                super::test_support::RecordingFileMutationCommitPort::shared(),
            )
    }

    fn unique_suffix() -> String {
        let current_thread = std::thread::current();
        let thread_name = current_thread.name().unwrap_or("t");
        let safe_thread = thread_name
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>();
        format!(
            "{}_{}_{}",
            std::process::id(),
            safe_thread,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock moved backwards")
                .as_nanos()
        )
    }

    fn remove_dir_all_with_retries(path: &std::path::Path, attempts: usize) {
        let mut last_error = None;
        for _ in 0..attempts {
            match fs::remove_dir_all(path) {
                Ok(_) => return,
                Err(_) if !path.exists() => return,
                Err(err) => {
                    last_error = Some(err);
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
        }
        match fs::remove_dir_all(path) {
            Ok(_) => {}
            Err(_) if !path.exists() => {}
            Err(err) => {
                let previous = last_error
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string());
                panic!(
                    "cleanup test directory failed: path={}, error={}, previous_error={}",
                    path.display(),
                    err,
                    previous
                );
            }
        }
    }

    #[derive(Debug)]
    struct EchoDynamicProvider {
        provider_id: String,
        observed_args: Arc<Mutex<Vec<String>>>,
    }

    impl DynamicToolProvider for EchoDynamicProvider {
        fn provider_id(&self) -> &str {
            self.provider_id.as_str()
        }

        fn execute<'a>(
            &'a self,
            req: DynamicToolProviderRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
        {
            Box::pin(async move {
                self.observed_args
                    .lock()
                    .expect("observed args lock")
                    .push(req.args_json.clone());
                let is_error = req.args_json == r#"{"query":"banana"}"#;
                Ok(DynamicToolProviderResponse {
                    content: "echo dynamic result".to_string(),
                    details: json!({
                        "echoTool": req.tool_name,
                        "provider": self.provider_id,
                        "contractProvider": req.contract.provider_id,
                        "args": serde_json::from_str::<serde_json::Value>(req.args_json.as_str())
                            .unwrap_or(serde_json::Value::Null),
                    }),
                    is_error,
                    facts: Vec::new(),
                    transition_reason: Some("test_dynamic_provider_exec".to_string()),
                })
            })
        }
    }

    #[derive(Debug)]
    struct FailingDynamicProvider;

    impl DynamicToolProvider for FailingDynamicProvider {
        fn provider_id(&self) -> &str {
            "ragflow.local"
        }

        fn execute<'a>(
            &'a self,
            _req: DynamicToolProviderRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
        {
            Box::pin(async { Err("secret provider diagnostic".to_string()) })
        }
    }

    #[derive(Debug)]
    struct RetryableDynamicProvider;

    impl DynamicToolProvider for RetryableDynamicProvider {
        fn provider_id(&self) -> &str {
            "ragflow.local"
        }

        fn execute<'a>(
            &'a self,
            _req: DynamicToolProviderRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
        {
            Box::pin(async { unreachable!("typed provider path must be used") })
        }

        fn execute_with_error_info<'a>(
            &'a self,
            _req: DynamicToolProviderRequest,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<DynamicToolProviderResponse, ToolErrorInfo>> + Send + 'a,
            >,
        > {
            Box::pin(async {
                Err(ToolErrorInfo::new(
                    ToolFailureKind::TimedOut,
                    "provider poll timed out",
                    "Provider poll timed out",
                )
                .with_diagnostic("provider-poll-timeout")
                .with_retryable(true))
            })
        }
    }

    struct CancellationAwareDynamicProvider {
        entered: Arc<AtomicBool>,
    }

    impl DynamicToolProvider for CancellationAwareDynamicProvider {
        fn provider_id(&self) -> &str {
            "ragflow.local"
        }

        fn execute<'a>(
            &'a self,
            request: DynamicToolProviderRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
        {
            Box::pin(async move {
                self.entered.store(true, Ordering::SeqCst);
                let reason = request.wait_for_cancellation().await?;
                Err(format!("dynamic tool execution cancelled: {reason}"))
            })
        }
    }

    struct StaticBashHandler {
        details: Value,
    }

    impl LocalToolHandler for StaticBashHandler {
        fn name(&self) -> &'static str {
            "bash"
        }

        fn invoke(
            &self,
            _args_json: &str,
            _runtime_context: &ToolRuntimeContext,
        ) -> Result<LocalToolOutput, LocalToolError> {
            Ok(bash_local_tool_output(self.details.clone()))
        }
    }

    #[test]
    fn bash_tool_returns_bash_result_payload() {
        let layer = tool_layer_with_test_workspace_root(ToolLayer::new());
        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-bash".to_string(),
            tool_name: "bash".to_string(),
            args_json: "{\"command\":\"echo bash-ok\",\"timeout_ms\":30000}".to_string(),
        });
        let payload = &result.details;
        assert_eq!(
            payload.get("schema").and_then(Value::as_str),
            Some("bash_result_v1")
        );
        assert_eq!(
            payload.get("bashDialect").and_then(Value::as_str),
            Some("bash")
        );
        assert_eq!(result.status, "ok", "result={result:#?}");
        assert_eq!(payload.get("executed").and_then(Value::as_bool), Some(true));
        assert_eq!(payload.get("exitCode").and_then(Value::as_i64), Some(0));
        assert!(payload.get("stdout").is_none());
        assert!(result.content.contains("bash-ok"));
    }

    #[test]
    fn bash_nonzero_exit_is_a_structured_tool_error() {
        let mut layer = ToolLayer::new();
        layer.register(Arc::new(StaticBashHandler {
            details: json!({
                "schema": "bash_result_v1",
                "command": "cargo check",
                "executed": true,
                "exitCode": 101,
                "timedOut": false,
                "stdout": "",
                "stderr": "error: could not compile centaeris-core",
            }),
        }));

        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-bash-nonzero".to_string(),
            tool_name: "bash".to_string(),
            args_json: json!({"command": "cargo check"}).to_string(),
        });

        assert_eq!(result.status, "error");
        assert_eq!(
            result.error.as_ref().map(|error| &error.kind),
            Some(&ToolFailureKind::CommandFailed)
        );
        assert_eq!(
            result
                .error
                .as_ref()
                .map(|error| error.model_message.as_str()),
            Some("command failed with exit code 101")
        );
        assert!(result.content.contains("could not compile"));
    }

    #[test]
    fn bash_model_content_preserves_stdout_and_stderr_for_the_result_boundary() {
        let stdout = (0..250)
            .map(|index| format!("stdout-line-{index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let stderr = format!("{}stderr-final-marker", "error-byte".repeat(4_000));

        let output = bash_local_tool_output(json!({
            "schema": "bash_result_v1",
            "executed": true,
            "exitCode": 1,
            "timedOut": false,
            "stdout": stdout,
            "stderr": stderr,
        }));

        assert!(output.content.contains("stdout-line-000"));
        assert!(output.content.contains("stdout-line-249"));
        assert!(output.content.contains("stderr-final-marker"));
    }

    #[test]
    fn bash_model_content_reports_input_state_changes() {
        let output = bash_local_tool_output(json!({
            "schema": "bash_result_v1",
            "executed": true,
            "exitCode": 0,
            "timedOut": false,
            "stdout": "",
            "stderr": "",
            "inputStateChanges": [{
                "inputRef": "input_1",
                "state": "access_revoked"
            }]
        }));

        assert!(output
            .content
            .contains("Input `input_1` is no longer available: access_revoked."));
    }

    #[test]
    fn bash_structured_tool_error_keeps_host_diagnostics_out_of_content() {
        let mut layer = ToolLayer::new();
        layer.register(Arc::new(StaticBashHandler {
            details: json!({
                "schema": "bash_result_v1",
                "command": "echo denied",
                "executed": false,
                "exitCode": Value::Null,
                "timedOut": false,
                "stdout": "host-internal-token",
                "stderr": "",
                "runtimeDiagnostics": [
                    {
                        "source": "sandbox",
                        "message": "raw diagnostic",
                        "diagnosticId": "sandbox:abc"
                    }
                ],
                "toolError": {
                    "kind": "permission_denied",
                    "modelMessage": "tool execution was denied by policy or permission requirements",
                    "userMessage": "Tool execution denied",
                    "diagnosticId": "sandbox:abc",
                    "retryable": false
                }
            }),
        }));

        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-bash-tool-error".to_string(),
            tool_name: "bash".to_string(),
            args_json: "{\"command\":\"echo denied\"}".to_string(),
        });

        assert_eq!(result.status, "error");
        let err = result.error.as_ref().expect("error should be present");
        assert_eq!(err.kind, ToolFailureKind::PermissionDenied);
        assert_eq!(err.diagnostic_id.as_deref(), Some("sandbox:abc"));
        assert!(!err.model_message.contains("host-internal-token"));
        assert!(result.content.contains("denied by policy"));
        assert!(!result.content.contains("host-internal-token"));
        assert!(!result.content.contains("raw diagnostic"));
        assert_eq!(
            result.details["runtimeDiagnostics"][0]["message"],
            "raw diagnostic"
        );
    }

    #[test]
    fn bash_network_policy_failure_keeps_actionable_fact_but_hides_host_details() {
        let mut layer = ToolLayer::new();
        layer.register(Arc::new(StaticBashHandler {
            details: json!({
                "schema": "bash_result_v1",
                "command": "curl https://localhost",
                "executed": true,
                "exitCode": 5,
                "timedOut": false,
                "stdout": "",
                "stderr": "curl: CONNECT tunnel failed, response 403",
                "runtimeDiagnostics": [{
                    "source": "networkProxy",
                    "stream": "internal",
                    "severity": "warning",
                    "code": "network_policy_denied",
                    "message": "private address blocked",
                    "details": {
                        "targetHost": "localhost",
                        "targetPort": 443,
                        "networkPolicyMode": "publicInternet"
                    }
                }]
            }),
        }));

        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-bash-network-denied".to_string(),
            tool_name: "bash".to_string(),
            args_json: r#"{"command":"curl https://localhost"}"#.to_string(),
        });

        assert_eq!(result.status, "error");
        assert!(result.content.contains("exit code 5"));
        assert!(result.content.contains("response 403"));
        assert!(result.content.contains("network policy denied"));
        assert!(!result.content.contains("targetHost"));
        assert!(!result.content.contains("publicInternet"));
        assert_eq!(
            result.details["runtimeDiagnostics"][0]["details"]["targetHost"],
            "localhost"
        );
    }

    #[test]
    fn bash_network_host_diagnostics_keep_failure_categories_distinct() {
        let details = json!({
            "runtimeDiagnostics": [
                { "code": "network_dns_failed" },
                { "code": "network_unreachable" },
                { "code": "network_policy_denied" }
            ]
        });
        let messages = super::network_diagnostic_model_messages(&details);

        assert_eq!(messages.len(), 3);
        assert!(messages.iter().any(|message| message.contains("DNS")));
        assert!(messages
            .iter()
            .any(|message| message.contains("unreachable")));
        assert!(messages
            .iter()
            .any(|message| message.contains("policy denied")));
    }

    #[test]
    fn write_tool_creates_workspace_file() {
        let layer = tool_layer_with_test_workspace_root(ToolLayer::new());
        let relative = format!(
            "target/centaeris-tool-layer-tests/{}/write.txt",
            unique_suffix()
        );
        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-write".to_string(),
            tool_name: "write".to_string(),
            args_json: json!({
                "path": relative.clone(),
                "content": "alpha\nbeta\n"
            })
            .to_string(),
        });
        assert_eq!(result.status, "ok");
        let payload = &result.details;
        let path = payload
            .get("path")
            .and_then(Value::as_str)
            .expect("write path");
        assert_eq!(path, relative);
        assert_eq!(
            payload
                .get("fileFact")
                .and_then(|item| item.get("schema"))
                .and_then(Value::as_str),
            Some("file_write_fact_v1")
        );
        assert_eq!(
            fs::read_to_string(layer.cwd().expect("working directory").join(path),)
                .expect("read written file"),
            "alpha\nbeta\n"
        );
    }

    #[test]
    fn edit_tool_updates_workspace_file_with_exact_text() {
        let layer = tool_layer_with_test_workspace_root(ToolLayer::new());
        let relative = format!(
            "target/centaeris-tool-layer-tests/{}/edit.txt",
            unique_suffix()
        );
        let parent = std::path::Path::new(&relative)
            .parent()
            .expect("relative parent");
        fs::create_dir_all(parent).expect("create edit parent");
        fs::write(&relative, "alpha\nbeta\ngamma\n").expect("write edit source");
        let read_result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-edit-read".to_string(),
            tool_name: "read".to_string(),
            args_json: json!({ "path": relative.clone() }).to_string(),
        });
        assert_eq!(read_result.status, "ok");
        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-edit".to_string(),
            tool_name: "edit".to_string(),
            args_json: json!({
                "path": relative.clone(),
                "edits": [{
                    "old_text": "alpha\nbeta\ngamma",
                    "new_text": "alpha\nbravo\ngamma"
                }]
            })
            .to_string(),
        });
        assert_eq!(result.status, "ok");
        let payload = &result.details;
        assert_eq!(
            payload.get("schema").and_then(Value::as_str),
            Some("edit_result_v1")
        );
        assert_eq!(
            fs::read_to_string(&relative).expect("read edited file"),
            "alpha\nbravo\ngamma\n"
        );
    }

    #[test]
    fn tool_layer_concurrency_safety_uses_dynamic_contract() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "clinic_writeback".to_string(),
            category: "external.write".to_string(),
            summary: "Write back to external clinic system.".to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: "clinic.local".to_string(),
            scopes: vec!["clinic:write".to_string()],
            concurrency_safe: false,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));

        assert!(layer.is_concurrency_safe("read"));
        assert!(!layer.is_concurrency_safe("bash"));
        assert!(layer.can_handle("clinic_writeback"));
        assert!(!layer.is_concurrency_safe("clinic_writeback"));
    }

    #[tokio::test]
    async fn dynamic_tool_provider_executes_registered_contract() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "ragflow_clinic_search".to_string(),
            category: "external.context".to_string(),
            summary: "Search clinic knowledge base through RagFlow.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            provider_id: "ragflow.local".to_string(),
            scopes: vec!["kb:read".to_string()],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let observed_args = Arc::new(Mutex::new(Vec::new()));
        let mut layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));
        layer
            .register_dynamic_tool_provider(Arc::new(EchoDynamicProvider {
                provider_id: "ragflow.local".to_string(),
                observed_args: observed_args.clone(),
            }))
            .expect("dynamic provider binding");

        assert!(layer.can_handle("ragflow_clinic_search"));
        assert!(layer.is_concurrency_safe("ragflow_clinic_search"));
        let result = layer
            .execute_async(ToolInvocationRequest {
                tool_call_id: "c-dyn-exec".to_string(),
                tool_name: "ragflow_clinic_search".to_string(),
                args_json: json!({ "query": "hypertension guideline" }).to_string(),
            })
            .await;

        assert_eq!(result.status, "ok");
        assert_eq!(
            result.transition_reason.as_deref(),
            Some("test_dynamic_provider_exec")
        );
        let payload = &result.details;
        assert_eq!(
            payload.get("dynamicTool").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            payload.get("providerId").and_then(Value::as_str),
            Some("ragflow.local")
        );
        let runtime_schema_hash = layer
            .tool_contract("ragflow_clinic_search")
            .expect("dynamic runtime contract")
            .schema_hash
            .expect("dynamic runtime schema hash");
        assert_ne!(runtime_schema_hash, "schema-v1");
        assert!(layer.dynamic_tool_providers.contains_key(&(
            "ragflow.local".to_string(),
            "ragflow_clinic_search".to_string(),
            runtime_schema_hash.clone(),
        )));
        assert!(!layer.dynamic_tool_providers.contains_key(&(
            "ragflow.local".to_string(),
            "ragflow_clinic_search".to_string(),
            "banana".to_string(),
        )));
        assert_eq!(
            payload.get("schemaHash").and_then(Value::as_str),
            Some(runtime_schema_hash.as_str())
        );
        assert_eq!(
            payload
                .get("result")
                .and_then(|item| item.get("args"))
                .and_then(|item| item.get("query"))
                .and_then(Value::as_str),
            Some("hypertension guideline")
        );
        assert_eq!(
            observed_args.lock().expect("observed args").as_slice(),
            &[json!({ "query": "hypertension guideline" }).to_string()]
        );
        let error_result = layer
            .execute_async(ToolInvocationRequest {
                tool_call_id: "c-dyn-error".to_string(),
                tool_name: "ragflow_clinic_search".to_string(),
                args_json: json!({ "query": "banana" }).to_string(),
            })
            .await;
        assert_eq!(error_result.status, "error");
        assert!(error_result.error.is_some());
        assert!(layer
            .register_dynamic_tool_provider(Arc::new(EchoDynamicProvider {
                provider_id: "ragflow.local".to_string(),
                observed_args: Arc::new(Mutex::new(Vec::new())),
            }))
            .expect_err("duplicate provider binding must fail")
            .contains("duplicate dynamic tool provider binding"));
        assert!(layer
            .register_dynamic_tool_provider(Arc::new(EchoDynamicProvider {
                provider_id: "banana".to_string(),
                observed_args: Arc::new(Mutex::new(Vec::new())),
            }))
            .expect_err("unknown provider must fail")
            .contains("no authorized contract"));
    }

    #[tokio::test]
    async fn running_dynamic_tool_provider_observes_agent_run_cancellation() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "ragflow_clinic_search".to_string(),
            category: "external.context".to_string(),
            summary: "Search clinic knowledge base through RagFlow.".to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: "ragflow.local".to_string(),
            scopes: vec!["kb:read".to_string()],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let entered = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation_state = cancelled.clone();
        let mut layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry))
            .with_execution_cancellation_probe(Arc::new(move || {
                Ok(cancellation_state
                    .load(Ordering::SeqCst)
                    .then(|| "agent_run_cancel_requested".to_string()))
            }));
        layer
            .register_dynamic_tool_provider(Arc::new(CancellationAwareDynamicProvider {
                entered: entered.clone(),
            }))
            .expect("dynamic provider binding");

        let task = tokio::spawn(async move {
            layer
                .execute_async(ToolInvocationRequest {
                    tool_call_id: "c-dyn-cancel".to_string(),
                    tool_name: "ragflow_clinic_search".to_string(),
                    args_json: "{}".to_string(),
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !entered.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("provider entered execution");
        cancelled.store(true, Ordering::SeqCst);

        let result = tokio::time::timeout(std::time::Duration::from_millis(250), task)
            .await
            .expect("provider observed cancellation")
            .expect("provider task");
        assert_eq!(result.status, "cancelled");
        assert_eq!(
            result.error.as_ref().map(|error| &error.kind),
            Some(&ToolFailureKind::Cancelled)
        );
        assert_eq!(
            result.transition_reason.as_deref(),
            Some("dynamic_tool_provider_cancelled")
        );
        assert_eq!(
            result.details.get("reason").and_then(Value::as_str),
            Some("agent_run_cancel_requested")
        );
    }

    #[tokio::test]
    async fn dynamic_tool_provider_missing_returns_observable_error() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "ragflow_clinic_search".to_string(),
            category: "external.context".to_string(),
            summary: "Search clinic knowledge base through RagFlow.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            provider_id: "ragflow.local".to_string(),
            scopes: vec!["kb:read".to_string()],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));

        let result = layer
            .execute_async(ToolInvocationRequest {
                tool_call_id: "c-dyn-missing".to_string(),
                tool_name: "ragflow_clinic_search".to_string(),
                args_json: json!({ "query": "hypertension guideline" }).to_string(),
            })
            .await;

        assert_eq!(result.status, "error");
        assert_eq!(
            result.transition_reason.as_deref(),
            Some("dynamic_tool_provider_missing")
        );
        assert!(result
            .error
            .as_ref()
            .map(|e| e.model_message.as_str())
            .unwrap_or_default()
            .contains("ragflow.local"));
        assert_eq!(
            result.details.get("dynamicTool").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[tokio::test]
    async fn dynamic_tool_provider_error_is_structured_without_raw_diagnostic() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "ragflow_clinic_search".to_string(),
            category: "external.context".to_string(),
            summary: "Search clinic knowledge base through RagFlow.".to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: "ragflow.local".to_string(),
            scopes: vec!["kb:read".to_string()],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let mut layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));
        layer
            .register_dynamic_tool_provider(Arc::new(FailingDynamicProvider))
            .expect("dynamic provider binding");

        let result = layer
            .execute_async(ToolInvocationRequest {
                tool_call_id: "c-dyn-error".to_string(),
                tool_name: "ragflow_clinic_search".to_string(),
                args_json: "{}".to_string(),
            })
            .await;

        assert_eq!(result.status, "error");
        assert_eq!(result.content, "dynamic tool provider returned an error");
        assert_eq!(
            result.details.get("error").and_then(Value::as_str),
            Some("dynamic tool provider returned an error")
        );
        let error = result.error.expect("structured provider error");
        assert_eq!(
            error.model_message,
            "dynamic tool provider returned an error"
        );
        assert_eq!(error.user_message, "Dynamic tool execution failed");
        assert!(!error.model_message.contains("secret provider diagnostic"));
        assert!(!error.user_message.contains("secret provider diagnostic"));
        assert!(!result
            .details
            .to_string()
            .contains("secret provider diagnostic"));
    }

    #[tokio::test]
    async fn dynamic_tool_provider_preserves_typed_retryable_error() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "ragflow_clinic_search".to_string(),
            category: "external.context".to_string(),
            summary: "Search clinic knowledge base through RagFlow.".to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: "ragflow.local".to_string(),
            scopes: vec!["kb:read".to_string()],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let mut layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));
        layer
            .register_dynamic_tool_provider(Arc::new(RetryableDynamicProvider))
            .expect("dynamic provider binding");

        let result = layer
            .execute_async(ToolInvocationRequest {
                tool_call_id: "c-dyn-timeout".to_string(),
                tool_name: "ragflow_clinic_search".to_string(),
                args_json: "{}".to_string(),
            })
            .await;

        let error = result.error.expect("typed provider error");
        assert_eq!(error.kind, ToolFailureKind::TimedOut);
        assert_eq!(error.model_message, "provider poll timed out");
        assert_eq!(
            error.diagnostic_id.as_deref(),
            Some("provider-poll-timeout")
        );
        assert!(error.retryable);
    }

    #[test]
    fn read_tool_reads_project_file() {
        let layer = tool_layer_with_test_workspace_root(ToolLayer::new());
        let path = project_file_path_for_tests();
        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-read".to_string(),
            tool_name: "read".to_string(),
            args_json: format!("{{\"path\":\"{}\",\"limit\":20}}", path),
        });
        assert_eq!(result.status, "ok");
        assert!(result.content.contains("pub mod"));
    }

    #[test]
    fn read_tool_rejects_binary_document_formats() {
        let workspace_root =
            std::env::temp_dir().join(format!("centaeris_read_pdf_reject_{}", unique_suffix()));
        fs::create_dir_all(workspace_root.as_path()).expect("workspace root");
        fs::write(workspace_root.join("scan.pdf"), b"%PDF-1.7\n").expect("write pdf");
        let layer = ToolLayer::new()
            .with_cwd(workspace_root.clone())
            .expect("workspace");

        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-read-pdf".to_string(),
            tool_name: "read".to_string(),
            args_json: json!({ "path": "scan.pdf" }).to_string(),
        });

        assert_eq!(result.status, "error");
        let payload = &result.details;
        assert!(payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("hosted Processing Pipeline"));

        remove_dir_all_with_retries(workspace_root.as_path(), 6);
    }

    #[test]
    fn read_tool_directory_error_is_structured_without_raw_path() {
        let workspace_root = std::env::temp_dir().join(format!(
            "centaeris_read_directory_error_{}",
            unique_suffix()
        ));
        fs::create_dir_all(workspace_root.join("skill").as_path()).expect("create skill dir");
        let layer = ToolLayer::new()
            .with_cwd(workspace_root.clone())
            .expect("set workspace root");

        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-read-dir".to_string(),
            tool_name: "read".to_string(),
            args_json: json!({ "path": "skill" }).to_string(),
        });

        assert_eq!(result.status, "error");
        let err = result.error.as_ref().expect("structured error");
        assert_eq!(err.kind, ToolFailureKind::InvalidInput);
        assert_eq!(
            err.model_message,
            "Read target is not a file; provide a file path instead of a directory"
        );
        assert_eq!(err.user_message, "Read target is not a file");
        assert!(!err.model_message.contains("centaeris_read_directory_error"));
        assert!(!err.user_message.contains("centaeris_read_directory_error"));
        let payload = &result.details;
        assert_eq!(
            payload.get("schema").and_then(Value::as_str),
            Some("file_tool_rejected_v1")
        );
        assert_eq!(
            payload
                .get("fileFact")
                .and_then(|item| item.get("schema"))
                .and_then(Value::as_str),
            Some("file_tool_rejected_fact_v1")
        );

        remove_dir_all_with_retries(workspace_root.as_path(), 6);
    }

    #[test]
    fn local_file_tools_require_explicit_cwd() {
        let layer = ToolLayer::new();
        let result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-read-no-root".to_string(),
            tool_name: "read".to_string(),
            args_json: "{\"path\":\"Cargo.toml\"}".to_string(),
        });
        assert_eq!(result.status, "error");
        assert_eq!(
            result.error.as_ref().map(|error| &error.kind),
            Some(&ToolFailureKind::HostUnavailable),
            "result={result:#?}"
        );
        assert_eq!(
            result.details.get("message").and_then(Value::as_str),
            Some("execution host binding is required for filesystem and command tools")
        );
    }

    #[test]
    fn local_file_tools_reject_paths_outside_policy_roots() {
        let workspace_root =
            std::env::temp_dir().join(format!("centaeris_tool_workspace_root_{}", unique_suffix()));
        let outside_root = std::env::temp_dir().join(format!(
            "centaeris_tool_workspace_outside_{}",
            unique_suffix()
        ));
        fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
        fs::create_dir_all(outside_root.as_path()).expect("create outside root");
        let outside_file = outside_root.join("outside.txt");
        fs::write(outside_file.as_path(), "outside").expect("write outside file");

        let layer = ToolLayer::new()
            .with_cwd(workspace_root.clone())
            .expect("set workspace root");
        let read_result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-read-outside-root".to_string(),
            tool_name: "read".to_string(),
            args_json: json!({
                "path": outside_file.to_string_lossy(),
            })
            .to_string(),
        });
        assert_eq!(read_result.status, "error", "result={read_result:#?}");
        assert_eq!(
            read_result.error.as_ref().map(|error| &error.kind),
            Some(&ToolFailureKind::PermissionDenied)
        );

        let bash_result = layer.execute(ToolInvocationRequest {
            tool_call_id: "c-bash-cwd-outside-root".to_string(),
            tool_name: "bash".to_string(),
            args_json: json!({
                "command": "pwd",
                "cwd": outside_root.to_string_lossy(),
            })
            .to_string(),
        });
        assert_eq!(bash_result.status, "error");
        assert!(bash_result
            .error
            .as_ref()
            .map(|e| e.model_message.as_str())
            .unwrap_or_default()
            .contains("Bash arguments contain unknown field: cwd"));

        remove_dir_all_with_retries(workspace_root.as_path(), 6);
        remove_dir_all_with_retries(outside_root.as_path(), 6);
    }

    #[test]
    fn unsupported_local_tools_fail_loudly() {
        let layer = ToolLayer::new();
        assert!(!layer.can_handle("UnknownToolForTest"));
        assert!(!layer.can_handle("AnotherUnknownToolForTest"));
        for tool_name in ["UnknownToolForTest", "AnotherUnknownToolForTest"] {
            let result = layer.execute(ToolInvocationRequest {
                tool_call_id: format!("c-{tool_name}"),
                tool_name: tool_name.to_string(),
                args_json: "{}".to_string(),
            });
            assert_eq!(result.status, "error", "{tool_name}");
            assert_eq!(
                result.transition_reason.as_deref(),
                Some("local_tool_unsupported"),
                "{tool_name}"
            );
            assert!(result
                .error
                .as_ref()
                .map(|e| e.model_message.as_str())
                .unwrap_or_default()
                .contains("unsupported local tool"));
        }
    }

    fn result_state_report(status: &str, tool_name: &str, details: Value) -> ToolExecutionResult {
        ToolExecutionResult {
            tool_call_id: "call-state".to_string(),
            tool_name: tool_name.to_string(),
            status: status.to_string(),
            content: details.to_string(),
            details,
            facts: Vec::new(),
            error: None,
            started_at_ms: 1,
            completed_at_ms: 2,
            latency_ms: 1,
            parallel_group: None,
            transition_reason: None,
        }
    }

    #[test]
    fn tool_result_state_does_not_treat_shell_commands_or_storage_as_outcomes() {
        let no_output = result_state_report(
            "ok",
            "bash",
            json!({
                "schema": "bash_result_v1",
                "command": "true",
                "exitCode": 0,
                "stdout": "",
                "stderr": ""
            }),
        );
        assert_eq!(no_output.result_state(), ToolResultState::SuccessNoOutput);

        let no_matches = result_state_report(
            "error",
            "bash",
            json!({
                "schema": "bash_result_v1",
                "command": "rg -n \"not-present\" core/src",
                "exitCode": 1,
                "stdout": "",
                "stderr": ""
            }),
        );
        assert_eq!(no_matches.result_state(), ToolResultState::Failed);

        let failed = result_state_report(
            "error",
            "bash",
            json!({
                "schema": "bash_result_v1",
                "command": "rg -n \"[\" core/src",
                "exitCode": 2,
                "stdout": "",
                "stderr": "regex parse error"
            }),
        );
        assert_eq!(failed.result_state(), ToolResultState::Failed);

        let externalized = result_state_report(
            "ok",
            "bash",
            json!({
                "result": {
                    "externalObject": {
                        "object": {
                            "objectId": "external_context:tool_output_1"
                        }
                    }
                }
            }),
        );
        assert_eq!(
            externalized.result_state(),
            ToolResultState::SuccessWithOutput
        );
    }
}
