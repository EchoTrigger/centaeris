use crate::runtime::contracts::{CheckpointRecord, RuntimeEvent};
use crate::session::external_context::{ExternalContextObject, ExternalContextObjectLink};
use crate::session::reliability::{
    CompleteRuntimeJobRequest, CreateDeadLetterRequest, CreateDeadLetterResult,
    FailRuntimeJobRequest, RuntimeJobRecord, ScheduleRuntimeJobResult,
};

pub mod actor;
pub use actor::RuntimeStoreActor;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeJobWaiterCursor {
    pub checkpoint_id: String,
    pub tool_call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeJobWaiter {
    pub cursor: RuntimeJobWaiterCursor,
    pub source_job_id: String,
    pub source_job_kind: String,
    pub session_id: String,
    pub agent_run_id: String,
}

/// Derived index rows; stores persist these with their source checkpoint in the
/// same transaction and delete them when that checkpoint is consumed.
pub fn runtime_job_waiters(checkpoint: &CheckpointRecord) -> Result<Vec<RuntimeJobWaiter>, String> {
    use crate::runtime::contracts::{CheckpointKindV1, RuntimeAwaitJobCheckpointV1};
    if checkpoint.kind != CheckpointKindV1::Wait
        || checkpoint.status != "waiting"
        || checkpoint.done_reason.as_deref() != Some("runtime_job")
    {
        return Ok(Vec::new());
    }
    let wait: RuntimeAwaitJobCheckpointV1 = serde_json::from_str(&checkpoint.payload_json)
        .map_err(|error| format!("invalid runtime job wait checkpoint: {error}"))?;
    wait.validate()?;
    if wait.turn_id != checkpoint.turn_id
        || checkpoint.checkpoint_id.trim().is_empty()
        || checkpoint.session_id.trim().is_empty()
    {
        return Err("runtime job wait checkpoint binding mismatch".to_string());
    }
    Ok(wait
        .waits
        .into_iter()
        .map(|item| RuntimeJobWaiter {
            cursor: RuntimeJobWaiterCursor {
                checkpoint_id: checkpoint.checkpoint_id.clone(),
                tool_call_id: item.tool_call_id,
            },
            source_job_id: item.job_id,
            source_job_kind: item.job_kind,
            session_id: checkpoint.session_id.clone(),
            agent_run_id: wait.agent_run_id.clone(),
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeStoreError {
    Backend {
        message: String,
    },
    ActorRuntimeUnavailable {
        message: String,
    },
    ActorClosed {
        operation_id: Option<u64>,
        operation_kind: Option<&'static str>,
    },
    ActorResponseDropped {
        operation_id: Option<u64>,
        operation_kind: Option<&'static str>,
    },
    ActorQueueTimeout {
        operation_id: u64,
        operation_kind: &'static str,
        timeout_ms: u64,
    },
    ActorOperationTimeout {
        operation_id: u64,
        operation_kind: &'static str,
        timeout_ms: u64,
    },
    InvalidRuntimeContext,
    ActorOperationJoinFailed {
        message: String,
    },
}

impl RuntimeStoreError {
    pub fn backend(message: impl Into<String>) -> Self {
        Self::Backend {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RuntimeStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend { message }
            | Self::ActorRuntimeUnavailable { message }
            | Self::ActorOperationJoinFailed { message } => formatter.write_str(message),
            Self::ActorClosed {
                operation_id,
                operation_kind,
            } => write_actor_identity(formatter, "runtime store actor is closed", *operation_id, *operation_kind),
            Self::ActorResponseDropped {
                operation_id,
                operation_kind,
            } => write_actor_identity(
                formatter,
                "runtime store actor dropped response",
                *operation_id,
                *operation_kind,
            ),
            Self::ActorQueueTimeout {
                operation_id,
                operation_kind,
                timeout_ms,
            } => write!(
                formatter,
                "runtime store actor sync operation timed out while waiting for queue capacity: operationId={operation_id} operationKind={operation_kind} timeoutMs={timeout_ms}"
            ),
            Self::ActorOperationTimeout {
                operation_id,
                operation_kind,
                timeout_ms,
            } => write!(
                formatter,
                "runtime store actor sync operation timed out: operationId={operation_id} operationKind={operation_kind} timeoutMs={timeout_ms}"
            ),
            Self::InvalidRuntimeContext => formatter.write_str(
                "runtime store actor sync API cannot run on a current-thread tokio runtime; use the async RuntimeStoreActor API",
            ),
        }
    }
}

impl std::error::Error for RuntimeStoreError {}

fn write_actor_identity(
    formatter: &mut std::fmt::Formatter<'_>,
    message: &str,
    operation_id: Option<u64>,
    operation_kind: Option<&'static str>,
) -> std::fmt::Result {
    match (operation_id, operation_kind) {
        (Some(operation_id), Some(operation_kind)) => write!(
            formatter,
            "{message}: operationId={operation_id} operationKind={operation_kind}"
        ),
        _ => formatter.write_str(message),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeJobWaitCheckpointCursor {
    pub session_id: String,
    pub turn_id: String,
}

pub trait RuntimeStore {
    fn list_runtime_job_waiters(
        &self,
        source_job_id: Option<&str>,
        after: Option<&RuntimeJobWaiterCursor>,
        limit: usize,
    ) -> Result<Vec<RuntimeJobWaiter>, RuntimeStoreError>;
    fn save_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<(), RuntimeStoreError>;
    fn load_latest_checkpoint(
        &self,
        session_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError>;
    fn load_checkpoint_by_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError>;
    fn list_checkpoints(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError>;
    fn list_waiting_runtime_job_checkpoints(
        &self,
        after: Option<&RuntimeJobWaitCheckpointCursor>,
        limit: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError>;

    fn append_event(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError>;
    fn append_event_idempotent(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError>;
    fn list_events(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<RuntimeEvent>, RuntimeStoreError>;
}

pub trait AgentRuntimeSnapshotStorePort {
    fn load_agent_runtime_snapshot(&self, session_id: &str) -> Result<Option<String>, String>;

    fn save_agent_runtime_snapshot(
        &self,
        session_id: &str,
        snapshot_json: &str,
        updated_at_ms: i64,
    ) -> Result<(), String>;
}

pub trait SessionDataStorePort {
    fn delete_session_data(&self, session_id: &str) -> Result<(), String>;
}

#[derive(Debug, Clone)]
pub struct UpsertExternalContextAndScheduleJobRequest {
    pub object: ExternalContextObject,
    pub job: RuntimeJobRecord,
}

#[derive(Debug, Clone)]
pub struct UpsertExternalContextLinkAndCompleteJobRequest {
    pub object: Option<ExternalContextObject>,
    pub link: Option<ExternalContextObjectLink>,
    pub complete_job: CompleteRuntimeJobRequest,
}

#[derive(Debug, Clone)]
pub struct CreateDeadLetterAndFailJobRequest {
    pub dead_letter: CreateDeadLetterRequest,
    pub fail_job: FailRuntimeJobRequest,
}

#[derive(Debug, Clone)]
pub struct SaveWaitCheckpointRequest {
    pub checkpoint: CheckpointRecord,
    pub event: RuntimeEvent,
}

#[derive(Debug, Clone)]
pub struct ConsumeWaitCheckpointRequest {
    pub checkpoint: CheckpointRecord,
    pub events: Vec<RuntimeEvent>,
}

pub trait RuntimeStoreTransactionPort {
    fn save_wait_checkpoint(&self, req: SaveWaitCheckpointRequest) -> Result<(), String>;

    fn consume_wait_checkpoint(&self, req: ConsumeWaitCheckpointRequest) -> Result<(), String>;

    fn upsert_external_context_and_schedule_job(
        &self,
        req: UpsertExternalContextAndScheduleJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, String>;

    fn upsert_external_context_link_and_complete_job(
        &self,
        req: UpsertExternalContextLinkAndCompleteJobRequest,
    ) -> Result<(), String>;

    fn create_dead_letter_and_fail_job(
        &self,
        req: CreateDeadLetterAndFailJobRequest,
    ) -> Result<CreateDeadLetterResult, String>;
}
