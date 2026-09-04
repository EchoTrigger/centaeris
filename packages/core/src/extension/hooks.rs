use crate::tool::permission::PermissionDecision;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_HOOK_TEXT_BYTES: usize = 4096;
const MAX_HOOK_CONTEXT_ITEMS: usize = 16;
const MAX_HOOK_CONTEXT_TOTAL_BYTES: usize = 8192;
const MAX_HOOK_STREAM_BYTES: usize = 64 * 1024;
const MAX_HOOK_EVENT_TIMEOUT_MS: u64 = 30_000;

pub const LIFECYCLE_HOOK_EVENT_SCHEMA_V1: &str = "lifecycle_hook_event_v1";
pub const LIFECYCLE_HOOK_RESULT_SCHEMA_V1: &str = "lifecycle_hook_result_v1";

static NEXT_HOOK_RUN_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleHookEventNameV1 {
    SessionStart,
    UserPromptSubmit,
    SubagentStart,
    SubagentStop,
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PreCompact,
    PostCompact,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleHookSourceKindV1 {
    User,
    Project,
    Plugin,
    Admin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookSourceV1 {
    pub kind: LifecycleHookSourceKindV1,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookHandlerV1 {
    pub id: String,
    pub event: LifecycleHookEventNameV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub source: LifecycleHookSourceV1,
    pub trusted: bool,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_timeout_ms", rename = "timeoutMs")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookEventV1 {
    pub schema: String,
    pub event: LifecycleHookEventNameV1,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default, rename = "cwd", skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, rename = "toolName", skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(
        default,
        rename = "subagentName",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_name: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookContextV1 {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleHookPermissionDecisionV1 {
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookOutputV1 {
    pub schema: String,
    #[serde(
        default,
        rename = "blockReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub block_reason: Option<String>,
    #[serde(default, rename = "additionalContext")]
    pub additional_context: Vec<LifecycleHookContextV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    #[serde(
        default,
        rename = "updatedInput",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_input: Option<Value>,
    #[serde(
        default,
        rename = "permissionDecision",
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_decision: Option<LifecycleHookPermissionDecisionV1>,
}

impl Default for LifecycleHookOutputV1 {
    fn default() -> Self {
        Self {
            schema: LIFECYCLE_HOOK_RESULT_SCHEMA_V1.to_string(),
            block_reason: None,
            additional_context: Vec::new(),
            diagnostic: None,
            updated_input: None,
            permission_decision: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleHookRunStatusV1 {
    Succeeded,
    Blocked,
    Failed,
    SkippedUntrusted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookRunV1 {
    #[serde(rename = "hookRunId")]
    pub hook_run_id: String,
    #[serde(rename = "handlerId")]
    pub handler_id: String,
    pub event: LifecycleHookEventNameV1,
    pub source: LifecycleHookSourceV1,
    pub status: LifecycleHookRunStatusV1,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: u128,
    #[serde(rename = "completedAtMs")]
    pub completed_at_ms: u128,
    #[serde(default, rename = "exitCode", skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookDispatchResultV1 {
    pub blocked: bool,
    #[serde(
        default,
        rename = "blockReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub block_reason: Option<String>,
    #[serde(default, rename = "additionalContext")]
    pub additional_context: Vec<LifecycleHookContextV1>,
    #[serde(
        default,
        rename = "updatedInput",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_input: Option<Value>,
    #[serde(
        default,
        rename = "permissionDecision",
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_decision: Option<LifecycleHookPermissionDecisionV1>,
    pub runs: Vec<LifecycleHookRunV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleHookCommandResultV1 {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub spawn_error: Option<String>,
}

pub trait LifecycleHookRunner {
    fn run_hook(
        &self,
        handler: &LifecycleHookHandlerV1,
        event: &LifecycleHookEventV1,
    ) -> LifecycleHookCommandResultV1;
}

pub trait LifecycleHookAuditSink {
    fn record_hook_runs(&self, runs: &[LifecycleHookRunV1]) -> Result<(), String>;
}

#[derive(Debug, Clone)]
pub struct LifecycleHookJsonlAuditSinkV1 {
    path: PathBuf,
}

impl LifecycleHookJsonlAuditSinkV1 {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl LifecycleHookAuditSink for LifecycleHookJsonlAuditSinkV1 {
    fn record_hook_runs(&self, runs: &[LifecycleHookRunV1]) -> Result<(), String> {
        if runs.is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create hook audit directory failed: {error}"))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| format!("open hook audit log failed: {error}"))?;
        for run in runs {
            serde_json::to_writer(&mut file, run)
                .map_err(|error| format!("serialize hook audit run failed: {error}"))?;
            file.write_all(b"\n")
                .map_err(|error| format!("write hook audit log failed: {error}"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct LifecycleHookEngineV1 {
    handlers: Vec<LifecycleHookHandlerV1>,
}

impl LifecycleHookEngineV1 {
    pub fn new(handlers: Vec<LifecycleHookHandlerV1>) -> Result<Self, String> {
        validate_handlers(&handlers)?;
        Ok(Self { handlers })
    }

    pub fn list_handlers(&self) -> &[LifecycleHookHandlerV1] {
        &self.handlers
    }

    pub fn dispatch<R: LifecycleHookRunner + ?Sized>(
        &self,
        event: &LifecycleHookEventV1,
        runner: &R,
    ) -> LifecycleHookDispatchResultV1 {
        let mut result = LifecycleHookDispatchResultV1::default();
        let dispatch_started = Instant::now();
        let mut current_event = event.clone();
        for handler in self.select_handlers(event) {
            let started_at_ms = now_ms();
            let mut run = LifecycleHookRunV1 {
                hook_run_id: next_hook_run_id(handler.id.as_str()),
                handler_id: handler.id.clone(),
                event: handler.event,
                source: handler.source.clone(),
                status: LifecycleHookRunStatusV1::Succeeded,
                started_at_ms,
                completed_at_ms: started_at_ms,
                exit_code: None,
                diagnostic: None,
            };

            if !handler.trusted {
                run.status = LifecycleHookRunStatusV1::SkippedUntrusted;
                run.diagnostic = Some("hook handler is not trusted".to_string());
                run.completed_at_ms = now_ms();
                result.runs.push(run);
                continue;
            }

            let remaining_timeout_ms = MAX_HOOK_EVENT_TIMEOUT_MS
                .saturating_sub(dispatch_started.elapsed().as_millis() as u64);
            if remaining_timeout_ms == 0 {
                fail_run(
                    &mut run,
                    &mut result,
                    format!("hook event exceeded {MAX_HOOK_EVENT_TIMEOUT_MS}ms"),
                );
                break;
            }
            let mut execution_handler = handler.clone();
            execution_handler.timeout_ms = execution_handler.timeout_ms.min(remaining_timeout_ms);
            let command_result = runner.run_hook(&execution_handler, &current_event);
            run.exit_code = command_result.exit_code;
            if let Some(error) = command_result.spawn_error {
                fail_run(
                    &mut run,
                    &mut result,
                    format!("hook {} failed to start: {error}", handler.id),
                );
                break;
            }
            if command_result.timed_out {
                fail_run(
                    &mut run,
                    &mut result,
                    format!(
                        "hook {} timed out after {}ms",
                        handler.id, execution_handler.timeout_ms
                    ),
                );
                break;
            }
            if command_result.stdout_truncated {
                fail_run(
                    &mut run,
                    &mut result,
                    format!("hook {} stdout exceeded bounded output limit", handler.id),
                );
                break;
            }
            if command_result.exit_code != Some(0) {
                let stderr =
                    bounded_hook_text(command_result.stderr.as_str()).unwrap_or_else(|error| error);
                fail_run(
                    &mut run,
                    &mut result,
                    format!("hook {} failed: {stderr}", handler.id),
                );
                break;
            }

            match parse_hook_output(event.event, &command_result.stdout) {
                Ok(output) => {
                    if let Err(error) =
                        apply_hook_output(event.event, output, &mut result, &mut run)
                    {
                        fail_run(&mut run, &mut result, error);
                        break;
                    }
                    if event.event == LifecycleHookEventNameV1::PreToolUse {
                        if let (Some(payload), Some(updated_input)) = (
                            current_event.payload.as_object_mut(),
                            result.updated_input.as_ref(),
                        ) {
                            payload.insert("toolInput".to_string(), updated_input.clone());
                        }
                    }
                    run.completed_at_ms = now_ms();
                    result.runs.push(run);
                    if result.blocked {
                        break;
                    }
                }
                Err(error) => {
                    fail_run(&mut run, &mut result, error);
                    break;
                }
            }
        }
        result
    }

    pub fn dispatch_and_record<R: LifecycleHookRunner + ?Sized, S: LifecycleHookAuditSink>(
        &self,
        event: &LifecycleHookEventV1,
        runner: &R,
        audit_sink: &S,
    ) -> Result<LifecycleHookDispatchResultV1, String> {
        let result = self.dispatch(event, runner);
        audit_sink.record_hook_runs(result.runs.as_slice())?;
        Ok(result)
    }

    fn select_handlers<'a>(
        &'a self,
        event: &'a LifecycleHookEventV1,
    ) -> impl Iterator<Item = &'a LifecycleHookHandlerV1> + 'a {
        self.handlers
            .iter()
            .filter(move |handler| handler.event == event.event && matcher_matches(handler, event))
    }
}

#[derive(Debug, Clone, Default)]
pub struct LocalLifecycleHookCommandRunnerV1 {
    environment_overrides: HashMap<String, String>,
}

impl LocalLifecycleHookCommandRunnerV1 {
    pub fn with_environment_overrides(environment_overrides: HashMap<String, String>) -> Self {
        Self {
            environment_overrides,
        }
    }
}

impl LifecycleHookRunner for LocalLifecycleHookCommandRunnerV1 {
    fn run_hook(
        &self,
        handler: &LifecycleHookHandlerV1,
        event: &LifecycleHookEventV1,
    ) -> LifecycleHookCommandResultV1 {
        run_local_hook_command(handler, event, &self.environment_overrides)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookHandlerDiagnosticsV1 {
    pub id: String,
    pub event: LifecycleHookEventNameV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub source: LifecycleHookSourceV1,
    pub trusted: bool,
    pub program: String,
    pub args: Vec<String>,
    #[serde(rename = "timeoutMs")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleHookDiagnosticsProjectionV1 {
    pub handlers: Vec<LifecycleHookHandlerDiagnosticsV1>,
    #[serde(rename = "recentRuns")]
    pub recent_runs: Vec<LifecycleHookRunV1>,
}

pub fn project_lifecycle_hook_diagnostics(
    handlers: &[LifecycleHookHandlerV1],
    recent_runs: &[LifecycleHookRunV1],
) -> LifecycleHookDiagnosticsProjectionV1 {
    LifecycleHookDiagnosticsProjectionV1 {
        handlers: handlers
            .iter()
            .map(|handler| LifecycleHookHandlerDiagnosticsV1 {
                id: handler.id.clone(),
                event: handler.event,
                matcher: handler.matcher.clone(),
                source: handler.source.clone(),
                trusted: handler.trusted,
                program: handler.program.clone(),
                args: handler.args.clone(),
                timeout_ms: handler.timeout_ms,
            })
            .collect(),
        recent_runs: recent_runs.to_vec(),
    }
}

pub fn user_prompt_submit_event(
    session_id: impl Into<String>,
    cwd: Option<String>,
    prompt: impl Into<String>,
) -> LifecycleHookEventV1 {
    LifecycleHookEventV1 {
        schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
        event: LifecycleHookEventNameV1::UserPromptSubmit,
        session_id: session_id.into(),
        cwd,
        tool_name: None,
        subagent_name: None,
        payload: serde_json::json!({ "prompt": prompt.into() }),
    }
}

pub fn pre_tool_use_event(
    session_id: impl Into<String>,
    cwd: Option<String>,
    tool_name: impl Into<String>,
    tool_input: Value,
) -> LifecycleHookEventV1 {
    LifecycleHookEventV1 {
        schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
        event: LifecycleHookEventNameV1::PreToolUse,
        session_id: session_id.into(),
        cwd,
        tool_name: Some(tool_name.into()),
        subagent_name: None,
        payload: serde_json::json!({ "toolInput": tool_input }),
    }
}

pub fn permission_request_event(
    session_id: impl Into<String>,
    cwd: Option<String>,
    tool_name: impl Into<String>,
    permission: &PermissionDecision,
) -> LifecycleHookEventV1 {
    LifecycleHookEventV1 {
        schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
        event: LifecycleHookEventNameV1::PermissionRequest,
        session_id: session_id.into(),
        cwd,
        tool_name: Some(tool_name.into()),
        subagent_name: None,
        payload: serde_json::json!({ "permissionDecision": permission.audit_json() }),
    }
}

pub fn post_tool_use_event(
    session_id: impl Into<String>,
    cwd: Option<String>,
    tool_name: impl Into<String>,
    tool_result: Value,
) -> LifecycleHookEventV1 {
    LifecycleHookEventV1 {
        schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
        event: LifecycleHookEventNameV1::PostToolUse,
        session_id: session_id.into(),
        cwd,
        tool_name: Some(tool_name.into()),
        subagent_name: None,
        payload: serde_json::json!({ "toolResult": tool_result }),
    }
}

pub fn subagent_lifecycle_event(
    event: LifecycleHookEventNameV1,
    session_id: impl Into<String>,
    cwd: Option<String>,
    subagent_name: impl Into<String>,
    payload: Value,
) -> Result<LifecycleHookEventV1, String> {
    if !matches!(
        event,
        LifecycleHookEventNameV1::SubagentStart | LifecycleHookEventNameV1::SubagentStop
    ) {
        return Err("subagent lifecycle helper requires SubagentStart or SubagentStop".to_string());
    }
    Ok(LifecycleHookEventV1 {
        schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
        event,
        session_id: session_id.into(),
        cwd,
        tool_name: None,
        subagent_name: Some(subagent_name.into()),
        payload,
    })
}

pub fn compose_permission_decision_with_hook(
    mut base: PermissionDecision,
    hook_decision: Option<LifecycleHookPermissionDecisionV1>,
) -> PermissionDecision {
    match hook_decision {
        Some(LifecycleHookPermissionDecisionV1::Deny) => {
            base.allowed = false;
            base.reason = format!("permission denied by lifecycle hook: {}", base.reason);
            base.reason_type = "lifecycle_hook_denied".to_string();
            base.policy_source = "lifecycle_hooks".to_string();
            base
        }
        None => base,
    }
}

fn run_local_hook_command(
    handler: &LifecycleHookHandlerV1,
    event: &LifecycleHookEventV1,
    environment_overrides: &HashMap<String, String>,
) -> LifecycleHookCommandResultV1 {
    let stdin_json = match serde_json::to_vec(event) {
        Ok(mut json) => {
            json.push(b'\n');
            json
        }
        Err(error) => return command_spawn_error(format!("serialize hook event failed: {error}")),
    };

    let mut child = match Command::new(handler.program.as_str())
        .args(handler.args.as_slice())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(handler.cwd.as_deref().unwrap_or("."))
        .envs(environment_overrides)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return command_spawn_error(error.to_string()),
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin.write_all(stdin_json.as_slice()) {
            let _ = child.kill();
            let _ = child.wait();
            return command_spawn_error(format!("write hook stdin failed: {error}"));
        }
    }

    let stdout = child
        .stdout
        .take()
        .map(|stdout| thread::spawn(move || read_bounded_stream(stdout, MAX_HOOK_STREAM_BYTES)));
    let stderr = child
        .stderr
        .take()
        .map(|stderr| thread::spawn(move || read_bounded_stream(stderr, MAX_HOOK_STREAM_BYTES)));

    let timeout = Duration::from_millis(handler.timeout_ms);
    let deadline = Instant::now() + timeout;
    let (exit_code, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code(), false),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => return command_spawn_error(format!("wait hook process failed: {error}")),
        }
    };

    let (stdout, stdout_truncated) = join_stream_reader(stdout);
    let (stderr, stderr_truncated) = join_stream_reader(stderr);
    LifecycleHookCommandResultV1 {
        exit_code,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        timed_out,
        spawn_error: None,
    }
}

fn validate_handlers(handlers: &[LifecycleHookHandlerV1]) -> Result<(), String> {
    let mut seen_ids = HashSet::new();
    for handler in handlers {
        validate_handler(handler)?;
        if !seen_ids.insert(handler.id.as_str()) {
            return Err(format!("duplicate hook handler id: {}", handler.id));
        }
    }
    Ok(())
}

fn validate_handler(handler: &LifecycleHookHandlerV1) -> Result<(), String> {
    if handler.id.trim().is_empty() {
        return Err("hook handler id is required".to_string());
    }
    if handler.program.trim().is_empty() {
        return Err(format!("hook handler {} program is required", handler.id));
    }
    if handler.timeout_ms == 0 {
        return Err(format!(
            "hook handler {} timeoutMs must be positive",
            handler.id
        ));
    }
    Ok(())
}

fn matcher_matches(handler: &LifecycleHookHandlerV1, event: &LifecycleHookEventV1) -> bool {
    let Some(matcher) = handler.matcher.as_deref().map(str::trim) else {
        return true;
    };
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    match event.event {
        LifecycleHookEventNameV1::PreToolUse
        | LifecycleHookEventNameV1::PermissionRequest
        | LifecycleHookEventNameV1::PostToolUse => event.tool_name.as_deref() == Some(matcher),
        LifecycleHookEventNameV1::SubagentStart | LifecycleHookEventNameV1::SubagentStop => {
            event.subagent_name.as_deref() == Some(matcher)
        }
        _ => true,
    }
}

fn parse_hook_output(
    event_name: LifecycleHookEventNameV1,
    stdout: &str,
) -> Result<LifecycleHookOutputV1, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(LifecycleHookOutputV1::default());
    }
    let mut output: LifecycleHookOutputV1 = serde_json::from_str(trimmed)
        .map_err(|error| format!("hook output is not valid JSON: {error}"))?;
    if output.schema != LIFECYCLE_HOOK_RESULT_SCHEMA_V1 {
        return Err("hook output schema mismatch".to_string());
    }
    normalize_output(&mut output)?;
    validate_output_for_event(event_name, &output)?;
    Ok(output)
}

fn normalize_output(output: &mut LifecycleHookOutputV1) -> Result<(), String> {
    if let Some(reason) = output.block_reason.take() {
        output.block_reason = Some(bounded_hook_text(reason.as_str())?);
    }
    if let Some(diagnostic) = output.diagnostic.take() {
        let diagnostic = bounded_hook_text(diagnostic.as_str())?;
        if !diagnostic.is_empty() {
            output.diagnostic = Some(diagnostic);
        }
    }
    if output.additional_context.len() > MAX_HOOK_CONTEXT_ITEMS {
        return Err(format!(
            "hook additionalContext exceeds {MAX_HOOK_CONTEXT_ITEMS} items"
        ));
    }
    let mut total_context_bytes = 0;
    for context in &mut output.additional_context {
        context.text = bounded_hook_text(context.text.as_str())?;
        if context.text.is_empty() {
            return Err("hook additionalContext text must not be empty".to_string());
        }
        total_context_bytes += context.text.len();
        if total_context_bytes > MAX_HOOK_CONTEXT_TOTAL_BYTES {
            return Err(format!(
                "hook additionalContext exceeds {MAX_HOOK_CONTEXT_TOTAL_BYTES} bytes"
            ));
        }
    }
    Ok(())
}

fn validate_output_for_event(
    event_name: LifecycleHookEventNameV1,
    output: &LifecycleHookOutputV1,
) -> Result<(), String> {
    match event_name {
        LifecycleHookEventNameV1::PreToolUse => {
            if output.permission_decision.is_some() {
                return Err("PreToolUse hook cannot decide permission".to_string());
            }
            Ok(())
        }
        LifecycleHookEventNameV1::PermissionRequest => {
            if output.updated_input.is_some() {
                return Err("PermissionRequest hook cannot update tool input".to_string());
            }
            if output.block_reason.is_some() {
                return Err("PermissionRequest hook must use permissionDecision".to_string());
            }
            Ok(())
        }
        LifecycleHookEventNameV1::PostToolUse => {
            if output.updated_input.is_some() || output.permission_decision.is_some() {
                return Err("PostToolUse hook cannot update input or decide permission".to_string());
            }
            if output.block_reason.is_some() {
                return Err("PostToolUse hook cannot block after tool execution".to_string());
            }
            Ok(())
        }
        LifecycleHookEventNameV1::PreCompact => {
            if output.updated_input.is_some()
                || output.permission_decision.is_some()
                || !output.additional_context.is_empty()
            {
                return Err("PreCompact hook only supports blockReason and diagnostics".to_string());
            }
            Ok(())
        }
        LifecycleHookEventNameV1::PostCompact => {
            if output.block_reason.is_some()
                || output.updated_input.is_some()
                || output.permission_decision.is_some()
                || !output.additional_context.is_empty()
            {
                return Err("PostCompact hook only supports diagnostics".to_string());
            }
            Ok(())
        }
        LifecycleHookEventNameV1::UserPromptSubmit | LifecycleHookEventNameV1::Stop => {
            if output.updated_input.is_some() || output.permission_decision.is_some() {
                return Err("this hook event cannot update input or decide permission".to_string());
            }
            Ok(())
        }
        LifecycleHookEventNameV1::SubagentStart | LifecycleHookEventNameV1::SubagentStop => {
            if output.block_reason.is_some()
                || output.updated_input.is_some()
                || output.permission_decision.is_some()
                || !output.additional_context.is_empty()
            {
                return Err("subagent hooks only support diagnostics".to_string());
            }
            Ok(())
        }
        LifecycleHookEventNameV1::SessionStart => {
            if output.block_reason.is_some()
                || output.updated_input.is_some()
                || output.permission_decision.is_some()
            {
                return Err("SessionStart hook only supports context and diagnostics".to_string());
            }
            Ok(())
        }
    }
}

fn apply_hook_output(
    event_name: LifecycleHookEventNameV1,
    output: LifecycleHookOutputV1,
    result: &mut LifecycleHookDispatchResultV1,
    run: &mut LifecycleHookRunV1,
) -> Result<(), String> {
    let next_context_items = result.additional_context.len() + output.additional_context.len();
    let next_context_bytes = result
        .additional_context
        .iter()
        .chain(output.additional_context.iter())
        .map(|context| context.text.len())
        .sum::<usize>();
    if next_context_items > MAX_HOOK_CONTEXT_ITEMS {
        return Err(format!(
            "hook additionalContext exceeds {MAX_HOOK_CONTEXT_ITEMS} items"
        ));
    }
    if next_context_bytes > MAX_HOOK_CONTEXT_TOTAL_BYTES {
        return Err(format!(
            "hook additionalContext exceeds {MAX_HOOK_CONTEXT_TOTAL_BYTES} bytes"
        ));
    }
    result.additional_context.extend(output.additional_context);
    if let Some(updated_input) = output.updated_input {
        result.updated_input = Some(updated_input);
    }
    if let Some(permission_decision) = output.permission_decision {
        result.permission_decision = Some(permission_decision);
    }
    if let Some(diagnostic) = output.diagnostic {
        run.diagnostic = Some(diagnostic);
    }
    if let Some(block_reason) = output.block_reason {
        result.blocked = true;
        result.block_reason = Some(block_reason);
        run.status = LifecycleHookRunStatusV1::Blocked;
        return Ok(());
    }
    if event_name == LifecycleHookEventNameV1::PermissionRequest
        && result.permission_decision == Some(LifecycleHookPermissionDecisionV1::Deny)
    {
        result.blocked = true;
        result.block_reason = Some("permission denied by hook".to_string());
        run.status = LifecycleHookRunStatusV1::Blocked;
    }
    Ok(())
}

fn fail_run(
    run: &mut LifecycleHookRunV1,
    result: &mut LifecycleHookDispatchResultV1,
    reason: String,
) {
    run.status = LifecycleHookRunStatusV1::Failed;
    run.diagnostic = Some(reason.clone());
    run.completed_at_ms = now_ms();
    result.blocked = true;
    result.block_reason = Some(reason);
    result.runs.push(run.clone());
}

fn bounded_hook_text(text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.len() > MAX_HOOK_TEXT_BYTES {
        return Err(format!("hook text exceeds {MAX_HOOK_TEXT_BYTES} bytes"));
    }
    Ok(trimmed.to_string())
}

fn read_bounded_stream<R: Read>(mut reader: R, max_bytes: usize) -> (String, bool) {
    let mut stored = Vec::new();
    let mut buffer = [0u8; 4096];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let remaining = max_bytes.saturating_sub(stored.len());
                if remaining > 0 {
                    let keep = remaining.min(count);
                    stored.extend_from_slice(&buffer[..keep]);
                }
                if count > remaining {
                    truncated = true;
                }
            }
            Err(error) => {
                return (format!("read hook stream failed: {error}"), true);
            }
        }
    }
    (
        String::from_utf8_lossy(stored.as_slice()).to_string(),
        truncated,
    )
}

fn join_stream_reader(reader: Option<thread::JoinHandle<(String, bool)>>) -> (String, bool) {
    match reader {
        Some(reader) => reader
            .join()
            .unwrap_or_else(|_| ("hook stream reader panicked".to_string(), true)),
        None => (String::new(), false),
    }
}

fn command_spawn_error(error: String) -> LifecycleHookCommandResultV1 {
    LifecycleHookCommandResultV1 {
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        timed_out: false,
        spawn_error: Some(error),
    }
}

fn next_hook_run_id(handler_id: &str) -> String {
    let sequence = NEXT_HOOK_RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("hook_run_{}_{}_{}", now_ms(), sequence, handler_id)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn default_timeout_ms() -> u64 {
    5_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::permission::{evaluate_tool_action, ToolPermissionRequest};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct StubRunner {
        outputs: HashMap<String, LifecycleHookCommandResultV1>,
    }

    impl LifecycleHookRunner for StubRunner {
        fn run_hook(
            &self,
            handler: &LifecycleHookHandlerV1,
            _event: &LifecycleHookEventV1,
        ) -> LifecycleHookCommandResultV1 {
            self.outputs.get(&handler.id).cloned().unwrap()
        }
    }

    struct ChainingRunner {
        inputs: Mutex<Vec<Value>>,
    }

    impl LifecycleHookRunner for ChainingRunner {
        fn run_hook(
            &self,
            handler: &LifecycleHookHandlerV1,
            event: &LifecycleHookEventV1,
        ) -> LifecycleHookCommandResultV1 {
            self.inputs
                .lock()
                .unwrap()
                .push(event.payload["toolInput"].clone());
            if handler.id == "first" {
                output(json!({ "updatedInput": { "path": "second.txt" } }))
            } else {
                output(json!({}))
            }
        }
    }

    fn handler(id: &str, event: LifecycleHookEventNameV1) -> LifecycleHookHandlerV1 {
        LifecycleHookHandlerV1 {
            id: id.to_string(),
            event,
            matcher: None,
            source: LifecycleHookSourceV1 {
                kind: LifecycleHookSourceKindV1::Plugin,
                name: "test".to_string(),
            },
            trusted: true,
            program: "test-hook".to_string(),
            args: Vec::new(),
            cwd: None,
            timeout_ms: 1000,
        }
    }

    fn event(event: LifecycleHookEventNameV1) -> LifecycleHookEventV1 {
        LifecycleHookEventV1 {
            schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
            event,
            session_id: "chat_1".to_string(),
            cwd: None,
            tool_name: None,
            subagent_name: None,
            payload: Value::Null,
        }
    }

    fn output(mut stdout: Value) -> LifecycleHookCommandResultV1 {
        stdout
            .as_object_mut()
            .expect("hook output fixture must be an object")
            .insert(
                "schema".to_string(),
                Value::String(LIFECYCLE_HOOK_RESULT_SCHEMA_V1.to_string()),
            );
        LifecycleHookCommandResultV1 {
            exit_code: Some(0),
            stdout: stdout.to_string(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            spawn_error: None,
        }
    }

    #[test]
    fn pre_tool_hook_can_block_matching_tool() {
        let mut write_handler = handler("write-policy", LifecycleHookEventNameV1::PreToolUse);
        write_handler.matcher = Some("write".to_string());
        let engine = LifecycleHookEngineV1::new(vec![write_handler]).unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "write-policy".to_string(),
                output(json!({ "blockReason": "writes disabled" })),
            )]),
        };
        let mut pre_tool = event(LifecycleHookEventNameV1::PreToolUse);
        pre_tool.tool_name = Some("write".to_string());

        let result = engine.dispatch(&pre_tool, &runner);

        assert!(result.blocked);
        assert_eq!(result.block_reason.as_deref(), Some("writes disabled"));
        assert_eq!(result.runs[0].status, LifecycleHookRunStatusV1::Blocked);
    }

    #[test]
    fn pre_tool_updated_input_chains_to_the_next_hook() {
        let mut first = handler("first", LifecycleHookEventNameV1::PreToolUse);
        first.matcher = Some("write".to_string());
        let mut second = handler("second", LifecycleHookEventNameV1::PreToolUse);
        second.matcher = Some("write".to_string());
        let engine = LifecycleHookEngineV1::new(vec![first, second]).unwrap();
        let runner = ChainingRunner {
            inputs: Mutex::new(Vec::new()),
        };
        let event = pre_tool_use_event("chat_1", None, "write", json!({ "path": "first.txt" }));

        let result = engine.dispatch(&event, &runner);

        assert_eq!(
            *runner.inputs.lock().unwrap(),
            vec![
                json!({ "path": "first.txt" }),
                json!({ "path": "second.txt" })
            ]
        );
        assert_eq!(result.updated_input, Some(json!({ "path": "second.txt" })));
    }

    #[test]
    fn unmatched_tool_hook_does_not_run() {
        let mut bash_handler = handler("bash-policy", LifecycleHookEventNameV1::PreToolUse);
        bash_handler.matcher = Some("bash".to_string());
        let engine = LifecycleHookEngineV1::new(vec![bash_handler]).unwrap();
        let runner = StubRunner {
            outputs: HashMap::new(),
        };
        let mut pre_tool = event(LifecycleHookEventNameV1::PreToolUse);
        pre_tool.tool_name = Some("read".to_string());

        let result = engine.dispatch(&pre_tool, &runner);

        assert!(!result.blocked);
        assert!(result.runs.is_empty());
    }

    #[test]
    fn untrusted_hook_is_visible_but_not_executed() {
        let mut untrusted = handler("plugin-hook", LifecycleHookEventNameV1::UserPromptSubmit);
        untrusted.trusted = false;
        let engine = LifecycleHookEngineV1::new(vec![untrusted]).unwrap();
        let runner = StubRunner {
            outputs: HashMap::new(),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::UserPromptSubmit), &runner);

        assert!(!result.blocked);
        assert_eq!(
            result.runs[0].status,
            LifecycleHookRunStatusV1::SkippedUntrusted
        );
        assert_eq!(
            result.runs[0].diagnostic.as_deref(),
            Some("hook handler is not trusted")
        );
    }

    #[test]
    fn permission_request_deny_blocks_without_granting_new_power() {
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "permission-policy",
            LifecycleHookEventNameV1::PermissionRequest,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "permission-policy".to_string(),
                output(json!({ "permissionDecision": "deny" })),
            )]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PermissionRequest), &runner);

        assert!(result.blocked);
        assert_eq!(
            result.permission_decision,
            Some(LifecycleHookPermissionDecisionV1::Deny)
        );
        assert_eq!(
            result.block_reason.as_deref(),
            Some("permission denied by hook")
        );
    }

    #[test]
    fn absent_hook_decision_cannot_override_core_permission_block() {
        let base = evaluate_tool_action(&ToolPermissionRequest::new("UnknownTool", None));
        assert!(!base.allowed);

        let composed = compose_permission_decision_with_hook(base, None);

        assert!(!composed.allowed);
        assert_eq!(composed.reason_type, "unknown_tool_blocked");
    }

    #[test]
    fn hook_deny_can_narrow_allowed_permission() {
        let base = evaluate_tool_action(&ToolPermissionRequest::new("read", None));
        assert!(base.allowed);

        let composed = compose_permission_decision_with_hook(
            base,
            Some(LifecycleHookPermissionDecisionV1::Deny),
        );

        assert!(!composed.allowed);
        assert_eq!(composed.reason_type, "lifecycle_hook_denied");
    }

    #[test]
    fn invalid_hook_output_fails_closed() {
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "bad-output",
            LifecycleHookEventNameV1::PostToolUse,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "bad-output".to_string(),
                LifecycleHookCommandResultV1 {
                    exit_code: Some(0),
                    stdout: "not json".to_string(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    timed_out: false,
                    spawn_error: None,
                },
            )]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostToolUse), &runner);

        assert!(result.blocked);
        assert_eq!(result.runs[0].status, LifecycleHookRunStatusV1::Failed);
        assert!(result.block_reason.unwrap().contains("not valid JSON"));
    }

    #[test]
    fn nonempty_hook_output_requires_exact_schema() {
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "bad-schema",
            LifecycleHookEventNameV1::PostToolUse,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "bad-schema".to_string(),
                LifecycleHookCommandResultV1 {
                    exit_code: Some(0),
                    stdout: json!({ "schema": "banana" }).to_string(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    timed_out: false,
                    spawn_error: None,
                },
            )]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostToolUse), &runner);

        assert!(result.blocked);
        assert_eq!(
            result.block_reason.as_deref(),
            Some("hook output schema mismatch")
        );
    }

    #[test]
    fn unknown_hook_output_fields_fail_closed() {
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "bad-output",
            LifecycleHookEventNameV1::PostToolUse,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "bad-output".to_string(),
                LifecycleHookCommandResultV1 {
                    exit_code: Some(0),
                    stdout: json!({ "diagnostic": "ok", "extra": true }).to_string(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                    timed_out: false,
                    spawn_error: None,
                },
            )]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostToolUse), &runner);

        assert!(result.blocked);
        assert!(result.block_reason.unwrap().contains("unknown field"));
    }

    #[test]
    fn pre_tool_hook_cannot_decide_permission() {
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "bad-pre-tool",
            LifecycleHookEventNameV1::PreToolUse,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "bad-pre-tool".to_string(),
                output(json!({ "permissionDecision": "deny" })),
            )]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PreToolUse), &runner);

        assert!(result.blocked);
        assert_eq!(result.runs[0].status, LifecycleHookRunStatusV1::Failed);
        assert_eq!(
            result.block_reason.as_deref(),
            Some("PreToolUse hook cannot decide permission")
        );
    }

    #[test]
    fn pre_compact_hook_can_block_without_mutating_input() {
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "pre-compact",
            LifecycleHookEventNameV1::PreCompact,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "pre-compact".to_string(),
                output(json!({ "blockReason": "wait for project policy" })),
            )]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PreCompact), &runner);

        assert!(result.blocked);
        assert_eq!(
            result.block_reason.as_deref(),
            Some("wait for project policy")
        );
        assert_eq!(result.runs[0].status, LifecycleHookRunStatusV1::Blocked);
    }

    #[test]
    fn post_compact_hook_is_diagnostic_only() {
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "post-compact",
            LifecycleHookEventNameV1::PostCompact,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "post-compact".to_string(),
                output(json!({ "blockReason": "too late" })),
            )]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostCompact), &runner);

        assert!(result.blocked);
        assert_eq!(
            result.block_reason.as_deref(),
            Some("PostCompact hook only supports diagnostics")
        );
        assert_eq!(result.runs[0].status, LifecycleHookRunStatusV1::Failed);
    }

    #[test]
    fn post_tool_hook_can_add_bounded_context() {
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "post-tool",
            LifecycleHookEventNameV1::PostToolUse,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "post-tool".to_string(),
                output(json!({
                    "additionalContext": [{ "text": "formatter changed two files" }],
                    "diagnostic": "ok"
                })),
            )]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostToolUse), &runner);

        assert!(!result.blocked);
        assert_eq!(
            result.additional_context[0].text,
            "formatter changed two files"
        );
        assert_eq!(result.runs[0].diagnostic.as_deref(), Some("ok"));
    }

    #[test]
    fn oversized_context_fails_closed() {
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "too-large",
            LifecycleHookEventNameV1::PostToolUse,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([(
                "too-large".to_string(),
                output(
                    json!({ "additionalContext": [{ "text": "x".repeat(MAX_HOOK_TEXT_BYTES + 1) }] }),
                ),
            )]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostToolUse), &runner);

        assert!(result.blocked);
        assert!(result.block_reason.unwrap().contains("exceeds"));
    }

    #[test]
    fn context_limit_is_aggregated_across_handlers() {
        let engine = LifecycleHookEngineV1::new(vec![
            handler("first", LifecycleHookEventNameV1::PostToolUse),
            handler("second", LifecycleHookEventNameV1::PostToolUse),
        ])
        .unwrap();
        let contexts = (0..9)
            .map(|index| json!({ "text": format!("context-{index}") }))
            .collect::<Vec<_>>();
        let runner = StubRunner {
            outputs: HashMap::from([
                (
                    "first".to_string(),
                    output(json!({ "additionalContext": contexts.clone() })),
                ),
                (
                    "second".to_string(),
                    output(json!({ "additionalContext": contexts })),
                ),
            ]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostToolUse), &runner);

        assert!(result.blocked);
        assert!(result.block_reason.unwrap().contains("16 items"));
    }

    #[test]
    fn duplicate_handler_ids_fail_loudly() {
        let first = handler("same", LifecycleHookEventNameV1::PostToolUse);
        let second = handler("same", LifecycleHookEventNameV1::PreToolUse);

        let error = LifecycleHookEngineV1::new(vec![first, second]).unwrap_err();

        assert!(error.contains("duplicate hook handler id"));
    }

    #[test]
    fn hook_run_ids_are_unique() {
        let first = handler("one", LifecycleHookEventNameV1::PostToolUse);
        let second = handler("two", LifecycleHookEventNameV1::PostToolUse);
        let engine = LifecycleHookEngineV1::new(vec![first, second]).unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([
                ("one".to_string(), output(json!({}))),
                ("two".to_string(), output(json!({}))),
            ]),
        };

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostToolUse), &runner);

        assert_eq!(result.runs.len(), 2);
        assert_ne!(result.runs[0].hook_run_id, result.runs[1].hook_run_id);
    }

    #[test]
    fn dispatch_and_record_writes_jsonl_audit() {
        let path = std::env::temp_dir().join(format!(
            "centaeris_hook_audit_{}.jsonl",
            NEXT_HOOK_RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let sink = LifecycleHookJsonlAuditSinkV1::new(path.clone());
        let engine = LifecycleHookEngineV1::new(vec![handler(
            "post-tool",
            LifecycleHookEventNameV1::PostToolUse,
        )])
        .unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([("post-tool".to_string(), output(json!({})))]),
        };

        let result = engine
            .dispatch_and_record(
                &event(LifecycleHookEventNameV1::PostToolUse),
                &runner,
                &sink,
            )
            .unwrap();
        let audit = fs::read_to_string(path.as_path()).unwrap();
        let _ = fs::remove_file(path.as_path());

        assert_eq!(result.runs.len(), 1);
        assert!(audit.contains("\"handlerId\":\"post-tool\""));
    }

    #[test]
    fn diagnostics_projection_lists_handlers_and_runs() {
        let hook = handler("post-tool", LifecycleHookEventNameV1::PostToolUse);
        let engine = LifecycleHookEngineV1::new(vec![hook]).unwrap();
        let runner = StubRunner {
            outputs: HashMap::from([("post-tool".to_string(), output(json!({})))]),
        };
        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostToolUse), &runner);

        let projection = project_lifecycle_hook_diagnostics(engine.list_handlers(), &result.runs);

        assert_eq!(projection.handlers.len(), 1);
        assert_eq!(projection.recent_runs.len(), 1);
    }

    #[cfg(target_os = "windows")]
    fn echo_hook_handler() -> (LifecycleHookHandlerV1, Option<PathBuf>) {
        let script_path = std::env::temp_dir().join(format!(
            "centaeris_hook_echo_{}.cmd",
            NEXT_HOOK_RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(
            script_path.as_path(),
            "@echo off\r\nmore >nul\r\necho {\"schema\":\"lifecycle_hook_result_v1\",\"diagnostic\":\"ok\"}\r\n",
        )
        .expect("write hook test script");
        (
            LifecycleHookHandlerV1 {
                program: "cmd.exe".to_string(),
                args: vec![
                    "/D".to_string(),
                    "/C".to_string(),
                    script_path.to_string_lossy().to_string(),
                ],
                timeout_ms: 5_000,
                ..handler("local-command", LifecycleHookEventNameV1::PostToolUse)
            },
            Some(script_path),
        )
    }

    #[cfg(not(target_os = "windows"))]
    fn echo_hook_handler() -> (LifecycleHookHandlerV1, Option<PathBuf>) {
        (
            LifecycleHookHandlerV1 {
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "cat >/dev/null; printf '{\"schema\":\"lifecycle_hook_result_v1\",\"diagnostic\":\"ok\"}\\n'".to_string(),
                ],
                timeout_ms: 5_000,
                ..handler("local-command", LifecycleHookEventNameV1::PostToolUse)
            },
            None,
        )
    }

    #[test]
    fn local_command_runner_sends_event_to_stdin_and_parses_stdout() {
        let (hook, script_path) = echo_hook_handler();
        let engine = LifecycleHookEngineV1::new(vec![hook]).unwrap();

        let result = engine.dispatch(
            &event(LifecycleHookEventNameV1::PostToolUse),
            &LocalLifecycleHookCommandRunnerV1::default(),
        );
        if let Some(path) = script_path {
            fs::remove_file(path).expect("remove hook test script");
        }

        assert!(!result.blocked, "{:?}", result.block_reason);
        assert_eq!(result.runs[0].diagnostic.as_deref(), Some("ok"));
    }

    #[test]
    fn local_command_runner_applies_host_environment_overrides() {
        let (hook, script_path) = echo_hook_handler();
        #[cfg(target_os = "windows")]
        {
            let path = script_path.as_ref().expect("Windows hook script");
            fs::write(
                path,
                "@echo off\r\nmore >nul\r\necho {\"schema\":\"lifecycle_hook_result_v1\",\"diagnostic\":\"%CENTAERIS_HOOK_ENV_TEST%\"}\r\n",
            )
            .expect("write environment hook script");
        }
        #[cfg(not(target_os = "windows"))]
        let hook = {
            let mut hook = hook;
            hook.args = vec![
                "-c".to_string(),
                "cat >/dev/null; printf '{\"schema\":\"lifecycle_hook_result_v1\",\"diagnostic\":\"%s\"}\\n' \"$CENTAERIS_HOOK_ENV_TEST\"".to_string(),
            ];
            hook
        };
        let engine = LifecycleHookEngineV1::new(vec![hook]).unwrap();
        let runner =
            LocalLifecycleHookCommandRunnerV1::with_environment_overrides(HashMap::from([(
                "CENTAERIS_HOOK_ENV_TEST".to_string(),
                "injected".to_string(),
            )]));

        let result = engine.dispatch(&event(LifecycleHookEventNameV1::PostToolUse), &runner);
        if let Some(path) = script_path {
            fs::remove_file(path).expect("remove hook test script");
        }

        assert!(!result.blocked, "{:?}", result.block_reason);
        assert_eq!(result.runs[0].diagnostic.as_deref(), Some("injected"));
    }

    #[cfg(target_os = "windows")]
    fn sleep_hook_handler() -> LifecycleHookHandlerV1 {
        LifecycleHookHandlerV1 {
            program: "cmd.exe".to_string(),
            args: vec![
                "/D".to_string(),
                "/C".to_string(),
                "ping -n 2 127.0.0.1 >nul".to_string(),
            ],
            timeout_ms: 10,
            ..handler("sleep-command", LifecycleHookEventNameV1::PostToolUse)
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn sleep_hook_handler() -> LifecycleHookHandlerV1 {
        LifecycleHookHandlerV1 {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 1".to_string()],
            timeout_ms: 10,
            ..handler("sleep-command", LifecycleHookEventNameV1::PostToolUse)
        }
    }

    #[test]
    fn local_command_runner_enforces_timeout() {
        let engine = LifecycleHookEngineV1::new(vec![sleep_hook_handler()]).unwrap();

        let result = engine.dispatch(
            &event(LifecycleHookEventNameV1::PostToolUse),
            &LocalLifecycleHookCommandRunnerV1::default(),
        );

        assert!(result.blocked);
        assert!(result.block_reason.unwrap().contains("timed out"));
    }
}
