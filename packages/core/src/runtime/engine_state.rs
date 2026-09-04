use super::*;

#[derive(Debug, Clone)]
pub(super) struct ToolExecutionBatch {
    pub(super) tool_results: Vec<ToolExecutionResult>,
    pub(super) runtime_job_waits: Vec<RuntimeJobWaitV1>,
    pub(super) lifecycle_hook_contexts: Vec<String>,
    pub(super) tool_progress_events: Vec<RuntimeEventProjection>,
    pub(super) transition_reason: String,
    pub(super) recovery_policy_trace_json: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PendingRuntimeToolBatchV1 {
    pub(super) schema: String,
    pub(super) turn_id: String,
    pub(super) waiting_at_ms: i64,
    pub(super) wait_checkpoint: RuntimeAwaitJobCheckpointV1,
    pub(super) agent_run_resource_usage: AgentRunResourceUsageV1,
    pub(super) system_prompt_manifest_json: Option<String>,
    pub(super) compression_stats_json: Option<String>,
    pub(super) lifecycle_hook_contexts: Vec<String>,
    pub(super) transition_reason: String,
    pub(super) recovery_policy_trace_json: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct PromptCompactionApplyResult {
    pub(super) stats_json: Option<String>,
    pub(super) runtime_events: Vec<RuntimeEventProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ActiveObjectiveState {
    pub(super) schema: String,
    pub(super) objective_id: String,
    pub(super) source_turn_id: String,
    pub(super) objective: String,
    pub(super) root_user_message: String,
    pub(super) supplements: Vec<ActiveObjectiveSupplement>,
    pub(super) updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ActiveObjectiveSupplement {
    pub(super) content: String,
    pub(super) at_ms: i64,
}
