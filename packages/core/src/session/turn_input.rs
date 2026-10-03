//! Run-owned inputs consumed at main-request boundaries. Hosts own durable
//! admission, authorization and lease fencing; Core owns uptake semantics.

use super::host_event_input::HostEventInput;
use super::supplement::*;

pub(crate) mod recovery;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnInputPayload {
    UserSupplement {
        supplement_id: String,
        message: String,
    },
    HostEvent(HostEventInput),
}

impl TurnInputPayload {
    pub fn input_id(&self) -> &str {
        match self {
            Self::UserSupplement { supplement_id, .. } => supplement_id,
            Self::HostEvent(input) => &input.input_id,
        }
    }

    pub fn validate(&self, session_id: &str) -> Result<(), String> {
        match self {
            Self::UserSupplement {
                supplement_id,
                message,
            } => {
                validate_turn_supplement_id(supplement_id).map_err(|e| e.to_string())?;
                validate_turn_supplement_message(message).map_err(|e| e.to_string())?;
                Ok(())
            }
            Self::HostEvent(input) => input.validate(session_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableTurnInput {
    pub payload: TurnInputPayload,
    pub sequence: u64,
    pub created_at_ms: i64,
    pub claim_token: Option<String>,
    pub claim_lease_owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimTurnInputsRequest {
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
pub struct AcknowledgeTurnInputsRequest {
    pub agent_run_id: String,
    pub lifecycle_job_id: String,
    pub session_id: String,
    pub authorization_digest: String,
    pub lease_owner: String,
    pub claim_token: String,
    pub input_ids: Vec<String>,
    pub acknowledged_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseTurnInputQueueRequest {
    pub agent_run_id: String,
    pub lifecycle_job_id: String,
    pub session_id: String,
    pub authorization_digest: String,
    pub lease_owner: Option<String>,
    pub reason: String,
    pub closed_at_ms: i64,
}

/// One queue and one owner fence cover both kinds of input. IDs are unique
/// within a Run. Claims prioritize users, then preserve sequence within each
/// kind. A claim excludes items already held by its claim token; recovery under
/// a newly fenced token may reclaim the same stable identities. Admission is
/// already durable when an item is returned. `close_if_empty` must atomically
/// test both kinds and close admission only if no pending item exists.
///
/// Acknowledgement follows a committed main request. Neither claiming,
/// materializing nor acknowledging is evidence that the model read the input.
pub trait TurnInputStorePort: std::fmt::Debug + Send + Sync {
    fn claim_turn_inputs(
        &self,
        request: ClaimTurnInputsRequest,
    ) -> Result<Vec<DurableTurnInput>, String>;
    fn acknowledge_turn_inputs(&self, request: AcknowledgeTurnInputsRequest) -> Result<(), String>;
    fn close_turn_input_queue(&self, request: CloseTurnInputQueueRequest) -> Result<(), String>;
}

#[derive(Debug)]
pub(crate) struct SupplementInputStore(pub std::sync::Arc<dyn TurnSupplementStorePort>);

impl TurnInputStorePort for SupplementInputStore {
    fn claim_turn_inputs(
        &self,
        request: ClaimTurnInputsRequest,
    ) -> Result<Vec<DurableTurnInput>, String> {
        self.0
            .claim_turn_supplements(ClaimTurnSupplementsRequest {
                agent_run_id: request.agent_run_id,
                lifecycle_job_id: request.lifecycle_job_id,
                session_id: request.session_id,
                authorization_digest: request.authorization_digest,
                lease_owner: request.lease_owner,
                claim_token: request.claim_token,
                now_ms: request.now_ms,
                close_if_empty: request.close_if_empty,
                limit: request.limit,
            })
            .map(|items| items.into_iter().map(DurableTurnInput::from).collect())
            .map_err(|e| e.to_string())
    }

    fn acknowledge_turn_inputs(&self, request: AcknowledgeTurnInputsRequest) -> Result<(), String> {
        self.0
            .acknowledge_turn_supplements(AcknowledgeTurnSupplementsRequest {
                agent_run_id: request.agent_run_id,
                lifecycle_job_id: request.lifecycle_job_id,
                session_id: request.session_id,
                authorization_digest: request.authorization_digest,
                lease_owner: request.lease_owner,
                claim_token: request.claim_token,
                supplement_ids: request.input_ids,
                acknowledged_at_ms: request.acknowledged_at_ms,
            })
            .map_err(|e| e.to_string())
    }

    fn close_turn_input_queue(&self, request: CloseTurnInputQueueRequest) -> Result<(), String> {
        self.0
            .close_turn_supplement_queue(CloseTurnSupplementQueueRequest {
                agent_run_id: request.agent_run_id,
                lifecycle_job_id: request.lifecycle_job_id,
                session_id: request.session_id,
                authorization_digest: request.authorization_digest,
                lease_owner: request.lease_owner,
                reason: request.reason,
                closed_at_ms: request.closed_at_ms,
            })
            .map_err(|e| e.to_string())
    }
}

impl From<DurableTurnSupplement> for DurableTurnInput {
    fn from(input: DurableTurnSupplement) -> Self {
        Self {
            payload: TurnInputPayload::UserSupplement {
                supplement_id: input.supplement_id,
                message: input.message,
            },
            sequence: input.sequence,
            created_at_ms: input.created_at_ms,
            claim_token: input.claim_token,
            claim_lease_owner: input.claim_lease_owner,
        }
    }
}

impl DurableTurnInput {
    pub(crate) fn user_supplement(&self) -> Option<DurableTurnSupplement> {
        match &self.payload {
            TurnInputPayload::UserSupplement {
                supplement_id,
                message,
            } => Some(DurableTurnSupplement {
                supplement_id: supplement_id.clone(),
                message: message.clone(),
                sequence: self.sequence,
                created_at_ms: self.created_at_ms,
                claim_token: self.claim_token.clone(),
                claim_lease_owner: self.claim_lease_owner.clone(),
            }),
            TurnInputPayload::HostEvent(_) => None,
        }
    }
}
