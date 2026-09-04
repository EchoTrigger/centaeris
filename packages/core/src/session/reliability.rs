use serde::{Deserialize, Serialize};

use crate::runtime::contracts::TimestampMs;

pub const RUNTIME_JOB_TERMINAL_EVENT: &str = "runtime_job.terminal";
pub const AGENT_RUN_LIFECYCLE_JOB_KIND: &str = "agent_run.lifecycle";

pub fn agent_run_lifecycle_job_id(agent_run_id: &str) -> Result<String, String> {
    let agent_run_id = agent_run_id.trim();
    if agent_run_id.is_empty()
        || agent_run_id.len() > 128
        || !agent_run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err("agent_run_lifecycle_agent_run_id_invalid".to_string());
    }
    Ok(format!("agent_run.lifecycle:{agent_run_id}"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeJobStatus {
    Queued,
    Leased,
    Running,
    Succeeded,
    Failed,
    DeadLettered,
    Cancelled,
}

impl RuntimeJobStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::DeadLettered | Self::Cancelled
        )
    }

    pub fn can_transition_to(&self, next: &Self) -> bool {
        use RuntimeJobStatus::*;

        matches!(
            (self, next),
            (Queued, Leased)
                | (Queued, Cancelled)
                | (Leased, Running)
                | (Leased, Queued)
                | (Leased, Succeeded)
                | (Leased, Failed)
                | (Leased, DeadLettered)
                | (Leased, Cancelled)
                | (Running, Queued)
                | (Running, Succeeded)
                | (Running, Failed)
                | (Running, DeadLettered)
                | (Running, Cancelled)
        ) || self == next
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeBackoffPolicy {
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub multiplier: f64,
    pub jitter_ms: u64,
}

impl Default for RuntimeBackoffPolicy {
    fn default() -> Self {
        Self {
            base_delay_ms: 2_000,
            max_delay_ms: 60_000,
            multiplier: 2.0,
            jitter_ms: 250,
        }
    }
}

pub fn runtime_job_retry_delay_ms(
    policy: &RuntimeBackoffPolicy,
    attempt: u32,
    job_id: &str,
    now_ms: TimestampMs,
) -> TimestampMs {
    let safe_attempt = attempt.max(1);
    let exponent = safe_attempt.saturating_sub(1);
    let mut backoff = (policy.base_delay_ms as f64) * policy.multiplier.powi(exponent as i32);
    if !backoff.is_finite() || backoff.is_sign_negative() {
        backoff = policy.max_delay_ms as f64;
    }
    let capped = backoff.min(policy.max_delay_ms as f64) as u64;
    let jitter = if policy.jitter_ms == 0 {
        0
    } else {
        let mut hash = 1469598103934665603u64;
        for byte in job_id.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(1099511628211);
        }
        hash ^= u64::from(safe_attempt);
        hash ^= now_ms as u64;
        hash % policy.jitter_ms.saturating_add(1)
    };
    i64::try_from(capped.saturating_add(jitter)).unwrap_or(i64::MAX)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeJobRecord {
    pub job_id: String,
    pub job_kind: String,
    pub status: RuntimeJobStatus,
    pub run_at_ms: TimestampMs,
    pub lease_owner: Option<String>,
    pub lease_expires_at_ms: Option<TimestampMs>,
    #[serde(default)]
    pub heartbeat_at_ms: Option<TimestampMs>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub backoff_policy: RuntimeBackoffPolicy,
    pub idempotency_key: String,
    pub session_id: Option<String>,
    pub branch_id: Option<String>,
    pub checkpoint_id: Option<String>,
    pub payload_ref: Option<String>,
    #[serde(default)]
    pub output_refs: Vec<String>,
    pub last_error: Option<String>,
    pub created_at_ms: TimestampMs,
    pub updated_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRuntimeJobDisposition {
    Inserted,
    Existing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRuntimeJobRequest {
    pub job: RuntimeJobRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRuntimeJobResult {
    pub disposition: ScheduleRuntimeJobDisposition,
    pub job: RuntimeJobRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClaimDueRuntimeJobsRequest {
    pub now_ms: TimestampMs,
    pub worker_id: String,
    #[serde(default)]
    pub job_id: Option<String>,
    pub job_kind: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    pub limit: usize,
    pub lease_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListRuntimeJobsRequest {
    pub statuses: Vec<RuntimeJobStatus>,
    pub job_kind: Option<String>,
    pub session_id: Option<String>,
    pub branch_id: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompleteRuntimeJobRequest {
    pub job_id: String,
    pub lease_owner: String,
    #[serde(default)]
    pub output_refs: Vec<String>,
    pub completed_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartRuntimeJobRequest {
    pub job_id: String,
    pub lease_owner: String,
    pub started_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenewRuntimeJobLeaseRequest {
    pub job_id: String,
    pub lease_owner: String,
    pub heartbeat_at_ms: TimestampMs,
    pub lease_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct YieldRuntimeJobRequest {
    pub job_id: String,
    pub lease_owner: String,
    pub yielded_at_ms: TimestampMs,
    pub run_at_ms: TimestampMs,
    pub transition_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WakeRuntimeJobRequest {
    pub job_id: String,
    pub source_job_id: String,
    pub woken_at_ms: TimestampMs,
    pub transition_reason: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WakeRuntimeJobDisposition {
    Woken,
    AlreadyRunnable,
    Active,
    Terminal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeJobFailureDisposition {
    RetryScheduled,
    Failed,
    DeadLettered,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FailRuntimeJobRequest {
    pub job_id: String,
    pub lease_owner: String,
    pub failed_at_ms: TimestampMs,
    pub last_error: String,
    pub next_run_at_ms: Option<TimestampMs>,
    pub disposition: RuntimeJobFailureDisposition,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CancelRuntimeJobRequest {
    pub job_id: String,
    pub reason: String,
    pub cancelled_at_ms: TimestampMs,
    #[serde(default)]
    pub expected_status: Option<RuntimeJobStatus>,
}

pub trait RuntimeJobStorePort {
    fn schedule_runtime_job(
        &self,
        req: ScheduleRuntimeJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, String>;
    fn get_runtime_job(&self, job_id: &str) -> Result<Option<RuntimeJobRecord>, String>;
    fn list_runtime_jobs(
        &self,
        req: ListRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String>;
    fn claim_due_runtime_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String>;
    fn start_runtime_job(&self, req: StartRuntimeJobRequest) -> Result<(), String>;
    fn renew_runtime_job_lease(&self, req: RenewRuntimeJobLeaseRequest) -> Result<(), String>;
    fn yield_runtime_job(&self, req: YieldRuntimeJobRequest) -> Result<(), String>;
    fn wake_runtime_job(
        &self,
        req: WakeRuntimeJobRequest,
    ) -> Result<WakeRuntimeJobDisposition, String>;
    fn complete_runtime_job(&self, req: CompleteRuntimeJobRequest) -> Result<(), String>;
    fn fail_runtime_job(&self, req: FailRuntimeJobRequest) -> Result<(), String>;
    fn cancel_runtime_job(&self, req: CancelRuntimeJobRequest) -> Result<(), String>;
    fn reclaim_expired_runtime_job_leases(&self, now_ms: TimestampMs) -> Result<usize, String>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeJobOutboxRecord {
    pub job_id: String,
    pub event_type: String,
    pub published_at_ms: Option<TimestampMs>,
    pub generation: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeJobOutboxPublishDisposition {
    Published,
    AlreadyPublished,
    Stale,
}

pub trait RuntimeJobOutboxPort {
    fn list_pending_runtime_job_outbox(
        &self,
        limit: usize,
    ) -> Result<Vec<RuntimeJobOutboxRecord>, String>;
    fn mark_runtime_job_outbox_published(
        &self,
        job_id: &str,
        event_type: &str,
        generation: u32,
        published_at_ms: TimestampMs,
    ) -> Result<RuntimeJobOutboxPublishDisposition, String>;
    fn requeue_runtime_job_notifications(
        &self,
        published_before_ms: TimestampMs,
    ) -> Result<usize, String>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceClaimRecord {
    pub resource_kind: String,
    pub resource_key: String,
    pub owner: String,
    pub owner_kind: String,
    pub session_id: Option<String>,
    pub branch_id: Option<String>,
    pub expires_at_ms: TimestampMs,
    pub metadata_json: String,
    pub created_at_ms: TimestampMs,
    pub updated_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AcquireResourceClaimDisposition {
    Acquired,
    AlreadyOwned,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcquireResourceClaimRequest {
    pub resource_kind: String,
    pub resource_key: String,
    pub owner: String,
    pub owner_kind: String,
    pub session_id: Option<String>,
    pub branch_id: Option<String>,
    pub now_ms: TimestampMs,
    pub ttl_ms: u64,
    pub metadata_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcquireResourceClaimResult {
    pub disposition: AcquireResourceClaimDisposition,
    pub claim: ResourceClaimRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseResourceClaimRequest {
    pub resource_kind: String,
    pub resource_key: String,
    pub owner: String,
    pub released_at_ms: TimestampMs,
}

pub trait ResourceClaimStorePort {
    fn acquire_resource_claim(
        &self,
        req: AcquireResourceClaimRequest,
    ) -> Result<AcquireResourceClaimResult, String>;
    fn get_resource_claim(
        &self,
        resource_kind: &str,
        resource_key: &str,
    ) -> Result<Option<ResourceClaimRecord>, String>;
    fn release_resource_claim(&self, req: ReleaseResourceClaimRequest) -> Result<bool, String>;
    fn reclaim_expired_resource_claims(&self, now_ms: TimestampMs) -> Result<usize, String>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeadLetterStatus {
    Open,
    Replaying,
    Replayed,
    Dismissed,
}

impl DeadLetterStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Replayed | Self::Dismissed)
    }

    pub fn can_transition_to(&self, next: &Self) -> bool {
        use DeadLetterStatus::*;

        matches!(
            (self, next),
            (Open, Replaying) | (Open, Dismissed) | (Replaying, Replayed) | (Replaying, Open)
        ) || self == next
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeadLetterReplayPolicy {
    pub max_replays: u32,
    pub reuse_idempotency_key: bool,
    pub fork_on_conflict: bool,
}

impl Default for DeadLetterReplayPolicy {
    fn default() -> Self {
        Self {
            max_replays: 3,
            reuse_idempotency_key: true,
            fork_on_conflict: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeadLetterRecord {
    pub dead_letter_id: String,
    pub original_job_id: String,
    pub job_kind: String,
    pub status: DeadLetterStatus,
    pub session_id: Option<String>,
    pub branch_id: Option<String>,
    pub checkpoint_id: Option<String>,
    pub payload_ref: Option<String>,
    pub idempotency_key: String,
    pub failure_reason: String,
    pub last_error: String,
    pub attempts: u32,
    pub first_failed_at_ms: TimestampMs,
    pub last_failed_at_ms: TimestampMs,
    pub replay_policy: DeadLetterReplayPolicy,
    pub replayed_job_id: Option<String>,
    pub dismissed_by: Option<String>,
    pub dismissed_reason: Option<String>,
    pub updated_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CreateDeadLetterDisposition {
    Inserted,
    Existing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreateDeadLetterRequest {
    pub dead_letter: DeadLetterRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreateDeadLetterResult {
    pub disposition: CreateDeadLetterDisposition,
    pub dead_letter: DeadLetterRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ListDeadLettersRequest {
    pub statuses: Vec<DeadLetterStatus>,
    pub job_kind: Option<String>,
    pub session_id: Option<String>,
    pub branch_id: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MarkDeadLetterReplayingRequest {
    pub dead_letter_id: String,
    pub updated_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MarkDeadLetterReplayedRequest {
    pub dead_letter_id: String,
    pub replayed_job_id: Option<String>,
    pub updated_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReplayDeadLetterRequest {
    pub dead_letter_id: String,
    pub replay_job: RuntimeJobRecord,
    pub replayed_at_ms: TimestampMs,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReplayDeadLetterResult {
    pub disposition: ScheduleRuntimeJobDisposition,
    pub job: RuntimeJobRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DismissDeadLetterRequest {
    pub dead_letter_id: String,
    pub dismissed_by: String,
    pub dismissed_reason: String,
    pub updated_at_ms: TimestampMs,
}

pub trait DeadLetterStorePort {
    fn create_dead_letter(
        &self,
        req: CreateDeadLetterRequest,
    ) -> Result<CreateDeadLetterResult, String>;
    fn get_dead_letter(&self, dead_letter_id: &str) -> Result<Option<DeadLetterRecord>, String>;
    fn list_dead_letters(
        &self,
        req: ListDeadLettersRequest,
    ) -> Result<Vec<DeadLetterRecord>, String>;
    fn mark_dead_letter_replaying(&self, req: MarkDeadLetterReplayingRequest)
        -> Result<(), String>;
    fn mark_dead_letter_replayed(&self, req: MarkDeadLetterReplayedRequest) -> Result<(), String>;
    fn replay_dead_letter(
        &self,
        req: ReplayDeadLetterRequest,
    ) -> Result<ReplayDeadLetterResult, String>;
    fn dismiss_dead_letter(&self, req: DismissDeadLetterRequest) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::{DeadLetterStatus, RuntimeJobStatus};

    #[test]
    fn runtime_job_status_transitions_are_stable() {
        assert!(RuntimeJobStatus::Queued.can_transition_to(&RuntimeJobStatus::Leased));
        assert!(RuntimeJobStatus::Leased.can_transition_to(&RuntimeJobStatus::Running));
        assert!(RuntimeJobStatus::Running.can_transition_to(&RuntimeJobStatus::Succeeded));
        assert!(RuntimeJobStatus::Running.can_transition_to(&RuntimeJobStatus::DeadLettered));
        assert!(!RuntimeJobStatus::Succeeded.can_transition_to(&RuntimeJobStatus::Running));
        assert!(RuntimeJobStatus::Cancelled.is_terminal());
    }

    #[test]
    fn dead_letter_status_transitions_are_stable() {
        assert!(DeadLetterStatus::Open.can_transition_to(&DeadLetterStatus::Replaying));
        assert!(DeadLetterStatus::Replaying.can_transition_to(&DeadLetterStatus::Replayed));
        assert!(DeadLetterStatus::Replaying.can_transition_to(&DeadLetterStatus::Open));
        assert!(!DeadLetterStatus::Dismissed.can_transition_to(&DeadLetterStatus::Replaying));
        assert!(DeadLetterStatus::Replayed.is_terminal());
    }
}
