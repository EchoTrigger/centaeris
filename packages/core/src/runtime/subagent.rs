use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::time::{Duration, Instant};

use futures::FutureExt;

use crate::runtime::contracts::TimestampMs;
use crate::runtime::keys::external_context as runtime_external_context_keys;
use crate::runtime::keys::runtime_job as runtime_job_keys;
use crate::runtime::subagent_contracts::{
    ContextTransferMode, DelegatedToolContractV1, SubAgentWorkPacket,
};
use crate::session::external_context::{
    ExternalContextObject, ExternalContextStorePort, EXTERNAL_CONTEXT_SCHEMA_VERSION,
};
use crate::session::reliability::{
    CancelRuntimeJobRequest, ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest,
    FailRuntimeJobRequest, ListRuntimeJobsRequest, RenewRuntimeJobLeaseRequest,
    RuntimeBackoffPolicy, RuntimeJobFailureDisposition, RuntimeJobRecord, RuntimeJobStatus,
    RuntimeJobStorePort, ScheduleRuntimeJobRequest, ScheduleRuntimeJobResult,
    StartRuntimeJobRequest,
};
use crate::session::store::{RuntimeStoreActor, UpsertExternalContextLinkAndCompleteJobRequest};
use crate::tool::layer::ToolLayer;

pub const SUBAGENT_RUN_JOB_KIND: &str = runtime_job_keys::SUBAGENT_RUN;
const SUBAGENT_AGENT_RUN_IDENTITY_SCHEMA: &str = "subagent_agent_run_identity_v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentLifecycleStatus {
    Queued,
    Leased,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
}

impl SubagentLifecycleStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    pub fn can_transition_to(&self, next: &Self) -> bool {
        use SubagentLifecycleStatus::*;

        matches!(
            (self, next),
            (Queued, Leased)
                | (Queued, Cancelled)
                | (Leased, Running)
                | (Leased, Queued)
                | (Leased, Failed)
                | (Leased, Cancelled)
                | (Running, Waiting)
                | (Running, Succeeded)
                | (Running, Failed)
                | (Running, Cancelled)
                | (Waiting, Running)
                | (Waiting, Succeeded)
                | (Waiting, Failed)
                | (Waiting, Cancelled)
        ) || self == next
    }

    pub fn runtime_job_status(&self) -> RuntimeJobStatus {
        match self {
            Self::Queued => RuntimeJobStatus::Queued,
            Self::Leased => RuntimeJobStatus::Leased,
            Self::Running | Self::Waiting => RuntimeJobStatus::Running,
            Self::Succeeded => RuntimeJobStatus::Succeeded,
            Self::Failed => RuntimeJobStatus::Failed,
            Self::Cancelled => RuntimeJobStatus::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentLifecycleRecord {
    pub subagent_id: String,
    pub parent_turn_id: String,
    pub session_id: String,
    pub status: SubagentLifecycleStatus,
    pub job_id: String,
    pub work_packet_ref: String,
    pub result_ref: Option<String>,
    pub last_error: Option<String>,
    pub updated_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRunJobRequest {
    pub session_id: String,
    pub parent_turn_id: String,
    pub tool_call_id: String,
    pub subagent_id: String,
    pub work_packet_ref: String,
    pub checkpoint_id: Option<String>,
    pub run_at_ms: TimestampMs,
    pub created_at_ms: TimestampMs,
    pub max_retries: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubagentRunIdentityV1 {
    schema: String,
    session_id: String,
    parent_turn_id: String,
    tool_call_id: String,
    subagent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClaimSubagentRunJobsRequest {
    pub now_ms: TimestampMs,
    pub worker_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub limit: usize,
    pub lease_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaimedSubagentRunJob {
    pub job: RuntimeJobRecord,
    pub lifecycle: SubagentLifecycleRecord,
    pub event: SubagentSchedulerEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompleteSubagentRunJobRequest {
    pub job_id: String,
    pub lease_owner: String,
    pub output_refs: Vec<String>,
    pub completed_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FailSubagentRunJobRequest {
    pub job_id: String,
    pub lease_owner: String,
    pub failed_at_ms: TimestampMs,
    pub last_error: String,
    pub retry: Option<SubagentRunRetry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRunRetry {
    pub next_run_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CancelSubagentRunJobRequest {
    pub job_id: String,
    pub reason: String,
    pub cancelled_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CancelSubagentRunJobsRequest {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub parent_turn_id: Option<String>,
    #[serde(default)]
    pub subagent_id: Option<String>,
    pub reason: String,
    pub cancelled_at_ms: TimestampMs,
    pub limit: usize,
    #[serde(default)]
    pub include_running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CancelSubagentRunJobsResult {
    pub cancelled: usize,
    pub skipped: usize,
    pub events: Vec<SubagentSchedulerEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentSchedulerEventKind {
    Claimed,
    Running,
    Succeeded,
    Failed,
    Requeued,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentSchedulerEvent {
    pub kind: SubagentSchedulerEventKind,
    pub subagent_id: String,
    pub child_session_id: String,
    pub parent_turn_id: String,
    pub job_id: String,
    pub work_packet_ref: Option<String>,
    #[serde(default)]
    pub result_ref: Option<String>,
    pub worker_id: Option<String>,
    pub status: SubagentLifecycleStatus,
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub started_at_ms: Option<TimestampMs>,
    #[serde(default)]
    pub completed_at_ms: Option<TimestampMs>,
    pub at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentWorkPacketEnvelope {
    pub ref_id: String,
    pub content_json: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentWorkerRunRequest {
    pub job: RuntimeJobRecord,
    pub lifecycle: SubagentLifecycleRecord,
    pub work_packet: SubagentWorkPacketEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentWorkerRunOutcome {
    Succeeded {
        summary: String,
        output_refs: Vec<String>,
    },
    Failed {
        error: String,
        retry: Option<SubagentRunRetry>,
    },
    Cancelled {
        reason: String,
    },
}

pub type SubagentWorkerRunFuture<'a> =
    Pin<Box<dyn Future<Output = SubagentWorkerRunOutcome> + Send + 'a>>;

pub trait AsyncSubagentWorkerRunner {
    fn run_async<'a>(&'a self, req: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentLifecycleHookPhase {
    Start,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentLifecycleHookEvent {
    pub schema: String,
    pub phase: SubagentLifecycleHookPhase,
    pub job_id: String,
    pub subagent_id: String,
    pub session_id: String,
    pub parent_turn_id: String,
    pub work_packet_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SubagentLifecycleStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<TimestampMs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<TimestampMs>,
}

pub type SubagentLifecycleObserverFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

pub trait AsyncSubagentLifecycleObserver {
    fn on_subagent_start<'a>(
        &'a self,
        event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'a>;

    fn on_subagent_stop<'a>(
        &'a self,
        event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'a>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSubagentLifecycleObserver;

impl AsyncSubagentLifecycleObserver for NoopSubagentLifecycleObserver {
    fn on_subagent_start<'a>(
        &'a self,
        _event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'a> {
        Box::pin(async { Ok(()) })
    }

    fn on_subagent_stop<'a>(
        &'a self,
        _event: SubagentLifecycleHookEvent,
    ) -> SubagentLifecycleObserverFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunClaimedSubagentJobRequest {
    pub worker_id: String,
    pub started_at_ms: TimestampMs,
    pub finished_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunClaimedSubagentJobResult {
    pub job_id: String,
    pub subagent_id: String,
    pub events: Vec<SubagentSchedulerEvent>,
    pub final_lifecycle: SubagentLifecycleRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RunDueSubagentJobsRequest {
    pub now_ms: TimestampMs,
    pub worker_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub limit: usize,
    pub lease_ms: u64,
    pub started_at_ms: TimestampMs,
    pub finished_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentWorkerPoolPolicy {
    pub max_parallelism: usize,
}

impl Default for SubagentWorkerPoolPolicy {
    fn default() -> Self {
        Self { max_parallelism: 1 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunDueSubagentJobsResult {
    pub claimed: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub requeued: usize,
    pub events: Vec<SubagentSchedulerEvent>,
    pub results: Vec<RunClaimedSubagentJobResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentResourceAccessMode {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentResourceClaim {
    pub resource_key: String,
    pub access_mode: SubagentResourceAccessMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentWorkPacketRuntimeBindingV1 {
    pub child_session_id: String,
    pub child_turn_id: String,
    pub subagent_id: String,
    pub parent_agent_run_id: String,
    pub parent_turn_id: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub delegated_tool_contracts: Vec<DelegatedToolContractV1>,
}

impl SubagentWorkPacketRuntimeBindingV1 {
    pub fn validate_tool_contracts(&self, tool_layer: &ToolLayer) -> Result<(), String> {
        for delegated in &self.delegated_tool_contracts {
            let contract = tool_layer.tool_contract(delegated.name.as_str())?;
            let provider_id = contract.provider_id.as_deref().ok_or_else(|| {
                format!("delegated tool providerId is required: {}", delegated.name)
            })?;
            let contract_digest = contract.contract_digest()?;
            if provider_id != delegated.provider_id
                || contract_digest != delegated.contract_digest
                || contract.concurrency_safe != delegated.concurrency_safe
            {
                return Err(format!("delegated tool contract drift: {}", delegated.name));
            }
        }
        Ok(())
    }
}

fn required_non_empty(field: &str, raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        Err(format!("{field} is required"))
    } else {
        Ok(value.to_string())
    }
}

pub fn subagent_work_packet_runtime_binding(
    envelope: &SubagentWorkPacketEnvelope,
    job: &RuntimeJobRecord,
) -> Result<SubagentWorkPacketRuntimeBindingV1, String> {
    let packet = decode_work_packet_for_resource_claim(&envelope.content_json)?;
    let child_session_id = required_non_empty(
        "workPacket.runContext.branchId",
        packet.run_context.branch_id.as_str(),
    )?;
    let subagent_id = required_non_empty(
        "workPacket.runContext.agentRef.agentId",
        packet.run_context.agent_ref.agent_id.as_str(),
    )?;
    let child_turn_id = required_non_empty(
        "workPacket.runContext.turnId",
        packet.run_context.turn_id.as_str(),
    )?;
    let parent_agent_run_id = required_non_empty(
        "workPacket.runContext.parentAgentRef.agentRunId",
        packet
            .run_context
            .parent_agent_ref
            .as_ref()
            .ok_or_else(|| "workPacket.runContext.parentAgentRef is required".to_string())?
            .agent_run_id
            .as_str(),
    )?;
    let parent_turn_id = required_non_empty(
        "workPacket.runContext.parentTurnId",
        packet
            .run_context
            .parent_turn_id
            .as_deref()
            .unwrap_or_default(),
    )?;
    if job.branch_id.as_deref() != Some(parent_turn_id.as_str()) {
        return Err(format!(
            "subagent work packet parentTurnId does not match runtime job branchId: {}",
            job.job_id
        ));
    }
    let description = packet
        .task_brief
        .output_hint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(packet.task_brief.objective.as_str())
        .to_string();
    Ok(SubagentWorkPacketRuntimeBindingV1 {
        child_session_id,
        child_turn_id,
        subagent_id,
        parent_agent_run_id,
        parent_turn_id,
        description,
        allowed_tools: packet.allowed_tools,
        delegated_tool_contracts: packet.delegated_tool_contracts,
    })
}

pub fn build_subagent_run_job(req: SubagentRunJobRequest) -> RuntimeJobRecord {
    let stable_key = serde_json::to_string(&SubagentRunIdentityV1 {
        schema: SUBAGENT_AGENT_RUN_IDENTITY_SCHEMA.to_string(),
        session_id: req.session_id.clone(),
        parent_turn_id: req.parent_turn_id.clone(),
        tool_call_id: req.tool_call_id.clone(),
        subagent_id: req.subagent_id.clone(),
    })
    .expect("serialize subagent run identity");
    RuntimeJobRecord {
        job_id: runtime_job_keys::subagent_run_job_id(stable_hash(stable_key.as_str()).as_str()),
        job_kind: SUBAGENT_RUN_JOB_KIND.to_string(),
        status: RuntimeJobStatus::Queued,
        run_at_ms: req.run_at_ms,
        lease_owner: None,
        lease_expires_at_ms: None,
        heartbeat_at_ms: None,
        retry_count: 0,
        max_retries: req.max_retries,
        backoff_policy: RuntimeBackoffPolicy::default(),
        idempotency_key: runtime_job_keys::subagent_run_idempotency_key(stable_key.as_str()),
        session_id: Some(req.session_id),
        branch_id: Some(req.parent_turn_id),
        checkpoint_id: req.checkpoint_id,
        payload_ref: Some(req.work_packet_ref),
        output_refs: vec![],
        last_error: None,
        created_at_ms: req.created_at_ms,
        updated_at_ms: req.created_at_ms,
    }
}

pub fn build_queued_lifecycle_record(
    session_id: impl Into<String>,
    parent_turn_id: impl Into<String>,
    subagent_id: impl Into<String>,
    job: &RuntimeJobRecord,
    updated_at_ms: TimestampMs,
) -> Result<SubagentLifecycleRecord, String> {
    Ok(SubagentLifecycleRecord {
        subagent_id: subagent_id.into(),
        parent_turn_id: parent_turn_id.into(),
        session_id: session_id.into(),
        status: SubagentLifecycleStatus::Queued,
        job_id: job.job_id.clone(),
        work_packet_ref: subagent_job_work_packet_ref(job)?.to_string(),
        result_ref: None,
        last_error: None,
        updated_at_ms,
    })
}

pub fn enqueue_subagent_run_job<S: RuntimeJobStorePort>(
    store: &S,
    req: SubagentRunJobRequest,
) -> Result<ScheduleRuntimeJobResult, String> {
    store.schedule_runtime_job(ScheduleRuntimeJobRequest {
        job: build_subagent_run_job(req),
    })
}

pub fn claim_subagent_run_jobs<S: RuntimeJobStorePort>(
    store: &S,
    req: ClaimSubagentRunJobsRequest,
) -> Result<Vec<ClaimedSubagentRunJob>, String> {
    let jobs = store.claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
        now_ms: req.now_ms,
        worker_id: req.worker_id.clone(),
        job_id: None,
        job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
        session_id: req.session_id.clone(),
        limit: req.limit,
        lease_ms: req.lease_ms,
    })?;
    jobs.into_iter()
        .map(|job| {
            let lifecycle = lifecycle_record_from_job(&job, req.now_ms)?;
            let event = scheduler_event_from_job(
                &job,
                SubagentSchedulerEventKind::Claimed,
                lifecycle.status.clone(),
                Some(req.worker_id.clone()),
                "Subagent work packet leased by scheduler.",
                req.now_ms,
            )?;
            Ok(ClaimedSubagentRunJob {
                job,
                lifecycle,
                event,
            })
        })
        .collect()
}

pub async fn claim_subagent_run_jobs_async(
    store: &RuntimeStoreActor,
    req: ClaimSubagentRunJobsRequest,
) -> Result<Vec<ClaimedSubagentRunJob>, String> {
    let jobs = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: req.now_ms,
            worker_id: req.worker_id.clone(),
            job_id: None,
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: req.session_id.clone(),
            limit: req.limit,
            lease_ms: req.lease_ms,
        })
        .await?;
    jobs.into_iter()
        .map(|job| {
            let lifecycle = lifecycle_record_from_job(&job, req.now_ms)?;
            let event = scheduler_event_from_job(
                &job,
                SubagentSchedulerEventKind::Claimed,
                lifecycle.status.clone(),
                Some(req.worker_id.clone()),
                "Subagent work packet leased by scheduler.",
                req.now_ms,
            )?;
            Ok(ClaimedSubagentRunJob {
                job,
                lifecycle,
                event,
            })
        })
        .collect()
}

pub fn build_running_lifecycle_record(
    job: &RuntimeJobRecord,
    worker_id: impl Into<String>,
    updated_at_ms: TimestampMs,
) -> Result<(SubagentLifecycleRecord, SubagentSchedulerEvent), String> {
    let mut lifecycle = lifecycle_record_from_job(job, updated_at_ms)?;
    lifecycle.status = SubagentLifecycleStatus::Running;
    let event = scheduler_event_from_job(
        job,
        SubagentSchedulerEventKind::Running,
        SubagentLifecycleStatus::Running,
        Some(worker_id.into()),
        "Subagent worker started processing the work packet.",
        updated_at_ms,
    )?;
    Ok((lifecycle, event))
}

pub fn complete_subagent_run_job<S: RuntimeJobStorePort>(
    store: &S,
    req: CompleteSubagentRunJobRequest,
) -> Result<SubagentSchedulerEvent, String> {
    store.complete_runtime_job(CompleteRuntimeJobRequest {
        job_id: req.job_id.clone(),
        lease_owner: req.lease_owner.clone(),
        output_refs: req.output_refs,
        completed_at_ms: req.completed_at_ms,
    })?;
    let job = store.get_runtime_job(req.job_id.as_str())?.ok_or_else(|| {
        format!(
            "subagent runtime job not found after complete: {}",
            req.job_id
        )
    })?;
    scheduler_event_from_job(
        &job,
        SubagentSchedulerEventKind::Succeeded,
        SubagentLifecycleStatus::Succeeded,
        Some(req.lease_owner),
        "Subagent worker completed successfully.",
        req.completed_at_ms,
    )
}

pub async fn complete_subagent_run_job_async(
    store: &RuntimeStoreActor,
    req: CompleteSubagentRunJobRequest,
) -> Result<SubagentSchedulerEvent, String> {
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: req.job_id.clone(),
            lease_owner: req.lease_owner.clone(),
            output_refs: req.output_refs,
            completed_at_ms: req.completed_at_ms,
        })
        .await?;
    let job = store
        .get_runtime_job(req.job_id.as_str())
        .await?
        .ok_or_else(|| {
            format!(
                "subagent runtime job not found after complete: {}",
                req.job_id
            )
        })?;
    scheduler_event_from_job(
        &job,
        SubagentSchedulerEventKind::Succeeded,
        SubagentLifecycleStatus::Succeeded,
        Some(req.lease_owner),
        "Subagent worker completed successfully.",
        req.completed_at_ms,
    )
}

pub fn fail_subagent_run_job<S: RuntimeJobStorePort>(
    store: &S,
    req: FailSubagentRunJobRequest,
) -> Result<SubagentSchedulerEvent, String> {
    let (disposition, next_run_at_ms, kind, status, summary) = if let Some(retry) = req.retry {
        (
            RuntimeJobFailureDisposition::RetryScheduled,
            Some(retry.next_run_at_ms),
            SubagentSchedulerEventKind::Requeued,
            SubagentLifecycleStatus::Queued,
            "Subagent worker failed; job requeued for retry.",
        )
    } else {
        (
            RuntimeJobFailureDisposition::Failed,
            None,
            SubagentSchedulerEventKind::Failed,
            SubagentLifecycleStatus::Failed,
            "Subagent worker failed without retry.",
        )
    };
    store.fail_runtime_job(FailRuntimeJobRequest {
        job_id: req.job_id.clone(),
        lease_owner: req.lease_owner.clone(),
        failed_at_ms: req.failed_at_ms,
        last_error: req.last_error,
        next_run_at_ms,
        disposition,
    })?;
    let job = store
        .get_runtime_job(req.job_id.as_str())?
        .ok_or_else(|| format!("subagent runtime job not found after fail: {}", req.job_id))?;
    scheduler_event_from_job(
        &job,
        kind,
        status,
        Some(req.lease_owner),
        summary,
        req.failed_at_ms,
    )
}

pub async fn fail_subagent_run_job_async(
    store: &RuntimeStoreActor,
    req: FailSubagentRunJobRequest,
) -> Result<SubagentSchedulerEvent, String> {
    let (disposition, next_run_at_ms, kind, status, summary) = if let Some(retry) = req.retry {
        (
            RuntimeJobFailureDisposition::RetryScheduled,
            Some(retry.next_run_at_ms),
            SubagentSchedulerEventKind::Requeued,
            SubagentLifecycleStatus::Queued,
            "Subagent worker failed; job requeued for retry.",
        )
    } else {
        (
            RuntimeJobFailureDisposition::Failed,
            None,
            SubagentSchedulerEventKind::Failed,
            SubagentLifecycleStatus::Failed,
            "Subagent worker failed without retry.",
        )
    };
    store
        .fail_runtime_job(FailRuntimeJobRequest {
            job_id: req.job_id.clone(),
            lease_owner: req.lease_owner.clone(),
            failed_at_ms: req.failed_at_ms,
            last_error: req.last_error,
            next_run_at_ms,
            disposition,
        })
        .await?;
    let job = store
        .get_runtime_job(req.job_id.as_str())
        .await?
        .ok_or_else(|| format!("subagent runtime job not found after fail: {}", req.job_id))?;
    scheduler_event_from_job(
        &job,
        kind,
        status,
        Some(req.lease_owner),
        summary,
        req.failed_at_ms,
    )
}

pub fn cancel_subagent_run_job<S: RuntimeJobStorePort>(
    store: &S,
    req: CancelSubagentRunJobRequest,
) -> Result<SubagentSchedulerEvent, String> {
    store.cancel_runtime_job(CancelRuntimeJobRequest {
        job_id: req.job_id.clone(),
        reason: req.reason,
        cancelled_at_ms: req.cancelled_at_ms,
        expected_status: None,
    })?;
    let job = store.get_runtime_job(req.job_id.as_str())?.ok_or_else(|| {
        format!(
            "subagent runtime job not found after cancel: {}",
            req.job_id
        )
    })?;
    scheduler_event_from_job(
        &job,
        SubagentSchedulerEventKind::Cancelled,
        SubagentLifecycleStatus::Cancelled,
        None,
        "Subagent job cancelled.",
        req.cancelled_at_ms,
    )
}

pub async fn cancel_subagent_run_job_async(
    store: &RuntimeStoreActor,
    req: CancelSubagentRunJobRequest,
) -> Result<SubagentSchedulerEvent, String> {
    store
        .cancel_runtime_job(CancelRuntimeJobRequest {
            job_id: req.job_id.clone(),
            reason: req.reason,
            cancelled_at_ms: req.cancelled_at_ms,
            expected_status: None,
        })
        .await?;
    let job = store
        .get_runtime_job(req.job_id.as_str())
        .await?
        .ok_or_else(|| {
            format!(
                "subagent runtime job not found after cancel: {}",
                req.job_id
            )
        })?;
    scheduler_event_from_job(
        &job,
        SubagentSchedulerEventKind::Cancelled,
        SubagentLifecycleStatus::Cancelled,
        None,
        "Subagent job cancelled.",
        req.cancelled_at_ms,
    )
}

pub fn cancel_subagent_run_jobs<S: RuntimeJobStorePort>(
    store: &S,
    req: CancelSubagentRunJobsRequest,
) -> Result<CancelSubagentRunJobsResult, String> {
    let mut statuses = vec![RuntimeJobStatus::Queued, RuntimeJobStatus::Leased];
    if req.include_running {
        statuses.push(RuntimeJobStatus::Running);
    }
    let jobs = store.list_runtime_jobs(ListRuntimeJobsRequest {
        statuses,
        job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
        session_id: req.session_id.clone(),
        branch_id: req.parent_turn_id.clone(),
        limit: req.limit.max(1),
        offset: 0,
    })?;
    let subagent_id_filter = req
        .subagent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut cancelled = 0usize;
    let mut skipped = 0usize;
    let mut events = Vec::new();
    for job in jobs {
        if let Some(subagent_id) = subagent_id_filter.as_deref() {
            if subagent_id_from_job(&job)? != subagent_id {
                skipped = skipped.saturating_add(1);
                continue;
            }
        }
        match cancel_subagent_run_job(
            store,
            CancelSubagentRunJobRequest {
                job_id: job.job_id.clone(),
                reason: req.reason.clone(),
                cancelled_at_ms: req.cancelled_at_ms,
            },
        ) {
            Ok(event) => {
                cancelled = cancelled.saturating_add(1);
                events.push(event);
            }
            Err(error) => {
                return Err(format!(
                    "cancel subagent runtime job failed: job_id={} error={error}",
                    job.job_id
                ));
            }
        }
    }
    Ok(CancelSubagentRunJobsResult {
        cancelled,
        skipped,
        events,
    })
}

pub async fn cancel_subagent_run_jobs_async(
    store: &RuntimeStoreActor,
    req: CancelSubagentRunJobsRequest,
) -> Result<CancelSubagentRunJobsResult, String> {
    let mut statuses = vec![RuntimeJobStatus::Queued, RuntimeJobStatus::Leased];
    if req.include_running {
        statuses.push(RuntimeJobStatus::Running);
    }
    let jobs = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses,
            job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: req.session_id.clone(),
            branch_id: req.parent_turn_id.clone(),
            limit: req.limit.max(1),
            offset: 0,
        })
        .await?;
    let subagent_id_filter = req
        .subagent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut cancelled = 0usize;
    let mut skipped = 0usize;
    let mut events = Vec::new();
    for job in jobs {
        if let Some(subagent_id) = subagent_id_filter.as_deref() {
            if subagent_id_from_job(&job)? != subagent_id {
                skipped = skipped.saturating_add(1);
                continue;
            }
        }
        match cancel_subagent_run_job_async(
            store,
            CancelSubagentRunJobRequest {
                job_id: job.job_id.clone(),
                reason: req.reason.clone(),
                cancelled_at_ms: req.cancelled_at_ms,
            },
        )
        .await
        {
            Ok(event) => {
                cancelled = cancelled.saturating_add(1);
                events.push(event);
            }
            Err(error) => {
                return Err(format!(
                    "cancel subagent runtime job failed: job_id={} error={error}",
                    job.job_id
                ));
            }
        }
    }
    Ok(CancelSubagentRunJobsResult {
        cancelled,
        skipped,
        events,
    })
}

pub fn lifecycle_record_from_job(
    job: &RuntimeJobRecord,
    updated_at_ms: TimestampMs,
) -> Result<SubagentLifecycleRecord, String> {
    Ok(SubagentLifecycleRecord {
        subagent_id: subagent_id_from_job(job)?,
        parent_turn_id: subagent_job_parent_turn_id(job)?.to_string(),
        session_id: subagent_job_session_id(job)?.to_string(),
        status: SubagentLifecycleStatus::from(job.status.clone()),
        job_id: job.job_id.clone(),
        work_packet_ref: subagent_job_work_packet_ref(job)?.to_string(),
        result_ref: job.output_refs.first().cloned(),
        last_error: job.last_error.clone(),
        updated_at_ms,
    })
}

pub async fn run_claimed_subagent_job_async<R, O>(
    store: &RuntimeStoreActor,
    claimed: ClaimedSubagentRunJob,
    runner: &R,
    observer: &O,
    req: RunClaimedSubagentJobRequest,
) -> Result<RunClaimedSubagentJobResult, String>
where
    R: AsyncSubagentWorkerRunner,
    O: AsyncSubagentLifecycleObserver,
{
    let lease_owner = subagent_job_lease_owner(&claimed.job)?.to_string();
    if let Err(error) = store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: claimed.job.job_id.clone(),
            lease_owner: lease_owner.clone(),
            started_at_ms: req.started_at_ms,
        })
        .await
    {
        if let Some(cancelled) = cancelled_subagent_event_if_requested_async(
            store,
            claimed.job.job_id.as_str(),
            req.started_at_ms,
        )
        .await?
        {
            let final_lifecycle = lifecycle_record_from_job(&cancelled.0, req.started_at_ms)?;
            let result = RunClaimedSubagentJobResult {
                job_id: cancelled.0.job_id,
                subagent_id: final_lifecycle.subagent_id.clone(),
                events: vec![cancelled.1],
                final_lifecycle,
            };
            observer
                .on_subagent_stop(build_subagent_stop_hook_event(
                    &result.final_lifecycle,
                    None,
                    req.started_at_ms,
                )?)
                .await?;
            return Ok(result);
        }
        return Err(error);
    }
    let running_job = store
        .get_runtime_job(claimed.job.job_id.as_str())
        .await?
        .unwrap_or(claimed.job.clone());
    let (running_lifecycle, running_event) =
        build_running_lifecycle_record(&running_job, req.worker_id.clone(), req.started_at_ms)?;
    let mut events = vec![running_event];
    let work_packet = match load_subagent_work_packet_async(store, &running_job).await {
        Ok(work_packet) => work_packet,
        Err(err) => {
            let failed_event = fail_subagent_run_job_async(
                store,
                FailSubagentRunJobRequest {
                    job_id: running_job.job_id.clone(),
                    lease_owner,
                    failed_at_ms: req.finished_at_ms,
                    last_error: err,
                    retry: None,
                },
            )
            .await?;
            let final_job = store
                .get_runtime_job(running_job.job_id.as_str())
                .await?
                .unwrap_or_else(|| running_job.clone());
            let final_lifecycle = lifecycle_record_from_job(&final_job, req.finished_at_ms)?;
            events.push(failed_event);
            let result = RunClaimedSubagentJobResult {
                job_id: final_job.job_id.clone(),
                subagent_id: running_lifecycle.subagent_id,
                events,
                final_lifecycle,
            };
            observer
                .on_subagent_stop(build_subagent_stop_hook_event(
                    &result.final_lifecycle,
                    Some(&final_job),
                    req.finished_at_ms,
                )?)
                .await?;
            return Ok(result);
        }
    };
    apply_work_packet_description_to_events(&mut events, &work_packet);
    if let Some((cancelled_job, cancelled_event)) = cancelled_subagent_event_if_requested_async(
        store,
        running_job.job_id.as_str(),
        req.finished_at_ms,
    )
    .await?
    {
        let final_lifecycle = lifecycle_record_from_job(&cancelled_job, req.finished_at_ms)?;
        events.push(cancelled_event);
        let result = RunClaimedSubagentJobResult {
            job_id: final_lifecycle.job_id.clone(),
            subagent_id: final_lifecycle.subagent_id.clone(),
            events,
            final_lifecycle,
        };
        observer
            .on_subagent_stop(build_subagent_stop_hook_event(
                &result.final_lifecycle,
                Some(&cancelled_job),
                req.finished_at_ms,
            )?)
            .await?;
        return Ok(result);
    }
    observer
        .on_subagent_start(build_subagent_start_hook_event(
            &running_job,
            &running_lifecycle,
            &work_packet,
            req.started_at_ms,
        )?)
        .await?;
    let lease_ms = running_job
        .lease_expires_at_ms
        .zip(running_job.heartbeat_at_ms)
        .and_then(|(expires_at_ms, heartbeat_at_ms)| {
            expires_at_ms
                .checked_sub(heartbeat_at_ms)
                .and_then(|value| u64::try_from(value).ok())
        })
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            format!(
                "running subagent runtime job has invalid lease window: {}",
                running_job.job_id
            )
        })?;
    let agent_run_started = Instant::now();
    let mut heartbeat = tokio::time::interval(Duration::from_millis((lease_ms / 3).max(1)));
    heartbeat.tick().await;
    let mut run_future = runner.run_async(SubagentWorkerRunRequest {
        job: running_job.clone(),
        lifecycle: running_lifecycle.clone(),
        work_packet: work_packet.clone(),
    });
    let outcome = loop {
        tokio::select! {
            outcome = &mut run_future => break outcome,
            _ = heartbeat.tick() => {
                let renewal = store.renew_runtime_job_lease(RenewRuntimeJobLeaseRequest {
                    job_id: running_job.job_id.clone(),
                    lease_owner: lease_owner.clone(),
                    heartbeat_at_ms: req.started_at_ms.saturating_add(
                        i64::try_from(agent_run_started.elapsed().as_millis()).unwrap_or(i64::MAX),
                    ),
                    lease_ms,
                }).await;
                if let Err(error) = renewal {
                    if cancelled_subagent_event_if_requested_async(
                        store,
                        running_job.job_id.as_str(),
                        req.started_at_ms,
                    )
                    .await?
                    .is_some()
                    {
                        break SubagentWorkerRunOutcome::Cancelled {
                            reason: "subagent_cancelled".to_string(),
                        };
                    }
                    return Err(error);
                }
            }
        }
    };
    let finished_at_ms = req.finished_at_ms.max(req.started_at_ms.saturating_add(
        i64::try_from(agent_run_started.elapsed().as_millis()).unwrap_or(i64::MAX),
    ));

    if let Some((cancelled_job, cancelled_event)) = cancelled_subagent_event_if_requested_async(
        store,
        running_job.job_id.as_str(),
        finished_at_ms,
    )
    .await?
    {
        let final_lifecycle = lifecycle_record_from_job(&cancelled_job, finished_at_ms)?;
        events.push(cancelled_event);
        let result = RunClaimedSubagentJobResult {
            job_id: final_lifecycle.job_id.clone(),
            subagent_id: final_lifecycle.subagent_id.clone(),
            events,
            final_lifecycle,
        };
        observer
            .on_subagent_stop(build_subagent_stop_hook_event(
                &result.final_lifecycle,
                Some(&cancelled_job),
                finished_at_ms,
            )?)
            .await?;
        return Ok(result);
    }

    let final_event = match outcome {
        SubagentWorkerRunOutcome::Succeeded { summary, .. } => {
            let result_ref =
                runtime_external_context_keys::subagent_result_ref(running_job.job_id.as_str());
            let packet = decode_work_packet_for_resource_claim(&work_packet.content_json)?;
            store
                .upsert_external_context_link_and_complete_job(
                    UpsertExternalContextLinkAndCompleteJobRequest {
                        object: Some(ExternalContextObject {
                            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
                            object_id: result_ref.clone(),
                            object_kind: "subagent_result".to_string(),
                            source_provider_id: "centaeris.core".to_string(),
                            source_tool_name: "agent".to_string(),
                            title: packet
                                .task_brief
                                .output_hint
                                .unwrap_or_else(|| "Agent result".to_string()),
                            content: summary,
                            metadata: serde_json::json!({
                                "schema": "subagent_result_v1",
                                "runtimeJobId": running_job.job_id,
                                "parentSessionId": running_lifecycle.session_id,
                                "parentTurnId": running_lifecycle.parent_turn_id,
                                "subagentId": running_lifecycle.subagent_id,
                                "childSessionId": packet.run_context.branch_id,
                            }),
                            updated_at_ms: finished_at_ms,
                        }),
                        link: None,
                        complete_job: CompleteRuntimeJobRequest {
                            job_id: running_job.job_id.clone(),
                            lease_owner: lease_owner.clone(),
                            output_refs: vec![result_ref],
                            completed_at_ms: finished_at_ms,
                        },
                    },
                )
                .await?;
            scheduler_event_from_job(
                &store
                    .get_runtime_job(running_job.job_id.as_str())
                    .await?
                    .ok_or_else(|| {
                        format!(
                            "completed subagent runtime job disappeared: {}",
                            running_job.job_id
                        )
                    })?,
                SubagentSchedulerEventKind::Succeeded,
                SubagentLifecycleStatus::Succeeded,
                Some(lease_owner),
                "Subagent job completed.",
                finished_at_ms,
            )?
        }
        SubagentWorkerRunOutcome::Failed { error, retry } => {
            fail_subagent_run_job_async(
                store,
                FailSubagentRunJobRequest {
                    job_id: running_job.job_id.clone(),
                    lease_owner,
                    failed_at_ms: finished_at_ms,
                    last_error: error,
                    retry,
                },
            )
            .await?
        }
        SubagentWorkerRunOutcome::Cancelled { reason } => {
            cancel_subagent_run_job_async(
                store,
                CancelSubagentRunJobRequest {
                    job_id: running_job.job_id.clone(),
                    reason,
                    cancelled_at_ms: finished_at_ms,
                },
            )
            .await?
        }
    };
    events.push(final_event);
    let final_job = store
        .get_runtime_job(running_job.job_id.as_str())
        .await?
        .unwrap_or(running_job);
    let final_lifecycle = lifecycle_record_from_job(&final_job, finished_at_ms)?;
    let result = RunClaimedSubagentJobResult {
        job_id: final_job.job_id.clone(),
        subagent_id: running_lifecycle.subagent_id,
        events,
        final_lifecycle,
    };
    observer
        .on_subagent_stop(build_subagent_stop_hook_event(
            &result.final_lifecycle,
            Some(&final_job),
            finished_at_ms,
        )?)
        .await?;
    Ok(result)
}

pub async fn run_due_subagent_jobs_async<R, O>(
    store: &RuntimeStoreActor,
    runner: &R,
    observer: &O,
    req: RunDueSubagentJobsRequest,
) -> Result<RunDueSubagentJobsResult, String>
where
    R: AsyncSubagentWorkerRunner,
    O: AsyncSubagentLifecycleObserver,
{
    let claimed = claim_subagent_run_jobs_async(
        store,
        ClaimSubagentRunJobsRequest {
            now_ms: req.now_ms,
            worker_id: req.worker_id.clone(),
            session_id: req.session_id.clone(),
            limit: req.limit,
            lease_ms: req.lease_ms,
        },
    )
    .await?;
    let mut events = Vec::with_capacity(claimed.len().saturating_mul(3));
    let mut results = Vec::with_capacity(claimed.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut cancelled = 0usize;
    let mut requeued = 0usize;

    for claimed_job in claimed {
        events.push(claimed_job.event.clone());
        let result = run_claimed_subagent_job_async(
            store,
            claimed_job,
            runner,
            observer,
            RunClaimedSubagentJobRequest {
                worker_id: req.worker_id.clone(),
                started_at_ms: req.started_at_ms,
                finished_at_ms: req.finished_at_ms,
            },
        )
        .await?;
        for event in &result.events {
            match event.kind {
                SubagentSchedulerEventKind::Succeeded => succeeded = succeeded.saturating_add(1),
                SubagentSchedulerEventKind::Failed => failed = failed.saturating_add(1),
                SubagentSchedulerEventKind::Cancelled => cancelled = cancelled.saturating_add(1),
                SubagentSchedulerEventKind::Requeued => requeued = requeued.saturating_add(1),
                SubagentSchedulerEventKind::Claimed | SubagentSchedulerEventKind::Running => {}
            }
        }
        events.extend(result.events.iter().cloned());
        results.push(result);
    }

    Ok(RunDueSubagentJobsResult {
        claimed: results.len(),
        succeeded,
        failed,
        cancelled,
        requeued,
        events,
        results,
    })
}

pub async fn run_due_subagent_jobs_with_worker_pool_async<R, O>(
    store: &RuntimeStoreActor,
    runner: &R,
    observer: &O,
    req: RunDueSubagentJobsRequest,
    policy: SubagentWorkerPoolPolicy,
) -> Result<RunDueSubagentJobsResult, String>
where
    R: AsyncSubagentWorkerRunner + Sync,
    O: AsyncSubagentLifecycleObserver + Sync,
{
    let max_parallelism = policy.max_parallelism.max(1).min(req.limit.max(1));
    if max_parallelism == 1 {
        return run_due_subagent_jobs_async(store, runner, observer, req).await;
    }

    let claimed = claim_subagent_run_jobs_async(
        store,
        ClaimSubagentRunJobsRequest {
            now_ms: req.now_ms,
            worker_id: req.worker_id.clone(),
            session_id: req.session_id.clone(),
            limit: req.limit,
            lease_ms: req.lease_ms,
        },
    )
    .await?;
    let claimed_count = claimed.len();
    let mut events = Vec::with_capacity(claimed_count.saturating_mul(3));
    for claimed_job in &claimed {
        events.push(claimed_job.event.clone());
    }

    let mut indexed_results = Vec::with_capacity(claimed_count);
    let mut pending = claimed.into_iter().enumerate().collect::<VecDeque<_>>();
    while !pending.is_empty() {
        let batch = next_subagent_worker_batch_async(store, &mut pending, max_parallelism).await?;
        let futures = batch
            .into_iter()
            .map(|(index, claimed_job)| {
                let worker_id = req.worker_id.clone();
                let started_at_ms = req.started_at_ms;
                let finished_at_ms = req.finished_at_ms;
                async move {
                    let result = match AssertUnwindSafe(run_claimed_subagent_job_async(
                        store,
                        claimed_job.clone(),
                        runner,
                        observer,
                        RunClaimedSubagentJobRequest {
                            worker_id: worker_id.clone(),
                            started_at_ms,
                            finished_at_ms,
                        },
                    ))
                    .catch_unwind()
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => {
                            recover_panicked_subagent_job_async(
                                store,
                                observer,
                                &claimed_job,
                                worker_id.as_str(),
                                finished_at_ms,
                            )
                            .await
                        }
                    };
                    (index, result)
                }
            })
            .collect::<Vec<_>>();
        for (index, result) in futures::future::join_all(futures).await {
            indexed_results.push((index, result?));
        }
    }

    indexed_results.sort_by_key(|(index, _)| *index);
    let mut results = Vec::with_capacity(indexed_results.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut cancelled = 0usize;
    let mut requeued = 0usize;
    for (_, result) in indexed_results {
        for event in &result.events {
            match event.kind {
                SubagentSchedulerEventKind::Succeeded => succeeded = succeeded.saturating_add(1),
                SubagentSchedulerEventKind::Failed => failed = failed.saturating_add(1),
                SubagentSchedulerEventKind::Cancelled => cancelled = cancelled.saturating_add(1),
                SubagentSchedulerEventKind::Requeued => requeued = requeued.saturating_add(1),
                SubagentSchedulerEventKind::Claimed | SubagentSchedulerEventKind::Running => {}
            }
            events.push(event.clone());
        }
        results.push(result);
    }

    Ok(RunDueSubagentJobsResult {
        claimed: claimed_count,
        succeeded,
        failed,
        cancelled,
        requeued,
        events,
        results,
    })
}

pub fn load_subagent_work_packet<S: ExternalContextStorePort>(
    store: &S,
    job: &RuntimeJobRecord,
) -> Result<SubagentWorkPacketEnvelope, String> {
    let raw_ref = job
        .payload_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("subagent job payload_ref is required: {}", job.job_id))?;
    if let Ok(value) = serde_json::from_str::<Value>(raw_ref) {
        return Ok(SubagentWorkPacketEnvelope {
            ref_id: inline_work_packet_ref(job),
            content_json: value,
        });
    }
    let object = store
        .load_external_context_object(raw_ref)?
        .ok_or_else(|| format!("subagent work packet object not found: {raw_ref}"))?;
    let content_json = serde_json::from_str::<Value>(object.content.as_str()).map_err(|err| {
        format!(
            "decode subagent work packet object content failed: object_id={} error={err}",
            object.object_id
        )
    })?;
    Ok(SubagentWorkPacketEnvelope {
        ref_id: object.object_id,
        content_json,
    })
}

pub async fn load_subagent_work_packet_async(
    store: &RuntimeStoreActor,
    job: &RuntimeJobRecord,
) -> Result<SubagentWorkPacketEnvelope, String> {
    let raw_ref = job
        .payload_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("subagent job payload_ref is required: {}", job.job_id))?;
    if let Ok(value) = serde_json::from_str::<Value>(raw_ref) {
        return Ok(SubagentWorkPacketEnvelope {
            ref_id: inline_work_packet_ref(job),
            content_json: value,
        });
    }
    let object = store
        .load_external_context_object(raw_ref)
        .await?
        .ok_or_else(|| format!("subagent work packet object not found: {raw_ref}"))?;
    let content_json = serde_json::from_str::<Value>(object.content.as_str()).map_err(|err| {
        format!(
            "decode subagent work packet object content failed: object_id={} error={err}",
            object.object_id
        )
    })?;
    Ok(SubagentWorkPacketEnvelope {
        ref_id: object.object_id,
        content_json,
    })
}

async fn recover_panicked_subagent_job_async<O>(
    store: &RuntimeStoreActor,
    observer: &O,
    claimed: &ClaimedSubagentRunJob,
    _worker_id: &str,
    failed_at_ms: TimestampMs,
) -> Result<RunClaimedSubagentJobResult, String>
where
    O: AsyncSubagentLifecycleObserver,
{
    if let Some((cancelled_job, cancelled_event)) = cancelled_subagent_event_if_requested_async(
        store,
        claimed.job.job_id.as_str(),
        failed_at_ms,
    )
    .await?
    {
        let final_lifecycle = lifecycle_record_from_job(&cancelled_job, failed_at_ms)?;
        let result = RunClaimedSubagentJobResult {
            job_id: final_lifecycle.job_id.clone(),
            subagent_id: final_lifecycle.subagent_id.clone(),
            events: vec![cancelled_event],
            final_lifecycle,
        };
        observer
            .on_subagent_stop(build_subagent_stop_hook_event(
                &result.final_lifecycle,
                Some(&cancelled_job),
                failed_at_ms,
            )?)
            .await?;
        return Ok(result);
    }

    let retry = if claimed.job.retry_count < claimed.job.max_retries {
        Some(SubagentRunRetry {
            next_run_at_ms: failed_at_ms,
        })
    } else {
        None
    };
    let event = fail_subagent_run_job_async(
        store,
        FailSubagentRunJobRequest {
            job_id: claimed.job.job_id.clone(),
            lease_owner: subagent_job_lease_owner(&claimed.job)?.to_string(),
            failed_at_ms,
            last_error: "subagent worker panicked".to_string(),
            retry,
        },
    )
    .await?;
    let final_job = store
        .get_runtime_job(claimed.job.job_id.as_str())
        .await?
        .unwrap_or_else(|| claimed.job.clone());
    let final_lifecycle = lifecycle_record_from_job(&final_job, failed_at_ms)?;
    let result = RunClaimedSubagentJobResult {
        job_id: final_lifecycle.job_id.clone(),
        subagent_id: final_lifecycle.subagent_id.clone(),
        events: vec![event],
        final_lifecycle,
    };
    observer
        .on_subagent_stop(build_subagent_stop_hook_event(
            &result.final_lifecycle,
            Some(&final_job),
            failed_at_ms,
        )?)
        .await?;
    Ok(result)
}

async fn next_subagent_worker_batch_async(
    store: &RuntimeStoreActor,
    pending: &mut VecDeque<(usize, ClaimedSubagentRunJob)>,
    max_parallelism: usize,
) -> Result<Vec<(usize, ClaimedSubagentRunJob)>, String> {
    let mut batch = Vec::with_capacity(max_parallelism.max(1));
    let mut active_claims = Vec::with_capacity(max_parallelism.max(1));
    let scan_len = pending.len();
    for _ in 0..scan_len {
        let Some(item) = pending.pop_front() else {
            break;
        };
        if batch.len() >= max_parallelism.max(1) {
            pending.push_back(item);
            continue;
        }
        let resource_claim = subagent_resource_claim_for_job_async(store, &item.1.job).await?;
        if subagent_resource_claim_can_join(&active_claims, &resource_claim) {
            active_claims.push(resource_claim);
            batch.push(item);
        } else {
            pending.push_back(item);
        }
    }
    if batch.is_empty() {
        if let Some(item) = pending.pop_front() {
            batch.push(item);
        }
    }
    Ok(batch)
}

pub fn subagent_resource_claim_for_job<S: ExternalContextStorePort>(
    store: &S,
    job: &RuntimeJobRecord,
) -> Result<SubagentResourceClaim, String> {
    let envelope = load_subagent_work_packet(store, job).map_err(|error| {
        format!(
            "load subagent resource claim work packet failed: job_id={} error={error}",
            job.job_id
        )
    })?;
    let packet =
        decode_work_packet_for_resource_claim(&envelope.content_json).map_err(|error| {
            format!(
                "decode subagent resource claim failed: job_id={} ref_id={} error={error}",
                job.job_id, envelope.ref_id
            )
        })?;
    Ok(subagent_resource_claim_from_packet(job, &packet))
}

pub async fn subagent_resource_claim_for_job_async(
    store: &RuntimeStoreActor,
    job: &RuntimeJobRecord,
) -> Result<SubagentResourceClaim, String> {
    let envelope = load_subagent_work_packet_async(store, job)
        .await
        .map_err(|error| {
            format!(
                "load subagent resource claim work packet failed: job_id={} error={error}",
                job.job_id
            )
        })?;
    let packet =
        decode_work_packet_for_resource_claim(&envelope.content_json).map_err(|error| {
            format!(
                "decode subagent resource claim failed: job_id={} ref_id={} error={error}",
                job.job_id, envelope.ref_id
            )
        })?;
    Ok(subagent_resource_claim_from_packet(job, &packet))
}

fn decode_work_packet_for_resource_claim(value: &Value) -> Result<SubAgentWorkPacket, String> {
    let candidate = value.get("workPacket").unwrap_or(value).clone();
    let packet = serde_json::from_value::<SubAgentWorkPacket>(candidate)
        .map_err(|error| format!("invalid subagent work packet resource claim: {error}"))?;
    packet.validate_for_agent_runtime()?;
    Ok(packet)
}

fn subagent_work_packet_description(envelope: &SubagentWorkPacketEnvelope) -> Option<String> {
    let packet = decode_work_packet_for_resource_claim(&envelope.content_json).ok()?;
    if let Some(output_hint) = packet
        .task_brief
        .output_hint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(output_hint.to_string());
    }
    Some(packet.task_brief.objective.trim().to_string()).filter(|value| !value.is_empty())
}

fn apply_work_packet_description_to_events(
    events: &mut [SubagentSchedulerEvent],
    envelope: &SubagentWorkPacketEnvelope,
) {
    let Some(description) = subagent_work_packet_description(envelope) else {
        return;
    };
    for event in events {
        if event.description.is_none() {
            event.description = Some(description.clone());
        }
    }
}

fn build_subagent_start_hook_event(
    job: &RuntimeJobRecord,
    lifecycle: &SubagentLifecycleRecord,
    envelope: &SubagentWorkPacketEnvelope,
    started_at_ms: TimestampMs,
) -> Result<SubagentLifecycleHookEvent, String> {
    Ok(SubagentLifecycleHookEvent {
        schema: "subagent_lifecycle_hook_v1".to_string(),
        phase: SubagentLifecycleHookPhase::Start,
        job_id: job.job_id.clone(),
        subagent_id: lifecycle.subagent_id.clone(),
        session_id: lifecycle.session_id.clone(),
        parent_turn_id: lifecycle.parent_turn_id.clone(),
        work_packet_ref: subagent_job_work_packet_ref(job)?.to_string(),
        description: subagent_work_packet_description(envelope),
        allowed_tools: subagent_work_packet_allowed_tools(envelope),
        status: Some(SubagentLifecycleStatus::Running),
        result_ref: None,
        output_refs: vec![],
        error: None,
        started_at_ms: Some(started_at_ms),
        finished_at_ms: None,
    })
}

fn build_subagent_stop_hook_event(
    lifecycle: &SubagentLifecycleRecord,
    job: Option<&RuntimeJobRecord>,
    finished_at_ms: TimestampMs,
) -> Result<SubagentLifecycleHookEvent, String> {
    let output_refs = job.map(|item| item.output_refs.clone()).unwrap_or_default();
    Ok(SubagentLifecycleHookEvent {
        schema: "subagent_lifecycle_hook_v1".to_string(),
        phase: SubagentLifecycleHookPhase::Stop,
        job_id: lifecycle.job_id.clone(),
        subagent_id: lifecycle.subagent_id.clone(),
        session_id: lifecycle.session_id.clone(),
        parent_turn_id: lifecycle.parent_turn_id.clone(),
        work_packet_ref: lifecycle.work_packet_ref.clone(),
        description: None,
        allowed_tools: vec![],
        status: Some(lifecycle.status.clone()),
        result_ref: lifecycle.result_ref.clone(),
        output_refs,
        error: lifecycle
            .last_error
            .as_deref()
            .map(|error| compact_hook_text(error, 600)),
        started_at_ms: None,
        finished_at_ms: Some(finished_at_ms),
    })
}

fn subagent_work_packet_allowed_tools(envelope: &SubagentWorkPacketEnvelope) -> Vec<String> {
    envelope
        .content_json
        .get("workPacket")
        .unwrap_or(&envelope.content_json)
        .get("allowedTools")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| compact_hook_text(value, 120))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn compact_hook_text(value: &str, max_chars: usize) -> String {
    let mut result = String::new();
    for (index, character) in value.chars().enumerate() {
        if index >= max_chars {
            result.push_str("...");
            return result;
        }
        result.push(character);
    }
    result
}

fn subagent_resource_claim_from_packet(
    job: &RuntimeJobRecord,
    packet: &SubAgentWorkPacket,
) -> SubagentResourceClaim {
    let resource_key = packet
        .hot_view
        .state_kv
        .get("subagentResourceKey")
        .or_else(|| packet.hot_view.state_kv.get("resourceKey"))
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| packet.parent_checkpoint_id.clone())
        .or_else(|| job.session_id.clone())
        .unwrap_or_else(|| job.job_id.clone());
    let has_unsafe_tool = packet
        .delegated_tool_contracts
        .iter()
        .any(|contract| !contract.concurrency_safe);
    let access_mode = if matches!(packet.context_mode, ContextTransferMode::Move)
        || !packet.writable_path_prefixes.is_empty()
        || has_unsafe_tool
    {
        SubagentResourceAccessMode::Exclusive
    } else {
        SubagentResourceAccessMode::Shared
    };
    SubagentResourceClaim {
        resource_key,
        access_mode,
    }
}

fn subagent_resource_claim_can_join(
    active_claims: &[SubagentResourceClaim],
    candidate: &SubagentResourceClaim,
) -> bool {
    active_claims.iter().all(|active| {
        active.resource_key != candidate.resource_key
            || (active.access_mode == SubagentResourceAccessMode::Shared
                && candidate.access_mode == SubagentResourceAccessMode::Shared)
    })
}

fn stable_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn scheduler_event_from_job(
    job: &RuntimeJobRecord,
    kind: SubagentSchedulerEventKind,
    status: SubagentLifecycleStatus,
    worker_id: Option<String>,
    summary: &str,
    at_ms: TimestampMs,
) -> Result<SubagentSchedulerEvent, String> {
    let completed_at_ms = if status.is_terminal() {
        Some(at_ms)
    } else {
        None
    };
    let started_at_ms = if kind == SubagentSchedulerEventKind::Running {
        Some(at_ms)
    } else {
        None
    };
    let subagent_id = subagent_id_from_job(job)?;
    Ok(SubagentSchedulerEvent {
        kind,
        child_session_id: format!("session-{subagent_id}"),
        subagent_id,
        parent_turn_id: subagent_job_parent_turn_id(job)?.to_string(),
        job_id: job.job_id.clone(),
        work_packet_ref: Some(subagent_job_work_packet_ref(job)?.to_string()),
        result_ref: job.output_refs.first().cloned(),
        worker_id,
        status,
        summary: summary.to_string(),
        description: None,
        started_at_ms,
        completed_at_ms,
        at_ms,
    })
}

async fn cancelled_subagent_event_if_requested_async(
    store: &RuntimeStoreActor,
    job_id: &str,
    at_ms: TimestampMs,
) -> Result<Option<(RuntimeJobRecord, SubagentSchedulerEvent)>, String> {
    let Some(job) = store.get_runtime_job(job_id).await? else {
        return Ok(None);
    };
    if job.status != RuntimeJobStatus::Cancelled {
        return Ok(None);
    }
    let reason = job
        .last_error
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Subagent job cancelled.");
    let event = scheduler_event_from_job(
        &job,
        SubagentSchedulerEventKind::Cancelled,
        SubagentLifecycleStatus::Cancelled,
        job.lease_owner.clone(),
        reason,
        at_ms,
    )?;
    Ok(Some((job, event)))
}

fn subagent_job_parent_turn_id(job: &RuntimeJobRecord) -> Result<&str, String> {
    required_subagent_job_field(job, "branch_id", job.branch_id.as_deref())
}

fn subagent_job_session_id(job: &RuntimeJobRecord) -> Result<&str, String> {
    required_subagent_job_field(job, "session_id", job.session_id.as_deref())
}

fn subagent_job_work_packet_ref(job: &RuntimeJobRecord) -> Result<&str, String> {
    required_subagent_job_field(job, "payload_ref", job.payload_ref.as_deref())
}

fn subagent_job_lease_owner(job: &RuntimeJobRecord) -> Result<&str, String> {
    required_subagent_job_field(job, "lease_owner", job.lease_owner.as_deref())
}

fn required_subagent_job_field<'a>(
    job: &RuntimeJobRecord,
    field_name: &str,
    value: Option<&'a str>,
) -> Result<&'a str, String> {
    value
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .ok_or_else(|| {
            format!(
                "subagent runtime job requires {field_name}: job_id={}",
                job.job_id
            )
        })
}

fn subagent_id_from_job(job: &RuntimeJobRecord) -> Result<String, String> {
    if job.job_kind != SUBAGENT_RUN_JOB_KIND {
        return Err(format!(
            "subagent runtime job has invalid job kind: job_id={} job_kind={}",
            job.job_id, job.job_kind
        ));
    }
    let raw = job
        .idempotency_key
        .strip_prefix(runtime_job_keys::SUBAGENT_RUN_PREFIX)
        .ok_or_else(|| {
            format!(
                "subagent runtime job has invalid idempotency identity: job_id={}",
                job.job_id
            )
        })?;
    let identity: SubagentRunIdentityV1 = serde_json::from_str(raw).map_err(|error| {
        format!(
            "subagent runtime job has invalid idempotency identity: job_id={} error={error}",
            job.job_id
        )
    })?;
    if identity.schema != SUBAGENT_AGENT_RUN_IDENTITY_SCHEMA {
        return Err(format!(
            "subagent runtime job has unsupported identity schema: job_id={} schema={}",
            job.job_id, identity.schema
        ));
    }
    let expected_job_id = runtime_job_keys::subagent_run_job_id(stable_hash(raw).as_str());
    if job.job_id != expected_job_id {
        return Err(format!(
            "subagent runtime job identity jobId mismatch: job_id={} expected={expected_job_id}",
            job.job_id
        ));
    }
    let session_id = required_non_empty(
        "subagentRunIdentity.sessionId",
        identity.session_id.as_str(),
    )?;
    let parent_turn_id = required_non_empty(
        "subagentRunIdentity.parentTurnId",
        identity.parent_turn_id.as_str(),
    )?;
    required_non_empty(
        "subagentRunIdentity.toolCallId",
        identity.tool_call_id.as_str(),
    )?;
    let subagent_id = required_non_empty(
        "subagentRunIdentity.subagentId",
        identity.subagent_id.as_str(),
    )?;
    if session_id != subagent_job_session_id(job)?
        || parent_turn_id != subagent_job_parent_turn_id(job)?
    {
        return Err(format!(
            "subagent runtime job identity binding mismatch: job_id={}",
            job.job_id
        ));
    }
    Ok(subagent_id)
}

impl From<RuntimeJobStatus> for SubagentLifecycleStatus {
    fn from(value: RuntimeJobStatus) -> Self {
        match value {
            RuntimeJobStatus::Queued => Self::Queued,
            RuntimeJobStatus::Leased => Self::Leased,
            RuntimeJobStatus::Running => Self::Running,
            RuntimeJobStatus::Succeeded => Self::Succeeded,
            RuntimeJobStatus::Failed | RuntimeJobStatus::DeadLettered => Self::Failed,
            RuntimeJobStatus::Cancelled => Self::Cancelled,
        }
    }
}

fn inline_work_packet_ref(job: &RuntimeJobRecord) -> String {
    format!("inline:{}", job.job_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::subagent_contracts::{AgentRunContext, ContextTransferMode};
    use crate::session::reliability::{
        CancelRuntimeJobRequest, ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest,
        FailRuntimeJobRequest, ListRuntimeJobsRequest, ScheduleRuntimeJobRequest,
        ScheduleRuntimeJobResult, StartRuntimeJobRequest,
    };

    fn test_parent_run_context(session_id: &str, parent_turn_id: &str) -> AgentRunContext {
        AgentRunContext::root(
            session_id,
            parent_turn_id,
            parent_turn_id,
            format!("agent-run-main-{parent_turn_id}"),
            "main-agent",
            std::env::temp_dir().to_string_lossy(),
            100,
        )
    }

    fn test_delegated_tool_contracts(
        tool_names: &[String],
    ) -> Vec<crate::runtime::subagent_contracts::DelegatedToolContractV1> {
        crate::tool::list_tool_contracts()
            .iter()
            .filter(|contract| tool_names.contains(&contract.name))
            .map(|contract| {
                crate::runtime::subagent_contracts::DelegatedToolContractV1::from_tool_contract(
                    contract,
                )
                .expect("delegated contract")
            })
            .collect()
    }

    #[test]
    fn subagent_lifecycle_transition_table_is_stable() {
        assert!(SubagentLifecycleStatus::Queued.can_transition_to(&SubagentLifecycleStatus::Leased));
        assert!(
            SubagentLifecycleStatus::Leased.can_transition_to(&SubagentLifecycleStatus::Running)
        );
        assert!(
            SubagentLifecycleStatus::Running.can_transition_to(&SubagentLifecycleStatus::Waiting)
        );
        assert!(
            SubagentLifecycleStatus::Waiting.can_transition_to(&SubagentLifecycleStatus::Running)
        );
        assert!(
            SubagentLifecycleStatus::Running.can_transition_to(&SubagentLifecycleStatus::Succeeded)
        );
        assert!(!SubagentLifecycleStatus::Succeeded
            .can_transition_to(&SubagentLifecycleStatus::Running));
        assert!(SubagentLifecycleStatus::Cancelled.is_terminal());
    }

    #[test]
    fn subagent_runtime_binding_rejects_delegated_tool_contract_drift() {
        let mut delegated_tool_contracts = test_delegated_tool_contracts(&["read".to_string()]);
        delegated_tool_contracts[0].provider_id = "banana".to_string();
        let binding = SubagentWorkPacketRuntimeBindingV1 {
            child_session_id: "chat-child".to_string(),
            child_turn_id: "turn-child".to_string(),
            subagent_id: "agent-child".to_string(),
            parent_agent_run_id: "agent-run-parent".to_string(),
            parent_turn_id: "turn-parent".to_string(),
            description: "Inspect one issue".to_string(),
            allowed_tools: vec!["read".to_string()],
            delegated_tool_contracts,
        };

        let error = binding
            .validate_tool_contracts(&ToolLayer::new())
            .expect_err("contract drift must fail");
        assert_eq!(error, "delegated tool contract drift: read");
    }

    #[test]
    fn subagent_run_job_uses_runtime_jobs_durable_truth() {
        let job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: "chat-1".to_string(),
            parent_turn_id: "turn-1:2".to_string(),
            tool_call_id: "call-1:tool-1".to_string(),
            subagent_id: "subagent:turn-1:tool-1".to_string(),
            work_packet_ref: "external_context:work_packet_1".to_string(),
            checkpoint_id: Some("checkpoint-1".to_string()),
            run_at_ms: 100,
            created_at_ms: 90,
            max_retries: 2,
        });

        let parent_context = test_parent_run_context("chat-1", "turn-1:2");
        let mut packet = crate::runtime::subagent_contracts::SubAgentWorkPacket::new(
            AgentRunContext::child(
                &parent_context,
                "session-child",
                "turn-child",
                "agent-run-child",
                "subagent:turn-1:tool-1",
                90,
            ),
            crate::runtime::subagent_contracts::TaskBrief {
                objective: "inspect durable truth".to_string(),
                ..Default::default()
            },
            crate::runtime::subagent_contracts::HotView::default(),
            crate::runtime::subagent_contracts::OutputContract {
                response_mode: "summary".to_string(),
                ..Default::default()
            },
            ContextTransferMode::Borrow,
        );
        packet.allowed_tools = vec!["read".to_string()];
        packet.delegated_tool_contracts =
            test_delegated_tool_contracts(packet.allowed_tools.as_slice());
        let envelope = SubagentWorkPacketEnvelope {
            ref_id: "external_context:work_packet_1".to_string(),
            content_json: serde_json::json!({ "workPacket": packet }),
        };

        assert_eq!(job.job_kind, SUBAGENT_RUN_JOB_KIND);
        assert_eq!(job.status, RuntimeJobStatus::Queued);
        assert_eq!(job.session_id.as_deref(), Some("chat-1"));
        assert_eq!(job.branch_id.as_deref(), Some("turn-1:2"));
        assert_eq!(
            job.payload_ref.as_deref(),
            Some("external_context:work_packet_1")
        );
        assert!(job.idempotency_key.contains("subagent:turn-1:tool-1"));
        assert_eq!(
            lifecycle_record_from_job(&job, 100)
                .expect("project lifecycle")
                .subagent_id,
            "subagent:turn-1:tool-1"
        );

        let duplicate = build_subagent_run_job(SubagentRunJobRequest {
            session_id: "chat-1".to_string(),
            parent_turn_id: "turn-1:2".to_string(),
            tool_call_id: "call-1:tool-1".to_string(),
            subagent_id: "subagent:turn-1:tool-1".to_string(),
            work_packet_ref: "external_context:work_packet_1".to_string(),
            checkpoint_id: Some("checkpoint-1".to_string()),
            run_at_ms: 100,
            created_at_ms: 90,
            max_retries: 2,
        });
        assert_eq!(job.job_id, duplicate.job_id);
        assert_eq!(job.idempotency_key, duplicate.idempotency_key);
        assert_eq!(
            subagent_work_packet_runtime_binding(&envelope, &job)
                .expect("bind work packet to runtime job")
                .parent_turn_id,
            "turn-1:2"
        );

        let mut mismatched_job = job;
        mismatched_job.branch_id = Some("banana".to_string());
        assert!(
            subagent_work_packet_runtime_binding(&envelope, &mismatched_job)
                .expect_err("runtime job and work packet must share parent turn")
                .contains("parentTurnId does not match runtime job branchId")
        );
    }

    #[test]
    fn subagent_lifecycle_projection_requires_runtime_job_identity_fields() {
        let mut job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: "chat-required".to_string(),
            parent_turn_id: "turn-required".to_string(),
            tool_call_id: "tool-required".to_string(),
            subagent_id: "subagent:required".to_string(),
            work_packet_ref: "external_context:subagent_work_packet:required".to_string(),
            checkpoint_id: None,
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 3,
        });

        let mut missing_parent = job.clone();
        missing_parent.branch_id = None;
        assert!(lifecycle_record_from_job(&missing_parent, 1_001)
            .expect_err("missing parent turn id must fail")
            .contains("branch_id"));

        let mut missing_chat = job.clone();
        missing_chat.session_id = None;
        assert!(lifecycle_record_from_job(&missing_chat, 1_001)
            .expect_err("missing chat session id must fail")
            .contains("session_id"));

        let mut invalid_identity = job.clone();
        invalid_identity.idempotency_key = format!(
            "{}chat-required:turn-required:tool-required:subagent-required",
            runtime_job_keys::SUBAGENT_RUN_PREFIX
        );
        assert!(lifecycle_record_from_job(&invalid_identity, 1_001)
            .expect_err("invalid identity must fail")
            .contains("invalid idempotency identity"));

        job.payload_ref = None;
        assert!(lifecycle_record_from_job(&job, 1_001)
            .expect_err("missing work packet ref must fail")
            .contains("payload_ref"));
    }

    #[test]
    fn cancel_subagent_run_jobs_loud_fails_when_cancel_store_op_fails() {
        let store = CancelFailRuntimeJobStore {
            job: test_subagent_runtime_job("job-cancel-fail", RuntimeJobStatus::Queued),
        };

        let error = cancel_subagent_run_jobs(
            &store,
            CancelSubagentRunJobsRequest {
                session_id: Some("chat-cancel-fail".to_string()),
                parent_turn_id: Some("turn-cancel-fail".to_string()),
                subagent_id: None,
                reason: "parent_cancelled".to_string(),
                cancelled_at_ms: 2_000,
                limit: 10,
                include_running: false,
            },
        )
        .expect_err("cancel failure must not be counted as skipped");

        assert!(error.contains("cancel subagent runtime job failed"));
        assert!(error.contains("job-cancel-fail"));
        assert!(error.contains("forced cancel failure"));
    }

    #[derive(Debug, Clone)]
    struct CancelFailRuntimeJobStore {
        job: RuntimeJobRecord,
    }

    impl RuntimeJobStorePort for CancelFailRuntimeJobStore {
        fn schedule_runtime_job(
            &self,
            _req: ScheduleRuntimeJobRequest,
        ) -> Result<ScheduleRuntimeJobResult, String> {
            Err("unexpected schedule_runtime_job".to_string())
        }

        fn get_runtime_job(&self, job_id: &str) -> Result<Option<RuntimeJobRecord>, String> {
            if self.job.job_id == job_id {
                Ok(Some(self.job.clone()))
            } else {
                Ok(None)
            }
        }

        fn list_runtime_jobs(
            &self,
            _req: ListRuntimeJobsRequest,
        ) -> Result<Vec<RuntimeJobRecord>, String> {
            Ok(vec![self.job.clone()])
        }

        fn claim_due_runtime_jobs(
            &self,
            _req: ClaimDueRuntimeJobsRequest,
        ) -> Result<Vec<RuntimeJobRecord>, String> {
            Err("unexpected claim_due_runtime_jobs".to_string())
        }

        fn start_runtime_job(&self, _req: StartRuntimeJobRequest) -> Result<(), String> {
            Err("unexpected start_runtime_job".to_string())
        }

        fn renew_runtime_job_lease(
            &self,
            _req: crate::session::reliability::RenewRuntimeJobLeaseRequest,
        ) -> Result<(), String> {
            Err("unexpected renew_runtime_job_lease".to_string())
        }

        fn yield_runtime_job(
            &self,
            _req: crate::session::reliability::YieldRuntimeJobRequest,
        ) -> Result<(), String> {
            Err("unexpected yield_runtime_job".to_string())
        }

        fn wake_runtime_job(
            &self,
            _req: crate::session::reliability::WakeRuntimeJobRequest,
        ) -> Result<crate::session::reliability::WakeRuntimeJobDisposition, String> {
            Err("unexpected wake_runtime_job".to_string())
        }

        fn complete_runtime_job(&self, _req: CompleteRuntimeJobRequest) -> Result<(), String> {
            Err("unexpected complete_runtime_job".to_string())
        }

        fn fail_runtime_job(&self, _req: FailRuntimeJobRequest) -> Result<(), String> {
            Err("unexpected fail_runtime_job".to_string())
        }

        fn cancel_runtime_job(&self, req: CancelRuntimeJobRequest) -> Result<(), String> {
            Err(format!("forced cancel failure: {}", req.job_id))
        }

        fn reclaim_expired_runtime_job_leases(
            &self,
            _now_ms: TimestampMs,
        ) -> Result<usize, String> {
            Err("unexpected reclaim_expired_runtime_job_leases".to_string())
        }
    }

    fn test_subagent_runtime_job(job_id: &str, status: RuntimeJobStatus) -> RuntimeJobRecord {
        let mut job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: "chat-cancel-fail".to_string(),
            parent_turn_id: "turn-cancel-fail".to_string(),
            tool_call_id: "tool-cancel-fail".to_string(),
            subagent_id: "subagent-cancel-fail".to_string(),
            work_packet_ref: "external_context:cancel-fail-work-packet".to_string(),
            checkpoint_id: Some("checkpoint-cancel-fail".to_string()),
            run_at_ms: 1_000,
            created_at_ms: 900,
            max_retries: 1,
        });
        job.job_id = job_id.to_string();
        job.status = status;
        job
    }
}
