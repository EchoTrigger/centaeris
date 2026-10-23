use super::*;
use crate::execution::{
    ExecutionCommandRequest, ExecutionError, ExecutionFileIdentity, ExecutionFileReadOutput,
    ExecutionFileSystemError, ExecutionFileSystemErrorKind, ExecutionFileSystemOperation,
    ExecutionFileSystemOutput, ExecutionFileSystemRequest, ExecutionHostBinding,
    ExecutionHostCommandOutput, ExecutionHostKind, ExecutionHostMode, ExecutionHostRunner,
    ExecutionHostStatus, ExecutionPolicy,
};
use crate::tool::layer::{ToolInvocationRequest, WorkspaceInstructionReadPort};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const MAX_BYTES: usize = 32_768 * 4;

struct LiveFiles {
    reply: Mutex<Result<Vec<u8>, ExecutionFileSystemError>>,
    reads: AtomicUsize,
}

impl LiveFiles {
    fn new(bytes: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            reply: Mutex::new(Ok(bytes.to_vec())),
            reads: AtomicUsize::new(0),
        })
    }

    fn read(&self, cwd: &Path, max_bytes: usize) -> Result<Vec<u8>, ExecutionFileSystemError> {
        assert!(cwd.is_absolute());
        self.reads.fetch_add(1, Ordering::SeqCst);
        let bytes = self.reply.lock().unwrap().clone()?;
        if bytes.len() > max_bytes {
            return Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::TooLarge,
                "live instruction file exceeds byte limit",
            ));
        }
        Ok(bytes)
    }
}

impl ExecutionHostRunner for LiveFiles {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::RemoteHost
    }

    fn status(&self, _policy: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        panic!("instruction reads must not ask for host status");
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        assert_eq!(request.model_path, "AGENTS.md");
        let ExecutionFileSystemOperation::ReadFile { max_bytes } = request.operation else {
            panic!("instruction fixture only permits reads");
        };
        let bytes = self.read(&request.cwd, max_bytes)?;
        Ok(ExecutionFileSystemOutput::ReadFile(
            ExecutionFileReadOutput {
                identity: ExecutionFileIdentity {
                    key: "live-agents-file".into(),
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
        _request: ExecutionCommandRequest,
        _cancellation_probe: Option<&crate::execution::ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        panic!("instruction reads must not execute commands");
    }
}

struct InstructionSource {
    reply: Mutex<Result<Vec<u8>, ExecutionFileSystemError>>,
    live: Arc<LiveFiles>,
    materialized: AtomicBool,
    reads: Mutex<Vec<(PathBuf, usize)>>,
}

impl InstructionSource {
    fn new(live: Arc<LiveFiles>, bytes: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            reply: Mutex::new(Ok(bytes.to_vec())),
            live,
            materialized: AtomicBool::new(false),
            reads: Mutex::new(Vec::new()),
        })
    }
}

impl WorkspaceInstructionReadPort for InstructionSource {
    fn read_agents_file(
        &self,
        cwd: &Path,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ExecutionFileSystemError> {
        self.reads
            .lock()
            .unwrap()
            .push((cwd.to_path_buf(), max_bytes));
        if self.materialized.load(Ordering::SeqCst) {
            self.live.read(cwd, max_bytes)
        } else {
            // Return exact provider bytes even when over budget: Core must
            // independently enforce its own prompt input limits.
            self.reply.lock().unwrap().clone()
        }
    }
}

fn fixture() -> (ToolLayer, Arc<LiveFiles>, Arc<InstructionSource>) {
    let live = LiveFiles::new(b"Live instruction bytes.\n");
    let source = InstructionSource::new(live.clone(), b"Frozen instruction bytes.\n");
    let cwd = std::env::temp_dir();
    let binding = ExecutionHostBinding::new(
        ExecutionHostMode::Remote,
        live.clone(),
        cwd.clone(),
        ExecutionPolicy::workspace_write_no_network(&cwd),
    )
    .unwrap();
    let tools = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        Arc::new(binding),
    )
    .unwrap();
    (tools, live, source)
}

#[test]
fn query_loop_workspace_instruction_port_overrides_only_startup_reads_and_clones() {
    let (tools, live, source) = fixture();
    let tools = tools.with_workspace_instruction_read_port(source.clone());
    let clone = tools.clone();
    for selected in [&tools, &clone] {
        assert_eq!(
            selected.read_agents_instructions().unwrap().unwrap().0,
            "Frozen instruction bytes.\n"
        );
    }
    assert_eq!(live.reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        source.reads.lock().unwrap().as_slice(),
        &[
            (std::env::temp_dir(), MAX_BYTES),
            (std::env::temp_dir(), MAX_BYTES)
        ]
    );
    let result = clone.execute(ToolInvocationRequest {
        tool_call_id: "read-live".into(),
        tool_name: "read".into(),
        args_json: r#"{"path":"AGENTS.md"}"#.into(),
    });
    assert_eq!(result.status, "ok", "{result:?}");
    assert!(result.content.contains("Live instruction bytes."));
    assert_eq!(live.reads.load(Ordering::SeqCst), 1);
    assert_eq!(source.reads.lock().unwrap().len(), 2);
}

#[test]
fn query_loop_workspace_instruction_port_not_found_and_failure_are_not_live_fallback() {
    let (tools, live, source) = fixture();
    let tools = tools.with_workspace_instruction_read_port(source.clone());
    *source.reply.lock().unwrap() = Err(ExecutionFileSystemError::new(
        ExecutionFileSystemErrorKind::NotFound,
        "authoritative startup file absent",
    ));
    assert_eq!(tools.read_agents_instructions().unwrap(), None);
    for kind in [
        ExecutionFileSystemErrorKind::PermissionDenied,
        ExecutionFileSystemErrorKind::HostUnavailable,
        ExecutionFileSystemErrorKind::Io,
    ] {
        *source.reply.lock().unwrap() = Err(ExecutionFileSystemError::new(
            kind,
            "instruction read rejected",
        ));
        assert_eq!(
            tools.read_agents_instructions().unwrap_err(),
            "read AGENTS.md failed: instruction read rejected"
        );
    }
    assert_eq!(source.reads.lock().unwrap().len(), 4);
    assert_eq!(live.reads.load(Ordering::SeqCst), 0);
}

#[test]
fn query_loop_workspace_instruction_port_retains_core_bytes_utf8_character_and_hash_checks() {
    let (tools, live, source) = fixture();
    let tools = tools.with_workspace_instruction_read_port(source.clone());
    for (bytes, reason) in [
        (vec![b'x'; MAX_BYTES + 1], "exceeds byte limit"),
        (vec![0xff, 0xfe], "must be valid UTF-8"),
        (vec![b'x'; 32_769], "exceeds character limit"),
    ] {
        *source.reply.lock().unwrap() = Ok(bytes);
        assert!(tools
            .read_agents_instructions()
            .unwrap_err()
            .contains(reason));
    }
    for bytes in [Vec::new(), "😀".repeat(32_768).into_bytes()] {
        *source.reply.lock().unwrap() = Ok(bytes.clone());
        assert_eq!(
            tools.read_agents_instructions().unwrap(),
            Some((
                String::from_utf8(bytes.clone()).unwrap(),
                format!("sha256:{:x}", Sha256::digest(&bytes))
            ))
        );
    }
    assert_eq!(source.reads.lock().unwrap().len(), 5);
    assert_eq!(live.reads.load(Ordering::SeqCst), 0);
}

#[test]
fn query_loop_workspace_instruction_port_updates_model_context_cache_and_live_source() {
    let (tools, live, source) = fixture();
    let store = AgentRuntimeTestStore::new();
    let config = AgentRuntimeConfig {
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        tools
            .clone()
            .with_workspace_instruction_read_port(source.clone()),
        config.clone(),
    );
    let first = engine
        .build_generate_driver_request("instruction-session", "instruction-turn", "inspect", 0)
        .unwrap();
    let first_message = first
        .prepared_prompt
        .messages
        .iter()
        .find(|message| message.message_id.ends_with(":agents_context"))
        .unwrap();
    assert_eq!(
        first_message.content,
        prompt_projection::build_agents_context_message(
            "instruction-session",
            "instruction-turn",
            std::env::temp_dir().as_path(),
            "Frozen instruction bytes.\n",
        )
        .content
    );
    assert_eq!(live.reads.load(Ordering::SeqCst), 0);
    let frozen_reads = source.reads.lock().unwrap().len();
    assert!(frozen_reads > 0);

    source.materialized.store(true, Ordering::SeqCst);
    let second = engine
        .build_generate_driver_request("instruction-session", "instruction-turn", "inspect", 0)
        .unwrap();
    assert_ne!(
        first.provider_prompt_cache_key,
        second.provider_prompt_cache_key
    );
    assert!(second.prepared_prompt.messages.iter().any(|message| {
        message.message_id.ends_with(":agents_context")
            && message.content.contains("Live instruction bytes.")
    }));
    let port_live_reads = live.reads.load(Ordering::SeqCst);
    assert_eq!(
        source.reads.lock().unwrap().len() - frozen_reads,
        port_live_reads
    );
    let default_engine = AgentRuntime::new_for_test_with_tools(store.clone(), tools, config);
    let default_live = default_engine
        .build_generate_driver_request("instruction-session", "instruction-turn", "inspect", 0)
        .unwrap();
    assert_eq!(
        second.provider_prompt_cache_key,
        default_live.provider_prompt_cache_key
    );
    assert_eq!(second.prepared_prompt, default_live.prepared_prompt);
    // Core projects once for token estimation and again for the final request.
    // Compare actual reads with the live path instead of fixing that internal count.
    let default_live_reads = live.reads.load(Ordering::SeqCst) - port_live_reads;
    assert_eq!(port_live_reads, default_live_reads);

    let answer_now = engine
        .build_generate_driver_request_with_runtime_scope(
            "instruction-session",
            "instruction-answer-now",
            &TurnInput::AnswerNow {
                attachments: Vec::new(),
                message: "answer now".into(),
                intervention: AgentRunInterventionV1::answer_now("instruction-stop", "agent-run"),
                supplement_ids: Vec::new(),
            },
            0,
            PromptCompactionScopeV1::main(),
        )
        .unwrap();
    assert!(answer_now.prepared_prompt.messages.iter().any(|message| {
        message.message_id.ends_with(":agents_context")
            && message.content.contains("Live instruction bytes.")
    }));
    assert_eq!(
        source.reads.lock().unwrap().len(),
        frozen_reads + live.reads.load(Ordering::SeqCst) - default_live_reads
    );
    let persisted = SessionManager::new(store)
        .load_or_create_session("instruction-session")
        .unwrap();
    assert!(persisted
        .messages
        .iter()
        .all(|message| { !message.content.contains("# AGENTS.md instructions") }));
}

#[test]
fn query_loop_workspace_instruction_port_preserves_unbound_and_default_live_behavior() {
    let (tools, live, source) = fixture();
    assert_eq!(
        tools.read_agents_instructions().unwrap().unwrap().0,
        "Live instruction bytes.\n"
    );
    assert_eq!(live.reads.load(Ordering::SeqCst), 1);
    assert!(source.reads.lock().unwrap().is_empty());

    let unbound = ToolLayer::new().with_workspace_instruction_read_port(source.clone());
    assert_eq!(unbound.read_agents_instructions().unwrap(), None);
    assert!(source.reads.lock().unwrap().is_empty());
}
