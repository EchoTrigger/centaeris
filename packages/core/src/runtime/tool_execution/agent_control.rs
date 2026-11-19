use super::*;
use crate::runtime::contracts::TimestampMs;
use crate::runtime::keys::runtime_job as runtime_job_keys;
use crate::runtime::subagent::{
    delegated_agent_retired, delegation_id_suffix, load_subagent_work_packet_async,
    request_subagent_task_cancellation, subagent_id_from_job, SubagentWorkPacketEnvelope,
};
use crate::session::reliability::{ListRuntimeJobsRequest, RequestSubagentJobCancellation};
use crate::session::store::job::JobStoreError;
use crate::tool::AGENT_CONTROL_TOOL_NAMES;

const AGENT_TOOL_RESULT_SCHEMA_V1: &str = "agent_tool_result_v1";
const TASK_OUTPUT_REF_SCHEMA_V1: &str = "task_output_ref_v1";
const AGENT_TASK_OUTPUT_WAIT_SCHEMA_V1: &str = "agent_task_output_wait_v1";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentToolArgsV1 {
    prompt: String,
    description: String,
    #[serde(default)]
    budget: Option<AgentToolBudgetV1>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentToolBudgetV1 {
    max_summary_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AgentTaskOutputRefV1 {
    schema: String,
    kind: String,
    pub(super) runtime_job_id: String,
    child_session_id: String,
    result_ref: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowupTaskArgs {
    agent_id: String,
    prompt: String,
    description: String,
    budget: Option<AgentToolBudgetV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskIdArgs {
    task_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListAgentsArgs {}

impl AgentToolArgsV1 {
    fn validate(mut self) -> Result<Self, String> {
        self.prompt = required_agent_arg(self.prompt, "prompt")?;
        self.description = required_agent_arg(self.description, "description")?;
        let max_summary_chars = self
            .budget
            .as_ref()
            .map(|budget| budget.max_summary_chars)
            .unwrap_or(4_000);
        if !(1..=16_000).contains(&max_summary_chars) {
            return Err("Agent budget.max_summary_chars must be between 1 and 16000".to_string());
        }
        Ok(self)
    }

    fn max_summary_chars(&self) -> usize {
        self.budget
            .as_ref()
            .map(|budget| budget.max_summary_chars)
            .unwrap_or(4_000)
    }
}

impl AgentTaskOutputRefV1 {
    fn validate(&self) -> Result<(), String> {
        if self.schema != TASK_OUTPUT_REF_SCHEMA_V1 {
            return Err(format!(
                "TaskOutput output_ref schema mismatch: expected={TASK_OUTPUT_REF_SCHEMA_V1} actual={}",
                self.schema
            ));
        }
        if self.kind != "agent" {
            return Err(format!(
                "TaskOutput output_ref kind mismatch: expected=agent actual={}",
                self.kind
            ));
        }
        for (field, value) in [
            ("runtime_job_id", self.runtime_job_id.as_str()),
            ("child_session_id", self.child_session_id.as_str()),
            ("result_ref", self.result_ref.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("TaskOutput output_ref {field} is required"));
            }
        }
        if !self
            .result_ref
            .starts_with(runtime_external_context_keys::SUBAGENT_RESULT_PREFIX)
        {
            return Err(format!(
                "TaskOutput output_ref result_ref is invalid: {}",
                self.result_ref
            ));
        }
        Ok(())
    }
}

fn required_agent_arg(value: String, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("Agent requires {field}"))
    } else {
        Ok(value.to_string())
    }
}

impl<
        S: RuntimeStore
            + ExternalContextStorePort
            + RuntimeJobStorePort
            + RuntimeStoreTransactionPort
            + AgentRuntimeSnapshotStorePort
            + Clone
            + Send
            + Sync
            + 'static,
    > AgentRuntime<S>
{
    #[cfg(test)]
    pub(in crate::runtime) async fn execute_agent_tool_call(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        call: &ToolCallEnvelope,
    ) -> ToolExecutionResult {
        self.execute_agent_tool_call_with_tools(
            session_id,
            turn_id,
            agent_run_identity,
            call,
            &self.tools_port,
        )
        .await
    }

    pub(super) async fn execute_agent_tool_call_with_tools(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        call: &ToolCallEnvelope,
        execution_tools: &ToolLayer,
    ) -> ToolExecutionResult {
        let started_at_ms = now_ms();
        let result = self
            .build_and_schedule_agent_run(
                session_id,
                turn_id,
                agent_run_identity,
                call,
                execution_tools,
            )
            .await;
        match result {
            Ok(output) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: "ok".to_string(),
                content: json!({
                    "agentId": output["subagentId"],
                    "taskId": output["runtimeJobId"],
                    "childSessionId": output["childSessionId"],
                    "status": output["status"],
                })
                .to_string(),
                details: output,
                facts: Vec::new(),
                error: None,
                started_at_ms,
                completed_at_ms: now_ms(),
                latency_ms: now_ms().saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("agent_tool_job_scheduled".to_string()),
            },
            Err(err) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: "error".to_string(),
                content: err.diagnostic_message(),
                details: json!({ "message": err.diagnostic_message(), "storageResult": err }),
                facts: Vec::new(),
                error: Some(
                    ToolErrorInfo::from_unstructured_error(err.diagnostic_message())
                        .with_retryable(false),
                ),
                started_at_ms,
                completed_at_ms: now_ms(),
                latency_ms: now_ms().saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("agent_tool_exec_error".to_string()),
            },
        }
    }

    async fn build_and_schedule_agent_run(
        &self,
        session_id: &str,
        turn_id: &str,
        agent_run_identity: Option<&RuntimeAgentRunIdentityV1>,
        call: &ToolCallEnvelope,
        execution_tools: &ToolLayer,
    ) -> Result<Value, crate::session::store::job::JobStoreError> {
        let agent_run_identity = agent_run_identity
            .ok_or_else(|| "Agent requires the active runtime AgentRun identity".to_string())?;
        agent_run_identity.validate()?;
        let (args, existing_agent) = if call.name == "followup_task" {
            let followup: FollowupTaskArgs = serde_json::from_str(&call.args_json)
                .map_err(|err| format!("followup_task arguments are invalid: {err}"))?;
            let existing_agent = self
                .find_delegated_agent(session_id, &followup.agent_id)
                .await?;
            (
                AgentToolArgsV1 {
                    prompt: followup.prompt,
                    description: followup.description,
                    budget: followup.budget,
                },
                Some(existing_agent),
            )
        } else {
            (
                serde_json::from_str(&call.args_json)
                    .map_err(|err| format!("spawn_agent arguments are invalid: {err}"))?,
                None,
            )
        };
        let args = args.validate()?;
        let parent_tool_contracts = build_generate_tool_contracts(
            execution_tools.dynamic_tool_registry().as_ref(),
            self.config.allowed_tools.as_deref(),
            self.tools_port.execution_host_kind(),
        )?;
        let delegated_tool_contracts = parent_tool_contracts
            .iter()
            .filter(|contract| !AGENT_CONTROL_TOOL_NAMES.contains(&contract.name.as_str()))
            .map(DelegatedToolContractV1::from_tool_contract)
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .filter(|contract| {
                existing_agent
                    .as_ref()
                    .is_none_or(|agent| agent.delegated_tool_contracts.contains(contract))
            })
            .collect::<Vec<_>>();
        let allowed_tools = delegated_tool_contracts
            .iter()
            .map(|contract| contract.name.clone())
            .collect::<Vec<_>>();
        let max_summary_chars = args.max_summary_chars();
        let workspace_root = self
            .tools_port
            .cwd()
            .ok_or_else(|| "Agent requires a configured workspace root".to_string())?
            .to_string_lossy()
            .to_string();
        let now = now_ms();
        let origin_packet_ref = existing_agent.as_ref().and_then(|agent| {
            delegation_id_suffix(&agent.subagent_id, "agent-")
                .map(runtime_external_context_keys::subagent_work_packet_ref)
        });
        let run_hash = stable_text_hash(&json!([session_id, turn_id, call.id]).to_string());
        let (subagent_id, child_session_id) = existing_agent.map_or_else(
            || {
                (
                    format!("agent-{run_hash}"),
                    format!("session-agent-{run_hash}"),
                )
            },
            |agent| (agent.subagent_id, agent.child_session_id),
        );
        let child_turn_id = format!("turn-agent-{run_hash}");
        let parent_context = AgentRunContext::root(
            session_id,
            turn_id,
            turn_id,
            agent_run_identity.agent_run_id.clone(),
            "main-agent",
            workspace_root,
            now,
        );
        let work_packet_ref =
            runtime_external_context_keys::subagent_work_packet_ref(run_hash.as_str());
        let mut job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: session_id.to_string(),
            parent_turn_id: turn_id.to_string(),
            tool_call_id: call.id.clone(),
            subagent_id: subagent_id.clone(),
            work_packet_ref: work_packet_ref.clone(),
            checkpoint_id: None,
            run_at_ms: now,
            created_at_ms: now,
            max_retries: 0,
        });
        if let Some(existing) = self.job_store.get_runtime_job(&job.job_id).await? {
            if existing.idempotency_key != job.idempotency_key
                || existing.payload_ref != job.payload_ref
            {
                return Err(JobStoreError::Rejected(
                    "delegated task identity conflict".to_string(),
                ));
            }
            let packet =
                load_subagent_work_packet_async(self.job_store.as_ref(), &existing).await?;
            let binding = subagent_work_packet_runtime_binding(&packet, &existing)?;
            return Ok(agent_submission_details(&existing, &binding));
        }
        if origin_packet_ref.is_some() {
            job.run_at_ms = self
                .next_delegated_run_at(session_id, &subagent_id, now)
                .await?;
        }
        let child_context = AgentRunContext::child(
            &parent_context,
            child_session_id.clone(),
            child_turn_id.clone(),
            job.job_id.clone(),
            subagent_id.clone(),
            now,
        );
        let mut packet = SubAgentWorkPacket::new(
            child_context,
            TaskBrief {
                task_id: Some(job.job_id.clone()),
                objective: args.prompt,
                success_criteria: vec!["Return a concise, evidence-backed result.".to_string()],
                constraints: vec!["Do not expose internal runtime ids to the user.".to_string()],
                output_hint: Some(args.description.clone()),
            },
            HotView {
                summary: format!("Parent turn {turn_id}: {}", args.description),
                recent_message_ids: vec![],
                state_kv: JsonMap::new(),
            },
            OutputContract {
                response_mode: "bounded_agent_result".to_string(),
                expected_sections: vec!["summary".to_string()],
                require_artifact_refs: false,
                max_summary_chars: Some(max_summary_chars),
            },
            ContextTransferMode::Borrow,
        );
        packet.allowed_tools = allowed_tools;
        packet.delegated_tool_contracts = delegated_tool_contracts;
        if packet
            .allowed_tools
            .iter()
            .any(|tool| matches!(tool.as_str(), "write" | "edit" | "bash"))
        {
            packet.writable_path_prefixes = vec![self
                .tools_port
                .cwd()
                .expect("workspace root was validated above")
                .to_path_buf()
                .to_string_lossy()
                .to_string()];
        }
        packet.validate_for_agent_runtime()?;

        let result_ref = runtime_external_context_keys::subagent_result_ref(job.job_id.as_str());
        let mut object = ExternalContextObject {
            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
            object_id: work_packet_ref.clone(),
            object_kind: "subagent_work_packet".to_string(),
            source_provider_id: "centaeris.core".to_string(),
            source_tool_name: call.name.clone(),
            title: args.description.clone(),
            content: serde_json::to_string(&json!({ "workPacket": packet }))
                .map_err(|err| format!("serialize Agent work packet failed: {err}"))?,
            metadata: json!({
                "schema": "agent_tool_work_packet_v1",
                "sessionId": session_id,
                "parentTurnId": turn_id,
                "childTurnId": child_turn_id.clone(),
                "toolCallId": call.id,
                "childSessionId": child_session_id,
                "subagentId": subagent_id,
                "runtimeJobId": job.job_id,
                "resultRef": result_ref,
            }),
            updated_at_ms: now,
        };
        if let Some(origin) = origin_packet_ref {
            object.metadata["originWorkPacketRef"] = Value::String(origin);
        }
        let scheduled = self
            .persist_subagent_creation(UpsertExternalContextAndScheduleJobRequest { object, job })
            .await?;
        if scheduled.job.job_kind != SUBAGENT_RUN_JOB_KIND
            || scheduled.job.session_id.as_deref() != Some(session_id)
            || scheduled.job.payload_ref.as_deref() != Some(work_packet_ref.as_str())
        {
            return Err(format!(
                "Agent scheduled runtime job identity mismatch: {}",
                scheduled.job.job_id
            )
            .into());
        }
        let binding = SubagentWorkPacketRuntimeBindingV1 {
            child_session_id,
            child_turn_id,
            subagent_id,
            parent_agent_run_id: agent_run_identity.agent_run_id.clone(),
            parent_turn_id: turn_id.to_string(),
            cancel_on_parent_cancel: true,
            description: args.description,
            allowed_tools: Vec::new(),
            delegated_tool_contracts: Vec::new(),
        };
        Ok(agent_submission_details(&scheduled.job, &binding))
    }

    async fn next_delegated_run_at(
        &self,
        session_id: &str,
        agent_id: &str,
        now: TimestampMs,
    ) -> Result<TimestampMs, JobStoreError> {
        const PAGE: usize = 128;
        let mut due_at = now;
        let mut offset = 0;
        loop {
            let jobs = self
                .job_store
                .list_runtime_jobs(ListRuntimeJobsRequest {
                    job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
                    session_id: Some(session_id.to_string()),
                    statuses: vec![
                        RuntimeJobStatus::Queued,
                        RuntimeJobStatus::Leased,
                        RuntimeJobStatus::Running,
                    ],
                    limit: PAGE,
                    offset,
                    ..Default::default()
                })
                .await?;
            for job in &jobs {
                if subagent_id_from_job(job)? == agent_id {
                    due_at = due_at.max(job.run_at_ms.saturating_add(1));
                }
            }
            if jobs.len() < PAGE {
                return Ok(due_at);
            }
            offset += jobs.len();
        }
    }

    async fn find_delegated_agent(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> Result<SubagentWorkPacketRuntimeBindingV1, JobStoreError> {
        let missing = || {
            "agent_id not found in this Session. Use list_agents and copy agentId exactly."
                .to_string()
        };
        let suffix = delegation_id_suffix(agent_id, "agent-").ok_or_else(missing)?;
        let work_ref = runtime_external_context_keys::subagent_work_packet_ref(suffix);
        let object = self
            .job_store
            .load_external_context_object(&work_ref)
            .await?
            .filter(|object| {
                object.object_kind == "subagent_work_packet"
                    && object.source_provider_id == "centaeris.core"
                    && matches!(object.source_tool_name.as_str(), "agent" | "spawn_agent")
                    && object.metadata["agentRetiredAtMs"].is_null()
                    && object.metadata["sessionId"].as_str() == Some(session_id)
                    && object.metadata["subagentId"].as_str() == Some(agent_id)
            })
            .ok_or_else(missing)?;
        let task_id = object.metadata["runtimeJobId"]
            .as_str()
            .ok_or_else(missing)?;
        let job = self.find_delegated_task(session_id, task_id).await?;
        if job.payload_ref.as_deref() != Some(work_ref.as_str()) {
            return Err(JobStoreError::Rejected(
                "delegated Agent origin does not match its task".to_string(),
            ));
        }
        let packet = SubagentWorkPacketEnvelope {
            ref_id: object.object_id,
            content_json: serde_json::from_str(&object.content)
                .map_err(|error| format!("invalid Agent origin: {error}"))?,
        };
        let binding = subagent_work_packet_runtime_binding(&packet, &job)?;
        if binding.subagent_id != agent_id
            || binding.child_session_id != format!("session-{agent_id}")
        {
            return Err(JobStoreError::Rejected(
                "delegated Agent origin identity mismatch".to_string(),
            ));
        }
        Ok(binding)
    }

    async fn find_delegated_task(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<RuntimeJobRecord, JobStoreError> {
        let missing = || {
            "task_id not found in this Session. Use list_agents and copy taskId exactly."
                .to_string()
        };
        if delegation_id_suffix(task_id, runtime_job_keys::SUBAGENT_RUN_PREFIX).is_none() {
            return Err(missing().into());
        }
        self.job_store
            .get_runtime_job(task_id)
            .await?
            .filter(|job| {
                job.job_kind == SUBAGENT_RUN_JOB_KIND
                    && job.session_id.as_deref() == Some(session_id)
            })
            .ok_or_else(|| missing().into())
    }

    async fn persist_subagent_creation(
        &self,
        req: UpsertExternalContextAndScheduleJobRequest,
    ) -> Result<
        crate::session::reliability::ScheduleRuntimeJobResult,
        crate::session::store::job::JobStoreError,
    > {
        use crate::session::store::job::{JobStoreError, JobWriteOutcome};
        match self
            .job_store
            .upsert_external_context_and_schedule_job(req.clone())
            .await
        {
            Ok(result) => Ok(result),
            Err(error)
                if error.requires_settlement()
                    || matches!(
                        error,
                        JobStoreError::Storage {
                            outcome: JobWriteOutcome::Committed,
                            ..
                        }
                    ) =>
            {
                self.job_store
                    .settle_job_operations()
                    .await
                    .map_err(|settlement| {
                        error.clone().with_message(format!(
                            "{error}; creation settlement failed: {settlement}"
                        ))
                    })?;
                let job = self
                    .job_store
                    .get_runtime_job(&req.job.job_id)
                    .await
                    .map_err(|read| {
                        error.clone().with_message(format!(
                            "{error}; Job reconciliation read failed: {read}"
                        ))
                    })?;
                let object = self
                    .job_store
                    .load_external_context_object(&req.object.object_id)
                    .await
                    .map_err(|read| {
                        error.clone().with_message(format!(
                            "{error}; WorkPacket reconciliation read failed: {read}"
                        ))
                    })?;
                match (job, object) {
                    (Some(job), Some(object))
                        if job.job_id == req.job.job_id
                            && job.job_kind == req.job.job_kind
                            && job.idempotency_key == req.job.idempotency_key
                            && job.session_id == req.job.session_id
                            && job.branch_id == req.job.branch_id
                            && job.payload_ref == req.job.payload_ref
                            && object == req.object =>
                    {
                        Ok(crate::session::reliability::ScheduleRuntimeJobResult {
                            disposition:
                                crate::session::reliability::ScheduleRuntimeJobDisposition::Existing,
                            job,
                        })
                    }
                    _ => {
                        let message = format!("{error}; creation could not be reconciled from authoritative Job and WorkPacket");
                        Err(error.with_message(message))
                    }
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(in crate::runtime) async fn execute_task_runtime_tool_call(
        &self,
        session_id: &str,
        call: &ToolCallEnvelope,
    ) -> ToolExecutionResult {
        let started_at_ms = now_ms();
        let output = self
            .run_agent_control_tool(session_id, call)
            .await
            .map(|details| {
                let content = if call.name == "wait_agent" {
                    "Waiting for the delegated task.".to_string()
                } else {
                    details.to_string()
                };
                (content, details)
            })
            .map_err(|error| error.diagnostic_message());

        let completed_at_ms = now_ms();
        match output {
            Ok((content, details)) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: "ok".to_string(),
                content,
                details,
                facts: Vec::new(),
                error: None,
                started_at_ms,
                completed_at_ms,
                latency_ms: completed_at_ms.saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("task_runtime_tool_exec".to_string()),
            },
            Err(err) => ToolExecutionResult {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: "error".to_string(),
                content: err.clone(),
                details: json!({ "message": err }),
                facts: Vec::new(),
                error: Some(ToolErrorInfo::from_unstructured_error(err)),
                started_at_ms,
                completed_at_ms,
                latency_ms: completed_at_ms.saturating_sub(started_at_ms),
                parallel_group: None,
                transition_reason: Some("task_runtime_tool_exec_error".to_string()),
            },
        }
    }

    async fn run_agent_control_tool(
        &self,
        session_id: &str,
        call: &ToolCallEnvelope,
    ) -> Result<Value, JobStoreError> {
        if call.name == "list_agents" {
            serde_json::from_str::<ListAgentsArgs>(&call.args_json)
                .map_err(|error| format!("list_agents arguments are invalid: {error}"))?;
            return self.list_delegated_agents(session_id).await;
        }
        let args: TaskIdArgs = serde_json::from_str(&call.args_json)
            .map_err(|error| format!("{} arguments are invalid: {error}", call.name))?;
        let job = self.find_delegated_task(session_id, &args.task_id).await?;
        let packet = load_subagent_work_packet_async(self.job_store.as_ref(), &job).await?;
        let binding = subagent_work_packet_runtime_binding(&packet, &job)?;
        match call.name.as_str() {
            "wait_agent" => Ok(json!({
                "schema": AGENT_TASK_OUTPUT_WAIT_SCHEMA_V1,
                "outputRef": agent_task_output_ref(&job, &binding),
            })),
            "interrupt_agent" => {
                let receipt = request_subagent_task_cancellation(
                    self.job_store.as_ref(),
                    RequestSubagentJobCancellation {
                        job_id: job.job_id.clone(),
                        session_id: session_id.to_owned(),
                        requested_at_ms: now_ms(),
                    },
                )
                .await?;
                Ok(json!({
                    "agentId": binding.subagent_id,
                    "taskId": job.job_id,
                    "cancelAccepted": receipt.accepted,
                    "status": receipt.status,
                }))
            }
            _ => Err(format!("unsupported Agent control tool: {}", call.name).into()),
        }
    }

    async fn list_delegated_agents(&self, session_id: &str) -> Result<Value, JobStoreError> {
        let mut agents = std::collections::BTreeMap::<String, (Vec<Value>, Option<Value>)>::new();
        const PAGE: usize = 128;
        let mut offset = 0;
        loop {
            let jobs = self
                .job_store
                .list_runtime_jobs(ListRuntimeJobsRequest {
                    job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
                    session_id: Some(session_id.to_string()),
                    limit: PAGE,
                    offset,
                    ..Default::default()
                })
                .await?;
            let count = jobs.len();
            for job in jobs {
                let packet = load_subagent_work_packet_async(self.job_store.as_ref(), &job).await?;
                let binding = subagent_work_packet_runtime_binding(&packet, &job)?;
                let item = json!({
                    "taskId": job.job_id,
                    "description": binding.description,
                    "status": job.status,
                });
                let agent = agents.entry(binding.subagent_id).or_default();
                if job.status.is_terminal() {
                    agent.1 = Some(item);
                } else {
                    agent.0.push(item);
                }
            }
            if count < PAGE {
                break;
            }
            offset += count;
        }
        let mut listed = Vec::with_capacity(agents.len());
        for (id, (tasks, last)) in agents {
            if delegated_agent_retired(self.job_store.as_ref(), &id).await? {
                continue;
            }
            listed.push(json!({"agentId": id, "tasks": tasks, "lastTerminalTask": last}));
        }
        Ok(json!({ "agents": listed }))
    }
}

fn agent_task_output_ref(
    job: &RuntimeJobRecord,
    binding: &SubagentWorkPacketRuntimeBindingV1,
) -> Value {
    json!({
        "schema": TASK_OUTPUT_REF_SCHEMA_V1,
        "kind": "agent",
        "runtimeJobId": job.job_id,
        "childSessionId": binding.child_session_id,
        "resultRef": runtime_external_context_keys::subagent_result_ref(&job.job_id),
    })
}

fn agent_submission_details(
    job: &RuntimeJobRecord,
    binding: &SubagentWorkPacketRuntimeBindingV1,
) -> Value {
    json!({
        "schema": AGENT_TOOL_RESULT_SCHEMA_V1,
        "status": job.status,
        "description": binding.description,
        "subagentId": binding.subagent_id,
        "runtimeJobId": job.job_id,
        "workPacketRef": job.payload_ref,
        "childSessionId": binding.child_session_id,
        "childTurnId": binding.child_turn_id,
        "outputRef": agent_task_output_ref(job, binding),
    })
}

pub(super) fn extract_agent_task_output_wait(
    details: &Value,
) -> Option<Result<AgentTaskOutputRefV1, String>> {
    if details.get("schema").and_then(Value::as_str) != Some(AGENT_TASK_OUTPUT_WAIT_SCHEMA_V1) {
        return None;
    }
    Some(
        details
            .get("outputRef")
            .cloned()
            .ok_or_else(|| "Agent TaskOutput wait is missing outputRef".to_string())
            .and_then(|value| {
                serde_json::from_value::<AgentTaskOutputRefV1>(value)
                    .map_err(|error| format!("decode Agent TaskOutput outputRef failed: {error}"))
            })
            .and_then(|output_ref| {
                output_ref.validate()?;
                Ok(output_ref)
            }),
    )
}
