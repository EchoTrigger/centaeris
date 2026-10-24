use super::*;
use crate::execution::{
    ExecutionFileIdentity, ExecutionFileSystemError, ExecutionFileSystemErrorKind,
    ExecutionFileSystemOutput, ExecutionFileSystemRequest, ExecutionHostBinding, ExecutionHostKind,
    ExecutionHostMode, ExecutionHostRunner, ExecutionPolicy,
};
use crate::tool::layer::ToolInvocationRequest;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Remote host whose workspace cannot be reached: every file operation fails
/// with HostUnavailable and any command attempt is a contract violation.
struct UnavailableHost {
    file_operations: AtomicUsize,
}

impl ExecutionHostRunner for UnavailableHost {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::RemoteHost
    }

    fn status(
        &self,
        _policy: &ExecutionPolicy,
    ) -> Result<crate::execution::ExecutionHostStatus, crate::execution::ExecutionError> {
        panic!("model request construction must not ask for host status");
    }

    fn run_file_system_operation(
        &self,
        _request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        self.file_operations.fetch_add(1, Ordering::SeqCst);
        Err(ExecutionFileSystemError::new(
            ExecutionFileSystemErrorKind::HostUnavailable,
            "workspace unavailable",
        ))
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        _request: crate::execution::ExecutionCommandRequest,
        _cancellation_probe: Option<&crate::execution::ExecutionCancellationProbe>,
    ) -> Result<crate::execution::ExecutionHostCommandOutput, crate::execution::ExecutionError>
    {
        panic!("model request construction must not execute commands");
    }
}

/// Remote host serving a mutable AGENTS.md while recording every requested
/// file operation, so tests can prove request construction reads nothing.
struct MutableInstructionHost {
    content: Mutex<Vec<u8>>,
    file_operations: Mutex<Vec<String>>,
}

impl MutableInstructionHost {
    fn new(content: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            content: Mutex::new(content.to_vec()),
            file_operations: Mutex::new(Vec::new()),
        })
    }

    fn set_content(&self, content: &[u8]) {
        *self.content.lock().unwrap() = content.to_vec();
    }

    fn requested_paths(&self) -> Vec<String> {
        self.file_operations.lock().unwrap().clone()
    }
}

impl ExecutionHostRunner for MutableInstructionHost {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::RemoteHost
    }

    fn status(
        &self,
        _policy: &ExecutionPolicy,
    ) -> Result<crate::execution::ExecutionHostStatus, crate::execution::ExecutionError> {
        panic!("model request construction must not ask for host status");
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        self.file_operations
            .lock()
            .unwrap()
            .push(request.model_path.clone());
        let bytes = self.content.lock().unwrap().clone();
        Ok(ExecutionFileSystemOutput::ReadFile(
            crate::execution::ExecutionFileReadOutput {
                identity: ExecutionFileIdentity {
                    key: "mutable-agents-file".into(),
                    display_path: "AGENTS.md".into(),
                },
                file_hash: format!("sha256:{:x}", Sha256::digest(&bytes)),
                bytes,
            },
        ))
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        _request: crate::execution::ExecutionCommandRequest,
        _cancellation_probe: Option<&crate::execution::ExecutionCancellationProbe>,
    ) -> Result<crate::execution::ExecutionHostCommandOutput, crate::execution::ExecutionError>
    {
        panic!("model request construction must not execute commands");
    }
}

fn runtime_with_host(host: Arc<dyn ExecutionHostRunner>) -> AgentRuntime<AgentRuntimeTestStore> {
    let cwd = std::env::temp_dir();
    let binding = ExecutionHostBinding::new(
        ExecutionHostMode::Remote,
        host,
        cwd,
        ExecutionPolicy::workspace_write_no_network(std::env::temp_dir()),
    )
    .unwrap();
    let tools = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        Arc::new(binding),
    )
    .unwrap();
    AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        tools,
        AgentRuntimeConfig {
            enable_prompt_compaction: false,
            ..Default::default()
        },
    )
}

#[test]
fn query_loop_workspace_instructions_are_not_probed_when_workspace_is_unavailable() {
    let host = Arc::new(UnavailableHost {
        file_operations: AtomicUsize::new(0),
    });
    let engine = runtime_with_host(host.clone());
    let request = engine
        .build_generate_driver_request("instruction-session", "instruction-turn", "inspect", 0)
        .expect("model requests must build without reachable workspace instructions");
    assert!(
        request.prepared_prompt.messages.iter().all(|message| {
            !message.content.contains("instruction") || !message.content.contains("workspace")
        }),
        "no workspace instruction content may be injected"
    );
    assert_eq!(
        host.file_operations.load(Ordering::SeqCst),
        0,
        "request construction must not probe AGENTS.md"
    );
}

#[test]
fn query_loop_prompt_and_cache_identity_ignore_workspace_instruction_file_changes() {
    let host = MutableInstructionHost::new(b"Unread instruction bytes.\n");
    let engine = runtime_with_host(host.clone());
    let first = engine
        .build_generate_driver_request("instruction-session", "instruction-turn", "inspect", 0)
        .expect("build request without reading AGENTS.md");
    host.set_content(b"Changed instruction bytes.\n");
    let second = engine
        .build_generate_driver_request("instruction-session", "instruction-turn", "inspect", 0)
        .expect("rebuild request after an unread AGENTS.md change");
    assert_eq!(
        first.provider_prompt_cache_key, second.provider_prompt_cache_key,
        "unread AGENTS.md changes must not affect the provider prompt cache identity"
    );
    assert_eq!(
        first.prepared_prompt.messages, second.prepared_prompt.messages,
        "unread AGENTS.md changes must not affect the model prompt"
    );
    for sentinel in ["Unread instruction bytes.", "Changed instruction bytes."] {
        assert!(
            second
                .prepared_prompt
                .messages
                .iter()
                .all(|message| !message.content.contains(sentinel)),
            "workspace instructions must reach the model only through explicit tool reads"
        );
    }
    assert!(
        host.requested_paths().is_empty(),
        "request construction must not read AGENTS.md"
    );
}

#[test]
fn query_loop_workspace_instructions_flow_through_the_ordinary_read_tool() {
    let workspace_root = temp_dir_path("agents_tool_read_workspace");
    std::fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
    std::fs::write(
        workspace_root.join("AGENTS.md"),
        "Use exact protocol names.\n",
    )
    .expect("write AGENTS.md");
    let tools = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("bind workspace root");

    let read = tools.execute(ToolInvocationRequest {
        tool_call_id: "read-agents".into(),
        tool_name: "read".into(),
        args_json: r#"{"path":"AGENTS.md"}"#.into(),
    });
    assert_eq!(read.status, "ok", "{read:?}");
    assert!(read.content.contains("Use exact protocol names."));

    std::fs::remove_file(workspace_root.join("AGENTS.md")).expect("remove AGENTS.md");
    let missing = tools.execute(ToolInvocationRequest {
        tool_call_id: "read-agents-missing".into(),
        tool_name: "read".into(),
        args_json: r#"{"path":"AGENTS.md"}"#.into(),
    });
    assert_ne!(missing.status, "ok", "{missing:?}");
    assert!(!missing.content.is_empty());

    let _ = std::fs::remove_dir_all(workspace_root);
}
