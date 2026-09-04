use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

pub const MAX_PENDING_TURN_SUPPLEMENTS: usize = 8;
pub const MAX_TURN_SUPPLEMENT_BYTES: usize = 64 * 1024;
pub const MAX_TURN_SUPPLEMENT_ID_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DurableTurnSupplement {
    pub supplement_id: String,
    pub sequence: u64,
    pub message: String,
    pub created_at_ms: i64,
    pub claim_token: Option<String>,
    pub claim_lease_owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueTurnSupplementRequest {
    pub agent_run_id: String,
    pub lifecycle_job_id: String,
    pub session_id: String,
    pub authorization_digest: String,
    pub supplement_id: String,
    pub message: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueTurnSupplementDisposition {
    Accepted,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueTurnSupplementResult {
    pub disposition: EnqueueTurnSupplementDisposition,
    pub queued_count: usize,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnSupplementValidationError {
    MessageRequired,
    MessageTooLarge,
    IdInvalid,
}

impl fmt::Display for TurnSupplementValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MessageRequired => formatter.write_str("turn_supplement_message_required"),
            Self::MessageTooLarge => write!(
                formatter,
                "turn_supplement_message_too_large:maxBytes={MAX_TURN_SUPPLEMENT_BYTES}"
            ),
            Self::IdInvalid => formatter.write_str("turn_supplement_id_invalid"),
        }
    }
}

impl std::error::Error for TurnSupplementValidationError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnSupplementStoreError {
    Validation(TurnSupplementValidationError),
    IdentityRequired,
    JobIdMismatch,
    AgentRunNotActive,
    IdentityMismatch,
    QueueIdentityMismatch,
    AdmissionClosed,
    QueueFull,
    IdempotencyConflict,
    QueueCasConflict,
    ClaimIdentityInvalid,
    ClaimInProgress,
    QueueMissing,
    AcknowledgeIdentityInvalid,
    AcknowledgeIdentityMismatch,
    CloseReasonRequired,
    LeaseFenceRejected,
    Internal(String),
}

impl fmt::Display for TurnSupplementStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => error.fmt(formatter),
            Self::IdentityRequired => formatter.write_str("turn_supplement_identity_required"),
            Self::JobIdMismatch => formatter.write_str("turn_supplement_job_id_mismatch"),
            Self::AgentRunNotActive => formatter.write_str("turn_supplement_agent_run_not_active"),
            Self::IdentityMismatch => formatter.write_str("turn_supplement_identity_mismatch"),
            Self::QueueIdentityMismatch => {
                formatter.write_str("turn_supplement_queue_identity_mismatch")
            }
            Self::AdmissionClosed => formatter.write_str("turn_supplement_admission_closed"),
            Self::QueueFull => formatter.write_str("turn_supplement_queue_full"),
            Self::IdempotencyConflict => {
                formatter.write_str("turn_supplement_idempotency_conflict")
            }
            Self::QueueCasConflict => formatter.write_str("turn_supplement_queue_cas_conflict"),
            Self::ClaimIdentityInvalid => {
                formatter.write_str("turn_supplement_claim_identity_invalid")
            }
            Self::ClaimInProgress => formatter.write_str("turn_supplement_claim_in_progress"),
            Self::QueueMissing => formatter.write_str("turn_supplement_queue_missing"),
            Self::AcknowledgeIdentityInvalid => {
                formatter.write_str("turn_supplement_ack_identity_invalid")
            }
            Self::AcknowledgeIdentityMismatch => {
                formatter.write_str("turn_supplement_ack_identity_mismatch")
            }
            Self::CloseReasonRequired => {
                formatter.write_str("turn_supplement_close_reason_required")
            }
            Self::LeaseFenceRejected => formatter.write_str("turn_supplement_lease_fence_rejected"),
            Self::Internal(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for TurnSupplementStoreError {}

impl From<TurnSupplementValidationError> for TurnSupplementStoreError {
    fn from(error: TurnSupplementValidationError) -> Self {
        Self::Validation(error)
    }
}

impl From<String> for TurnSupplementStoreError {
    fn from(error: String) -> Self {
        Self::Internal(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimTurnSupplementsRequest {
    pub agent_run_id: String,
    pub lifecycle_job_id: String,
    pub session_id: String,
    pub authorization_digest: String,
    pub lease_owner: String,
    pub claim_token: String,
    pub now_ms: i64,
    pub close_if_empty: bool,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcknowledgeTurnSupplementsRequest {
    pub agent_run_id: String,
    pub lifecycle_job_id: String,
    pub session_id: String,
    pub authorization_digest: String,
    pub lease_owner: String,
    pub claim_token: String,
    pub supplement_ids: Vec<String>,
    pub acknowledged_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseTurnSupplementQueueRequest {
    pub agent_run_id: String,
    pub lifecycle_job_id: String,
    pub session_id: String,
    pub authorization_digest: String,
    pub lease_owner: Option<String>,
    pub reason: String,
    pub closed_at_ms: i64,
}

pub trait TurnSupplementStorePort: std::fmt::Debug + Send + Sync {
    fn enqueue_turn_supplement(
        &self,
        request: EnqueueTurnSupplementRequest,
    ) -> Result<EnqueueTurnSupplementResult, TurnSupplementStoreError>;

    fn claim_turn_supplements(
        &self,
        request: ClaimTurnSupplementsRequest,
    ) -> Result<Vec<DurableTurnSupplement>, TurnSupplementStoreError>;

    fn acknowledge_turn_supplements(
        &self,
        request: AcknowledgeTurnSupplementsRequest,
    ) -> Result<(), TurnSupplementStoreError>;

    fn close_turn_supplement_queue(
        &self,
        request: CloseTurnSupplementQueueRequest,
    ) -> Result<(), TurnSupplementStoreError>;
}

pub fn validate_turn_supplement_message(
    message: &str,
) -> Result<String, TurnSupplementValidationError> {
    let message = message.trim();
    if message.is_empty() {
        return Err(TurnSupplementValidationError::MessageRequired);
    }
    if message.len() > MAX_TURN_SUPPLEMENT_BYTES {
        return Err(TurnSupplementValidationError::MessageTooLarge);
    }
    Ok(message.to_string())
}

pub fn validate_turn_supplement_id(value: &str) -> Result<&str, TurnSupplementValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_TURN_SUPPLEMENT_ID_BYTES
        || trimmed.chars().any(char::is_control)
        || value != trimmed
    {
        return Err(TurnSupplementValidationError::IdInvalid);
    }
    Ok(trimmed)
}

pub fn turn_supplement_message_digest(message: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(message.as_bytes()))
}
