use super::*;

pub type ToolSafePointCommitPort = Arc<dyn Fn(ToolSafePoint) -> Result<(), String> + Send + Sync>;

pub fn build_subagent_scheduler_runtime_event(
    session_id: &str,
    turn_id: &str,
    scheduler_event: &SubagentSchedulerEvent,
) -> RuntimeEventProjection {
    build_runtime_event_subagent_event_from_scheduler_event(session_id, turn_id, scheduler_event)
}

#[derive(Debug, Clone)]
pub struct AgentRuntimeSubagentRunnerConfig {
    pub auto_continue_after_resume_wait: Option<bool>,
    pub agent_run_identity: Option<RuntimeAgentRunIdentityV1>,
}

impl Default for AgentRuntimeSubagentRunnerConfig {
    fn default() -> Self {
        Self {
            auto_continue_after_resume_wait: Some(false),
            agent_run_identity: None,
        }
    }
}

#[derive(Clone)]
pub struct ModelClientSubagentRunner<'a, S, M, CfgStore>
where
    S: RuntimeStore
        + ExternalContextStorePort
        + RuntimeJobStorePort
        + RuntimeStoreTransactionPort
        + AgentRuntimeSnapshotStorePort
        + Clone
        + Send
        + Sync
        + 'static,
    M: ModelClient,
    CfgStore: ModelSessionConfigStore,
{
    engine: &'a AgentRuntime<S>,
    model_client: &'a M,
    session_config_store: &'a CfgStore,
    config: AgentRuntimeSubagentRunnerConfig,
    stream_sink: Option<Arc<dyn Fn(TurnUpdate) + Send + Sync>>,
    tool_safe_point: Option<ToolSafePointCommitPort>,
}

#[derive(Clone)]
pub struct QueryLifecycleSubagentObserver<'a, S>
where
    S: RuntimeStore
        + ExternalContextStorePort
        + RuntimeJobStorePort
        + RuntimeStoreTransactionPort
        + AgentRuntimeSnapshotStorePort
        + Clone
        + Send
        + Sync
        + 'static,
{
    engine: &'a AgentRuntime<S>,
}

impl<'a, S> QueryLifecycleSubagentObserver<'a, S>
where
    S: RuntimeStore
        + ExternalContextStorePort
        + RuntimeJobStorePort
        + RuntimeStoreTransactionPort
        + AgentRuntimeSnapshotStorePort
        + Clone
        + Send
        + Sync
        + 'static,
{
    pub fn new(engine: &'a AgentRuntime<S>) -> Self {
        Self { engine }
    }
}

impl<'a, S> AsyncSubagentLifecycleObserver for QueryLifecycleSubagentObserver<'a, S>
where
    S: RuntimeStore
        + ExternalContextStorePort
        + RuntimeJobStorePort
        + RuntimeStoreTransactionPort
        + AgentRuntimeSnapshotStorePort
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn on_subagent_start<'b>(
        &'b self,
        event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'b> {
        Box::pin(async move {
            let session_id = event.session_id.clone();
            let subagent_name = event.subagent_id.clone();
            let Ok(payload) = serde_json::to_value(event) else {
                return Ok(());
            };
            let _ = self.engine.run_session_start_hook(
                session_id.as_str(),
                QueryLifecycleHookStartTargetV1::SubagentStart {
                    subagent_name,
                    payload,
                },
            );
            Ok(())
        })
    }

    fn on_subagent_stop<'b>(
        &'b self,
        event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'b> {
        Box::pin(async move {
            let session_id = event.session_id.clone();
            let subagent_name = event.subagent_id.clone();
            let Ok(payload) = serde_json::to_value(event) else {
                return Ok(());
            };
            let _ = self.engine.run_stop_hook(
                session_id.as_str(),
                QueryLifecycleHookStopTargetV1::SubagentStop {
                    subagent_name,
                    payload,
                },
            );
            Ok(())
        })
    }
}

impl<'a, S, M, CfgStore> ModelClientSubagentRunner<'a, S, M, CfgStore>
where
    S: RuntimeStore
        + ExternalContextStorePort
        + RuntimeJobStorePort
        + RuntimeStoreTransactionPort
        + AgentRuntimeSnapshotStorePort
        + Clone
        + Send
        + Sync
        + 'static,
    M: ModelClient,
    CfgStore: ModelSessionConfigStore,
{
    pub fn new(
        engine: &'a AgentRuntime<S>,
        model_client: &'a M,
        session_config_store: &'a CfgStore,
        config: AgentRuntimeSubagentRunnerConfig,
    ) -> Self {
        Self {
            engine,
            model_client,
            session_config_store,
            config,
            stream_sink: None,
            tool_safe_point: None,
        }
    }

    pub fn with_stream_sink(mut self, stream_sink: Arc<dyn Fn(TurnUpdate) + Send + Sync>) -> Self {
        self.stream_sink = Some(stream_sink);
        self
    }

    pub fn with_tool_safe_point(mut self, tool_safe_point: ToolSafePointCommitPort) -> Self {
        self.tool_safe_point = Some(tool_safe_point);
        self
    }
}

impl<'a, S, M, CfgStore> AsyncSubagentWorkerRunner for ModelClientSubagentRunner<'a, S, M, CfgStore>
where
    S: RuntimeStore
        + ExternalContextStorePort
        + RuntimeJobStorePort
        + RuntimeStoreTransactionPort
        + AgentRuntimeSnapshotStorePort
        + Clone
        + Send
        + Sync
        + 'static,
    M: ModelClient,
    CfgStore: ModelSessionConfigStore,
{
    fn run_async<'b>(&'b self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'b> {
        Box::pin(async move {
            let mut stream_sink = self
                .stream_sink
                .as_ref()
                .map(|sink| move |event| sink(event));
            let mut tool_safe_point = self
                .tool_safe_point
                .as_ref()
                .map(|sink| move |safe_point| sink(safe_point));
            self.engine
                .run_subagent_worker_with_model_client_async(
                    req,
                    self.model_client,
                    self.session_config_store,
                    &self.config,
                    stream_sink
                        .as_mut()
                        .map(|sink| sink as &mut (dyn FnMut(TurnUpdate) + Send)),
                    tool_safe_point.as_mut().map(|sink| {
                        sink as &mut (dyn FnMut(ToolSafePoint) -> Result<(), String> + Send)
                    }),
                )
                .await
        })
    }
}
