use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use super::{
    AgentRuntimeSnapshotStorePort, ConsumeWaitCheckpointRequest, CreateDeadLetterAndFailJobRequest,
    RuntimeJobWaitCheckpointCursor, RuntimeStore, RuntimeStoreError, RuntimeStoreTransactionPort,
    SaveWaitCheckpointRequest, SessionDataStorePort, UpsertExternalContextAndScheduleJobRequest,
    UpsertExternalContextLinkAndCompleteJobRequest,
};
use crate::runtime::contracts::{CheckpointRecord, RuntimeEvent, TimestampMs};
use crate::session::external_context::{
    ExternalContextObject, ExternalContextObjectIndexEntry, ExternalContextObjectLink,
    ExternalContextStorePort, ListExternalContextObjectsRequest,
};
use crate::session::reliability::{
    AcquireResourceClaimRequest, AcquireResourceClaimResult, CancelRuntimeJobRequest,
    ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest, CreateDeadLetterRequest,
    CreateDeadLetterResult, DeadLetterRecord, DeadLetterStorePort, DismissDeadLetterRequest,
    FailRuntimeJobRequest, ListDeadLettersRequest, ListRuntimeJobsRequest,
    MarkDeadLetterReplayedRequest, MarkDeadLetterReplayingRequest, ReleaseResourceClaimRequest,
    RenewRuntimeJobLeaseRequest, ReplayDeadLetterRequest, ReplayDeadLetterResult,
    ResourceClaimRecord, ResourceClaimStorePort, RuntimeJobRecord, RuntimeJobStorePort,
    ScheduleRuntimeJobRequest, ScheduleRuntimeJobResult, StartRuntimeJobRequest,
    WakeRuntimeJobDisposition, WakeRuntimeJobRequest, YieldRuntimeJobRequest,
};

const RUNTIME_STORE_ACTOR_CHANNEL_CAPACITY: usize = 128;
const RUNTIME_STORE_ACTOR_SYNC_TIMEOUT: Duration = Duration::from_secs(30);
const RUNTIME_STORE_ACTOR_SYNC_TIMEOUT_MS: u64 = 30_000;

pub trait RuntimeStoreActorBackend:
    RuntimeStore
    + RuntimeJobStorePort
    + ExternalContextStorePort
    + DeadLetterStorePort
    + RuntimeStoreTransactionPort
    + SessionDataStorePort
    + AgentRuntimeSnapshotStorePort
    + ResourceClaimStorePort
    + Clone
    + Send
    + Sync
    + 'static
{
}

impl<T> RuntimeStoreActorBackend for T where
    T: RuntimeStore
        + RuntimeJobStorePort
        + ExternalContextStorePort
        + DeadLetterStorePort
        + RuntimeStoreTransactionPort
        + SessionDataStorePort
        + AgentRuntimeSnapshotStorePort
        + ResourceClaimStorePort
        + Clone
        + Send
        + Sync
        + 'static
{
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeStoreActorOperationMeta {
    enqueued_at_ms: TimestampMs,
    deadline_after_ms: Option<u64>,
}

struct RuntimeStoreActorEnvelope {
    meta: RuntimeStoreActorOperationMeta,
    command: RuntimeStoreActorCommand,
}

#[derive(Clone)]
pub struct RuntimeStoreActor {
    sender: mpsc::Sender<RuntimeStoreActorEnvelope>,
    next_operation_id: Arc<AtomicU64>,
}

enum RuntimeStoreActorReply<T, E = String> {
    Async(oneshot::Sender<Result<T, E>>),
    Sync(std_mpsc::SyncSender<Result<T, E>>),
}

impl<T, E> RuntimeStoreActorReply<T, E> {
    fn send(self, value: Result<T, E>) {
        match self {
            Self::Async(reply) => {
                let _ = reply.send(value);
            }
            Self::Sync(reply) => {
                let _ = reply.send(value);
            }
        }
    }
}

enum RuntimeStoreActorTransportError {
    Closed {
        operation_id: Option<u64>,
        operation_kind: Option<&'static str>,
    },
    ResponseDropped {
        operation_id: Option<u64>,
        operation_kind: Option<&'static str>,
    },
    QueueTimeout {
        operation_id: u64,
        operation_kind: &'static str,
    },
    OperationTimeout {
        operation_id: u64,
        operation_kind: &'static str,
    },
    InvalidRuntimeContext,
    OperationJoinFailed(String),
}

impl RuntimeStoreActorTransportError {
    fn into_runtime_store_error(self) -> RuntimeStoreError {
        match self {
            Self::Closed {
                operation_id,
                operation_kind,
            } => RuntimeStoreError::ActorClosed {
                operation_id,
                operation_kind,
            },
            Self::ResponseDropped {
                operation_id,
                operation_kind,
            } => RuntimeStoreError::ActorResponseDropped {
                operation_id,
                operation_kind,
            },
            Self::QueueTimeout {
                operation_id,
                operation_kind,
            } => RuntimeStoreError::ActorQueueTimeout {
                operation_id,
                operation_kind,
                timeout_ms: RUNTIME_STORE_ACTOR_SYNC_TIMEOUT_MS,
            },
            Self::OperationTimeout {
                operation_id,
                operation_kind,
            } => RuntimeStoreError::ActorOperationTimeout {
                operation_id,
                operation_kind,
                timeout_ms: RUNTIME_STORE_ACTOR_SYNC_TIMEOUT_MS,
            },
            Self::InvalidRuntimeContext => RuntimeStoreError::InvalidRuntimeContext,
            Self::OperationJoinFailed(message) => {
                RuntimeStoreError::ActorOperationJoinFailed { message }
            }
        }
    }
}

trait RuntimeStoreActorReplyError: Sized {
    fn from_actor_transport(error: RuntimeStoreActorTransportError) -> Self;
}

impl RuntimeStoreActorReplyError for RuntimeStoreError {
    fn from_actor_transport(error: RuntimeStoreActorTransportError) -> Self {
        error.into_runtime_store_error()
    }
}

impl RuntimeStoreActorReplyError for String {
    fn from_actor_transport(error: RuntimeStoreActorTransportError) -> Self {
        error.into_runtime_store_error().to_string()
    }
}

impl RuntimeStoreActor {
    pub fn start<S>(store: S) -> Result<Self, RuntimeStoreError>
    where
        S: RuntimeStoreActorBackend,
    {
        let handle = tokio::runtime::Handle::try_current().map_err(|error| {
            RuntimeStoreError::ActorRuntimeUnavailable {
                message: format!("runtime store actor requires an active tokio runtime: {error}"),
            }
        })?;
        let (sender, receiver) = mpsc::channel(RUNTIME_STORE_ACTOR_CHANNEL_CAPACITY);
        handle.spawn(run_runtime_store_actor(store, receiver));
        Ok(Self {
            sender,
            next_operation_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub async fn save_checkpoint(
        &self,
        checkpoint: CheckpointRecord,
    ) -> Result<(), RuntimeStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::SaveCheckpoint {
                checkpoint,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn load_latest_checkpoint(
        &self,
        session_id: impl Into<String>,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::LoadLatestCheckpoint {
                session_id: session_id.into(),
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn load_checkpoint_by_turn(
        &self,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::LoadCheckpointByTurn {
                session_id: session_id.into(),
                turn_id: turn_id.into(),
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn list_checkpoints(
        &self,
        session_id: impl Into<String>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ListCheckpoints {
                session_id: session_id.into(),
                limit,
                offset,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn list_waiting_runtime_job_checkpoints(
        &self,
        after: Option<RuntimeJobWaitCheckpointCursor>,
        limit: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ListWaitingRuntimeJobCheckpoints {
                after,
                limit,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn append_event(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::AppendEvent {
                event,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn append_event_idempotent(
        &self,
        event: RuntimeEvent,
    ) -> Result<(), RuntimeStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::AppendEventIdempotent {
                event,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn list_events(
        &self,
        session_id: impl Into<String>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<RuntimeEvent>, RuntimeStoreError> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ListEvents {
                session_id: session_id.into(),
                limit,
                offset,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn save_wait_checkpoint(&self, req: SaveWaitCheckpointRequest) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::SaveWaitCheckpoint {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn consume_wait_checkpoint(
        &self,
        req: ConsumeWaitCheckpointRequest,
    ) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ConsumeWaitCheckpoint {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    async fn send<T, E>(
        &self,
        command: RuntimeStoreActorCommand,
        receiver: oneshot::Receiver<Result<T, E>>,
    ) -> Result<T, E>
    where
        E: RuntimeStoreActorReplyError,
    {
        let operation_id = self.next_operation_id.fetch_add(1, Ordering::Relaxed);
        let operation_kind = command.operation_kind();
        let meta = RuntimeStoreActorOperationMeta {
            enqueued_at_ms: crate::runtime::contracts::current_timestamp_ms(),
            deadline_after_ms: None,
        };
        let envelope = RuntimeStoreActorEnvelope { meta, command };
        self.sender.send(envelope).await.map_err(|_| {
            E::from_actor_transport(RuntimeStoreActorTransportError::Closed {
                operation_id: Some(operation_id),
                operation_kind: Some(operation_kind),
            })
        })?;
        receiver.await.map_err(|_| {
            E::from_actor_transport(RuntimeStoreActorTransportError::ResponseDropped {
                operation_id: Some(operation_id),
                operation_kind: Some(operation_kind),
            })
        })?
    }

    fn send_blocking<T, E, F>(&self, build_command: F) -> Result<T, E>
    where
        E: RuntimeStoreActorReplyError,
        F: FnOnce(RuntimeStoreActorReply<T, E>) -> RuntimeStoreActorCommand,
    {
        let runtime_flavor = tokio::runtime::Handle::try_current()
            .ok()
            .map(|handle| handle.runtime_flavor());
        if runtime_flavor == Some(tokio::runtime::RuntimeFlavor::CurrentThread) {
            return Err(E::from_actor_transport(
                RuntimeStoreActorTransportError::InvalidRuntimeContext,
            ));
        }

        let (reply, receiver) = std_mpsc::sync_channel(1);
        let command = build_command(RuntimeStoreActorReply::Sync(reply));
        let operation_id = self.next_operation_id.fetch_add(1, Ordering::Relaxed);
        let operation_kind = command.operation_kind();
        let meta = RuntimeStoreActorOperationMeta {
            enqueued_at_ms: crate::runtime::contracts::current_timestamp_ms(),
            deadline_after_ms: Some(RUNTIME_STORE_ACTOR_SYNC_TIMEOUT_MS),
        };
        let mut envelope = RuntimeStoreActorEnvelope { meta, command };
        let wait_for_reply = || {
            let deadline = Instant::now() + RUNTIME_STORE_ACTOR_SYNC_TIMEOUT;
            loop {
                match self.sender.try_send(envelope) {
                    Ok(()) => break,
                    Err(mpsc::error::TrySendError::Full(returned)) => {
                        envelope = returned;
                        if Instant::now() >= deadline {
                            return Err(E::from_actor_transport(
                                RuntimeStoreActorTransportError::QueueTimeout {
                                    operation_id,
                                    operation_kind,
                                },
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        return Err(E::from_actor_transport(
                            RuntimeStoreActorTransportError::Closed {
                                operation_id: Some(operation_id),
                                operation_kind: Some(operation_kind),
                            },
                        ));
                    }
                }
            }

            receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| match error {
                    std_mpsc::RecvTimeoutError::Timeout => {
                        E::from_actor_transport(RuntimeStoreActorTransportError::OperationTimeout {
                            operation_id,
                            operation_kind,
                        })
                    }
                    std_mpsc::RecvTimeoutError::Disconnected => {
                        E::from_actor_transport(RuntimeStoreActorTransportError::ResponseDropped {
                            operation_id: Some(operation_id),
                            operation_kind: Some(operation_kind),
                        })
                    }
                })
        };
        let reply = if runtime_flavor == Some(tokio::runtime::RuntimeFlavor::MultiThread) {
            tokio::task::block_in_place(wait_for_reply)
        } else {
            wait_for_reply()
        };
        reply?
    }
}

impl RuntimeStore for RuntimeStoreActor {
    fn save_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<(), RuntimeStoreError> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::SaveCheckpoint { checkpoint, reply })
    }

    fn load_latest_checkpoint(
        &self,
        session_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        let session_id = session_id.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::LoadLatestCheckpoint {
            session_id,
            reply,
        })
    }

    fn load_checkpoint_by_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        let session_id = session_id.to_string();
        let turn_id = turn_id.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::LoadCheckpointByTurn {
            session_id,
            turn_id,
            reply,
        })
    }

    fn list_checkpoints(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        let session_id = session_id.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::ListCheckpoints {
            session_id,
            limit,
            offset,
            reply,
        })
    }

    fn list_waiting_runtime_job_checkpoints(
        &self,
        after: Option<&RuntimeJobWaitCheckpointCursor>,
        limit: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        let after = after.cloned();
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::ListWaitingRuntimeJobCheckpoints {
                after,
                limit,
                reply,
            },
        )
    }

    fn append_event(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::AppendEvent { event, reply })
    }

    fn append_event_idempotent(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::AppendEventIdempotent { event, reply })
    }

    fn list_events(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<RuntimeEvent>, RuntimeStoreError> {
        let session_id = session_id.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::ListEvents {
            session_id,
            limit,
            offset,
            reply,
        })
    }
}

impl RuntimeJobStorePort for RuntimeStoreActor {
    fn schedule_runtime_job(
        &self,
        req: ScheduleRuntimeJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::ScheduleRuntimeJob { req, reply })
    }

    fn get_runtime_job(&self, job_id: &str) -> Result<Option<RuntimeJobRecord>, String> {
        let job_id = job_id.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::GetRuntimeJob { job_id, reply })
    }

    fn list_runtime_jobs(
        &self,
        req: ListRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::ListRuntimeJobs { req, reply })
    }

    fn claim_due_runtime_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::ClaimDueRuntimeJobs { req, reply })
    }

    fn start_runtime_job(&self, req: StartRuntimeJobRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::StartRuntimeJob { req, reply })
    }

    fn renew_runtime_job_lease(&self, req: RenewRuntimeJobLeaseRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::RenewRuntimeJobLease { req, reply })
    }

    fn yield_runtime_job(&self, req: YieldRuntimeJobRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::YieldRuntimeJob { req, reply })
    }

    fn wake_runtime_job(
        &self,
        req: WakeRuntimeJobRequest,
    ) -> Result<WakeRuntimeJobDisposition, String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::WakeRuntimeJob { req, reply })
    }

    fn complete_runtime_job(&self, req: CompleteRuntimeJobRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::CompleteRuntimeJob { req, reply })
    }

    fn fail_runtime_job(&self, req: FailRuntimeJobRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::FailRuntimeJob { req, reply })
    }

    fn cancel_runtime_job(&self, req: CancelRuntimeJobRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::CancelRuntimeJob { req, reply })
    }

    fn reclaim_expired_runtime_job_leases(&self, now_ms: TimestampMs) -> Result<usize, String> {
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::ReclaimExpiredRuntimeJobLeases { now_ms, reply },
        )
    }
}

impl DeadLetterStorePort for RuntimeStoreActor {
    fn create_dead_letter(
        &self,
        req: CreateDeadLetterRequest,
    ) -> Result<CreateDeadLetterResult, String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::CreateDeadLetter { req, reply })
    }

    fn get_dead_letter(&self, dead_letter_id: &str) -> Result<Option<DeadLetterRecord>, String> {
        let dead_letter_id = dead_letter_id.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::GetDeadLetter {
            dead_letter_id,
            reply,
        })
    }

    fn list_dead_letters(
        &self,
        req: ListDeadLettersRequest,
    ) -> Result<Vec<DeadLetterRecord>, String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::ListDeadLetters { req, reply })
    }

    fn mark_dead_letter_replaying(
        &self,
        req: MarkDeadLetterReplayingRequest,
    ) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::MarkDeadLetterReplaying { req, reply })
    }

    fn mark_dead_letter_replayed(&self, req: MarkDeadLetterReplayedRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::MarkDeadLetterReplayed { req, reply })
    }

    fn replay_dead_letter(
        &self,
        req: ReplayDeadLetterRequest,
    ) -> Result<ReplayDeadLetterResult, String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::ReplayDeadLetter { req, reply })
    }

    fn dismiss_dead_letter(&self, req: DismissDeadLetterRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::DismissDeadLetter { req, reply })
    }
}

impl ExternalContextStorePort for RuntimeStoreActor {
    fn upsert_external_context_object(&self, object: ExternalContextObject) -> Result<(), String> {
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::UpsertExternalContextObject { object, reply },
        )
    }

    fn load_external_context_object(
        &self,
        object_id: &str,
    ) -> Result<Option<ExternalContextObject>, String> {
        let object_id = object_id.to_string();
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::LoadExternalContextObject { object_id, reply },
        )
    }

    fn link_external_context_object(&self, link: ExternalContextObjectLink) -> Result<(), String> {
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::LinkExternalContextObject { link, reply },
        )
    }

    fn load_external_context_object_link(
        &self,
        session_id: &str,
        object_id: &str,
        turn_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<ExternalContextObjectLink>, String> {
        let session_id = session_id.to_string();
        let object_id = object_id.to_string();
        let turn_id = turn_id.to_string();
        let tool_call_id = tool_call_id.to_string();
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::LoadExternalContextObjectLink {
                session_id,
                object_id,
                turn_id,
                tool_call_id,
                reply,
            },
        )
    }

    fn list_external_context_objects(
        &self,
        req: ListExternalContextObjectsRequest,
    ) -> Result<Vec<ExternalContextObjectIndexEntry>, String> {
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::ListExternalContextObjects { req, reply },
        )
    }
}

impl ResourceClaimStorePort for RuntimeStoreActor {
    fn acquire_resource_claim(
        &self,
        req: AcquireResourceClaimRequest,
    ) -> Result<AcquireResourceClaimResult, String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::AcquireResourceClaim { req, reply })
    }

    fn get_resource_claim(
        &self,
        resource_kind: &str,
        resource_key: &str,
    ) -> Result<Option<ResourceClaimRecord>, String> {
        let resource_kind = resource_kind.to_string();
        let resource_key = resource_key.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::GetResourceClaim {
            resource_kind,
            resource_key,
            reply,
        })
    }

    fn release_resource_claim(&self, req: ReleaseResourceClaimRequest) -> Result<bool, String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::ReleaseResourceClaim { req, reply })
    }

    fn reclaim_expired_resource_claims(&self, now_ms: TimestampMs) -> Result<usize, String> {
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::ReclaimExpiredResourceClaims { now_ms, reply },
        )
    }
}

impl RuntimeStoreTransactionPort for RuntimeStoreActor {
    fn save_wait_checkpoint(&self, req: SaveWaitCheckpointRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::SaveWaitCheckpoint { req, reply })
    }

    fn consume_wait_checkpoint(&self, req: ConsumeWaitCheckpointRequest) -> Result<(), String> {
        self.send_blocking(|reply| RuntimeStoreActorCommand::ConsumeWaitCheckpoint { req, reply })
    }

    fn upsert_external_context_and_schedule_job(
        &self,
        req: UpsertExternalContextAndScheduleJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, String> {
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::UpsertExternalContextAndScheduleJob { req, reply },
        )
    }

    fn upsert_external_context_link_and_complete_job(
        &self,
        req: UpsertExternalContextLinkAndCompleteJobRequest,
    ) -> Result<(), String> {
        self.send_blocking(|reply| {
            RuntimeStoreActorCommand::UpsertExternalContextLinkAndCompleteJob { req, reply }
        })
    }

    fn create_dead_letter_and_fail_job(
        &self,
        req: CreateDeadLetterAndFailJobRequest,
    ) -> Result<CreateDeadLetterResult, String> {
        self.send_blocking(
            |reply| RuntimeStoreActorCommand::CreateDeadLetterAndFailJob { req, reply },
        )
    }
}

impl RuntimeStoreActor {
    pub async fn upsert_external_context_link_and_complete_job(
        &self,
        req: UpsertExternalContextLinkAndCompleteJobRequest,
    ) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::UpsertExternalContextLinkAndCompleteJob {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }
}

impl AgentRuntimeSnapshotStorePort for RuntimeStoreActor {
    fn load_agent_runtime_snapshot(&self, session_id: &str) -> Result<Option<String>, String> {
        let session_id = session_id.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::LoadAgentRuntimeSnapshot {
            session_id,
            reply,
        })
    }

    fn save_agent_runtime_snapshot(
        &self,
        session_id: &str,
        snapshot_json: &str,
        updated_at_ms: i64,
    ) -> Result<(), String> {
        let session_id = session_id.to_string();
        let snapshot_json = snapshot_json.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::SaveAgentRuntimeSnapshot {
            session_id,
            snapshot_json,
            updated_at_ms,
            reply,
        })
    }
}

impl SessionDataStorePort for RuntimeStoreActor {
    fn delete_session_data(&self, session_id: &str) -> Result<(), String> {
        let session_id = session_id.to_string();
        self.send_blocking(|reply| RuntimeStoreActorCommand::DeleteSessionData {
            session_id,
            reply,
        })
    }
}

impl RuntimeStoreActor {
    pub async fn schedule_runtime_job(
        &self,
        req: ScheduleRuntimeJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ScheduleRuntimeJob {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn get_runtime_job(&self, job_id: &str) -> Result<Option<RuntimeJobRecord>, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::GetRuntimeJob {
                job_id: job_id.to_string(),
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn list_runtime_jobs(
        &self,
        req: ListRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ListRuntimeJobs {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn claim_due_runtime_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ClaimDueRuntimeJobs {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn start_runtime_job(&self, req: StartRuntimeJobRequest) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::StartRuntimeJob {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn renew_runtime_job_lease(
        &self,
        req: RenewRuntimeJobLeaseRequest,
    ) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::RenewRuntimeJobLease {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn yield_runtime_job(&self, req: YieldRuntimeJobRequest) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::YieldRuntimeJob {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn wake_runtime_job(
        &self,
        req: WakeRuntimeJobRequest,
    ) -> Result<WakeRuntimeJobDisposition, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::WakeRuntimeJob {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn complete_runtime_job(&self, req: CompleteRuntimeJobRequest) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::CompleteRuntimeJob {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn fail_runtime_job(&self, req: FailRuntimeJobRequest) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::FailRuntimeJob {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn cancel_runtime_job(&self, req: CancelRuntimeJobRequest) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::CancelRuntimeJob {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn reclaim_expired_runtime_job_leases(
        &self,
        now_ms: TimestampMs,
    ) -> Result<usize, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ReclaimExpiredRuntimeJobLeases {
                now_ms,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }
}

impl RuntimeStoreActor {
    pub async fn create_dead_letter(
        &self,
        req: CreateDeadLetterRequest,
    ) -> Result<CreateDeadLetterResult, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::CreateDeadLetter {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn get_dead_letter(
        &self,
        dead_letter_id: &str,
    ) -> Result<Option<DeadLetterRecord>, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::GetDeadLetter {
                dead_letter_id: dead_letter_id.to_string(),
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn list_dead_letters(
        &self,
        req: ListDeadLettersRequest,
    ) -> Result<Vec<DeadLetterRecord>, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ListDeadLetters {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn mark_dead_letter_replaying(
        &self,
        req: MarkDeadLetterReplayingRequest,
    ) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::MarkDeadLetterReplaying {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn mark_dead_letter_replayed(
        &self,
        req: MarkDeadLetterReplayedRequest,
    ) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::MarkDeadLetterReplayed {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn replay_dead_letter(
        &self,
        req: ReplayDeadLetterRequest,
    ) -> Result<ReplayDeadLetterResult, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ReplayDeadLetter {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn dismiss_dead_letter(&self, req: DismissDeadLetterRequest) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::DismissDeadLetter {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }
}

impl RuntimeStoreActor {
    pub async fn upsert_external_context_object(
        &self,
        object: ExternalContextObject,
    ) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::UpsertExternalContextObject {
                object,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn load_external_context_object(
        &self,
        object_id: &str,
    ) -> Result<Option<ExternalContextObject>, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::LoadExternalContextObject {
                object_id: object_id.to_string(),
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn link_external_context_object(
        &self,
        link: ExternalContextObjectLink,
    ) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::LinkExternalContextObject {
                link,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }

    pub async fn list_external_context_objects(
        &self,
        req: ListExternalContextObjectsRequest,
    ) -> Result<Vec<ExternalContextObjectIndexEntry>, String> {
        let (reply, receiver) = oneshot::channel();
        self.send(
            RuntimeStoreActorCommand::ListExternalContextObjects {
                req,
                reply: RuntimeStoreActorReply::Async(reply),
            },
            receiver,
        )
        .await
    }
}

enum RuntimeStoreActorCommand {
    SaveWaitCheckpoint {
        req: SaveWaitCheckpointRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    ConsumeWaitCheckpoint {
        req: ConsumeWaitCheckpointRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    SaveCheckpoint {
        checkpoint: CheckpointRecord,
        reply: RuntimeStoreActorReply<(), RuntimeStoreError>,
    },
    LoadLatestCheckpoint {
        session_id: String,
        reply: RuntimeStoreActorReply<Option<CheckpointRecord>, RuntimeStoreError>,
    },
    LoadCheckpointByTurn {
        session_id: String,
        turn_id: String,
        reply: RuntimeStoreActorReply<Option<CheckpointRecord>, RuntimeStoreError>,
    },
    ListCheckpoints {
        session_id: String,
        limit: usize,
        offset: usize,
        reply: RuntimeStoreActorReply<Vec<CheckpointRecord>, RuntimeStoreError>,
    },
    ListWaitingRuntimeJobCheckpoints {
        after: Option<RuntimeJobWaitCheckpointCursor>,
        limit: usize,
        reply: RuntimeStoreActorReply<Vec<CheckpointRecord>, RuntimeStoreError>,
    },
    AppendEvent {
        event: RuntimeEvent,
        reply: RuntimeStoreActorReply<(), RuntimeStoreError>,
    },
    AppendEventIdempotent {
        event: RuntimeEvent,
        reply: RuntimeStoreActorReply<(), RuntimeStoreError>,
    },
    ListEvents {
        session_id: String,
        limit: usize,
        offset: usize,
        reply: RuntimeStoreActorReply<Vec<RuntimeEvent>, RuntimeStoreError>,
    },
    ScheduleRuntimeJob {
        req: ScheduleRuntimeJobRequest,
        reply: RuntimeStoreActorReply<ScheduleRuntimeJobResult>,
    },
    GetRuntimeJob {
        job_id: String,
        reply: RuntimeStoreActorReply<Option<RuntimeJobRecord>>,
    },
    ListRuntimeJobs {
        req: ListRuntimeJobsRequest,
        reply: RuntimeStoreActorReply<Vec<RuntimeJobRecord>>,
    },
    ClaimDueRuntimeJobs {
        req: ClaimDueRuntimeJobsRequest,
        reply: RuntimeStoreActorReply<Vec<RuntimeJobRecord>>,
    },
    StartRuntimeJob {
        req: StartRuntimeJobRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    RenewRuntimeJobLease {
        req: RenewRuntimeJobLeaseRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    YieldRuntimeJob {
        req: YieldRuntimeJobRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    WakeRuntimeJob {
        req: WakeRuntimeJobRequest,
        reply: RuntimeStoreActorReply<WakeRuntimeJobDisposition>,
    },
    CompleteRuntimeJob {
        req: CompleteRuntimeJobRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    FailRuntimeJob {
        req: FailRuntimeJobRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    CancelRuntimeJob {
        req: CancelRuntimeJobRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    ReclaimExpiredRuntimeJobLeases {
        now_ms: TimestampMs,
        reply: RuntimeStoreActorReply<usize>,
    },
    CreateDeadLetter {
        req: CreateDeadLetterRequest,
        reply: RuntimeStoreActorReply<CreateDeadLetterResult>,
    },
    GetDeadLetter {
        dead_letter_id: String,
        reply: RuntimeStoreActorReply<Option<DeadLetterRecord>>,
    },
    ListDeadLetters {
        req: ListDeadLettersRequest,
        reply: RuntimeStoreActorReply<Vec<DeadLetterRecord>>,
    },
    MarkDeadLetterReplaying {
        req: MarkDeadLetterReplayingRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    MarkDeadLetterReplayed {
        req: MarkDeadLetterReplayedRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    ReplayDeadLetter {
        req: ReplayDeadLetterRequest,
        reply: RuntimeStoreActorReply<ReplayDeadLetterResult>,
    },
    DismissDeadLetter {
        req: DismissDeadLetterRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    UpsertExternalContextObject {
        object: ExternalContextObject,
        reply: RuntimeStoreActorReply<()>,
    },
    LoadExternalContextObject {
        object_id: String,
        reply: RuntimeStoreActorReply<Option<ExternalContextObject>>,
    },
    LinkExternalContextObject {
        link: ExternalContextObjectLink,
        reply: RuntimeStoreActorReply<()>,
    },
    LoadExternalContextObjectLink {
        session_id: String,
        object_id: String,
        turn_id: String,
        tool_call_id: String,
        reply: RuntimeStoreActorReply<Option<ExternalContextObjectLink>>,
    },
    ListExternalContextObjects {
        req: ListExternalContextObjectsRequest,
        reply: RuntimeStoreActorReply<Vec<ExternalContextObjectIndexEntry>>,
    },
    AcquireResourceClaim {
        req: AcquireResourceClaimRequest,
        reply: RuntimeStoreActorReply<AcquireResourceClaimResult>,
    },
    GetResourceClaim {
        resource_kind: String,
        resource_key: String,
        reply: RuntimeStoreActorReply<Option<ResourceClaimRecord>>,
    },
    ReleaseResourceClaim {
        req: ReleaseResourceClaimRequest,
        reply: RuntimeStoreActorReply<bool>,
    },
    ReclaimExpiredResourceClaims {
        now_ms: TimestampMs,
        reply: RuntimeStoreActorReply<usize>,
    },
    UpsertExternalContextAndScheduleJob {
        req: UpsertExternalContextAndScheduleJobRequest,
        reply: RuntimeStoreActorReply<ScheduleRuntimeJobResult>,
    },
    UpsertExternalContextLinkAndCompleteJob {
        req: UpsertExternalContextLinkAndCompleteJobRequest,
        reply: RuntimeStoreActorReply<()>,
    },
    CreateDeadLetterAndFailJob {
        req: CreateDeadLetterAndFailJobRequest,
        reply: RuntimeStoreActorReply<CreateDeadLetterResult>,
    },
    LoadAgentRuntimeSnapshot {
        session_id: String,
        reply: RuntimeStoreActorReply<Option<String>>,
    },
    SaveAgentRuntimeSnapshot {
        session_id: String,
        snapshot_json: String,
        updated_at_ms: i64,
        reply: RuntimeStoreActorReply<()>,
    },
    DeleteSessionData {
        session_id: String,
        reply: RuntimeStoreActorReply<()>,
    },
}

impl RuntimeStoreActorCommand {
    fn operation_kind(&self) -> &'static str {
        match self {
            Self::SaveWaitCheckpoint { .. } => "save_wait_checkpoint",
            Self::ConsumeWaitCheckpoint { .. } => "consume_wait_checkpoint",
            Self::SaveCheckpoint { .. } => "save_checkpoint",
            Self::LoadLatestCheckpoint { .. } => "load_latest_checkpoint",
            Self::LoadCheckpointByTurn { .. } => "load_checkpoint_by_turn",
            Self::ListCheckpoints { .. } => "list_checkpoints",
            Self::ListWaitingRuntimeJobCheckpoints { .. } => "list_waiting_runtime_job_checkpoints",
            Self::AppendEvent { .. } => "append_event",
            Self::AppendEventIdempotent { .. } => "append_event_idempotent",
            Self::ListEvents { .. } => "list_events",
            Self::ScheduleRuntimeJob { .. } => "schedule_runtime_job",
            Self::GetRuntimeJob { .. } => "get_runtime_job",
            Self::ListRuntimeJobs { .. } => "list_runtime_jobs",
            Self::ClaimDueRuntimeJobs { .. } => "claim_due_runtime_jobs",
            Self::StartRuntimeJob { .. } => "start_runtime_job",
            Self::RenewRuntimeJobLease { .. } => "renew_runtime_job_lease",
            Self::YieldRuntimeJob { .. } => "yield_runtime_job",
            Self::WakeRuntimeJob { .. } => "wake_runtime_job",
            Self::CompleteRuntimeJob { .. } => "complete_runtime_job",
            Self::FailRuntimeJob { .. } => "fail_runtime_job",
            Self::CancelRuntimeJob { .. } => "cancel_runtime_job",
            Self::ReclaimExpiredRuntimeJobLeases { .. } => "reclaim_expired_runtime_job_leases",
            Self::CreateDeadLetter { .. } => "create_dead_letter",
            Self::GetDeadLetter { .. } => "get_dead_letter",
            Self::ListDeadLetters { .. } => "list_dead_letters",
            Self::MarkDeadLetterReplaying { .. } => "mark_dead_letter_replaying",
            Self::MarkDeadLetterReplayed { .. } => "mark_dead_letter_replayed",
            Self::ReplayDeadLetter { .. } => "replay_dead_letter",
            Self::DismissDeadLetter { .. } => "dismiss_dead_letter",
            Self::UpsertExternalContextObject { .. } => "upsert_external_context_object",
            Self::LoadExternalContextObject { .. } => "load_external_context_object",
            Self::LinkExternalContextObject { .. } => "link_external_context_object",
            Self::LoadExternalContextObjectLink { .. } => "load_external_context_object_link",
            Self::ListExternalContextObjects { .. } => "list_external_context_objects",
            Self::AcquireResourceClaim { .. } => "acquire_resource_claim",
            Self::GetResourceClaim { .. } => "get_resource_claim",
            Self::ReleaseResourceClaim { .. } => "release_resource_claim",
            Self::ReclaimExpiredResourceClaims { .. } => "reclaim_expired_resource_claims",
            Self::UpsertExternalContextAndScheduleJob { .. } => {
                "upsert_external_context_and_schedule_job"
            }
            Self::UpsertExternalContextLinkAndCompleteJob { .. } => {
                "upsert_external_context_link_and_complete_job"
            }
            Self::CreateDeadLetterAndFailJob { .. } => "create_dead_letter_and_fail_job",
            Self::LoadAgentRuntimeSnapshot { .. } => "load_agent_runtime_snapshot",
            Self::SaveAgentRuntimeSnapshot { .. } => "save_agent_runtime_snapshot",
            Self::DeleteSessionData { .. } => "delete_session_data",
        }
    }
}

async fn run_runtime_store_actor<S>(
    store: S,
    mut receiver: mpsc::Receiver<RuntimeStoreActorEnvelope>,
) where
    S: RuntimeStoreActorBackend,
{
    while let Some(envelope) = receiver.recv().await {
        let RuntimeStoreActorEnvelope { meta, command } = envelope;
        let started_at_ms = crate::runtime::contracts::current_timestamp_ms();
        if operation_deadline_expired(&meta, started_at_ms) {
            continue;
        }
        match command {
            RuntimeStoreActorCommand::SaveWaitCheckpoint { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.save_wait_checkpoint(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ConsumeWaitCheckpoint { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.consume_wait_checkpoint(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::SaveCheckpoint { checkpoint, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.save_checkpoint(checkpoint)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::LoadLatestCheckpoint { session_id, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.load_latest_checkpoint(session_id.as_str())
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::LoadCheckpointByTurn {
                session_id,
                turn_id,
                reply,
            } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.load_checkpoint_by_turn(session_id.as_str(), turn_id.as_str())
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ListCheckpoints {
                session_id,
                limit,
                offset,
                reply,
            } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.list_checkpoints(session_id.as_str(), limit, offset)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ListWaitingRuntimeJobCheckpoints {
                after,
                limit,
                reply,
            } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.list_waiting_runtime_job_checkpoints(after.as_ref(), limit)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::AppendEvent { event, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.append_event(event))
                        .await,
                );
            }
            RuntimeStoreActorCommand::ListEvents {
                session_id,
                limit,
                offset,
                reply,
            } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.list_events(session_id.as_str(), limit, offset)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ScheduleRuntimeJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.schedule_runtime_job(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::GetRuntimeJob { job_id, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.get_runtime_job(job_id.as_str())
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ListRuntimeJobs { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.list_runtime_jobs(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::ClaimDueRuntimeJobs { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.claim_due_runtime_jobs(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::StartRuntimeJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.start_runtime_job(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::RenewRuntimeJobLease { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.renew_runtime_job_lease(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::YieldRuntimeJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.yield_runtime_job(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::WakeRuntimeJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.wake_runtime_job(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::CompleteRuntimeJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.complete_runtime_job(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::FailRuntimeJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.fail_runtime_job(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::CancelRuntimeJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.cancel_runtime_job(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::ReclaimExpiredRuntimeJobLeases { now_ms, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.reclaim_expired_runtime_job_leases(now_ms)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::CreateDeadLetter { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.create_dead_letter(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::GetDeadLetter {
                dead_letter_id,
                reply,
            } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.get_dead_letter(dead_letter_id.as_str())
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ListDeadLetters { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.list_dead_letters(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::MarkDeadLetterReplaying { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.mark_dead_letter_replaying(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::MarkDeadLetterReplayed { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.mark_dead_letter_replayed(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ReplayDeadLetter { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.replay_dead_letter(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::DismissDeadLetter { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| store.dismiss_dead_letter(req))
                        .await,
                );
            }
            RuntimeStoreActorCommand::UpsertExternalContextObject { object, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.upsert_external_context_object(object)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::LoadExternalContextObject { object_id, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.load_external_context_object(object_id.as_str())
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::LinkExternalContextObject { link, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.link_external_context_object(link)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::LoadExternalContextObjectLink {
                session_id,
                object_id,
                turn_id,
                tool_call_id,
                reply,
            } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.load_external_context_object_link(
                            session_id.as_str(),
                            object_id.as_str(),
                            turn_id.as_str(),
                            tool_call_id.as_str(),
                        )
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ListExternalContextObjects { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.list_external_context_objects(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::AcquireResourceClaim { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.acquire_resource_claim(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::GetResourceClaim {
                resource_kind,
                resource_key,
                reply,
            } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.get_resource_claim(resource_kind.as_str(), resource_key.as_str())
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ReleaseResourceClaim { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.release_resource_claim(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::ReclaimExpiredResourceClaims { now_ms, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.reclaim_expired_resource_claims(now_ms)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::UpsertExternalContextAndScheduleJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.upsert_external_context_and_schedule_job(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::UpsertExternalContextLinkAndCompleteJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.upsert_external_context_link_and_complete_job(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::CreateDeadLetterAndFailJob { req, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.create_dead_letter_and_fail_job(req)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::LoadAgentRuntimeSnapshot { session_id, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.load_agent_runtime_snapshot(session_id.as_str())
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::SaveAgentRuntimeSnapshot {
                session_id,
                snapshot_json,
                updated_at_ms,
                reply,
            } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.save_agent_runtime_snapshot(
                            session_id.as_str(),
                            snapshot_json.as_str(),
                            updated_at_ms,
                        )
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::AppendEventIdempotent { event, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.append_event_idempotent(event)
                    })
                    .await,
                );
            }
            RuntimeStoreActorCommand::DeleteSessionData { session_id, reply } => {
                reply.send(
                    run_store_operation(store.clone(), move |store| {
                        store.delete_session_data(session_id.as_str())
                    })
                    .await,
                );
            }
        }
    }
}

fn operation_deadline_expired(
    meta: &RuntimeStoreActorOperationMeta,
    started_at_ms: TimestampMs,
) -> bool {
    let Some(deadline_after_ms) = meta.deadline_after_ms else {
        return false;
    };
    let deadline_after_ms = i64::try_from(deadline_after_ms).unwrap_or(i64::MAX);
    started_at_ms > meta.enqueued_at_ms.saturating_add(deadline_after_ms)
}

async fn run_store_operation<S, T, E, F>(store: S, operation: F) -> Result<T, E>
where
    S: Send + 'static,
    T: Send + 'static,
    E: RuntimeStoreActorReplyError + Send + 'static,
    F: FnOnce(S) -> Result<T, E> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(store))
        .await
        .map_err(|error| {
            E::from_actor_transport(RuntimeStoreActorTransportError::OperationJoinFailed(
                format!("runtime store actor operation join failed: {error}"),
            ))
        })?
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::session::store::RuntimeStore;

    use super::{
        operation_deadline_expired, RuntimeStoreActor, RuntimeStoreActorCommand,
        RuntimeStoreActorEnvelope, RuntimeStoreActorOperationMeta, RuntimeStoreActorReply,
    };

    #[test]
    fn runtime_store_actor_deadline_gate_rejects_expired_operation_before_start() {
        let meta = RuntimeStoreActorOperationMeta {
            enqueued_at_ms: 1_000,
            deadline_after_ms: Some(30_000),
        };

        assert!(!operation_deadline_expired(&meta, 31_000));
        assert!(operation_deadline_expired(&meta, 31_001));
    }

    #[tokio::test]
    async fn runtime_store_actor_sync_trait_proxy_waits_for_queue_capacity() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let actor = RuntimeStoreActor {
            sender: sender.clone(),
            next_operation_id: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
        };
        let (prefill_reply, _prefill_receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .try_send(RuntimeStoreActorEnvelope {
                meta: RuntimeStoreActorOperationMeta {
                    enqueued_at_ms: 0,
                    deadline_after_ms: None,
                },
                command: RuntimeStoreActorCommand::LoadLatestCheckpoint {
                    session_id: "chat-prefill".to_string(),
                    reply: RuntimeStoreActorReply::Sync(prefill_reply),
                },
            })
            .expect("prefill actor queue");

        let started = Instant::now();
        let actor_for_thread = actor.clone();
        let join = std::thread::spawn(move || {
            RuntimeStore::load_latest_checkpoint(&actor_for_thread, "chat-backpressure")
        });
        std::thread::sleep(Duration::from_millis(20));
        let _ = receiver.try_recv().expect("drain prefilled command");
        let envelope = receiver.recv().await.expect("receive waiting sync command");
        match envelope.command {
            RuntimeStoreActorCommand::LoadLatestCheckpoint { reply, .. } => {
                reply.send(Ok(None));
            }
            _ => panic!("unexpected actor command"),
        }

        assert!(join.join().expect("join sync caller").is_ok());
        assert!(started.elapsed() >= Duration::from_millis(20));
    }
}
