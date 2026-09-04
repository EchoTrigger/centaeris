use super::*;
use crate::extension::hooks::{
    post_tool_use_event, pre_tool_use_event, project_lifecycle_hook_diagnostics,
    subagent_lifecycle_event, user_prompt_submit_event, LifecycleHookAuditSink,
    LifecycleHookContextV1, LifecycleHookDiagnosticsProjectionV1, LifecycleHookDispatchResultV1,
    LifecycleHookEngineV1, LifecycleHookEventNameV1, LifecycleHookEventV1,
    LifecycleHookJsonlAuditSinkV1, LifecycleHookPermissionDecisionV1, LifecycleHookRunV1,
    LifecycleHookRunner, LocalLifecycleHookCommandRunnerV1, LIFECYCLE_HOOK_EVENT_SCHEMA_V1,
};

const QUERY_LIFECYCLE_HOOK_RECENT_RUN_LIMIT: usize = 100;

#[derive(Clone)]
pub struct QueryLifecycleHookRuntime {
    engine: LifecycleHookEngineV1,
    runner: Arc<dyn LifecycleHookRunner + Send + Sync>,
    audit_sink: Option<Arc<dyn LifecycleHookAuditSink + Send + Sync>>,
    recent_runs: Arc<Mutex<Vec<LifecycleHookRunV1>>>,
}

impl std::fmt::Debug for QueryLifecycleHookRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QueryLifecycleHookRuntime")
            .field("handler_count", &self.engine.list_handlers().len())
            .field("has_audit_sink", &self.audit_sink.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for QueryLifecycleHookRuntime {
    fn default() -> Self {
        Self::empty()
    }
}

impl QueryLifecycleHookRuntime {
    pub fn empty() -> Self {
        Self::new(
            LifecycleHookEngineV1::default(),
            Arc::new(LocalLifecycleHookCommandRunnerV1::default()),
            None,
        )
    }

    pub fn local(engine: LifecycleHookEngineV1) -> Self {
        Self::new(
            engine,
            Arc::new(LocalLifecycleHookCommandRunnerV1::default()),
            None,
        )
    }

    pub fn local_with_jsonl_audit(
        engine: LifecycleHookEngineV1,
        audit_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::new(
            engine,
            Arc::new(LocalLifecycleHookCommandRunnerV1::default()),
            Some(Arc::new(LifecycleHookJsonlAuditSinkV1::new(audit_path))),
        )
    }

    pub fn new(
        engine: LifecycleHookEngineV1,
        runner: Arc<dyn LifecycleHookRunner + Send + Sync>,
        audit_sink: Option<Arc<dyn LifecycleHookAuditSink + Send + Sync>>,
    ) -> Self {
        Self {
            engine,
            runner,
            audit_sink,
            recent_runs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn has_handlers(&self) -> bool {
        !self.engine.list_handlers().is_empty()
    }

    pub fn composition_digest(&self) -> Result<String, String> {
        crate::runtime::canonical_json::sha256(
            "centaeris.lifecycle_hook_composition.v1",
            &self.engine.list_handlers(),
        )
    }

    pub fn run_event(
        &self,
        event: LifecycleHookEventV1,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        let result = self.engine.dispatch(&event, self.runner.as_ref());
        if let Some(audit_sink) = &self.audit_sink {
            audit_sink.record_hook_runs(result.runs.as_slice())?;
        }
        self.record_recent_runs(result.runs.as_slice())?;
        Ok(QueryLifecycleHookOutcome::from(result))
    }

    pub fn run_session_start(
        &self,
        session_id: &str,
        cwd: Option<String>,
        target: QueryLifecycleHookStartTargetV1,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        match target {
            QueryLifecycleHookStartTargetV1::SessionStart { payload } => {
                self.run_event(LifecycleHookEventV1 {
                    schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
                    event: LifecycleHookEventNameV1::SessionStart,
                    session_id: session_id.to_string(),
                    cwd,
                    tool_name: None,
                    subagent_name: None,
                    payload,
                })
            }
            QueryLifecycleHookStartTargetV1::SubagentStart {
                subagent_name,
                payload,
            } => self.run_event(subagent_lifecycle_event(
                LifecycleHookEventNameV1::SubagentStart,
                session_id.to_string(),
                cwd,
                subagent_name,
                payload,
            )?),
        }
    }

    pub fn run_user_prompt_submit(
        &self,
        session_id: &str,
        cwd: Option<String>,
        prompt: &str,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.run_event(user_prompt_submit_event(
            session_id.to_string(),
            cwd,
            prompt.to_string(),
        ))
    }

    pub fn run_pre_tool_use(
        &self,
        session_id: &str,
        cwd: Option<String>,
        tool_name: &str,
        tool_input: Value,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.run_event(pre_tool_use_event(
            session_id.to_string(),
            cwd,
            tool_name.to_string(),
            tool_input,
        ))
    }

    pub fn run_permission_request(
        &self,
        session_id: &str,
        cwd: Option<String>,
        tool_name: &str,
        permission: &PermissionDecision,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.run_event(crate::extension::hooks::permission_request_event(
            session_id.to_string(),
            cwd,
            tool_name.to_string(),
            permission,
        ))
    }

    pub fn run_post_tool_use(
        &self,
        session_id: &str,
        cwd: Option<String>,
        tool_name: &str,
        tool_result: Value,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.run_event(post_tool_use_event(
            session_id.to_string(),
            cwd,
            tool_name.to_string(),
            tool_result,
        ))
    }

    pub fn run_stop(
        &self,
        session_id: &str,
        cwd: Option<String>,
        target: QueryLifecycleHookStopTargetV1,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        match target {
            QueryLifecycleHookStopTargetV1::Stop { payload } => {
                self.run_event(LifecycleHookEventV1 {
                    schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
                    event: LifecycleHookEventNameV1::Stop,
                    session_id: session_id.to_string(),
                    cwd,
                    tool_name: None,
                    subagent_name: None,
                    payload,
                })
            }
            QueryLifecycleHookStopTargetV1::SubagentStop {
                subagent_name,
                payload,
            } => self.run_event(subagent_lifecycle_event(
                LifecycleHookEventNameV1::SubagentStop,
                session_id.to_string(),
                cwd,
                subagent_name,
                payload,
            )?),
        }
    }

    pub fn run_pre_compact(
        &self,
        session_id: &str,
        cwd: Option<String>,
        payload: Value,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.run_event(LifecycleHookEventV1 {
            schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
            event: LifecycleHookEventNameV1::PreCompact,
            session_id: session_id.to_string(),
            cwd,
            tool_name: None,
            subagent_name: None,
            payload,
        })
    }

    pub fn run_post_compact(
        &self,
        session_id: &str,
        cwd: Option<String>,
        payload: Value,
    ) -> Result<QueryLifecycleHookOutcome, String> {
        self.run_event(LifecycleHookEventV1 {
            schema: LIFECYCLE_HOOK_EVENT_SCHEMA_V1.to_string(),
            event: LifecycleHookEventNameV1::PostCompact,
            session_id: session_id.to_string(),
            cwd,
            tool_name: None,
            subagent_name: None,
            payload,
        })
    }

    pub fn diagnostics_projection(&self) -> Result<LifecycleHookDiagnosticsProjectionV1, String> {
        let recent_runs = self
            .recent_runs
            .lock()
            .map_err(|_| "lifecycle hook recent runs lock poisoned".to_string())?;
        Ok(project_lifecycle_hook_diagnostics(
            self.engine.list_handlers(),
            recent_runs.as_slice(),
        ))
    }

    fn record_recent_runs(&self, runs: &[LifecycleHookRunV1]) -> Result<(), String> {
        if runs.is_empty() {
            return Ok(());
        }
        let mut recent_runs = self
            .recent_runs
            .lock()
            .map_err(|_| "lifecycle hook recent runs lock poisoned".to_string())?;
        recent_runs.extend_from_slice(runs);
        let overflow = recent_runs
            .len()
            .saturating_sub(QUERY_LIFECYCLE_HOOK_RECENT_RUN_LIMIT);
        if overflow > 0 {
            recent_runs.drain(0..overflow);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum QueryLifecycleHookStartTargetV1 {
    SessionStart {
        payload: Value,
    },
    SubagentStart {
        #[serde(rename = "subagentName")]
        subagent_name: String,
        payload: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum QueryLifecycleHookStopTargetV1 {
    Stop {
        payload: Value,
    },
    SubagentStop {
        #[serde(rename = "subagentName")]
        subagent_name: String,
        payload: Value,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryLifecycleHookOutcome {
    pub blocked: bool,
    #[serde(
        default,
        rename = "blockReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub block_reason: Option<String>,
    #[serde(
        default,
        rename = "updatedInput",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_input: Option<Value>,
    #[serde(default, rename = "additionalContext")]
    pub additional_context: Vec<LifecycleHookContextV1>,
    #[serde(
        default,
        rename = "permissionDecision",
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_decision: Option<LifecycleHookPermissionDecisionV1>,
    #[serde(default)]
    pub runs: Vec<LifecycleHookRunV1>,
}

impl From<LifecycleHookDispatchResultV1> for QueryLifecycleHookOutcome {
    fn from(result: LifecycleHookDispatchResultV1) -> Self {
        Self {
            blocked: result.blocked,
            block_reason: result.block_reason,
            updated_input: result.updated_input,
            additional_context: result.additional_context,
            permission_decision: result.permission_decision,
            runs: result.runs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::hooks::{
        LifecycleHookCommandResultV1, LifecycleHookHandlerV1, LifecycleHookSourceKindV1,
        LifecycleHookSourceV1, LIFECYCLE_HOOK_RESULT_SCHEMA_V1,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct StaticRunner {
        stdout: String,
    }

    impl LifecycleHookRunner for StaticRunner {
        fn run_hook(
            &self,
            _handler: &LifecycleHookHandlerV1,
            _event: &LifecycleHookEventV1,
        ) -> LifecycleHookCommandResultV1 {
            LifecycleHookCommandResultV1 {
                exit_code: Some(0),
                stdout: self.stdout.clone(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                spawn_error: None,
            }
        }
    }

    #[derive(Debug, Default)]
    struct CountingAuditSink {
        calls: AtomicUsize,
        fail: bool,
    }

    impl LifecycleHookAuditSink for CountingAuditSink {
        fn record_hook_runs(&self, runs: &[LifecycleHookRunV1]) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail && !runs.is_empty() {
                return Err("audit sink failed".to_string());
            }
            Ok(())
        }
    }

    fn handler(event: LifecycleHookEventNameV1) -> LifecycleHookHandlerV1 {
        handler_with_id("test-hook", event)
    }

    fn handler_with_id(id: &str, event: LifecycleHookEventNameV1) -> LifecycleHookHandlerV1 {
        LifecycleHookHandlerV1 {
            id: id.to_string(),
            event,
            matcher: None,
            source: LifecycleHookSourceV1 {
                kind: LifecycleHookSourceKindV1::Project,
                name: "project".to_string(),
            },
            trusted: true,
            program: "hook".to_string(),
            args: vec![],
            cwd: None,
            timeout_ms: 1000,
        }
    }

    #[test]
    fn query_lifecycle_hook_runtime_empty_is_noop() {
        let outcome = QueryLifecycleHookRuntime::empty()
            .run_user_prompt_submit("session-1", None, "hello")
            .expect("empty hook runtime should dispatch");

        assert!(!outcome.blocked);
        assert!(outcome.runs.is_empty());
        assert!(outcome.additional_context.is_empty());
    }

    #[test]
    fn query_lifecycle_hook_runtime_returns_hook_output_and_audits() {
        let engine =
            LifecycleHookEngineV1::new(vec![handler(LifecycleHookEventNameV1::PreToolUse)])
                .expect("valid hook engine");
        let runner = Arc::new(StaticRunner {
            stdout: serde_json::json!({
                "schema": LIFECYCLE_HOOK_RESULT_SCHEMA_V1,
                "blockReason": "blocked by policy",
                "updatedInput": { "path": "safe.txt" },
                "additionalContext": [{ "text": "use safe path" }]
            })
            .to_string(),
        });
        let audit_sink = Arc::new(CountingAuditSink::default());
        let runtime = QueryLifecycleHookRuntime::new(engine, runner, Some(audit_sink.clone()));

        let outcome = runtime
            .run_pre_tool_use(
                "session-1",
                Some("D:/workspace".to_string()),
                "write",
                json!({}),
            )
            .expect("hook runtime should dispatch");

        assert!(outcome.blocked);
        assert_eq!(outcome.block_reason.as_deref(), Some("blocked by policy"));
        assert_eq!(outcome.updated_input, Some(json!({ "path": "safe.txt" })));
        assert_eq!(outcome.additional_context.len(), 1);
        assert_eq!(outcome.runs.len(), 1);
        assert_eq!(audit_sink.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn query_lifecycle_hook_runtime_audit_failure_loud_fails() {
        let engine =
            LifecycleHookEngineV1::new(vec![handler(LifecycleHookEventNameV1::UserPromptSubmit)])
                .expect("valid hook engine");
        let runner = Arc::new(StaticRunner {
            stdout: serde_json::json!({ "schema": LIFECYCLE_HOOK_RESULT_SCHEMA_V1 }).to_string(),
        });
        let audit_sink = Arc::new(CountingAuditSink {
            calls: AtomicUsize::new(0),
            fail: true,
        });
        let runtime = QueryLifecycleHookRuntime::new(engine, runner, Some(audit_sink));

        let error = runtime
            .run_user_prompt_submit("session-1", None, "hello")
            .expect_err("audit sink failure must propagate");

        assert_eq!(error, "audit sink failed");
    }

    #[test]
    fn query_lifecycle_hook_runtime_start_and_stop_targets_map_to_event_names() {
        let engine = LifecycleHookEngineV1::new(vec![
            handler_with_id(
                "subagent-start-hook",
                LifecycleHookEventNameV1::SubagentStart,
            ),
            handler_with_id("stop-hook", LifecycleHookEventNameV1::Stop),
        ])
        .expect("valid hook engine");
        let runtime = QueryLifecycleHookRuntime::new(
            engine,
            Arc::new(StaticRunner {
                stdout: serde_json::json!({ "schema": LIFECYCLE_HOOK_RESULT_SCHEMA_V1 })
                    .to_string(),
            }),
            None,
        );

        let subagent_start = runtime
            .run_session_start(
                "session-1",
                None,
                QueryLifecycleHookStartTargetV1::SubagentStart {
                    subagent_name: "research".to_string(),
                    payload: json!({ "reason": "spawned" }),
                },
            )
            .expect("subagent start target should dispatch");
        assert_eq!(subagent_start.runs.len(), 1);
        assert_eq!(
            subagent_start.runs[0].event,
            LifecycleHookEventNameV1::SubagentStart
        );

        let stop = runtime
            .run_stop(
                "session-1",
                None,
                QueryLifecycleHookStopTargetV1::Stop {
                    payload: json!({ "reason": "turn_done" }),
                },
            )
            .expect("stop target should dispatch");
        assert_eq!(stop.runs.len(), 1);
        assert_eq!(stop.runs[0].event, LifecycleHookEventNameV1::Stop);
    }

    #[test]
    fn query_lifecycle_hook_runtime_diagnostics_lists_handlers_and_recent_runs() {
        let engine =
            LifecycleHookEngineV1::new(vec![handler(LifecycleHookEventNameV1::PostCompact)])
                .expect("valid hook engine");
        let runtime = QueryLifecycleHookRuntime::new(
            engine,
            Arc::new(StaticRunner {
                stdout: serde_json::json!({ "schema": LIFECYCLE_HOOK_RESULT_SCHEMA_V1 })
                    .to_string(),
            }),
            None,
        );

        runtime
            .run_post_compact("session-1", None, json!({ "reason": "test" }))
            .expect("hook runtime should dispatch");
        let projection = runtime
            .diagnostics_projection()
            .expect("diagnostics should project");

        assert_eq!(projection.handlers.len(), 1);
        assert_eq!(projection.recent_runs.len(), 1);
        assert_eq!(projection.handlers[0].id, "test-hook");
    }
}
