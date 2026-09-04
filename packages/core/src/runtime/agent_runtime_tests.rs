use super::*;
use crate::extension::hooks::{
    LifecycleHookAuditSink, LifecycleHookCommandResultV1, LifecycleHookEngineV1,
    LifecycleHookEventNameV1, LifecycleHookEventV1, LifecycleHookHandlerV1,
    LifecycleHookRunStatusV1, LifecycleHookRunV1, LifecycleHookRunner, LifecycleHookSourceKindV1,
    LifecycleHookSourceV1,
};
use crate::extension::skills::{
    add_skill_source, SkillCatalogLoadConfig, SkillSourceAddRequest, SkillSourceKindV1,
    SkillSourceScopeV1, SkillSourcesConfigV1,
};
use crate::model::{
    ModelClientError, ModelClientErrorKind, ModelClientFuture, ModelClientRequest,
    ModelClientResponse, ModelClientStreamEvent, ModelSessionConfig,
};
use crate::runtime::contracts::ProviderTokenUsageV1;
use crate::runtime::query_loop::AgentRunResourceUsageV1;
use crate::runtime::subagent::{
    SubagentLifecycleHookPhase, SubagentLifecycleRecord, SubagentLifecycleStatus,
    SubagentWorkPacketEnvelope,
};
use crate::runtime::tool_context_writer::write_tool_results_to_context;
use crate::session::manager::SessionManager;
use crate::session::store::{
    AgentRuntimeSnapshotStorePort, ConsumeWaitCheckpointRequest, CreateDeadLetterAndFailJobRequest,
    RuntimeJobWaitCheckpointCursor, RuntimeStore, RuntimeStoreError, RuntimeStoreTransactionPort,
    SaveWaitCheckpointRequest, UpsertExternalContextAndScheduleJobRequest,
    UpsertExternalContextLinkAndCompleteJobRequest,
};
use crate::session::supplement::{
    AcknowledgeTurnSupplementsRequest, ClaimTurnSupplementsRequest,
    CloseTurnSupplementQueueRequest, DurableTurnSupplement, EnqueueTurnSupplementRequest,
    EnqueueTurnSupplementResult, TurnSupplementStoreError, TurnSupplementStorePort,
};
use crate::tool::RiskLevel;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default)]
struct AgentRuntimeTestState {
    checkpoints: Vec<CheckpointRecord>,
    events: Vec<RuntimeEvent>,
    snapshots: HashMap<String, String>,
    external_objects: HashMap<String, ExternalContextObject>,
    external_links: Vec<ExternalContextObjectLink>,
    jobs: HashMap<String, RuntimeJobRecord>,
    dead_letters: HashMap<String, crate::session::reliability::DeadLetterRecord>,
}

#[derive(Clone, Debug)]
struct AgentRuntimeTestStore {
    state: Arc<Mutex<AgentRuntimeTestState>>,
}

impl AgentRuntimeTestStore {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(AgentRuntimeTestState::default())),
        }
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, AgentRuntimeTestState>, String> {
        self.state
            .lock()
            .map_err(|_| "agent runtime test store poisoned".to_string())
    }

    fn schedule_job_locked(
        state: &mut AgentRuntimeTestState,
        job: RuntimeJobRecord,
    ) -> Result<crate::session::reliability::ScheduleRuntimeJobResult, String> {
        if let Some(existing) = state.jobs.get(&job.job_id).or_else(|| {
            state
                .jobs
                .values()
                .find(|existing| existing.idempotency_key == job.idempotency_key)
        }) {
            if existing.job_id == job.job_id && existing.idempotency_key != job.idempotency_key {
                return Err("runtime_job_idempotency_conflict".to_string());
            }
            return Ok(crate::session::reliability::ScheduleRuntimeJobResult {
                disposition: crate::session::reliability::ScheduleRuntimeJobDisposition::Existing,
                job: existing.clone(),
            });
        }
        state.jobs.insert(job.job_id.clone(), job.clone());
        Ok(crate::session::reliability::ScheduleRuntimeJobResult {
            disposition: crate::session::reliability::ScheduleRuntimeJobDisposition::Inserted,
            job,
        })
    }

    fn complete_job_locked(
        state: &mut AgentRuntimeTestState,
        request: crate::session::reliability::CompleteRuntimeJobRequest,
    ) -> Result<(), String> {
        let job = state
            .jobs
            .get_mut(&request.job_id)
            .ok_or_else(|| format!("runtime job not found: {}", request.job_id))?;
        if job.status == RuntimeJobStatus::Succeeded {
            return Ok(());
        }
        if job.lease_owner.as_deref() != Some(request.lease_owner.as_str()) {
            return Err("runtime_job_lease_owner_mismatch".to_string());
        }
        job.status = RuntimeJobStatus::Succeeded;
        job.output_refs = request.output_refs;
        job.lease_owner = None;
        job.lease_expires_at_ms = None;
        job.heartbeat_at_ms = None;
        job.updated_at_ms = request.completed_at_ms;
        Ok(())
    }

    fn fail_job_locked(
        state: &mut AgentRuntimeTestState,
        request: crate::session::reliability::FailRuntimeJobRequest,
    ) -> Result<(), String> {
        let job = state
            .jobs
            .get_mut(&request.job_id)
            .ok_or_else(|| format!("runtime job not found: {}", request.job_id))?;
        if job.lease_owner.as_deref() != Some(request.lease_owner.as_str()) {
            return Err("runtime_job_lease_owner_mismatch".to_string());
        }
        job.last_error = Some(request.last_error);
        job.updated_at_ms = request.failed_at_ms;
        job.lease_owner = None;
        job.lease_expires_at_ms = None;
        job.heartbeat_at_ms = None;
        match request.disposition {
            crate::session::reliability::RuntimeJobFailureDisposition::RetryScheduled => {
                job.status = RuntimeJobStatus::Queued;
                job.retry_count = job.retry_count.saturating_add(1);
                job.run_at_ms = request
                    .next_run_at_ms
                    .ok_or_else(|| "runtime_job_retry_run_at_required".to_string())?;
            }
            crate::session::reliability::RuntimeJobFailureDisposition::Failed => {
                job.status = RuntimeJobStatus::Failed;
            }
            crate::session::reliability::RuntimeJobFailureDisposition::DeadLettered => {
                job.status = RuntimeJobStatus::DeadLettered;
            }
        }
        Ok(())
    }
}

impl RuntimeStore for AgentRuntimeTestStore {
    fn save_checkpoint(&self, checkpoint: CheckpointRecord) -> Result<(), RuntimeStoreError> {
        let mut state = self.state().map_err(RuntimeStoreError::backend)?;
        state.checkpoints.retain(|existing| {
            existing.checkpoint_id != checkpoint.checkpoint_id
                && !(existing.session_id == checkpoint.session_id
                    && existing.turn_id == checkpoint.turn_id
                    && existing.kind == checkpoint.kind)
        });
        state.checkpoints.push(checkpoint);
        Ok(())
    }

    fn load_latest_checkpoint(
        &self,
        session_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        Ok(self
            .state()
            .map_err(RuntimeStoreError::backend)?
            .checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.session_id == session_id)
            .max_by_key(|checkpoint| checkpoint.updated_at_ms)
            .cloned())
    }

    fn load_checkpoint_by_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<CheckpointRecord>, RuntimeStoreError> {
        Ok(self
            .state()
            .map_err(RuntimeStoreError::backend)?
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.session_id == session_id && checkpoint.turn_id == turn_id)
            .cloned())
    }

    fn list_checkpoints(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        let mut checkpoints = self
            .state()
            .map_err(RuntimeStoreError::backend)?
            .checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.session_id == session_id)
            .cloned()
            .collect::<Vec<_>>();
        checkpoints.sort_by_key(|checkpoint| std::cmp::Reverse(checkpoint.updated_at_ms));
        Ok(checkpoints.into_iter().skip(offset).take(limit).collect())
    }

    fn list_waiting_runtime_job_checkpoints(
        &self,
        after: Option<&RuntimeJobWaitCheckpointCursor>,
        limit: usize,
    ) -> Result<Vec<CheckpointRecord>, RuntimeStoreError> {
        let mut checkpoints = self
            .state()
            .map_err(RuntimeStoreError::backend)?
            .checkpoints
            .iter()
            .filter(|checkpoint| {
                checkpoint.kind == crate::runtime::contracts::CheckpointKindV1::Wait
                    && checkpoint.status == "waiting"
                    && checkpoint.done_reason.as_deref() == Some("runtime_job")
                    && after.is_none_or(|cursor| {
                        (checkpoint.session_id.as_str(), checkpoint.turn_id.as_str())
                            > (cursor.session_id.as_str(), cursor.turn_id.as_str())
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        checkpoints.sort_by(|left, right| {
            (&left.session_id, &left.turn_id).cmp(&(&right.session_id, &right.turn_id))
        });
        checkpoints.truncate(limit);
        Ok(checkpoints)
    }

    fn append_event(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError> {
        let mut state = self.state().map_err(RuntimeStoreError::backend)?;
        if state
            .events
            .iter()
            .any(|existing| existing.event_id == event.event_id)
        {
            return Err(RuntimeStoreError::backend("runtime event already exists"));
        }
        state.events.push(event);
        Ok(())
    }

    fn append_event_idempotent(&self, event: RuntimeEvent) -> Result<(), RuntimeStoreError> {
        let mut state = self.state().map_err(RuntimeStoreError::backend)?;
        if let Some(existing) = state
            .events
            .iter()
            .find(|existing| existing.event_id == event.event_id)
        {
            return if existing == &event {
                Ok(())
            } else {
                Err(RuntimeStoreError::backend(
                    "runtime event idempotency conflict",
                ))
            };
        }
        state.events.push(event);
        Ok(())
    }

    fn list_events(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<RuntimeEvent>, RuntimeStoreError> {
        Ok(self
            .state()
            .map_err(RuntimeStoreError::backend)?
            .events
            .iter()
            .filter(|event| event.session_id == session_id)
            .skip(offset)
            .take(limit)
            .cloned()
            .collect())
    }
}

impl AgentRuntimeSnapshotStorePort for AgentRuntimeTestStore {
    fn load_agent_runtime_snapshot(&self, session_id: &str) -> Result<Option<String>, String> {
        Ok(self.state()?.snapshots.get(session_id).cloned())
    }

    fn save_agent_runtime_snapshot(
        &self,
        session_id: &str,
        snapshot_json: &str,
        _updated_at_ms: i64,
    ) -> Result<(), String> {
        self.state()?
            .snapshots
            .insert(session_id.to_string(), snapshot_json.to_string());
        Ok(())
    }
}

impl crate::session::external_context::ExternalContextStorePort for AgentRuntimeTestStore {
    fn upsert_external_context_object(&self, object: ExternalContextObject) -> Result<(), String> {
        if object.object_id.trim().is_empty() {
            return Err("external context object_id is required".to_string());
        }
        self.state()?
            .external_objects
            .insert(object.object_id.clone(), object);
        Ok(())
    }

    fn load_external_context_object(
        &self,
        object_id: &str,
    ) -> Result<Option<ExternalContextObject>, String> {
        Ok(self.state()?.external_objects.get(object_id).cloned())
    }

    fn link_external_context_object(&self, link: ExternalContextObjectLink) -> Result<(), String> {
        let mut state = self.state()?;
        if !state.external_objects.contains_key(&link.object_id) {
            return Err(format!(
                "external context object not found for link: {}",
                link.object_id
            ));
        }
        state.external_links.retain(|existing| {
            !(existing.session_id == link.session_id
                && existing.object_id == link.object_id
                && existing.turn_id == link.turn_id
                && existing.tool_call_id == link.tool_call_id)
        });
        state.external_links.push(link);
        Ok(())
    }

    fn load_external_context_object_link(
        &self,
        session_id: &str,
        object_id: &str,
        turn_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<ExternalContextObjectLink>, String> {
        Ok(self
            .state()?
            .external_links
            .iter()
            .find(|link| {
                link.session_id == session_id
                    && link.object_id == object_id
                    && link.turn_id.as_deref().unwrap_or_default() == turn_id
                    && link.tool_call_id.as_deref().unwrap_or_default() == tool_call_id
            })
            .cloned())
    }

    fn list_external_context_objects(
        &self,
        request: crate::session::external_context::ListExternalContextObjectsRequest,
    ) -> Result<Vec<crate::session::external_context::ExternalContextObjectIndexEntry>, String>
    {
        let state = self.state()?;
        let mut entries = state
            .external_objects
            .values()
            .filter_map(|object| {
                let links = state
                    .external_links
                    .iter()
                    .filter(|link| {
                        link.object_id == object.object_id
                            && request
                                .session_id
                                .as_ref()
                                .is_none_or(|session_id| link.session_id == *session_id)
                    })
                    .collect::<Vec<_>>();
                if request.session_id.is_some() && links.is_empty() {
                    return None;
                }
                Some(
                    crate::session::external_context::ExternalContextObjectIndexEntry {
                        object_id: object.object_id.clone(),
                        object_kind: object.object_kind.clone(),
                        source_provider_id: object.source_provider_id.clone(),
                        source_tool_name: object.source_tool_name.clone(),
                        title: object.title.clone(),
                        updated_at_ms: object.updated_at_ms,
                        link_count: links.len(),
                        last_linked_at_ms: links.iter().map(|link| link.linked_at_ms).max(),
                    },
                )
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .last_linked_at_ms
                .unwrap_or(right.updated_at_ms)
                .cmp(&left.last_linked_at_ms.unwrap_or(left.updated_at_ms))
                .then_with(|| left.object_id.cmp(&right.object_id))
        });
        Ok(entries
            .into_iter()
            .skip(request.offset)
            .take(request.limit.clamp(1, 128))
            .collect())
    }
}

impl crate::session::reliability::RuntimeJobStorePort for AgentRuntimeTestStore {
    fn schedule_runtime_job(
        &self,
        request: crate::session::reliability::ScheduleRuntimeJobRequest,
    ) -> Result<crate::session::reliability::ScheduleRuntimeJobResult, String> {
        let mut state = self.state()?;
        Self::schedule_job_locked(&mut state, request.job)
    }

    fn get_runtime_job(&self, job_id: &str) -> Result<Option<RuntimeJobRecord>, String> {
        Ok(self.state()?.jobs.get(job_id).cloned())
    }

    fn list_runtime_jobs(
        &self,
        request: crate::session::reliability::ListRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        let mut jobs = self
            .state()?
            .jobs
            .values()
            .filter(|job| {
                (request.statuses.is_empty() || request.statuses.contains(&job.status))
                    && request
                        .job_kind
                        .as_ref()
                        .is_none_or(|job_kind| job.job_kind == *job_kind)
                    && request
                        .session_id
                        .as_ref()
                        .is_none_or(|session_id| job.session_id.as_ref() == Some(session_id))
                    && request
                        .branch_id
                        .as_ref()
                        .is_none_or(|branch_id| job.branch_id.as_ref() == Some(branch_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        jobs.sort_by(|left, right| {
            left.run_at_ms
                .cmp(&right.run_at_ms)
                .then_with(|| left.created_at_ms.cmp(&right.created_at_ms))
                .then_with(|| left.job_id.cmp(&right.job_id))
        });
        Ok(jobs
            .into_iter()
            .skip(request.offset)
            .take(request.limit)
            .collect())
    }

    fn claim_due_runtime_jobs(
        &self,
        request: crate::session::reliability::ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        let mut state = self.state()?;
        let mut job_ids = state
            .jobs
            .values()
            .filter(|job| {
                job.status == RuntimeJobStatus::Queued
                    && job.run_at_ms <= request.now_ms
                    && request
                        .job_id
                        .as_ref()
                        .is_none_or(|job_id| job.job_id == *job_id)
                    && request
                        .job_kind
                        .as_ref()
                        .is_none_or(|job_kind| job.job_kind == *job_kind)
                    && request
                        .session_id
                        .as_ref()
                        .is_none_or(|session_id| job.session_id.as_ref() == Some(session_id))
            })
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>();
        job_ids.sort_by_key(|job_id| {
            let job = &state.jobs[job_id];
            (job.run_at_ms, job.created_at_ms, job.job_id.clone())
        });
        job_ids.truncate(request.limit);
        let lease_ms = i64::try_from(request.lease_ms).unwrap_or(i64::MAX);
        Ok(job_ids
            .into_iter()
            .filter_map(|job_id| {
                let job = state.jobs.get_mut(&job_id)?;
                job.status = RuntimeJobStatus::Leased;
                job.lease_owner = Some(request.worker_id.clone());
                job.lease_expires_at_ms = Some(request.now_ms.saturating_add(lease_ms));
                job.heartbeat_at_ms = Some(request.now_ms);
                job.updated_at_ms = request.now_ms;
                Some(job.clone())
            })
            .collect())
    }

    fn start_runtime_job(
        &self,
        request: crate::session::reliability::StartRuntimeJobRequest,
    ) -> Result<(), String> {
        let mut state = self.state()?;
        let job = state
            .jobs
            .get_mut(&request.job_id)
            .ok_or_else(|| format!("runtime job not found: {}", request.job_id))?;
        if job.status != RuntimeJobStatus::Leased
            || job.lease_owner.as_deref() != Some(request.lease_owner.as_str())
        {
            return Err("runtime_job_start_lease_rejected".to_string());
        }
        job.status = RuntimeJobStatus::Running;
        job.updated_at_ms = request.started_at_ms;
        Ok(())
    }

    fn renew_runtime_job_lease(
        &self,
        request: crate::session::reliability::RenewRuntimeJobLeaseRequest,
    ) -> Result<(), String> {
        let mut state = self.state()?;
        let job = state
            .jobs
            .get_mut(&request.job_id)
            .ok_or_else(|| format!("runtime job not found: {}", request.job_id))?;
        if !matches!(
            job.status,
            RuntimeJobStatus::Leased | RuntimeJobStatus::Running
        ) || job.lease_owner.as_deref() != Some(request.lease_owner.as_str())
        {
            return Err("runtime_job_lease_renew_rejected".to_string());
        }
        job.heartbeat_at_ms = Some(request.heartbeat_at_ms);
        job.lease_expires_at_ms = Some(
            request
                .heartbeat_at_ms
                .saturating_add(i64::try_from(request.lease_ms).unwrap_or(i64::MAX)),
        );
        job.updated_at_ms = request.heartbeat_at_ms;
        Ok(())
    }

    fn yield_runtime_job(
        &self,
        request: crate::session::reliability::YieldRuntimeJobRequest,
    ) -> Result<(), String> {
        let mut state = self.state()?;
        let job = state
            .jobs
            .get_mut(&request.job_id)
            .ok_or_else(|| format!("runtime job not found: {}", request.job_id))?;
        if !matches!(
            job.status,
            RuntimeJobStatus::Leased | RuntimeJobStatus::Running
        ) || job.lease_owner.as_deref() != Some(request.lease_owner.as_str())
        {
            return Err("runtime_job_yield_lease_rejected".to_string());
        }
        job.status = RuntimeJobStatus::Queued;
        job.run_at_ms = request.run_at_ms;
        job.last_error = Some(request.transition_reason);
        job.lease_owner = None;
        job.lease_expires_at_ms = None;
        job.heartbeat_at_ms = None;
        job.updated_at_ms = request.yielded_at_ms;
        Ok(())
    }

    fn wake_runtime_job(
        &self,
        request: crate::session::reliability::WakeRuntimeJobRequest,
    ) -> Result<crate::session::reliability::WakeRuntimeJobDisposition, String> {
        let mut state = self.state()?;
        let job = state
            .jobs
            .get_mut(&request.job_id)
            .ok_or_else(|| format!("runtime job not found: {}", request.job_id))?;
        use crate::session::reliability::WakeRuntimeJobDisposition;
        match job.status {
            RuntimeJobStatus::Queued if job.run_at_ms <= request.woken_at_ms => {
                Ok(WakeRuntimeJobDisposition::AlreadyRunnable)
            }
            RuntimeJobStatus::Queued => {
                job.run_at_ms = request.woken_at_ms;
                job.updated_at_ms = request.woken_at_ms;
                job.last_error = Some(request.transition_reason);
                Ok(WakeRuntimeJobDisposition::Woken)
            }
            RuntimeJobStatus::Leased | RuntimeJobStatus::Running => {
                Ok(WakeRuntimeJobDisposition::Active)
            }
            _ => Ok(WakeRuntimeJobDisposition::Terminal),
        }
    }

    fn complete_runtime_job(
        &self,
        request: crate::session::reliability::CompleteRuntimeJobRequest,
    ) -> Result<(), String> {
        let mut state = self.state()?;
        Self::complete_job_locked(&mut state, request)
    }

    fn fail_runtime_job(
        &self,
        request: crate::session::reliability::FailRuntimeJobRequest,
    ) -> Result<(), String> {
        let mut state = self.state()?;
        Self::fail_job_locked(&mut state, request)
    }

    fn cancel_runtime_job(
        &self,
        request: crate::session::reliability::CancelRuntimeJobRequest,
    ) -> Result<(), String> {
        let mut state = self.state()?;
        let job = state
            .jobs
            .get_mut(&request.job_id)
            .ok_or_else(|| format!("runtime job not found: {}", request.job_id))?;
        if request
            .expected_status
            .as_ref()
            .is_some_and(|expected| expected != &job.status)
        {
            return Err("runtime_job_cancel_status_mismatch".to_string());
        }
        if !job.status.is_terminal() {
            job.status = RuntimeJobStatus::Cancelled;
            job.last_error = Some(request.reason);
            job.lease_owner = None;
            job.lease_expires_at_ms = None;
            job.heartbeat_at_ms = None;
            job.updated_at_ms = request.cancelled_at_ms;
        }
        Ok(())
    }

    fn reclaim_expired_runtime_job_leases(&self, now_ms: i64) -> Result<usize, String> {
        let mut state = self.state()?;
        let mut reclaimed = 0;
        for job in state.jobs.values_mut().filter(|job| {
            matches!(
                job.status,
                RuntimeJobStatus::Leased | RuntimeJobStatus::Running
            ) && job
                .lease_expires_at_ms
                .is_some_and(|expires| expires <= now_ms)
        }) {
            job.status = RuntimeJobStatus::Queued;
            job.lease_owner = None;
            job.lease_expires_at_ms = None;
            job.heartbeat_at_ms = None;
            job.run_at_ms = now_ms;
            job.updated_at_ms = now_ms;
            reclaimed += 1;
        }
        Ok(reclaimed)
    }
}

impl RuntimeStoreTransactionPort for AgentRuntimeTestStore {
    fn save_wait_checkpoint(&self, request: SaveWaitCheckpointRequest) -> Result<(), String> {
        let mut state = self.state()?;
        let mut transaction = state.clone();
        transaction.checkpoints.retain(|existing| {
            existing.checkpoint_id != request.checkpoint.checkpoint_id
                && !(existing.session_id == request.checkpoint.session_id
                    && existing.turn_id == request.checkpoint.turn_id
                    && existing.kind == request.checkpoint.kind)
        });
        transaction.checkpoints.push(request.checkpoint);
        if let Some(existing) = transaction
            .events
            .iter()
            .find(|existing| existing.event_id == request.event.event_id)
        {
            if existing != &request.event {
                return Err("runtime event idempotency conflict".to_string());
            }
        } else {
            transaction.events.push(request.event);
        }
        *state = transaction;
        Ok(())
    }

    fn consume_wait_checkpoint(&self, request: ConsumeWaitCheckpointRequest) -> Result<(), String> {
        let mut state = self.state()?;
        let mut transaction = state.clone();
        transaction.checkpoints.retain(|checkpoint| {
            !(checkpoint.session_id == request.checkpoint.session_id
                && checkpoint.turn_id == request.checkpoint.turn_id)
        });
        for event in request.events {
            if let Some(existing) = transaction
                .events
                .iter()
                .find(|existing| existing.event_id == event.event_id)
            {
                if existing != &event {
                    return Err("runtime event idempotency conflict".to_string());
                }
            } else {
                transaction.events.push(event);
            }
        }
        *state = transaction;
        Ok(())
    }

    fn upsert_external_context_and_schedule_job(
        &self,
        request: UpsertExternalContextAndScheduleJobRequest,
    ) -> Result<crate::session::reliability::ScheduleRuntimeJobResult, String> {
        let mut state = self.state()?;
        let mut transaction = state.clone();
        transaction
            .external_objects
            .insert(request.object.object_id.clone(), request.object);
        let result = Self::schedule_job_locked(&mut transaction, request.job)?;
        *state = transaction;
        Ok(result)
    }

    fn upsert_external_context_link_and_complete_job(
        &self,
        request: UpsertExternalContextLinkAndCompleteJobRequest,
    ) -> Result<(), String> {
        let mut state = self.state()?;
        let mut transaction = state.clone();
        if let Some(object) = request.object {
            transaction
                .external_objects
                .insert(object.object_id.clone(), object);
        }
        if let Some(link) = request.link {
            if !transaction.external_objects.contains_key(&link.object_id) {
                return Err(format!(
                    "external context object not found for link: {}",
                    link.object_id
                ));
            }
            transaction.external_links.retain(|existing| {
                !(existing.session_id == link.session_id
                    && existing.object_id == link.object_id
                    && existing.turn_id == link.turn_id
                    && existing.tool_call_id == link.tool_call_id)
            });
            transaction.external_links.push(link);
        }
        Self::complete_job_locked(&mut transaction, request.complete_job)?;
        *state = transaction;
        Ok(())
    }

    fn create_dead_letter_and_fail_job(
        &self,
        request: CreateDeadLetterAndFailJobRequest,
    ) -> Result<crate::session::reliability::CreateDeadLetterResult, String> {
        let mut state = self.state()?;
        let mut transaction = state.clone();
        let dead_letter = request.dead_letter.dead_letter;
        let disposition =
            if let Some(existing) = transaction.dead_letters.get(&dead_letter.dead_letter_id) {
                if existing != &dead_letter {
                    return Err("dead_letter_idempotency_conflict".to_string());
                }
                crate::session::reliability::CreateDeadLetterDisposition::Existing
            } else {
                transaction
                    .dead_letters
                    .insert(dead_letter.dead_letter_id.clone(), dead_letter.clone());
                crate::session::reliability::CreateDeadLetterDisposition::Inserted
            };
        Self::fail_job_locked(&mut transaction, request.fail_job)?;
        *state = transaction;
        Ok(crate::session::reliability::CreateDeadLetterResult {
            disposition,
            dead_letter,
        })
    }
}

impl TurnSupplementStorePort for AgentRuntimeTestStore {
    fn enqueue_turn_supplement(
        &self,
        _request: EnqueueTurnSupplementRequest,
    ) -> Result<EnqueueTurnSupplementResult, TurnSupplementStoreError> {
        panic!("unexpected durable turn supplement enqueue in AgentRuntime core test")
    }

    fn claim_turn_supplements(
        &self,
        _request: ClaimTurnSupplementsRequest,
    ) -> Result<Vec<DurableTurnSupplement>, TurnSupplementStoreError> {
        panic!("unexpected durable turn supplement claim in AgentRuntime core test")
    }

    fn acknowledge_turn_supplements(
        &self,
        _request: AcknowledgeTurnSupplementsRequest,
    ) -> Result<(), TurnSupplementStoreError> {
        panic!("unexpected durable turn supplement acknowledgement in AgentRuntime core test")
    }

    fn close_turn_supplement_queue(
        &self,
        _request: CloseTurnSupplementQueueRequest,
    ) -> Result<(), TurnSupplementStoreError> {
        panic!("unexpected durable turn supplement queue close in AgentRuntime core test")
    }
}

struct HostProcessTestRunner;

impl crate::execution::ExecutionHostRunner for HostProcessTestRunner {
    fn kind(&self) -> crate::execution::ExecutionHostKind {
        crate::execution::ExecutionHostKind::LocalProcess
    }

    fn status(
        &self,
        _policy: &crate::execution::sandbox::SandboxPolicy,
    ) -> Result<crate::execution::ExecutionHostStatus, crate::execution::sandbox::SandboxErr> {
        Ok(crate::execution::ExecutionHostStatus {
            kind: crate::execution::ExecutionHostKind::LocalProcess,
            sandbox_type: crate::execution::sandbox::SandboxType::HostProcess,
            health: crate::execution::ExecutionHostHealth::Ready,
            detail: None,
        })
    }

    fn run_file_system_operation(
        &self,
        request: crate::execution::ExecutionFileSystemRequest,
    ) -> Result<
        crate::execution::ExecutionFileSystemOutput,
        crate::execution::ExecutionFileSystemError,
    > {
        crate::execution::run_policy_scoped_execution_file_system_operation(request)
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        _request: crate::execution::sandbox::SandboxTransformRequest,
        _cancellation_probe: Option<&crate::execution::ExecutionCancellationProbe>,
    ) -> Result<crate::execution::ExecutionHostCommandOutput, crate::execution::sandbox::SandboxErr>
    {
        unreachable!("permission preview must not execute the host command")
    }
}

fn host_process_test_tool_layer(workspace_root: &Path) -> ToolLayer {
    let binding = std::sync::Arc::new(
        crate::execution::ExecutionHostBinding::new(
            crate::execution::ExecutionHostMode::Local,
            std::sync::Arc::new(HostProcessTestRunner),
            workspace_root.to_path_buf(),
            crate::execution::sandbox::SandboxPolicy::workspace_write_no_network(workspace_root),
        )
        .expect("host process test binding"),
    );
    ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
        SkillCatalogLoadConfig::default(),
        binding,
    )
    .expect("host process test tool layer")
}

fn empty_agent_composition_environment(
) -> crate::extension::composition::AgentCompositionEnvironmentV1 {
    let digest = crate::extension::composition::empty_composition_digest("test").expect("digest");
    crate::extension::composition::AgentCompositionEnvironmentV1 {
        tool_contracts: vec![],
        skill_catalog_digest: digest.clone(),
        plugin_activation_digest: digest.clone(),
        hook_composition_digest: digest.clone(),
        execution_profile_digest: digest,
        policy_version: "test.v1".to_string(),
        model_binding_override: None,
    }
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

fn explicit_workspace_skill_catalog_config(
    workspace_root: &Path,
    catalog_directory: &Path,
) -> SkillCatalogLoadConfig {
    let mut sources_config = SkillSourcesConfigV1::default();
    add_skill_source(
        &mut sources_config,
        SkillSourceAddRequest {
            scope: SkillSourceScopeV1::Workspace,
            kind: SkillSourceKindV1::CatalogDirectory,
            path: catalog_directory.to_string_lossy().to_string(),
            workspace_root: Some(workspace_root.to_string_lossy().to_string()),
        },
    )
    .expect("add explicit workspace skill source");
    SkillCatalogLoadConfig {
        cwd: Some(workspace_root.to_path_buf()),
        sources_config,
        max_skills: 16,
    }
}

fn mcp_lifecycle_test_tool_layer(execution_count: Arc<AtomicUsize>) -> ToolLayer {
    let registry =
        crate::tool::DynamicToolRegistry::from_contracts(vec![crate::tool::DynamicToolContract {
            name: "mcp_lifecycle_test".to_string(),
            category: "external.mcp".to_string(),
            summary: "MCP lifecycle test tool".to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: "mcp:test:source:2026-08-27".to_string(),
            scopes: Vec::new(),
            concurrency_safe: true,
            turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
        }])
        .expect("MCP lifecycle test registry");
    let mut tool_layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));
    tool_layer
        .register_dynamic_tool_provider(Arc::new(McpLifecycleTestProvider { execution_count }))
        .expect("MCP lifecycle provider binding");
    tool_layer
}

struct McpLifecycleTestProvider {
    execution_count: Arc<AtomicUsize>,
}

impl crate::tool::layer::DynamicToolProvider for McpLifecycleTestProvider {
    fn provider_id(&self) -> &str {
        "mcp:test:source:2026-08-27"
    }

    fn execute<'a>(
        &'a self,
        _request: crate::tool::layer::DynamicToolProviderRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::tool::layer::DynamicToolProviderResponse, String>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.execution_count.fetch_add(1, Ordering::SeqCst);
            Ok(crate::tool::layer::DynamicToolProviderResponse {
                content: "MCP result".to_string(),
                details: json!({"source": "mcp"}),
                is_error: false,
                facts: Vec::new(),
                transition_reason: Some("mcp_lifecycle_test".to_string()),
            })
        })
    }
}

#[derive(Debug, Clone)]
struct EventOutputHookRunner {
    outputs: Vec<(LifecycleHookEventNameV1, String)>,
}

fn exact_hook_result(raw: &str) -> String {
    let mut output = serde_json::from_str::<Value>(raw).expect("Hook output fixture");
    output
        .as_object_mut()
        .expect("Hook output fixture must be an object")
        .insert(
            "schema".to_string(),
            Value::String("lifecycle_hook_result_v1".to_string()),
        );
    output.to_string()
}

impl LifecycleHookRunner for EventOutputHookRunner {
    fn run_hook(
        &self,
        _handler: &LifecycleHookHandlerV1,
        event: &LifecycleHookEventV1,
    ) -> LifecycleHookCommandResultV1 {
        LifecycleHookCommandResultV1 {
            exit_code: Some(0),
            stdout: self
                .outputs
                .iter()
                .find(|(event_name, _)| *event_name == event.event)
                .map(|(_, output)| exact_hook_result(output))
                .unwrap_or_else(|| exact_hook_result("{}")),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            spawn_error: None,
        }
    }
}

#[derive(Debug)]
struct CapturingHookRunner {
    outputs: Vec<(LifecycleHookEventNameV1, String)>,
    events: Arc<Mutex<Vec<LifecycleHookEventV1>>>,
}

struct FailingHookAuditSink;

impl LifecycleHookAuditSink for FailingHookAuditSink {
    fn record_hook_runs(&self, _runs: &[LifecycleHookRunV1]) -> Result<(), String> {
        Err("audit unavailable".to_string())
    }
}

impl LifecycleHookRunner for CapturingHookRunner {
    fn run_hook(
        &self,
        _handler: &LifecycleHookHandlerV1,
        event: &LifecycleHookEventV1,
    ) -> LifecycleHookCommandResultV1 {
        self.events
            .lock()
            .expect("captured hook events lock")
            .push(event.clone());
        LifecycleHookCommandResultV1 {
            exit_code: Some(0),
            stdout: self
                .outputs
                .iter()
                .find(|(event_name, _)| *event_name == event.event)
                .map(|(_, output)| exact_hook_result(output))
                .unwrap_or_else(|| exact_hook_result("{}")),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            spawn_error: None,
        }
    }
}

#[derive(Debug)]
struct StaticModelSessionConfigStore {
    config: Option<ModelSessionConfig>,
}

#[derive(Debug)]
struct CompleteTurnTestModelClient {
    request_count: AtomicUsize,
    include_sibling: bool,
    sibling_first: bool,
}

#[derive(Debug)]
struct SubagentReadModelClient {
    request_count: AtomicUsize,
}

impl ModelClient for SubagentReadModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
            Ok(ModelClientResponse {
                generate_result: if request_index == 0 {
                    read_generate_result("call-subagent-read", "fixture.txt")
                } else {
                    GenerateResult {
                        content: "subagent read completed".to_string(),
                        tool_calls: vec![],
                        reasoning_content: None,
                        input_tokens: None,
                        total_tokens: None,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                    }
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

impl ModelClient for CompleteTurnTestModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
            let mut tool_calls = if request_index == 0 {
                vec![ToolCallEnvelope {
                    id: "call-complete-turn".to_string(),
                    name: "complete_turn_test_tool".to_string(),
                    args_json: json!({ "value": "done" }).to_string(),
                }]
            } else {
                vec![]
            };
            if self.include_sibling && request_index == 0 {
                let sibling = ToolCallEnvelope {
                    id: "call-sibling".to_string(),
                    name: "read".to_string(),
                    args_json: json!({ "path": "README.md" }).to_string(),
                };
                if self.sibling_first {
                    tool_calls.insert(0, sibling);
                } else {
                    tool_calls.push(sibling);
                }
            }
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if tool_calls.is_empty() {
                        "finished after failed tool".to_string()
                    } else {
                        String::new()
                    },
                    tool_calls,
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

struct CompleteTurnTestProvider {
    execution_count: Arc<AtomicUsize>,
    succeed: bool,
}

#[derive(Debug)]
struct StreamExecutionBoundaryModelClient {
    request_count: AtomicUsize,
    execution_count: Arc<AtomicUsize>,
    turn_control: Option<TurnControl>,
}

#[derive(Debug)]
struct NoToolSupplementBoundaryModelClient {
    request_count: AtomicUsize,
    turn_control: TurnControl,
    enqueue_supplement: bool,
}

#[derive(Debug)]
struct AnswerNowBoundaryModelClient {
    request_count: AtomicUsize,
    with_tool: bool,
    enqueue_intervention: bool,
    execution_count: Arc<AtomicUsize>,
    turn_control: TurnControl,
}

#[derive(Debug)]
struct RuntimeJobWaitModelClient {
    request_count: AtomicUsize,
    follow_up_tool_result_count: AtomicUsize,
    expected_tool_result_fragment: &'static str,
    expect_toolless_follow_up: bool,
}

impl ModelClient for RuntimeJobWaitModelClient {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
            let tool_calls = if request_index == 0 {
                vec![ToolCallEnvelope {
                    id: "call-runtime-wait".to_string(),
                    name: "runtime_wait_test_tool".to_string(),
                    args_json: json!({"query": "durable result"}).to_string(),
                }]
            } else {
                let tool_result_count = request
                    .prepared_prompt
                    .messages
                    .iter()
                    .filter(|message| {
                        message.role == crate::model::prepared_prompt::ModelMessageRoleV1::Tool
                    })
                    .count();
                self.follow_up_tool_result_count
                    .store(tool_result_count, Ordering::SeqCst);
                assert!(request
                    .prepared_prompt
                    .messages
                    .iter()
                    .any(|message| message.content.contains(self.expected_tool_result_fragment)));
                if self.expect_toolless_follow_up {
                    assert!(request.prepared_prompt.tool_definitions.is_empty());
                    assert_eq!(
                        request.prepared_prompt.tool_choice,
                        crate::tool::ModelToolChoice::None
                    );
                }
                Vec::new()
            };
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if tool_calls.is_empty() {
                        "finished from the durable background result".to_string()
                    } else {
                        String::new()
                    },
                    tool_calls,
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

impl AnswerNowBoundaryModelClient {
    fn response_for(&self, request_index: usize) -> ModelClientResponse {
        let tool_calls = if self.with_tool && request_index == 0 {
            vec![ToolCallEnvelope {
                id: "call-answer-now-boundary".to_string(),
                name: "stream_boundary_test_tool".to_string(),
                args_json: json!({"value": "finish before convergence"}).to_string(),
            }]
        } else {
            Vec::new()
        };
        ModelClientResponse {
            generate_result: GenerateResult {
                content: if tool_calls.is_empty() {
                    if request_index == 0 {
                        "natural final before intervention boundary".to_string()
                    } else {
                        "converged from committed evidence".to_string()
                    }
                } else {
                    String::new()
                },
                tool_calls,
                reasoning_content: None,
                input_tokens: None,
                total_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
            provider_request_id: None,
            provider_latency_ms: None,
            provider_attempts: 1,
        }
    }
}

impl ModelClient for AnswerNowBoundaryModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.response_for(request_index))
        })
    }

    fn generate_stream<'a>(
        &'a self,
        request: &'a ModelClientRequest,
        _sink: &'a mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
            if request_index == 0 && self.enqueue_intervention {
                assert_eq!(self.execution_count.load(Ordering::SeqCst), 0);
                assert_eq!(
                    self.turn_control
                        .enqueue_answer_now_with(
                            AgentRunInterventionV1::answer_now(
                                "intervention-answer-now",
                                "agent-run-answer-now"
                            ),
                            || Ok(())
                        )
                        .expect("enqueue answer now during provider response"),
                    AnswerNowEnqueueDisposition::Accepted
                );
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                assert_eq!(
                    self.execution_count.load(Ordering::SeqCst),
                    0,
                    "answer-now must not start tools before the provider completes"
                );
            } else if request_index > 0 {
                assert!(
                    self.with_tool,
                    "natural final must not trigger another request"
                );
                assert_eq!(self.execution_count.load(Ordering::SeqCst), 1);
                assert!(request.prepared_prompt.tool_definitions.is_empty());
                assert_eq!(
                    request.prepared_prompt.tool_choice,
                    crate::tool::ModelToolChoice::None
                );
                let message = request
                    .prepared_prompt
                    .messages
                    .last()
                    .expect("answer-now instruction");
                assert_eq!(
                    message.role,
                    crate::model::prepared_prompt::ModelMessageRoleV1::User
                );
                assert!(message.content.contains("停止新增研究或工具调用"));
                assert!(request.prepared_prompt.messages.iter().any(|message| {
                    message.role == crate::model::prepared_prompt::ModelMessageRoleV1::Tool
                }));
            }
            Ok(self.response_for(request_index))
        })
    }
}

#[derive(Debug)]
struct CompleteBatchModelClient {
    request_count: AtomicUsize,
    follow_up_tool_result_count: AtomicUsize,
}

impl ModelClient for CompleteBatchModelClient {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
            let tool_calls = if request_index == 0 {
                (0..4)
                    .map(|index| ToolCallEnvelope {
                        id: format!("call-batch-{index}"),
                        name: "batch_test_tool".to_string(),
                        args_json: json!({ "index": index }).to_string(),
                    })
                    .collect()
            } else {
                self.follow_up_tool_result_count.store(
                    request
                        .prepared_prompt
                        .messages
                        .iter()
                        .filter(|message| {
                            message.role == crate::model::prepared_prompt::ModelMessageRoleV1::Tool
                        })
                        .count(),
                    Ordering::SeqCst,
                );
                Vec::new()
            };
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if tool_calls.is_empty() {
                        "verified all four tool results".to_string()
                    } else {
                        "collect four independent facts".to_string()
                    },
                    tool_calls,
                    reasoning_content: Some("one complete response boundary".to_string()),
                    input_tokens: Some(64),
                    total_tokens: Some(80),
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

impl StreamExecutionBoundaryModelClient {
    fn response_for(&self, request_index: usize) -> ModelClientResponse {
        let tool_calls = if request_index == 0 {
            vec![ToolCallEnvelope {
                id: "call-stream-boundary".to_string(),
                name: "stream_boundary_test_tool".to_string(),
                args_json: json!({"value": "run after response"}).to_string(),
            }]
        } else {
            Vec::new()
        };
        ModelClientResponse {
            generate_result: GenerateResult {
                content: if tool_calls.is_empty() {
                    "finished after tool result".to_string()
                } else {
                    String::new()
                },
                tool_calls,
                reasoning_content: Some("complete the model response first".to_string()),
                input_tokens: None,
                total_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
            provider_request_id: None,
            provider_latency_ms: None,
            provider_attempts: 1,
        }
    }
}

impl ModelClient for StreamExecutionBoundaryModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.response_for(request_index))
        })
    }

    fn generate_stream<'a>(
        &'a self,
        request: &'a ModelClientRequest,
        sink: &'a mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
            if request_index == 0 {
                sink(ModelClientStreamEvent::ToolCallReady {
                    call_id: "call-stream-boundary".to_string(),
                    provider_item_id: Some("provider-item-stream-boundary".to_string()),
                    name: "stream_boundary_test_tool".to_string(),
                    args_json: json!({"value": "run after response"}).to_string(),
                    args_preview: "{\"value\":\"run after response\"}".to_string(),
                });
                if let Some(control) = self.turn_control.as_ref() {
                    control
                        .enqueue_supplement_with(
                            "Use the completed tool result, then change direction.".to_string(),
                            || Ok(()),
                        )
                        .expect("enqueue supplement during provider response");
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                assert_eq!(
                    self.execution_count.load(Ordering::SeqCst),
                    0,
                    "tool execution started before the model response completed"
                );
            } else if self.turn_control.is_some() {
                assert_eq!(
                    self.execution_count.load(Ordering::SeqCst),
                    1,
                    "supplement was consumed before the tool batch completed"
                );
                let messages = request.prepared_prompt.messages.as_slice();
                let supplement = messages.last().expect("supplement model message");
                assert_eq!(
                    supplement.role,
                    crate::model::prepared_prompt::ModelMessageRoleV1::User
                );
                assert_eq!(
                    supplement.content,
                    "Use the completed tool result, then change direction."
                );
                assert!(messages[..messages.len() - 1].iter().any(|message| {
                    message.role == crate::model::prepared_prompt::ModelMessageRoleV1::Tool
                }));
            }
            Ok(self.response_for(request_index))
        })
    }
}

impl NoToolSupplementBoundaryModelClient {
    fn response_for(&self, request: &ModelClientRequest) -> ModelClientResponse {
        let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
        if request_index == 0 && self.enqueue_supplement {
            self.turn_control
                .enqueue_supplement_with("Use the newer constraint.".to_string(), || Ok(()))
                .expect("enqueue supplement before provider response ends");
        } else if self.enqueue_supplement {
            assert_eq!(
                request
                    .prepared_prompt
                    .messages
                    .last()
                    .expect("supplement model message")
                    .content,
                "Use the newer constraint."
            );
        }
        ModelClientResponse {
            generate_result: GenerateResult {
                content: if request_index == 0 {
                    "obsolete final".to_string()
                } else {
                    "final with newer constraint".to_string()
                },
                tool_calls: Vec::new(),
                reasoning_content: None,
                input_tokens: None,
                total_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
            provider_request_id: None,
            provider_latency_ms: None,
            provider_attempts: 1,
        }
    }
}

impl ModelClient for NoToolSupplementBoundaryModelClient {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move { Ok(self.response_for(request)) })
    }

    fn generate_stream<'a>(
        &'a self,
        request: &'a ModelClientRequest,
        _sink: &'a mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move { Ok(self.response_for(request)) })
    }
}

impl crate::tool::layer::DynamicToolProvider for CompleteTurnTestProvider {
    fn provider_id(&self) -> &str {
        "test.complete_turn"
    }

    fn execute<'a>(
        &'a self,
        _request: crate::tool::layer::DynamicToolProviderRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::tool::layer::DynamicToolProviderResponse, String>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.execution_count.fetch_add(1, Ordering::SeqCst);
            if !self.succeed {
                return Err("complete-turn test tool failed".to_string());
            }
            Ok(crate::tool::layer::DynamicToolProviderResponse {
                content: "accepted".to_string(),
                details: json!({ "accepted": true }),
                is_error: false,
                facts: Vec::new(),
                transition_reason: Some("complete_turn_test_provider".to_string()),
            })
        })
    }
}

struct PendingRuntimeJobProvider {
    execution_count: Arc<AtomicUsize>,
}

impl crate::tool::layer::DynamicToolProvider for PendingRuntimeJobProvider {
    fn provider_id(&self) -> &str {
        "test.runtime_wait"
    }

    fn execute<'a>(
        &'a self,
        _request: crate::tool::layer::DynamicToolProviderRequest,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<crate::tool::layer::DynamicToolProviderResponse, String>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.execution_count.fetch_add(1, Ordering::SeqCst);
            Ok(crate::tool::layer::DynamicToolProviderResponse {
                content: "background work accepted".to_string(),
                details: json!({
                    "providerPolling": {
                        "status": "pending",
                        "pollKey": "runtime-wait-ticket",
                        "pollArgs": {"ticket": "runtime-wait-ticket"},
                        "nextPollAtMs": 0,
                        "leaseMs": 30_000,
                        "maxPollAttempts": 4
                    }
                }),
                is_error: false,
                facts: Vec::new(),
                transition_reason: Some("runtime_wait_test_pending".to_string()),
            })
        })
    }
}

fn complete_turn_test_tool_layer(execution_count: Arc<AtomicUsize>, succeed: bool) -> ToolLayer {
    let registry =
        crate::tool::DynamicToolRegistry::from_contracts(vec![crate::tool::DynamicToolContract {
            name: "complete_turn_test_tool".to_string(),
            category: "test".to_string(),
            summary: "Complete the current test turn.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            }),
            provider_id: "test.complete_turn".to_string(),
            scopes: vec![],
            concurrency_safe: false,
            turn_behavior: crate::tool::ToolTurnBehavior::CompleteTurnOnSuccess,
        }])
        .expect("complete-turn test registry");
    let mut tool_layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));
    tool_layer
        .register_dynamic_tool_provider(Arc::new(CompleteTurnTestProvider {
            execution_count,
            succeed,
        }))
        .expect("provider binding");
    tool_layer
}

fn stream_execution_boundary_tool_layer(execution_count: Arc<AtomicUsize>) -> ToolLayer {
    let registry =
        crate::tool::DynamicToolRegistry::from_contracts(vec![crate::tool::DynamicToolContract {
            name: "stream_boundary_test_tool".to_string(),
            category: "test".to_string(),
            summary: "Record execution after a complete model response.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
            provider_id: "test.complete_turn".to_string(),
            scopes: Vec::new(),
            concurrency_safe: true,
            turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
        }])
        .expect("stream boundary registry");
    let mut tool_layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));
    tool_layer
        .register_dynamic_tool_provider(Arc::new(CompleteTurnTestProvider {
            execution_count,
            succeed: true,
        }))
        .expect("provider binding");
    tool_layer
}

fn runtime_job_wait_tool_layer(execution_count: Arc<AtomicUsize>) -> ToolLayer {
    let registry =
        crate::tool::DynamicToolRegistry::from_contracts(vec![crate::tool::DynamicToolContract {
            name: "runtime_wait_test_tool".to_string(),
            category: "test".to_string(),
            summary: "Return one result through a durable Runtime Job.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": false
            }),
            provider_id: "test.runtime_wait".to_string(),
            scopes: Vec::new(),
            concurrency_safe: true,
            turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
        }])
        .expect("runtime wait registry");
    let mut tool_layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));
    tool_layer
        .register_dynamic_tool_provider(Arc::new(PendingRuntimeJobProvider { execution_count }))
        .expect("provider binding");
    tool_layer
}

fn link_runtime_wait_test_object(store: &AgentRuntimeTestStore, session_id: &str) {
    store
        .link_external_context_object(ExternalContextObjectLink {
            session_id: session_id.to_string(),
            turn_id: Some("turn-runtime-wait".to_string()),
            tool_call_id: Some("call-runtime-wait".to_string()),
            object_id: "runtime-wait-object".to_string(),
            source_provider_id: "test.runtime_wait".to_string(),
            source_tool_name: "runtime_wait_test_tool".to_string(),
            linked_at_ms: now_ms(),
        })
        .expect("link provider output object to the waiting call");
}

fn complete_runtime_wait_test_job(
    store: &AgentRuntimeTestStore,
    job_id: String,
    linked_session_id: &str,
) {
    let worker_id = "runtime-wait-test-worker".to_string();
    let claimed = store
        .claim_due_runtime_jobs(crate::session::reliability::ClaimDueRuntimeJobsRequest {
            now_ms: now_ms(),
            worker_id: worker_id.clone(),
            job_id: Some(job_id.clone()),
            job_kind: Some(PROVIDER_POLL_RUNTIME_JOB_KIND.to_string()),
            session_id: Some("chat-runtime-wait".to_string()),
            limit: 1,
            lease_ms: 30_000,
        })
        .expect("claim provider polling job");
    assert_eq!(claimed.len(), 1);
    store
        .upsert_external_context_object(ExternalContextObject {
            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
            object_id: "runtime-wait-object".to_string(),
            object_kind: "externalKnowledge".to_string(),
            source_provider_id: "test.runtime_wait".to_string(),
            source_tool_name: "runtime_wait_test_tool".to_string(),
            title: "Durable result".to_string(),
            content: "durable background evidence".to_string(),
            metadata: json!({"test": true}),
            updated_at_ms: now_ms(),
        })
        .expect("persist provider output object");
    link_runtime_wait_test_object(store, linked_session_id);
    store
        .complete_runtime_job(crate::session::reliability::CompleteRuntimeJobRequest {
            job_id,
            lease_owner: worker_id,
            output_refs: vec!["runtime-wait-object".to_string()],
            completed_at_ms: now_ms(),
        })
        .expect("complete provider polling job");
}

fn complete_batch_test_tool_layer(execution_count: Arc<AtomicUsize>) -> ToolLayer {
    let registry =
        crate::tool::DynamicToolRegistry::from_contracts(vec![crate::tool::DynamicToolContract {
            name: "batch_test_tool".to_string(),
            category: "test".to_string(),
            summary: "Execute one member of a complete test batch.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "index": { "type": "integer" } },
                "required": ["index"],
                "additionalProperties": false
            }),
            provider_id: "test.complete_turn".to_string(),
            scopes: Vec::new(),
            concurrency_safe: true,
            turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
        }])
        .expect("complete batch registry");
    let mut tool_layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(registry));
    tool_layer
        .register_dynamic_tool_provider(Arc::new(CompleteTurnTestProvider {
            execution_count,
            succeed: true,
        }))
        .expect("provider binding");
    tool_layer
}

#[derive(Debug)]
struct UnboundedToolLoopModelClient {
    request_count: AtomicUsize,
    tool_turns: usize,
    request_roles: Mutex<Vec<Vec<MessageRole>>>,
    request_tail_roles: Mutex<Vec<Option<MessageRole>>>,
}

impl ModelClient for UnboundedToolLoopModelClient {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
            self.request_roles.lock().expect("request roles lock").push(
                request
                    .prepared_prompt
                    .messages
                    .iter()
                    .filter(|message| {
                        message.role != crate::model::prepared_prompt::ModelMessageRoleV1::System
                    })
                    .map(|message| model_message_role_to_chat_role(&message.role))
                    .collect(),
            );
            self.request_tail_roles
                .lock()
                .expect("request tail roles lock")
                .push(
                    request
                        .prepared_prompt
                        .messages
                        .last()
                        .map(|message| model_message_role_to_chat_role(&message.role)),
                );
            let tool_calls = if request_index < self.tool_turns {
                vec![ToolCallEnvelope {
                    id: format!("call-unbounded-{request_index}"),
                    name: "stream_boundary_test_tool".to_string(),
                    args_json: json!({ "value": format!("turn-{request_index}") }).to_string(),
                }]
            } else {
                vec![]
            };
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: if tool_calls.is_empty() {
                        "natural final after long tool loop".to_string()
                    } else {
                        String::new()
                    },
                    tool_calls,
                    reasoning_content: None,
                    input_tokens: Some(40_000),
                    total_tokens: Some(40_100),
                    prompt_cache_hit_tokens: Some(38_000),
                    prompt_cache_miss_tokens: Some(2_000),
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

impl ModelSessionConfigStore for StaticModelSessionConfigStore {
    fn get_session_config(&self, _session_id: &str) -> Result<Option<ModelSessionConfig>, String> {
        Ok(self.config.clone())
    }
}

#[derive(Debug)]
struct PartialStreamFailureModelClient;

impl ModelClient for PartialStreamFailureModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async {
            Err(ModelClientError::new(
                ModelClientErrorKind::ProviderResponseInterrupted,
                "provider stopped generation at the output-token limit before producing a complete response",
                false,
            ))
        })
    }

    fn generate_stream<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
        sink: &'a mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        sink(ModelClientStreamEvent::Token {
            content: "partial response".to_string(),
        });
        Box::pin(async {
            Err(ModelClientError::new(
                ModelClientErrorKind::ProviderResponseInterrupted,
                "provider stopped generation at the output-token limit before producing a complete response",
                false,
            ))
        })
    }
}

#[derive(Debug)]
struct PendingStreamModelClient {
    dropped: Arc<AtomicBool>,
}

struct PendingStreamGuard(Arc<AtomicBool>);

impl Drop for PendingStreamGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl ModelClient for PendingStreamModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        let dropped = self.dropped.clone();
        Box::pin(async move {
            let _guard = PendingStreamGuard(dropped);
            std::future::pending().await
        })
    }
}

#[derive(Debug)]
struct OutputLimitRecoveryModelClient {
    request_count: AtomicUsize,
    succeed_at_request: Option<usize>,
    emit_truncated_tool_call: bool,
    emit_incomplete_tool_identity: bool,
    request_messages: Mutex<Vec<Vec<crate::model::prepared_prompt::ModelMessageV1>>>,
}

impl ModelClient for OutputLimitRecoveryModelClient {
    fn generate<'a>(
        &'a self,
        _request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        let emit_truncated_tool_call = self.emit_truncated_tool_call;
        let emit_incomplete_tool_identity = self.emit_incomplete_tool_identity;
        Box::pin(async move {
            Err(ModelClientError {
                kind: ModelClientErrorKind::ProviderResponseInterrupted,
                message: "provider stopped generation at the output-token limit".to_string(),
                retryable: false,
                provider_code: Some("incomplete_output_token_limit".to_string()),
                provider_attempts: 1,
                truncated_tool_calls: if emit_truncated_tool_call {
                    vec![crate::model::TruncatedToolCall {
                        call_id: (!emit_incomplete_tool_identity)
                            .then(|| "call-truncated".to_string()),
                        tool_name: Some("bash".to_string()),
                        args_bytes: 34,
                        args_sha256: "sha256:test".to_string(),
                    }]
                } else {
                    Vec::new()
                },
            })
        })
    }

    fn generate_stream<'a>(
        &'a self,
        request: &'a ModelClientRequest,
        sink: &'a mut (dyn FnMut(ModelClientStreamEvent) + Send),
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        let request_index = self.request_count.fetch_add(1, Ordering::SeqCst);
        let emit_truncated_tool_call = self.emit_truncated_tool_call;
        let emit_incomplete_tool_identity = self.emit_incomplete_tool_identity;
        self.request_messages
            .lock()
            .expect("recovery request messages lock")
            .push(request.prepared_prompt.messages.clone());
        if self.succeed_at_request == Some(request_index) {
            sink(ModelClientStreamEvent::Token {
                content: "done".to_string(),
            });
            sink(ModelClientStreamEvent::Done {
                finish_reason: Some("stop".to_string()),
            });
            return Box::pin(async {
                Ok(ModelClientResponse {
                    generate_result: GenerateResult {
                        content: "done".to_string(),
                        tool_calls: vec![],
                        reasoning_content: None,
                        input_tokens: Some(10),
                        total_tokens: Some(11),
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                    },
                    provider_request_id: None,
                    provider_latency_ms: None,
                    provider_attempts: 1,
                })
            });
        }
        sink(ModelClientStreamEvent::Token {
            content: format!("part-{request_index}"),
        });
        Box::pin(async move {
            Err(ModelClientError {
                kind: ModelClientErrorKind::ProviderResponseInterrupted,
                message: "provider stopped generation at the output-token limit".to_string(),
                retryable: false,
                provider_code: Some("incomplete_output_token_limit".to_string()),
                provider_attempts: 1,
                truncated_tool_calls: if emit_truncated_tool_call {
                    vec![crate::model::TruncatedToolCall {
                        call_id: (!emit_incomplete_tool_identity)
                            .then(|| "call-truncated".to_string()),
                        tool_name: Some("bash".to_string()),
                        args_bytes: 34,
                        args_sha256: "sha256:test".to_string(),
                    }]
                } else {
                    Vec::new()
                },
            })
        })
    }
}

#[tokio::test]
async fn failed_stream_retracts_tentative_content_before_runtime_error() {
    let model_client = PartialStreamFailureModelClient;
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let driver = ModelClientGenerateDriver::new(&model_client, &config_store);
    let request = GenerateDriverRequest {
        session_id: "chat-partial-stream".to_string(),
        turn_id: "turn-partial-stream".to_string(),
        loop_index: 0,
        provider_prompt_cache_key: None,
        provider_prompt_cache_retention: None,
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate: 1,
        prepared_prompt: crate::model::prepared_prompt::PreparedPromptV1::new(
            None,
            vec![crate::model::prepared_prompt::ModelMessageV1 {
                message_id: "message:turn-partial-stream:user".to_string(),
                role: crate::model::prepared_prompt::ModelMessageRoleV1::User,
                content: "continue".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
                reasoning_content: None,
            }],
            vec![],
            crate::tool::ModelToolChoice::None,
            8_192,
        )
        .expect("valid prepared prompt"),
        observations: vec![],
        live_content_prefix: String::new(),
    };
    let mut events = Vec::new();

    let error = driver
        .generate_next_with_sink_async(&request, &mut |event| events.push(event))
        .await
        .expect_err("incomplete response must fail");

    assert!(error.message.contains("retryable=false"));
    let token_index = events
        .iter()
        .position(|event| matches!(event, TurnUpdate::Token { content, .. } if content == "partial response"))
        .expect("tentative token");
    let clear_index = events
        .iter()
        .position(|event| matches!(event, TurnUpdate::ReplaceContent { content, .. } if content.is_empty()))
        .expect("tentative content rollback");
    let error_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                TurnUpdate::RuntimeError {
                    reason,
                    retryable: false,
                    ..
                } if reason == "provider_response_interrupted"
            )
        })
        .expect("non-retryable runtime error");
    assert!(token_index < clear_index && clear_index < error_index);
}

#[tokio::test]
async fn output_token_limit_continues_in_a_new_turn_and_preserves_partial_content() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());
    let model_client = OutputLimitRecoveryModelClient {
        request_count: AtomicUsize::new(0),
        succeed_at_request: Some(1),
        emit_truncated_tool_call: false,
        emit_incomplete_tool_identity: false,
        request_messages: Mutex::new(vec![]),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut events = Vec::new();

    let result = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_async(
            AgentRunRequest {
                session_id: "chat-output-token-recovery".to_string(),
                initial_turn_id: "turn-output-token-initial".to_string(),
                user_message: "finish the task".to_string(),
                agent_run_identity: None,
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| events.push(event),
            &|| Ok(None),
        )
        .await
        .expect("output-token recovery must finish");

    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(result.turn_responses.len(), 1);
    assert_eq!(result.stop, AgentRunStop::Finalized);
    assert_eq!(
        result.turn_responses[0]
            .session_snapshot
            .messages
            .last()
            .map(|message| message.content.as_str()),
        Some("part-0done")
    );
    assert_eq!(
        result.turn_responses[0]
            .agent_run_resource_usage
            .provider_attempts,
        2
    );
    assert!(events.iter().any(|event| matches!(
        event,
        TurnUpdate::Status {
            process_state: RuntimeProcessState::Recovering,
            ..
        }
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, TurnUpdate::RuntimeError { .. })));
    let request_turn_ids = events
        .iter()
        .filter_map(|event| match event {
            TurnUpdate::ModelRequestStart { turn_id, .. } => Some(turn_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(request_turn_ids.len(), 2);
    assert_eq!(request_turn_ids[0], "turn-output-token-initial");
    assert_ne!(request_turn_ids[0], request_turn_ids[1]);
    let first_done = events
        .iter()
        .position(|event| {
            matches!(event, TurnUpdate::ModelDone { turn_id, finish_reason: Some(reason), .. }
                if turn_id == request_turn_ids[0] && reason == "incomplete_output_token_limit")
        })
        .expect("initial capped request must close");
    let recovery_start = events
        .iter()
        .position(|event| {
            matches!(event, TurnUpdate::ModelRequestStart { turn_id, .. }
                if turn_id == request_turn_ids[1])
        })
        .expect("recovery request must start");
    assert!(first_done < recovery_start);
    let requests = model_client
        .request_messages
        .lock()
        .expect("recovery request messages lock");
    let recovery_messages = &requests[1];
    assert_eq!(
        recovery_messages[recovery_messages.len() - 2].role,
        crate::model::prepared_prompt::ModelMessageRoleV1::Assistant
    );
    assert_eq!(
        recovery_messages[recovery_messages.len() - 2].content,
        "part-0"
    );
    assert!(recovery_messages
        .last()
        .expect("recovery instruction")
        .content
        .contains("Resume directly"));
}

#[tokio::test]
async fn query_loop_recovery_surfaces_one_terminal_error_after_five_attempts() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());
    let model_client = OutputLimitRecoveryModelClient {
        request_count: AtomicUsize::new(0),
        succeed_at_request: None,
        emit_truncated_tool_call: false,
        emit_incomplete_tool_identity: false,
        request_messages: Mutex::new(vec![]),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut events = Vec::new();

    let error = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_async(
            AgentRunRequest {
                session_id: "chat-output-token-exhausted".to_string(),
                initial_turn_id: "turn-output-token-exhausted".to_string(),
                user_message: "finish the task".to_string(),
                agent_run_identity: None,
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| events.push(event),
            &|| Ok(None),
        )
        .await
        .expect_err("sixth capped response must terminate the run");

    assert!(error.contains("providerCode=incomplete_output_token_limit"));
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 6);
    let request_turn_ids = events
        .iter()
        .filter_map(|event| match event {
            TurnUpdate::ModelRequestStart { turn_id, .. } => Some(turn_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(request_turn_ids.len(), 6);
    assert_eq!(request_turn_ids[0], "turn-output-token-exhausted");
    assert_eq!(
        request_turn_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        6
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                TurnUpdate::ModelDone {
                    finish_reason: Some(reason),
                    ..
                } if reason == "incomplete_output_token_limit"
            ))
            .count(),
        6
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, TurnUpdate::RuntimeError { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn truncated_tool_identity_is_a_recovery_observation_without_execution() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());
    let model_client = OutputLimitRecoveryModelClient {
        request_count: AtomicUsize::new(0),
        succeed_at_request: Some(1),
        emit_truncated_tool_call: true,
        emit_incomplete_tool_identity: false,
        request_messages: Mutex::new(vec![]),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut events = Vec::new();
    let mut durable_tool_calls = 0;
    let mut durable_receipts = 0;
    let mut completed_turns = 0;

    let result = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            AgentRunRequest {
                session_id: "chat-truncated-tool-call".to_string(),
                initial_turn_id: "turn-truncated-tool-call".to_string(),
                user_message: "inspect the project".to_string(),
                agent_run_identity: None,
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| events.push(event),
            &|| Ok(None),
            &mut |safe_point| {
                match safe_point {
                    ToolSafePoint::DurableToolCall { .. } => durable_tool_calls += 1,
                    ToolSafePoint::DurableReceipt { .. } => durable_receipts += 1,
                    ToolSafePoint::CompletedTurn(_) => completed_turns += 1,
                    _ => {}
                }
                Ok(())
            },
        )
        .await
        .expect("truncated tool call must converge");

    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(result.turn_responses.len(), 1);
    assert!(result.turn_responses[0].tool_results.is_empty());
    assert_eq!(durable_tool_calls, 0);
    assert_eq!(durable_receipts, 0);
    assert_eq!(completed_turns, 0);
    assert!(!events
        .iter()
        .any(|event| matches!(event, TurnUpdate::ToolCallReady { .. })));
    assert_eq!(
        result.turn_responses[0]
            .session_snapshot
            .messages
            .last()
            .map(|message| message.content.as_str()),
        Some("part-0done")
    );
    assert!(!result.turn_responses[0]
        .session_snapshot
        .messages
        .iter()
        .any(|message| message.role == MessageRole::Tool));
    let requests = model_client
        .request_messages
        .lock()
        .expect("recovery request messages lock");
    let recovery_messages = &requests[1];
    let assistant_index = recovery_messages
        .iter()
        .position(|message| !message.tool_calls.is_empty())
        .expect("rejected call observation");
    assert_eq!(recovery_messages[assistant_index].content, "part-0");
    assert_eq!(recovery_messages[assistant_index].tool_calls.len(), 1);
    assert_eq!(
        recovery_messages[assistant_index].tool_calls[0].id,
        "call-truncated"
    );
    assert_eq!(
        recovery_messages[assistant_index].tool_calls[0].args_json,
        "{}"
    );
    let tool_result = &recovery_messages[assistant_index + 1];
    assert_eq!(
        tool_result.role,
        crate::model::prepared_prompt::ModelMessageRoleV1::Tool
    );
    assert_eq!(tool_result.tool_call_id.as_deref(), Some("call-truncated"));
    assert!(tool_result.content.contains("was not executed"));
    assert!(!requests[1]
        .iter()
        .any(|message| message.content.contains("Get-Content unfinished")));
    let cap_done_index = events
        .iter()
        .position(|event| matches!(event, TurnUpdate::ModelDone { finish_reason: Some(reason), .. } if reason == "incomplete_output_token_limit"))
        .expect("capped request boundary");
    let recovery_start_index = events
        .iter()
        .enumerate()
        .skip(cap_done_index + 1)
        .find_map(|(index, event)| matches!(event, TurnUpdate::ModelRequestStart { initial_content, .. } if initial_content == "part-0").then_some(index))
        .expect("recovery request start");
    assert!(cap_done_index < recovery_start_index);
}

#[tokio::test]
async fn incomplete_truncated_tool_identity_uses_protocol_recovery_without_fake_result() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());
    let model_client = OutputLimitRecoveryModelClient {
        request_count: AtomicUsize::new(0),
        succeed_at_request: Some(1),
        emit_truncated_tool_call: true,
        emit_incomplete_tool_identity: true,
        request_messages: Mutex::new(vec![]),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut events = Vec::new();

    let result = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_async(
            AgentRunRequest {
                session_id: "chat-incomplete-tool-identity".to_string(),
                initial_turn_id: "turn-incomplete-tool-identity".to_string(),
                user_message: "inspect the project".to_string(),
                agent_run_identity: None,
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| events.push(event),
            &|| Ok(None),
        )
        .await
        .expect("incomplete tool identity must recover");

    assert_eq!(result.turn_responses.len(), 1);
    assert!(result.turn_responses[0].tool_results.is_empty());
    assert!(!events
        .iter()
        .any(|event| matches!(event, TurnUpdate::ToolCallReady { .. })));
    let requests = model_client
        .request_messages
        .lock()
        .expect("recovery request messages lock");
    let recovery_messages = &requests[1];
    assert!(!recovery_messages
        .iter()
        .any(|message| !message.tool_calls.is_empty()
            || message.role == crate::model::prepared_prompt::ModelMessageRoleV1::Tool));
    assert!(recovery_messages
        .last()
        .expect("protocol recovery instruction")
        .content
        .contains("fresh valid identity"));
}

#[tokio::test]
async fn query_loop_preserves_dynamic_description_at_durable_model_request_boundary() {
    let description = "\n  Search canonical sources.\t\n";
    let registry =
        crate::tool::DynamicToolRegistry::from_contracts(vec![crate::tool::DynamicToolContract {
            name: "source_search".to_string(),
            category: "external.mcp".to_string(),
            summary: description.to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: "mcp:test:source".to_string(),
            scopes: vec![],
            concurrency_safe: true,
            turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
        }])
        .unwrap();
    let contract = registry.find_contract("source_search").unwrap();
    let expected_digest = contract.contract_digest().unwrap();
    let mut environment = empty_agent_composition_environment();
    environment.tool_contracts = registry.list_contracts();
    let prepared_prompt = crate::model::prepared_prompt::PreparedPromptV1::new(
        None,
        vec![crate::model::prepared_prompt::ModelMessageV1 {
            message_id: "message:description:user".to_string(),
            role: crate::model::prepared_prompt::ModelMessageRoleV1::User,
            content: "search".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        }],
        vec![crate::tool::ModelToolDefinition {
            name: contract.name.clone(),
            description: contract.summary,
            input_schema: contract.input_schema,
        }],
        crate::tool::ModelToolChoice::Auto,
        8192,
    )
    .unwrap();
    let mut observations = prepared_prompt
        .messages
        .iter()
        .cloned()
        .map(|message| ModelObservationV1::ContextMessage { message })
        .collect::<Vec<_>>();
    observations.push(ModelObservationV1::ToolCatalog {
        tool_definitions: prepared_prompt.tool_definitions.clone(),
    });
    let context_token_estimate = crate::model::prepared_prompt::estimate_text_tokens(
        &serde_json::to_string(&prepared_prompt.messages[0]).unwrap(),
    ) + crate::model::prepared_prompt::estimate_text_tokens(
        &serde_json::to_string(&prepared_prompt.tool_definitions).unwrap(),
    );
    let request = GenerateDriverRequest {
        session_id: "chat-description".to_string(),
        turn_id: "turn-description".to_string(),
        loop_index: 0,
        provider_prompt_cache_key: None,
        provider_prompt_cache_retention: None,
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate,
        observations,
        prepared_prompt,
        live_content_prefix: String::new(),
    };
    let model = CompleteTurnTestModelClient {
        request_count: AtomicUsize::new(0),
        include_sibling: false,
        sibling_first: false,
    };
    let config = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut committed = Vec::new();
    let mut sink = |safe_point| {
        if let ToolSafePoint::ModelRequestStarted(started) = safe_point {
            assert_eq!(model.request_count.load(Ordering::SeqCst), 0);
            committed.push(started);
        }
        Ok(())
    };
    let dispatcher = ToolSafePointDispatcher {
        sink: Mutex::new(&mut sink),
    };
    let driver = ModelClientGenerateDriver::new_with_tool_safe_point(
        &model,
        &config,
        &dispatcher,
        &environment,
    );
    driver
        .generate_next_async(&request)
        .await
        .expect("whitespace description reaches fake model");
    assert_eq!(model.request_count.load(Ordering::SeqCst), 1);
    let started = &committed[0];
    let records = canonical_model_request_started_records("task-description", started, 1).unwrap();
    let value = &records[0].payload;
    let composition = &value["agentComposition"];
    assert_eq!(composition["toolContracts"][0]["summary"], description);
    assert_eq!(
        composition["toolContracts"][0]["contractDigest"],
        expected_digest
    );
    assert_eq!(
        value["observations"][1]["toolDefinitions"][0]["description"],
        description
    );
}

#[tokio::test]
async fn query_loop_persistence_failure_prevents_provider_call() {
    let model_client = CompleteTurnTestModelClient {
        request_count: AtomicUsize::new(0),
        include_sibling: false,
        sibling_first: false,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut reject = |safe_point| match safe_point {
        ToolSafePoint::ModelRequestStarted(_) => Err("session log unavailable".to_string()),
        _ => Ok(()),
    };
    let safe_point = ToolSafePointDispatcher {
        sink: Mutex::new(&mut reject),
    };
    let composition_environment = empty_agent_composition_environment();
    let driver = ModelClientGenerateDriver::new_with_tool_safe_point(
        &model_client,
        &config_store,
        &safe_point,
        &composition_environment,
    );
    let prepared_prompt = crate::model::prepared_prompt::PreparedPromptV1::new(
        None,
        vec![crate::model::prepared_prompt::ModelMessageV1 {
            message_id: "message:turn-safe-point:user".to_string(),
            role: crate::model::prepared_prompt::ModelMessageRoleV1::User,
            content: "continue".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        }],
        vec![],
        crate::tool::ModelToolChoice::None,
        8_192,
    )
    .expect("valid prepared prompt");
    let context_token_estimate = crate::model::prepared_prompt::estimate_text_tokens(
        serde_json::to_string(&prepared_prompt.messages[0])
            .expect("serialize prompt message")
            .as_str(),
    );
    let request = GenerateDriverRequest {
        session_id: "chat-safe-point".to_string(),
        turn_id: "turn-safe-point".to_string(),
        loop_index: 0,
        provider_prompt_cache_key: None,
        provider_prompt_cache_retention: None,
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate,
        observations: prepared_prompt
            .messages
            .iter()
            .cloned()
            .map(|message| ModelObservationV1::ContextMessage { message })
            .collect(),
        prepared_prompt,
        live_content_prefix: String::new(),
    };

    let error = driver
        .generate_next_async(&request)
        .await
        .expect_err("durability failure must stop generation");

    assert_eq!(error.message, "session log unavailable");
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 0);
}

#[derive(Debug)]
enum PromptCompactionModelClientBehavior {
    ValidSummary,
    ProviderError,
}

#[derive(Debug)]
struct PromptCompactionModelClient {
    behavior: PromptCompactionModelClientBehavior,
    requests: Mutex<Vec<ModelClientRequest>>,
}

impl PromptCompactionModelClient {
    fn new(behavior: PromptCompactionModelClientBehavior) -> Self {
        Self {
            behavior,
            requests: Mutex::new(vec![]),
        }
    }

    fn requests(&self) -> Vec<ModelClientRequest> {
        self.requests
            .lock()
            .expect("model client requests lock")
            .clone()
    }
}

impl ModelClient for PromptCompactionModelClient {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            self.requests
                .lock()
                .expect("model client requests lock")
                .push(request.clone());
            match &self.behavior {
                PromptCompactionModelClientBehavior::ValidSummary => Ok(ModelClientResponse {
                    generate_result: GenerateResult {
                        content: test_model_compaction_summary_from_prompt(
                            request.prepared_prompt.messages[0].content.as_str(),
                        ),
                        tool_calls: vec![],
                        reasoning_content: None,
                        input_tokens: None,
                        total_tokens: None,
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                    },
                    provider_request_id: Some("provider-request-compact".to_string()),
                    provider_latency_ms: Some(12),
                    provider_attempts: 1,
                }),
                PromptCompactionModelClientBehavior::ProviderError => Err(ModelClientError::new(
                    ModelClientErrorKind::Provider,
                    "fake compaction provider failed",
                    true,
                )),
            }
        })
    }
}

#[derive(Debug)]
struct TestPromptCompactionAsyncDriver;

fn test_model_compaction_summary(request: &ModelCompactionSummaryCandidateRequest) -> String {
    assert!(request
        .prompt
        .contains("Return only a concise Markdown summary"));
    "# Goal\n\nContinue the active task after compaction.\n\n## Next Steps\n\nResume from the recent suffix.".to_string()
}

fn test_model_compaction_summary_from_prompt(prompt: &str) -> String {
    assert!(prompt.contains("Return only a concise Markdown summary"));
    "# Goal\n\nContinue the active task after compaction.".to_string()
}

impl ModelCompactionSummaryCandidateProducer for TestPromptCompactionAsyncDriver {
    fn produce_model_compaction_summary(
        &self,
        request: &ModelCompactionSummaryCandidateRequest,
    ) -> Result<String, PromptCompactionError> {
        Ok(test_model_compaction_summary(request))
    }
}
impl AsyncGenerateDriver for TestPromptCompactionAsyncDriver {
    fn generate_next_async<'a>(
        &'a self,
        _req: &'a GenerateDriverRequest,
    ) -> GenerateDriverFuture<'a, GenerateDriverOutcome> {
        Box::pin(async {
            Ok(GenerateDriverOutcome {
                generate_result: GenerateResult {
                    content: "test main response after model prompt compaction".to_string(),
                    tool_calls: vec![],
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_attempts: 1,
            })
        })
    }

    fn generate_prompt_compaction_summary_async<'a>(
        &'a self,
        request: &'a ModelCompactionSummaryCandidateRequest,
    ) -> GenerateDriverPromptCompactionFuture<'a> {
        Box::pin(async move {
            let generate_result = GenerateResult {
                content: String::new(),
                tool_calls: Vec::new(),
                reasoning_content: None,
                input_tokens: Some(i64::from(request.prompt_token_estimate)),
                total_tokens: Some(i64::from(request.prompt_token_estimate)),
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            };
            let mut resource_usage = AgentRunResourceUsageV1::default();
            resource_usage.record_completed_provider_round(
                &generate_result,
                request.prompt_token_estimate,
                1,
            );
            Some(GenerateDriverPromptCompactionOutcome {
                result: Ok(test_model_compaction_summary(request)),
                resource_usage,
            })
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum P7PromptCompactionModelBehavior {
    Valid,
    Empty,
    Oversized,
}

#[derive(Debug)]
struct P7PromptCompactionModelClient {
    behavior: P7PromptCompactionModelBehavior,
    requests: Mutex<Vec<ModelClientRequest>>,
}

impl P7PromptCompactionModelClient {
    fn new(behavior: P7PromptCompactionModelBehavior) -> Self {
        Self {
            behavior,
            requests: Mutex::new(vec![]),
        }
    }

    fn requests(&self) -> Vec<ModelClientRequest> {
        self.requests
            .lock()
            .expect("p7 model client requests lock")
            .clone()
    }

    fn compaction_summary_content(&self, prompt: &str) -> String {
        assert!(prompt.contains("Return only a concise Markdown summary"));
        match self.behavior {
            P7PromptCompactionModelBehavior::Valid =>
                "# Goal\n\nContinue the pre-release compaction work.\n\n## Next Steps\n\nVerify the recent suffix.".to_string(),
            P7PromptCompactionModelBehavior::Empty => "  ".to_string(),
            P7PromptCompactionModelBehavior::Oversized => "summary ".repeat(20_000),
        }
    }
}

impl ModelClient for P7PromptCompactionModelClient {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            self.requests
                .lock()
                .expect("p7 model client requests lock")
                .push(request.clone());
            if request.prepared_prompt.tool_choice == ModelToolChoice::None
                && request.prepared_prompt.messages.len() == 1
                && request.prepared_prompt.messages[0]
                    .content
                    .contains("[model_compaction_prompt_v1]")
            {
                return Ok(ModelClientResponse {
                    generate_result: GenerateResult {
                        content: self.compaction_summary_content(
                            request.prepared_prompt.messages[0].content.as_str(),
                        ),
                        tool_calls: vec![],
                        reasoning_content: None,
                        input_tokens: Some(i64::from(request.context_token_estimate)),
                        total_tokens: Some(i64::from(
                            request.context_token_estimate.saturating_add(64),
                        )),
                        prompt_cache_hit_tokens: None,
                        prompt_cache_miss_tokens: None,
                    },
                    provider_request_id: Some("p7-provider-request-compact".to_string()),
                    provider_latency_ms: Some(7),
                    provider_attempts: 1,
                });
            }

            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: "p7 main response after compaction".to_string(),
                    tool_calls: vec![],
                    reasoning_content: None,
                    input_tokens: Some(i64::from(request.context_token_estimate)),
                    total_tokens: Some(i64::from(
                        request.context_token_estimate.saturating_add(12),
                    )),
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: Some("p7-provider-request-main".to_string()),
                provider_latency_ms: Some(9),
                provider_attempts: 1,
            })
        })
    }
}

fn temp_dir_path(suffix: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "centaeris_agent_runtime_{suffix}_{}_{}",
        std::process::id(),
        nanos
    ))
}

fn model_tool_result_semantics(
    session: &SessionStateSnapshot,
    message: &ChatMessage,
) -> Option<Value> {
    if message.role != MessageRole::Tool {
        return None;
    }
    let ModelMessageSemanticsV1::ToolResult {
        tool_call_id,
        status,
        transition_reason,
        ..
    } = session.model_semantics.get(message.message_id.as_str())?
    else {
        return None;
    };
    Some(json!({
        "toolCallId": tool_call_id,
        "status": status,
        "transitionReason": transition_reason,
    }))
}

#[test]
fn agent_runtime_lifecycle_hooks_default_empty_and_injectable() {
    let store = AgentRuntimeTestStore::new();
    let default_engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());

    let empty_outcome = default_engine
        .run_user_prompt_submit_hook("chat-hooks", "hello")
        .expect("default lifecycle hook runtime should dispatch");
    assert!(!empty_outcome.blocked);
    assert!(empty_outcome.runs.is_empty());

    let hook_engine = LifecycleHookEngineV1::new(vec![LifecycleHookHandlerV1 {
        id: "project-user-prompt".to_string(),
        event: LifecycleHookEventNameV1::UserPromptSubmit,
        matcher: None,
        source: LifecycleHookSourceV1 {
            kind: LifecycleHookSourceKindV1::Project,
            name: "project".to_string(),
        },
        trusted: false,
        program: "unused".to_string(),
        args: vec![],
        cwd: None,
        timeout_ms: 1000,
    }])
    .expect("valid lifecycle hook engine");
    let injected_engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default())
        .with_lifecycle_hooks(QueryLifecycleHookRuntime::local(hook_engine));

    let injected_outcome = injected_engine
        .run_user_prompt_submit_hook("chat-hooks", "hello")
        .expect("injected lifecycle hook runtime should dispatch");
    assert_eq!(injected_outcome.runs.len(), 1);
    assert_eq!(
        injected_outcome.runs[0].status,
        LifecycleHookRunStatusV1::SkippedUntrusted
    );

    let projection = injected_engine
        .lifecycle_hook_diagnostics_projection()
        .expect("diagnostics should project");
    assert_eq!(projection.handlers.len(), 1);
    assert_eq!(projection.recent_runs.len(), 1);
}

#[test]
fn one_session_allows_only_one_active_root_agent_run() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());

    let guard = engine
        .acquire_active_agent_run("session-one-active-agent-run")
        .expect("first root AgentRun should acquire the session");
    let error = match engine.acquire_active_agent_run("session-one-active-agent-run") {
        Ok(_) => panic!("second root AgentRun for the same session must fail"),
        Err(error) => error,
    };
    assert!(error.contains("Session already has an in-flight AgentRun"));

    drop(guard);
    let replacement = engine
        .acquire_active_agent_run("session-one-active-agent-run")
        .expect("terminal root AgentRun should release the session");
    drop(replacement);
}

fn lifecycle_hook_runtime_for_events(
    events: &[LifecycleHookEventNameV1],
    outputs: Vec<(LifecycleHookEventNameV1, String)>,
) -> QueryLifecycleHookRuntime {
    let handlers = events
        .iter()
        .map(|event| LifecycleHookHandlerV1 {
            id: format!("hook-{event:?}"),
            event: *event,
            matcher: None,
            source: LifecycleHookSourceV1 {
                kind: LifecycleHookSourceKindV1::Project,
                name: "project".to_string(),
            },
            trusted: true,
            program: "unused".to_string(),
            args: vec![],
            cwd: None,
            timeout_ms: 1000,
        })
        .collect::<Vec<_>>();
    QueryLifecycleHookRuntime::new(
        LifecycleHookEngineV1::new(handlers).expect("valid lifecycle hook engine"),
        Arc::new(EventOutputHookRunner { outputs }),
        None,
    )
}

fn read_generate_result(call_id: &str, path: &str) -> GenerateResult {
    GenerateResult {
        content: String::new(),
        tool_calls: vec![ToolCallEnvelope {
            id: call_id.to_string(),
            name: "read".to_string(),
            args_json: json!({ "path": path }).to_string(),
        }],
        reasoning_content: None,
        input_tokens: None,
        total_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    }
}

fn hook_test_engine(
    workspace_suffix: &str,
    events: &[LifecycleHookEventNameV1],
    outputs: Vec<(LifecycleHookEventNameV1, String)>,
) -> (AgentRuntime<AgentRuntimeTestStore>, std::path::PathBuf) {
    let workspace_root = temp_dir_path(workspace_suffix);
    std::fs::create_dir_all(&workspace_root).expect("create workspace");
    let store = AgentRuntimeTestStore::new();
    let tool_layer = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("tool layer workspace root");
    let engine =
        AgentRuntime::new_for_test_with_tools(store, tool_layer, AgentRuntimeConfig::default())
            .with_lifecycle_hooks(lifecycle_hook_runtime_for_events(events, outputs));
    (engine, workspace_root)
}

#[tokio::test]
async fn user_prompt_submit_hook_blocks_before_model_input_is_persisted() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default())
        .with_lifecycle_hooks(lifecycle_hook_runtime_for_events(
            &[LifecycleHookEventNameV1::UserPromptSubmit],
            vec![(
                LifecycleHookEventNameV1::UserPromptSubmit,
                json!({ "blockReason": "prompt rejected" }).to_string(),
            )],
        ));

    let error = engine
        .process_turn_with_stream_sink_async(
            ProcessTurnRequest {
                session_id: "chat-user-hook-block".to_string(),
                agent_run_identity: None,
                turn_id: "turn-user-hook-block".to_string(),
                input: TurnInput::UserMessage("do blocked thing".to_string()),
                generate_result: GenerateResult {
                    content: "should not persist".to_string(),
                    tool_calls: vec![],
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            },
            None,
        )
        .await
        .expect_err("blocked user prompt hook should fail the turn");

    assert!(error.contains("UserPromptSubmit hook blocked turn"));
    let session = SessionManager::new(store)
        .load_session("chat-user-hook-block")
        .expect("load session")
        .unwrap_or_else(|| SessionStateSnapshot::new("chat-user-hook-block".to_string(), 1));
    assert!(session.messages.is_empty());
}

#[tokio::test]
async fn empty_final_response_fails_before_assistant_commit() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());

    let error = engine
        .process_turn_with_stream_sink_async(
            ProcessTurnRequest {
                session_id: "chat-empty-final".to_string(),
                agent_run_identity: None,
                turn_id: "turn-empty-final".to_string(),
                input: TurnInput::UserMessage("answer me".to_string()),
                generate_result: GenerateResult {
                    content: String::new(),
                    tool_calls: vec![],
                    reasoning_content: Some("unfinished analysis".to_string()),
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            },
            None,
        )
        .await
        .expect_err("empty final response must fail loudly");

    assert!(error.contains("empty final response without tool calls"));
    let session = SessionManager::new(store)
        .load_session("chat-empty-final")
        .expect("load session")
        .unwrap_or_else(|| SessionStateSnapshot::new("chat-empty-final".to_string(), 1));
    assert!(session
        .messages
        .iter()
        .all(|message| message.role != MessageRole::Assistant));
}

#[tokio::test]
async fn pre_tool_use_hook_blocks_before_tool_execution() {
    let (engine, workspace_root) = hook_test_engine(
        "pre_tool_hook_block_workspace",
        &[LifecycleHookEventNameV1::PreToolUse],
        vec![(
            LifecycleHookEventNameV1::PreToolUse,
            json!({ "blockReason": "read blocked" }).to_string(),
        )],
    );
    std::fs::write(workspace_root.join("note.txt"), "secret").expect("write test file");
    let session = SessionStateSnapshot::new("chat-pre-tool-hook-block".to_string(), 1);

    let batch = engine
        .execute_tool_calls_async(
            "chat-pre-tool-hook-block",
            "turn-pre-tool-hook-block",
            &session,
            read_generate_result("call-read-block", "note.txt"),
            None,
        )
        .await
        .expect("tool execution should return blocked report");

    assert_eq!(batch.tool_results.len(), 1);
    assert_eq!(batch.tool_results[0].status, "blocked");
    assert_eq!(
        batch.tool_results[0].transition_reason.as_deref(),
        Some("pre_tool_use_blocked")
    );
}

#[tokio::test]
async fn pre_tool_use_hook_updated_input_rewrites_current_tool_call() {
    let (engine, workspace_root) = hook_test_engine(
        "pre_tool_hook_update_workspace",
        &[LifecycleHookEventNameV1::PreToolUse],
        vec![(
            LifecycleHookEventNameV1::PreToolUse,
            json!({ "updatedInput": { "path": "note.txt" } }).to_string(),
        )],
    );
    std::fs::write(workspace_root.join("note.txt"), "rewritten").expect("write test file");
    let session = SessionStateSnapshot::new("chat-pre-tool-hook-update".to_string(), 1);

    let batch = engine
        .execute_tool_calls_async(
            "chat-pre-tool-hook-update",
            "turn-pre-tool-hook-update",
            &session,
            read_generate_result("call-read-update", "missing.txt"),
            None,
        )
        .await
        .expect("updated input should execute");

    assert_eq!(batch.tool_results.len(), 1);
    assert_eq!(batch.tool_results[0].status, "ok");
    assert!(batch.tool_results[0].content.contains("rewritten"));
}

#[tokio::test]
async fn permission_request_hook_can_deny_allowed_tool_call() {
    let (engine, workspace_root) = hook_test_engine(
        "permission_hook_deny_workspace",
        &[LifecycleHookEventNameV1::PermissionRequest],
        vec![(
            LifecycleHookEventNameV1::PermissionRequest,
            json!({ "permissionDecision": "deny" }).to_string(),
        )],
    );
    std::fs::write(workspace_root.join("note.txt"), "allowed but denied").expect("write test file");
    let session = SessionStateSnapshot::new("chat-permission-hook-deny".to_string(), 1);

    let batch = engine
        .execute_tool_calls_async(
            "chat-permission-hook-deny",
            "turn-permission-hook-deny",
            &session,
            read_generate_result("call-read-deny", "note.txt"),
            None,
        )
        .await
        .expect("permission hook deny should return blocked report");

    assert_eq!(batch.tool_results.len(), 1);
    assert_eq!(batch.tool_results[0].status, "blocked");
    assert_eq!(
        batch.tool_results[0].transition_reason.as_deref(),
        Some("permission_blocked")
    );
    assert_eq!(
        batch.tool_results[0]
            .details
            .get("reason")
            .and_then(Value::as_str),
        Some("lifecycle_hook_denied")
    );
}

#[tokio::test]
async fn mcp_dynamic_tool_uses_the_common_lifecycle_chain() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        mcp_lifecycle_test_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    )
    .with_lifecycle_hooks(lifecycle_hook_runtime_for_events(
        &[
            LifecycleHookEventNameV1::PreToolUse,
            LifecycleHookEventNameV1::PermissionRequest,
            LifecycleHookEventNameV1::PostToolUse,
        ],
        Vec::new(),
    ));
    let session = SessionStateSnapshot::new("chat-mcp-lifecycle".to_string(), 1);
    let batch = engine
        .execute_tool_calls_async(
            "chat-mcp-lifecycle",
            "turn-mcp-lifecycle",
            &session,
            GenerateResult {
                content: String::new(),
                tool_calls: vec![ToolCallEnvelope {
                    id: "call-mcp-lifecycle".to_string(),
                    name: "mcp_lifecycle_test".to_string(),
                    args_json: "{}".to_string(),
                }],
                reasoning_content: None,
                input_tokens: None,
                total_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
            None,
        )
        .await
        .expect("MCP lifecycle chain");

    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(batch.tool_results[0].content, "MCP result");
    assert_eq!(
        engine
            .lifecycle_hook_diagnostics_projection()
            .unwrap()
            .recent_runs
            .iter()
            .map(|run| run.event)
            .collect::<Vec<_>>(),
        [
            LifecycleHookEventNameV1::PreToolUse,
            LifecycleHookEventNameV1::PermissionRequest,
            LifecycleHookEventNameV1::PostToolUse,
        ]
    );
}

#[tokio::test]
async fn subagent_hook_audit_failure_does_not_change_the_child_run() {
    let store = AgentRuntimeTestStore::new();
    let hook_engine = LifecycleHookEngineV1::new(vec![LifecycleHookHandlerV1 {
        id: "subagent-observer".to_string(),
        event: LifecycleHookEventNameV1::SubagentStart,
        matcher: None,
        source: LifecycleHookSourceV1 {
            kind: LifecycleHookSourceKindV1::Plugin,
            name: "test".to_string(),
        },
        trusted: true,
        program: "unused".to_string(),
        args: Vec::new(),
        cwd: None,
        timeout_ms: 1_000,
    }])
    .unwrap();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default())
        .with_lifecycle_hooks(QueryLifecycleHookRuntime::new(
            hook_engine,
            Arc::new(EventOutputHookRunner {
                outputs: Vec::new(),
            }),
            Some(Arc::new(FailingHookAuditSink)),
        ));
    let observer = QueryLifecycleSubagentObserver::new(&engine);

    observer
        .on_subagent_start(SubagentLifecycleHookEvent {
            schema: "subagent_lifecycle_hook_v1".to_string(),
            phase: SubagentLifecycleHookPhase::Start,
            job_id: "job_1".to_string(),
            subagent_id: "researcher".to_string(),
            session_id: "session_1".to_string(),
            parent_turn_id: "turn_1".to_string(),
            work_packet_ref: "packet_1".to_string(),
            description: None,
            allowed_tools: Vec::new(),
            status: None,
            result_ref: None,
            output_refs: Vec::new(),
            error: None,
            started_at_ms: Some(1),
            finished_at_ms: None,
        })
        .await
        .expect("observer failure must not change child execution");
}

#[tokio::test]
async fn protected_root_recursive_delete_is_blocked_while_scoped_cleanup_executes() {
    let workspace_root = temp_dir_path("protected_root_recursive_delete_workspace");
    std::fs::create_dir_all(workspace_root.as_path()).expect("create protected workspace");
    let marker_path = workspace_root.join("must-remain.txt");
    std::fs::write(marker_path.as_path(), "keep").expect("write protected marker");
    let scoped_cleanup_path = workspace_root.join("build");
    std::fs::create_dir_all(scoped_cleanup_path.as_path()).expect("create scoped cleanup target");
    std::fs::write(scoped_cleanup_path.join("output.bin"), "remove")
        .expect("write scoped cleanup fixture");
    let store = AgentRuntimeTestStore::new();
    let tool_layer = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("configure protected workspace");
    let engine =
        AgentRuntime::new_for_test_with_tools(store, tool_layer, AgentRuntimeConfig::default());
    let mut session = SessionStateSnapshot::new("chat-protected-delete".to_string(), 1);
    let generate_result = GenerateResult {
        content: String::new(),
        tool_calls: vec![
            ToolCallEnvelope {
                id: "call-protected-delete".to_string(),
                name: "bash".to_string(),
                args_json: json!({ "command": "rm -rf ./*" }).to_string(),
            },
            ToolCallEnvelope {
                id: "call-scoped-delete".to_string(),
                name: "bash".to_string(),
                args_json: json!({ "command": "rm -rf build" }).to_string(),
            },
        ],
        reasoning_content: None,
        input_tokens: None,
        total_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };

    let preview =
        engine.build_tool_permission_preview(&session, generate_result.tool_calls.as_slice());
    let preview_decision = preview
        .get("call-protected-delete")
        .expect("protected delete permission preview");
    assert!(!preview_decision.allowed);
    assert_eq!(
        preview_decision.reason_type,
        "bash_recursive_delete_protected_root"
    );
    let scoped_preview = preview
        .get("call-scoped-delete")
        .expect("scoped delete permission preview");
    assert!(scoped_preview.allowed);

    let batch = engine
        .execute_tool_calls_async(
            "chat-protected-delete",
            "turn-protected-delete",
            &session,
            generate_result,
            None,
        )
        .await
        .expect("protected delete should return a normal blocked tool result");

    assert_eq!(batch.tool_results.len(), 2);
    assert_eq!(
        batch
            .tool_results
            .iter()
            .map(|report| report.tool_call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["call-protected-delete", "call-scoped-delete"]
    );
    let report = batch
        .tool_results
        .iter()
        .find(|report| report.tool_call_id == "call-protected-delete")
        .expect("blocked protected-root result");
    assert_eq!(report.tool_call_id, "call-protected-delete");
    assert_eq!(report.tool_name, "bash");
    assert_eq!(report.status, "blocked");
    assert_eq!(
        report.content,
        crate::tool::permission::PROTECTED_ROOT_RECURSIVE_DELETE_MESSAGE
    );
    assert_eq!(
        report.transition_reason.as_deref(),
        Some("permission_blocked")
    );
    assert_eq!(
        report.error.as_ref().map(|error| &error.kind),
        Some(&ToolFailureKind::PermissionDenied)
    );
    assert_eq!(report.details["schema"], "permission_tool_result_v1");
    assert_eq!(
        report.details["permissionDecision"]["reasonType"],
        "bash_recursive_delete_protected_root"
    );
    assert_eq!(report.details["permissionDecision"]["allowed"], false);
    assert_eq!(
        report.details["permissionDecision"]["normalizedInput"]["path"],
        "$CWD"
    );
    assert_eq!(
        std::fs::read_to_string(marker_path.as_path()).expect("protected marker remains"),
        "keep"
    );
    let scoped_report = batch
        .tool_results
        .iter()
        .find(|report| report.tool_call_id == "call-scoped-delete")
        .expect("scoped cleanup result");
    assert_eq!(scoped_report.status, "ok", "{scoped_report:#?}");
    assert!(
        !scoped_cleanup_path.exists(),
        "scoped cleanup must reach the real Bash execution path"
    );

    tool_context_writer::write_tool_results_to_context(
        &engine.message_handler,
        &mut session,
        batch.tool_results.as_slice(),
    )
    .expect("write blocked result to model context");
    let tool_message = session
        .messages
        .iter()
        .find(|message| {
            message.content == crate::tool::permission::PROTECTED_ROOT_RECURSIVE_DELETE_MESSAGE
        })
        .expect("blocked tool message");
    assert_eq!(tool_message.role, MessageRole::Tool);
    assert_eq!(
        tool_message.content,
        crate::tool::permission::PROTECTED_ROOT_RECURSIVE_DELETE_MESSAGE
    );
    assert!(!tool_message.content.contains("permissionDecision"));

    drop(engine);
    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn host_process_capability_allows_bash_without_claiming_a_sandbox() {
    let workspace_root = temp_dir_path("host_process_bash_workspace");
    std::fs::create_dir_all(workspace_root.as_path()).expect("create host process workspace");
    let engine = AgentRuntime::new_for_test_with_tools(
        AgentRuntimeTestStore::new(),
        host_process_test_tool_layer(workspace_root.as_path()),
        AgentRuntimeConfig::default(),
    );
    let session = SessionStateSnapshot::new("chat-host-process".to_string(), 1);
    let call = ToolCallEnvelope {
        id: "call-host-process".to_string(),
        name: "bash".to_string(),
        args_json: json!({ "command": "whoami" }).to_string(),
    };

    let preview = engine.build_tool_permission_preview(&session, std::slice::from_ref(&call));
    let decision = preview
        .get("call-host-process")
        .expect("host process decision");
    assert!(decision.allowed);
    assert_eq!(decision.reason_type, "static_tool_default_allow");
    assert_eq!(decision.policy_source, "core_tool_policy");
    assert_eq!(decision.risk_level, RiskLevel::HighRisk);

    drop(engine);
    let _ = std::fs::remove_dir_all(workspace_root);
}

#[tokio::test]
async fn post_tool_use_hook_additional_context_is_returned_by_execution_batch() {
    let (engine, workspace_root) = hook_test_engine(
        "post_tool_hook_context_workspace",
        &[LifecycleHookEventNameV1::PostToolUse],
        vec![(
            LifecycleHookEventNameV1::PostToolUse,
            json!({ "additionalContext": [{ "text": "post hook context" }] }).to_string(),
        )],
    );
    std::fs::write(workspace_root.join("note.txt"), "post context file").expect("write test file");
    let session = SessionStateSnapshot::new("chat-post-tool-hook-context".to_string(), 1);

    let batch = engine
        .execute_tool_calls_async(
            "chat-post-tool-hook-context",
            "turn-post-tool-hook-context",
            &session,
            read_generate_result("call-read-post", "note.txt"),
            None,
        )
        .await
        .expect("post hook context should be collected");

    assert_eq!(batch.tool_results[0].status, "ok");
    assert_eq!(batch.lifecycle_hook_contexts, vec!["post hook context"]);
}

#[tokio::test]
async fn post_tool_hook_failure_stops_after_the_durable_tool_receipt() {
    let workspace_root = temp_dir_path("post_tool_hook_failure_workspace");
    std::fs::create_dir_all(&workspace_root).expect("create workspace");
    std::fs::write(workspace_root.join("note.txt"), "durable result").expect("write fixture");
    let store = AgentRuntimeTestStore::new();
    let tool_layer = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("tool layer workspace root");
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        tool_layer,
        AgentRuntimeConfig::default(),
    )
    .with_lifecycle_hooks(lifecycle_hook_runtime_for_events(
        &[LifecycleHookEventNameV1::PostToolUse],
        vec![(
            LifecycleHookEventNameV1::PostToolUse,
            json!({ "blockReason": "too late" }).to_string(),
        )],
    ));
    let session = SessionStateSnapshot::new("chat-post-tool-failure".to_string(), 1);

    let error = engine
        .execute_tool_calls_async(
            "chat-post-tool-failure",
            "turn-post-tool-failure",
            &session,
            read_generate_result("call-read-post-failure", "note.txt"),
            None,
        )
        .await
        .expect_err("invalid PostToolUse output must stop the turn");

    assert!(error.contains("PostToolUse hook cannot block after tool execution"));
    assert_eq!(
        store
            .list_events("chat-post-tool-failure", 100, 0)
            .unwrap()
            .iter()
            .filter(|event| event.event_type == "tool_execution.receipt.v1")
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(workspace_root);
}

#[tokio::test]
async fn durable_tool_receipts_prevent_tool_and_post_hook_reexecution() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let captured_events = Arc::new(Mutex::new(Vec::new()));
    let hook_engine = LifecycleHookEngineV1::new(vec![LifecycleHookHandlerV1 {
        id: "hook-post-tool-receipt".to_string(),
        event: LifecycleHookEventNameV1::PostToolUse,
        matcher: None,
        source: LifecycleHookSourceV1 {
            kind: LifecycleHookSourceKindV1::Project,
            name: "project".to_string(),
        },
        trusted: true,
        program: "unused".to_string(),
        args: vec![],
        cwd: None,
        timeout_ms: 1_000,
    }])
    .expect("valid lifecycle hook engine");
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    )
    .with_lifecycle_hooks(QueryLifecycleHookRuntime::new(
        hook_engine,
        Arc::new(CapturingHookRunner {
            outputs: vec![(
                LifecycleHookEventNameV1::PostToolUse,
                json!({ "additionalContext": [{ "text": "receipt context" }] }).to_string(),
            )],
            events: captured_events.clone(),
        }),
        None,
    ));
    let session = SessionStateSnapshot::new("chat-tool-receipt".to_string(), 1);
    let result = || GenerateResult {
        content: String::new(),
        tool_calls: vec![ToolCallEnvelope {
            id: "call-tool-receipt".to_string(),
            name: "stream_boundary_test_tool".to_string(),
            args_json: json!({ "value": "once" }).to_string(),
        }],
        reasoning_content: None,
        input_tokens: None,
        total_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };

    let first = engine
        .execute_tool_calls_async(
            "chat-tool-receipt",
            "turn-tool-receipt",
            &session,
            result(),
            None,
        )
        .await
        .expect("first execution");
    let replay = engine
        .execute_tool_calls_async(
            "chat-tool-receipt",
            "turn-tool-receipt",
            &session,
            result(),
            None,
        )
        .await
        .expect("receipt replay");

    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        first.tool_results[0].content,
        replay.tool_results[0].content
    );
    assert_eq!(replay.lifecycle_hook_contexts, vec!["receipt context"]);
    assert_eq!(captured_events.lock().expect("captured events").len(), 1);
    let facts = store
        .list_events("chat-tool-receipt", 100, 0)
        .expect("list durable tool facts");
    for event_type in [
        "tool_execution.intent.v1",
        "tool_execution.receipt.v1",
        "post_tool_hook.intent.v1",
        "post_tool_hook.receipt.v1",
    ] {
        assert_eq!(
            facts
                .iter()
                .filter(|event| event.event_type == event_type)
                .count(),
            1,
            "{event_type} must be exact-once"
        );
    }
}

fn tool_execution_intent_fixture(
    tools: &ToolLayer,
    session_id: &str,
    turn_id: &str,
    call_id: &str,
    tool_name: &str,
    args_json: &str,
) -> Value {
    use sha2::{Digest, Sha256};

    let contract = tools.tool_contract(tool_name).expect("tool contract");
    let tool_contract_digest = contract.contract_digest().expect("contract digest");
    let args_digest = format!("sha256:{:x}", Sha256::digest(args_json.as_bytes()));
    json!({
        "schema": "tool_execution.intent.v1",
        "sessionId": session_id,
        "turnId": turn_id,
        "toolCallId": call_id,
        "sourceToolName": tool_name,
        "sessionToolCallEventId": canonical_tool_call_event_id(session_id, turn_id, call_id),
        "providerId": contract.provider_id.expect("providerId"),
        "toolContractDigest": tool_contract_digest,
        "modelArgsDigest": args_digest,
        "argsDigest": args_digest,
        "effectiveArgsJson": args_json,
        "recordedAtMs": 1
    })
}

#[tokio::test]
async fn intent_without_receipt_reports_indeterminate_without_reexecuting_tool() {
    use sha2::{Digest, Sha256};

    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let session_id = "chat-tool-indeterminate";
    let turn_id = "turn-tool-indeterminate";
    let call_id = "call-tool-indeterminate";
    let args_json = json!({ "value": "possibly executed" }).to_string();
    let event_identity = format!("{session_id}\0{turn_id}\0{call_id}");
    store
        .append_event_idempotent(RuntimeEvent {
            event_id: format!(
                "tool_execution.intent:sha256:{:x}",
                Sha256::digest(event_identity.as_bytes())
            ),
            session_id: session_id.to_string(),
            task_id: Some(turn_id.to_string()),
            event_type: "tool_execution.intent.v1".to_string(),
            at_ms: 1,
            visibility: EventVisibility::Internal,
            payload_json: tool_execution_intent_fixture(
                &engine.tools_port,
                session_id,
                turn_id,
                call_id,
                "stream_boundary_test_tool",
                args_json.as_str(),
            )
            .to_string(),
        })
        .expect("persist interrupted tool intent");

    let batch = engine
        .execute_tool_calls_async(
            session_id,
            turn_id,
            &SessionStateSnapshot::new(session_id.to_string(), 1),
            GenerateResult {
                content: String::new(),
                tool_calls: vec![ToolCallEnvelope {
                    id: call_id.to_string(),
                    name: "stream_boundary_test_tool".to_string(),
                    args_json,
                }],
                reasoning_content: None,
                input_tokens: None,
                total_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
            None,
        )
        .await
        .expect("recover interrupted tool intent");

    assert_eq!(execution_count.load(Ordering::SeqCst), 0);
    assert_eq!(batch.tool_results.len(), 1);
    assert_eq!(batch.tool_results[0].status, "error");
    assert_eq!(
        batch.tool_results[0]
            .details
            .get("schema")
            .and_then(Value::as_str),
        Some("tool_execution_indeterminate.v1")
    );
    assert_eq!(batch.tool_results[0].details["reexecuted"], false);
}

#[tokio::test]
async fn new_user_generate_preflight_pairs_interrupted_tool_intent_before_materialization() {
    use sha2::{Digest, Sha256};

    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let session_id = "chat-new-user-preflight-interrupted-tool";
    let source_turn_id = "turn-interrupted:27";
    let call_id = "call-interrupted-tool";
    let args_json = json!({ "value": "possibly executed" }).to_string();
    let call = ToolCallEnvelope {
        id: call_id.to_string(),
        name: "stream_boundary_test_tool".to_string(),
        args_json: args_json.clone(),
    };
    let mut session = SessionStateSnapshot::new(session_id.to_string(), 1);
    engine
        .message_handler
        .push_user_message(&mut session, "run the tool", JsonMap::new());
    engine.message_handler.push_model_assistant_message(
        &mut session,
        "Running the tool.",
        JsonMap::new(),
        build_model_assistant_semantics(&GenerateResult {
            content: "Running the tool.".to_string(),
            tool_calls: vec![call],
            reasoning_content: None,
            input_tokens: None,
            total_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }),
    );
    engine
        .session_manager
        .save_session(&session)
        .expect("save interrupted session");
    let event_identity = format!("{session_id}\0{source_turn_id}\0{call_id}");
    let mut intent_payload = tool_execution_intent_fixture(
        &engine.tools_port,
        session_id,
        source_turn_id,
        call_id,
        "stream_boundary_test_tool",
        args_json.as_str(),
    );
    intent_payload["agentRunIdentity"] = json!({
        "agentRunId": "agent-run-interrupted-tool",
        "executionId": "execution-interrupted-tool",
        "authorizationDigest": format!("sha256:{}", "0".repeat(64)),
    });
    store
        .append_event_idempotent(RuntimeEvent {
            event_id: format!(
                "tool_execution.intent:sha256:{:x}",
                Sha256::digest(event_identity.as_bytes())
            ),
            session_id: session_id.to_string(),
            task_id: Some(source_turn_id.to_string()),
            event_type: "tool_execution.intent.v1".to_string(),
            at_ms: 1,
            visibility: EventVisibility::Internal,
            payload_json: intent_payload.to_string(),
        })
        .expect("persist interrupted tool intent");

    let mut safe_points = Vec::new();
    let mut safe_point_sink = |safe_point| {
        safe_points.push(safe_point);
        Ok(())
    };
    let safe_point = ToolSafePointDispatcher {
        sink: Mutex::new(&mut safe_point_sink),
    };
    engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            session_id,
            "turn-continue",
            &TurnInput::UserMessage("continue".to_string()),
            0,
            PromptCompactionScopeV1::main(),
            None,
            Some(&safe_point),
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .expect("new user request should materialize after repairing the open tool call");

    assert_eq!(execution_count.load(Ordering::SeqCst), 0);
    let recovered = engine
        .session_manager
        .load_or_create_session(session_id)
        .expect("load recovered session");
    crate::runtime::context_window::validate_model_context_window(
        recovered.context_window.as_slice(),
        &recovered.model_semantics,
    )
    .expect("recovered context should have complete tool pairing");
    assert!(recovered.messages.iter().any(|message| {
        model_tool_result_semantics(&recovered, message).is_some_and(|trace| {
            trace.get("toolCallId").and_then(Value::as_str) == Some(call_id)
                && trace.get("status").and_then(Value::as_str) == Some("error")
                && message
                    .content
                    .contains("without a durable completion receipt")
        })
    }));
    assert_eq!(
        store
            .list_events(session_id, 100, 0)
            .expect("list recovery facts")
            .iter()
            .filter(|event| event.event_type == "tool_execution.receipt.v1")
            .count(),
        1
    );
    assert!(matches!(
        safe_points.as_slice(),
        [
            ToolSafePoint::DurableToolCall {
                turn_id,
                agent_run_id,
                ..
            },
            ToolSafePoint::DurableReceipt {
                turn_id: result_turn_id,
                agent_run_id: result_agent_run_id,
                result,
                ..
            }
        ] if turn_id == source_turn_id
            && result_turn_id == source_turn_id
            && agent_run_id == "agent-run-interrupted-tool"
            && result_agent_run_id == "agent-run-interrupted-tool"
            && result.transition_reason.as_deref()
                == Some("execution_cancellation_indeterminate")
    ));
}

#[tokio::test]
async fn corrupt_tool_receipt_source_identity_loud_fails() {
    use sha2::{Digest, Sha256};

    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(Arc::new(AtomicUsize::new(0))),
        AgentRuntimeConfig::default(),
    );
    let session_id = "chat-corrupt-tool-receipt";
    let turn_id = "turn-corrupt-tool-receipt";
    let call_id = "call-corrupt-tool-receipt";
    let args_json = json!({"value": "once"}).to_string();
    let args_digest = format!("sha256:{:x}", Sha256::digest(args_json.as_bytes()));
    let event_identity = format!("{session_id}\0{turn_id}\0{call_id}");
    let event_digest = format!("sha256:{:x}", Sha256::digest(event_identity.as_bytes()));
    let result = ToolExecutionResult {
        tool_call_id: call_id.to_string(),
        tool_name: "banana".to_string(),
        status: "ok".to_string(),
        content: "corrupt".to_string(),
        details: json!({}),
        facts: Vec::new(),
        error: None,
        started_at_ms: 1,
        completed_at_ms: 2,
        latency_ms: 1,
        parallel_group: None,
        transition_reason: None,
    };
    for event in [
        RuntimeEvent {
            event_id: format!("tool_execution.intent:{event_digest}"),
            session_id: session_id.to_string(),
            task_id: Some(turn_id.to_string()),
            event_type: "tool_execution.intent.v1".to_string(),
            at_ms: 1,
            visibility: EventVisibility::Internal,
            payload_json: tool_execution_intent_fixture(
                &engine.tools_port,
                session_id,
                turn_id,
                call_id,
                "stream_boundary_test_tool",
                args_json.as_str(),
            )
            .to_string(),
        },
        RuntimeEvent {
            event_id: format!("tool_execution.receipt:{event_digest}"),
            session_id: session_id.to_string(),
            task_id: Some(turn_id.to_string()),
            event_type: "tool_execution.receipt.v1".to_string(),
            at_ms: 2,
            visibility: EventVisibility::Internal,
            payload_json: json!({
                "schema": "tool_execution.receipt.v1",
                "sessionId": session_id,
                "turnId": turn_id,
                "toolCallId": call_id,
                "sourceToolName": "stream_boundary_test_tool",
                "argsDigest": args_digest,
                "effectiveArgsJson": args_json,
                "preHookContexts": [],
                "runPostHook": true,
                "resultJson": serde_json::to_string(&result).expect("serialize corrupt result"),
            })
            .to_string(),
        },
    ] {
        store
            .append_event_idempotent(event)
            .expect("persist corrupt tool receipt fixture");
    }

    let error = engine
        .execute_tool_calls_async(
            session_id,
            turn_id,
            &SessionStateSnapshot::new(session_id.to_string(), 1),
            GenerateResult {
                content: String::new(),
                tool_calls: vec![ToolCallEnvelope {
                    id: call_id.to_string(),
                    name: "stream_boundary_test_tool".to_string(),
                    args_json,
                }],
                reasoning_content: None,
                input_tokens: None,
                total_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
            None,
        )
        .await
        .expect_err("corrupt receipt identity must fail loudly");
    assert_eq!(error, "tool execution receipt identity mismatch");
}

fn tool_message_matches_transition(
    session: &SessionStateSnapshot,
    message: &ChatMessage,
    tool_call_id: &str,
    transition_reason: &str,
) -> bool {
    let Some(trace) = model_tool_result_semantics(session, message) else {
        return false;
    };
    trace.get("toolCallId").and_then(Value::as_str) == Some(tool_call_id)
        && trace.get("status").and_then(Value::as_str) == Some("blocked")
        && trace.get("transitionReason").and_then(Value::as_str) == Some(transition_reason)
}

#[test]
fn prompt_compaction_default_user_replay_is_comfort_bounded() {
    let config = AgentRuntimeConfig::default();

    assert_eq!(config.prompt_compaction_user_replay_tokens, 20_000);
}

#[test]
fn agent_tool_schedules_subagent_run_job_with_frozen_tool_contracts() {
    let workspace_root = std::env::temp_dir().join(format!(
        "centaeris_agent_tool_workspace_{}_{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
    let store = AgentRuntimeTestStore::new();
    let dynamic_registry =
        crate::tool::DynamicToolRegistry::from_contracts(vec![crate::tool::DynamicToolContract {
            name: "weather_lookup".to_string(),
            category: "weather.read".to_string(),
            summary: "Look up weather.".to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: "example.weather".to_string(),
            scopes: vec![],
            concurrency_safe: false,
            turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
    let tool_layer = ToolLayer::new_with_dynamic_tool_registry(Arc::new(dynamic_registry))
        .with_cwd(workspace_root.clone())
        .expect("workspace tool layer");
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        tool_layer,
        AgentRuntimeConfig::default(),
    );
    let session_id = "chat-agent-tool-schedule";
    let turn_id = "turn-agent-tool-schedule";
    let agent_run_identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "agent-run-agent-tool-schedule".to_string(),
        execution_id: "execution-agent-tool-schedule".to_string(),
        authorization_digest: format!("sha256:{}", "a".repeat(64)),
    };
    let report = engine.execute_agent_tool_call(
        session_id,
        turn_id,
        Some(&agent_run_identity),
        &ToolCallEnvelope {
            id: "call-agent-1".to_string(),
            name: "agent".to_string(),
            args_json: json!({
                "prompt": "Inspect the docs and return a one paragraph finding.",
                "description": "Docs inspection",
                "budget": { "max_summary_chars": 1200 }
            })
            .to_string(),
        },
    );

    assert_eq!(report.status, "ok");
    assert_eq!(
        report.transition_reason.as_deref(),
        Some("agent_tool_job_scheduled")
    );
    let output = &report.details;
    assert_eq!(
        output.get("schema").and_then(Value::as_str),
        Some("agent_tool_result_v1")
    );
    let work_packet_ref = output
        .get("workPacketRef")
        .and_then(Value::as_str)
        .expect("work packet ref");
    assert!(output
        .get("childSessionId")
        .and_then(Value::as_str)
        .is_some_and(|value| value.starts_with("session-agent-")));
    let child_turn_id = output
        .get("childTurnId")
        .and_then(Value::as_str)
        .expect("child turn id");
    assert!(child_turn_id.starts_with("turn-"));
    assert_ne!(child_turn_id, turn_id);
    assert_eq!(
        output
            .get("outputRef")
            .and_then(|value| value.get("runtimeJobId")),
        output.get("runtimeJobId")
    );
    let object = store
        .load_external_context_object(work_packet_ref)
        .expect("load work packet object")
        .expect("work packet object");
    assert_eq!(object.object_kind, "subagent_work_packet");
    assert_eq!(object.source_tool_name, "agent");
    let content = serde_json::from_str::<Value>(object.content.as_str()).expect("packet json");
    assert_eq!(
        content
            .get("workPacket")
            .and_then(|packet| packet.get("allowedTools"))
            .and_then(Value::as_array)
            .map(|tools| { tools.iter().filter_map(Value::as_str).collect::<Vec<_>>() }),
        Some(vec!["read", "bash", "edit", "write", "weather_lookup"])
    );
    let delegated_contracts = content["workPacket"]["delegatedToolContracts"]
        .as_array()
        .expect("delegated tool contracts");
    assert_eq!(delegated_contracts.len(), 5);
    assert!(delegated_contracts.iter().any(|contract| {
        contract.get("name").and_then(Value::as_str) == Some("weather_lookup")
            && contract.get("providerId").and_then(Value::as_str) == Some("example.weather")
            && contract.get("concurrencySafe").and_then(Value::as_bool) == Some(false)
            && contract
                .get("contractDigest")
                .and_then(Value::as_str)
                .is_some_and(|digest| digest.starts_with("sha256:"))
    }));
    assert_eq!(
        content["workPacket"]["run_context"]["turnId"],
        child_turn_id
    );
    assert_eq!(
        content["workPacket"]["run_context"]["parentTurnId"],
        turn_id
    );
    assert_eq!(
        content["workPacket"]["run_context"]["agentRunId"],
        output["runtimeJobId"]
    );
    let jobs = store
        .list_runtime_jobs(ListRuntimeJobsRequest {
            statuses: vec![RuntimeJobStatus::Queued],
            job_kind: Some(crate::runtime::subagent::SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some(session_id.to_string()),
            branch_id: Some(turn_id.to_string()),
            limit: 10,
            offset: 0,
        })
        .expect("list subagent jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].payload_ref.as_deref(), Some(work_packet_ref));

    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn task_output_validates_agent_ref_and_returns_runtime_wait_marker() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());
    let session_id = "chat-task-output-ref";
    let child_session_id = "session-agent-task-output-test";
    let result_ref =
        crate::runtime::keys::external_context::subagent_result_ref("task-output-test");
    let work_packet_ref =
        crate::runtime::keys::external_context::subagent_work_packet_ref("task-output-test");
    let job = build_subagent_run_job(SubagentRunJobRequest {
        session_id: session_id.to_string(),
        parent_turn_id: "turn-task-output-ref".to_string(),
        tool_call_id: "call-agent-task-output-ref".to_string(),
        subagent_id: "agent-task-output-test".to_string(),
        work_packet_ref: work_packet_ref.clone(),
        checkpoint_id: None,
        run_at_ms: now_ms(),
        created_at_ms: now_ms(),
        max_retries: 0,
    });
    store
        .upsert_external_context_and_schedule_job(UpsertExternalContextAndScheduleJobRequest {
            object: ExternalContextObject {
                schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
                object_id: work_packet_ref,
                object_kind: "subagent_work_packet".to_string(),
                source_provider_id: "centaeris.core".to_string(),
                source_tool_name: "agent".to_string(),
                title: "Agent work packet".to_string(),
                content: "{}".to_string(),
                metadata: json!({
                    "runtimeJobId": job.job_id,
                    "childSessionId": child_session_id,
                    "resultRef": result_ref,
                }),
                updated_at_ms: now_ms(),
            },
            job: job.clone(),
        })
        .expect("schedule Agent runtime job");

    let report = engine.execute_task_runtime_tool_call(
        session_id,
        &ToolCallEnvelope {
            id: "call-task-output-ref".to_string(),
            name: "task_output".to_string(),
            args_json: json!({
                "output_ref": {
                    "schema": "task_output_ref_v1",
                    "kind": "agent",
                    "runtime_job_id": job.job_id,
                    "child_session_id": child_session_id,
                    "result_ref": result_ref,
                }
            })
            .to_string(),
        },
    );

    assert_eq!(report.status, "ok");
    let output = &report.details;
    assert_eq!(
        output.get("schema").and_then(Value::as_str),
        Some("agent_task_output_wait_v1")
    );
    assert_eq!(
        output
            .get("outputRef")
            .and_then(|value| value.get("runtimeJobId"))
            .and_then(Value::as_str),
        Some(job.job_id.as_str())
    );
}

#[test]
fn task_output_terminal_join_returns_only_the_bound_agent_result() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());
    let session_id = "chat-task-output-terminal";
    let parent_turn_id = "turn-task-output-terminal";
    let child_session_id = "session-agent-task-output-terminal";
    let subagent_id = "agent-task-output-terminal";
    let parent = AgentRunContext::root(
        session_id,
        parent_turn_id,
        parent_turn_id,
        "agent-run-task-output-terminal",
        "main-agent",
        std::env::temp_dir().to_string_lossy(),
        1_000,
    );
    let mut packet = SubAgentWorkPacket::new(
        AgentRunContext::child(
            &parent,
            child_session_id,
            "turn-child-task-output-terminal",
            "agent-run-child-task-output-terminal",
            subagent_id,
            1_000,
        ),
        TaskBrief {
            task_id: Some(subagent_id.to_string()),
            objective: "Inspect the durable Agent result.".to_string(),
            success_criteria: vec!["Return a bounded result.".to_string()],
            constraints: vec![],
            output_hint: Some("Durable Agent result".to_string()),
        },
        HotView::default(),
        OutputContract {
            response_mode: "bounded_agent_result".to_string(),
            expected_sections: vec!["summary".to_string()],
            require_artifact_refs: false,
            max_summary_chars: Some(4_000),
        },
        ContextTransferMode::Borrow,
    );
    packet.allowed_tools = vec!["read".to_string()];
    packet.delegated_tool_contracts =
        test_delegated_tool_contracts(packet.allowed_tools.as_slice());
    let work_packet_ref =
        crate::runtime::keys::external_context::subagent_work_packet_ref(subagent_id);
    let job = build_subagent_run_job(SubagentRunJobRequest {
        session_id: session_id.to_string(),
        parent_turn_id: parent_turn_id.to_string(),
        tool_call_id: "call-agent-task-output-terminal".to_string(),
        subagent_id: subagent_id.to_string(),
        work_packet_ref: work_packet_ref.clone(),
        checkpoint_id: None,
        run_at_ms: 1_000,
        created_at_ms: 1_000,
        max_retries: 0,
    });
    store
        .upsert_external_context_and_schedule_job(UpsertExternalContextAndScheduleJobRequest {
            object: ExternalContextObject {
                schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
                object_id: work_packet_ref,
                object_kind: "subagent_work_packet".to_string(),
                source_provider_id: "centaeris.core".to_string(),
                source_tool_name: "agent".to_string(),
                title: "Agent work packet".to_string(),
                content: json!({ "workPacket": packet }).to_string(),
                metadata: json!({}),
                updated_at_ms: 1_000,
            },
            job: job.clone(),
        })
        .expect("schedule Agent runtime job");
    let claimed = store
        .claim_due_runtime_jobs(crate::session::reliability::ClaimDueRuntimeJobsRequest {
            now_ms: 1_001,
            worker_id: "worker-task-output-terminal".to_string(),
            job_id: Some(job.job_id.clone()),
            job_kind: Some(crate::runtime::subagent::SUBAGENT_RUN_JOB_KIND.to_string()),
            session_id: Some(session_id.to_string()),
            limit: 1,
            lease_ms: 5_000,
        })
        .expect("claim Agent runtime job")
        .pop()
        .expect("claimed Agent runtime job");
    store
        .start_runtime_job(crate::session::reliability::StartRuntimeJobRequest {
            job_id: job.job_id.clone(),
            lease_owner: claimed.lease_owner.clone().expect("lease owner"),
            started_at_ms: 1_002,
        })
        .expect("start Agent runtime job");
    let result_ref =
        crate::runtime::keys::external_context::subagent_result_ref(job.job_id.as_str());
    store
        .upsert_external_context_link_and_complete_job(
            crate::session::store::UpsertExternalContextLinkAndCompleteJobRequest {
                object: Some(ExternalContextObject {
                    schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
                    object_id: result_ref.clone(),
                    object_kind: "subagent_result".to_string(),
                    source_provider_id: "centaeris.core".to_string(),
                    source_tool_name: "agent".to_string(),
                    title: "Agent result".to_string(),
                    content: "bounded Agent evidence".to_string(),
                    metadata: json!({
                        "schema": "subagent_result_v1",
                        "runtimeJobId": job.job_id,
                        "parentSessionId": session_id,
                        "parentTurnId": parent_turn_id,
                        "subagentId": subagent_id,
                        "childSessionId": child_session_id,
                    }),
                    updated_at_ms: 1_100,
                }),
                link: None,
                complete_job: crate::session::reliability::CompleteRuntimeJobRequest {
                    job_id: job.job_id.clone(),
                    lease_owner: claimed.lease_owner.expect("lease owner"),
                    output_refs: vec![result_ref],
                    completed_at_ms: 1_100,
                },
            },
        )
        .expect("complete Agent result atomically");
    let completed = store
        .get_runtime_job(job.job_id.as_str())
        .expect("load Agent runtime job")
        .expect("Agent runtime job");
    let result = engine
        .runtime_job_terminal_tool_result(
            session_id,
            parent_turn_id,
            &RuntimeJobWaitV1 {
                tool_call_id: "call-task-output-terminal".to_string(),
                source_tool_name: "task_output".to_string(),
                tool_definition_digest: format!("sha256:{}", "a".repeat(64)),
                job_id: job.job_id,
                job_kind: crate::runtime::subagent::SUBAGENT_RUN_JOB_KIND.to_string(),
            },
            &completed,
        )
        .expect("join terminal Agent result");

    assert_eq!(result.status, "ok");
    assert_eq!(result.content, "bounded Agent evidence");
    assert_eq!(
        result.details["externalObjects"].as_array().map(Vec::len),
        Some(1)
    );
}

#[test]
fn task_output_validates_canonical_output_ref_shape() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());
    let report = engine.execute_task_runtime_tool_call(
        "chat-parent",
        &ToolCallEnvelope {
            id: "call-task-output-checkpoint".to_string(),
            name: "task_output".to_string(),
            args_json: json!({
                "output_ref": {
                    "kind": "agent",
                    "banana": "unknown-value",
                    "child_session_id": "chat-parent:subagent:agent-test"
                }
            })
            .to_string(),
        },
    );

    assert_eq!(report.status, "error");
    assert!(report.content.contains("unknown field"));
}

#[tokio::test]
async fn new_user_turn_closes_unpaired_tool_calls_in_assistant_order() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());
    let session_id = "chat-unpaired-ordered-tombstones";
    let generate_result = GenerateResult {
        content: "Need several tools.".to_string(),
        tool_calls: vec![
            ToolCallEnvelope {
                id: "call-first".to_string(),
                name: "bash".to_string(),
                args_json: json!({ "command": "apt list --installed" }).to_string(),
            },
            ToolCallEnvelope {
                id: "call-second".to_string(),
                name: "bash".to_string(),
                args_json: json!({ "command": "echo second" }).to_string(),
            },
            ToolCallEnvelope {
                id: "call-third".to_string(),
                name: "bash".to_string(),
                args_json: json!({ "command": "echo third" }).to_string(),
            },
        ],
        reasoning_content: None,
        input_tokens: None,
        total_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };
    let mut session = SessionStateSnapshot::new(session_id.to_string(), 1);
    engine
        .message_handler
        .push_user_message(&mut session, "inspect dependencies", JsonMap::new());
    engine.message_handler.push_model_assistant_message(
        &mut session,
        "Need several tools.",
        JsonMap::new(),
        build_model_assistant_semantics(&generate_result),
    );
    engine
        .session_manager
        .save_session(&session)
        .expect("save unpaired session");

    let response = engine
        .process_turn_with_stream_sink_async(
            ProcessTurnRequest {
                session_id: session_id.to_string(),
                agent_run_identity: None,
                turn_id: "turn-new-user".to_string(),
                input: TurnInput::UserMessage("start a new request".to_string()),
                generate_result: GenerateResult {
                    content: "Recovered from stale tool batch.".to_string(),
                    tool_calls: vec![],
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            },
            None,
        )
        .await
        .expect("new user turn should close stale tools in order");

    crate::runtime::context_window::validate_model_context_window(
        response.session_snapshot.context_window.as_slice(),
        &response.session_snapshot.model_semantics,
    )
    .expect("context window should have complete tool pairing");
    let tombstone_call_ids = response
        .session_snapshot
        .messages
        .iter()
        .filter_map(|message| {
            let trace = model_tool_result_semantics(&response.session_snapshot, message)?;
            if trace.get("status").and_then(Value::as_str) != Some("blocked")
                || trace.get("transitionReason").and_then(Value::as_str)
                    != Some("unpaired_tool_call_closed_by_new_user_turn")
            {
                return None;
            }
            trace
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        tombstone_call_ids,
        vec!["call-first", "call-second", "call-third"]
    );
}

#[tokio::test]
async fn new_user_turn_closes_unpaired_tool_call() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());
    let session_id = "chat-unpaired-tool-tombstone";
    let mut session = SessionStateSnapshot::new(session_id.to_string(), 1);
    engine
        .message_handler
        .push_user_message(&mut session, "install dependency", JsonMap::new());
    engine.message_handler.push_model_assistant_message(
        &mut session,
        "I need to run a tool.",
        JsonMap::new(),
        build_model_assistant_semantics(&GenerateResult {
            content: "I need to run a tool.".to_string(),
            tool_calls: vec![ToolCallEnvelope {
                id: "call-missing-pending".to_string(),
                name: "bash".to_string(),
                args_json: json!({ "command": "apt-get update" }).to_string(),
            }],
            reasoning_content: None,
            input_tokens: None,
            total_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        }),
    );
    engine
        .session_manager
        .save_session(&session)
        .expect("save poisoned session");

    let response = engine
        .process_turn_with_stream_sink_async(
            ProcessTurnRequest {
                session_id: session_id.to_string(),
                agent_run_identity: None,
                turn_id: "turn-continue".to_string(),
                input: TurnInput::UserMessage("continue".to_string()),
                generate_result: GenerateResult {
                    content: "Recovered from an interrupted tool call.".to_string(),
                    tool_calls: vec![],
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            },
            None,
        )
        .await
        .expect("new turn should close unpaired tool call even without pending permission");

    crate::runtime::context_window::validate_model_context_window(
        response.session_snapshot.context_window.as_slice(),
        &response.session_snapshot.model_semantics,
    )
    .expect("context window should have complete tool pairing");
    assert!(response.session_snapshot.messages.iter().any(|message| {
        tool_message_matches_transition(
            &response.session_snapshot,
            message,
            "call-missing-pending",
            "unpaired_tool_call_closed_by_new_user_turn",
        )
    }));
}

#[test]
fn agent_runtime_default_system_prompt_is_compact_root() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());

    let request = engine
        .build_generate_driver_request(
            "chat-default-compact-system-prompt",
            "turn-default-compact-system-prompt",
            "检查当前默认 system prompt。",
            0,
        )
        .expect("build generate request");
    let system_prompt = request
        .prepared_prompt
        .system_prompt
        .as_deref()
        .expect("system prompt");
    let manifest = serde_json::from_str::<Value>(
        request
            .system_prompt_manifest_json
            .as_deref()
            .expect("system prompt manifest"),
    )
    .expect("parse manifest");

    assert!(manifest.get("profile").is_none());
    assert_eq!(
        manifest.get("schema").and_then(Value::as_str),
        Some("system_prompt_manifest_v1")
    );
    assert_eq!(
        system_prompt,
        crate::model::prompt::system::render_system_prompt()
            .expect("compile system prompt")
            .content
    );
    assert!(system_prompt.starts_with("# Harness\n"));
    assert_eq!(
        manifest.get("sectionCount").and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(manifest["includedSections"], json!(["Harness"]));
}

#[test]
fn provider_prompt_cache_key_ignores_turn_user_and_subagent_ids() {
    let store = AgentRuntimeTestStore::new();
    let config = AgentRuntimeConfig::default();
    assert_eq!(
        config.provider_prompt_cache_retention.as_deref(),
        Some(DEFAULT_PROVIDER_PROMPT_CACHE_RETENTION)
    );
    let engine = AgentRuntime::new_for_test(store.clone(), config);

    let main_request = engine
        .build_generate_driver_request(
            "chat-provider-cache-main",
            "turn-provider-cache-main",
            "First changing user message.",
            0,
        )
        .expect("build main request");
    let subagent_request = engine
        .build_generate_driver_request(
            "chat-provider-cache-main:subagent:worker-a",
            "turn-provider-cache-subagent",
            "Second changing delegated message.",
            0,
        )
        .expect("build subagent-shaped request");

    assert_eq!(
        main_request.provider_prompt_cache_key,
        subagent_request.provider_prompt_cache_key
    );
    let key = main_request
        .provider_prompt_cache_key
        .as_deref()
        .expect("provider prompt cache key");
    assert!(key.starts_with("centaeris-provider-pcache-seed-v1:"));
    assert!(!key.contains("chat-provider-cache-main"));
    assert!(!key.contains("turn-provider-cache"));
    assert!(!key.contains("changing"));
    assert_eq!(
        main_request.provider_prompt_cache_retention.as_deref(),
        Some("24h")
    );
}

#[test]
fn provider_prompt_cache_key_changes_when_tool_schema_changes() {
    let config = AgentRuntimeConfig::default();
    let base_tools = vec![ModelToolDefinition {
        name: "read".to_string(),
        description: "Read file content.".to_string(),
        input_schema: json!({"type":"object"}),
    }];
    let mut changed_tools = base_tools.clone();
    changed_tools.push(ModelToolDefinition {
        name: "bash".to_string(),
        description: "Run a bash command.".to_string(),
        input_schema: json!({"type":"object"}),
    });

    let base = build_provider_prompt_cache_key(
        Some("stable system prompt"),
        base_tools.as_slice(),
        None,
        None,
        None,
        &config,
    )
    .expect("build base key");
    let changed = build_provider_prompt_cache_key(
        Some("stable system prompt"),
        changed_tools.as_slice(),
        None,
        None,
        None,
        &config,
    )
    .expect("build changed key");

    assert_ne!(base, changed);
}

#[test]
fn provider_prompt_cache_key_changes_when_skill_catalog_changes() {
    let config = AgentRuntimeConfig::default();
    let tools = vec![ModelToolDefinition {
        name: "read".to_string(),
        description: "Read file content.".to_string(),
        input_schema: json!({"type":"object"}),
    }];
    let base = build_provider_prompt_cache_key(
        Some("stable system prompt"),
        tools.as_slice(),
        Some("catalog-a"),
        None,
        None,
        &config,
    )
    .expect("build base key");
    let changed = build_provider_prompt_cache_key(
        Some("stable system prompt"),
        tools.as_slice(),
        Some("catalog-b"),
        None,
        None,
        &config,
    )
    .expect("build changed key");
    assert_ne!(base, changed);
}

#[test]
fn task_output_rejects_invalid_args_json() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());
    let report = engine.execute_task_runtime_tool_call(
        "chat-task-runtime-invalid-args",
        &ToolCallEnvelope {
            id: "call-invalid-task-runtime-args".to_string(),
            name: "task_output".to_string(),
            args_json: "{".to_string(),
        },
    );

    assert_eq!(report.status, "error");
    assert_eq!(
        report.transition_reason.as_deref(),
        Some("task_runtime_tool_exec_error")
    );
    assert!(report
        .error
        .as_ref()
        .map(|e| e.model_message.as_str())
        .unwrap_or_default()
        .contains("args_json is invalid JSON"));
}

fn force_prompt_compaction_for_tests(config: &mut AgentRuntimeConfig) {
    config.model_context_tokens = 6_800;
    config.model_max_output_tokens = 4_000;
    config.prompt_compaction_trigger_headroom_tokens = u32::MAX;
    config.prompt_compaction_user_replay_tokens = 30;
}

fn model_prompt_compaction_session(session_id: &str) -> SessionStateSnapshot {
    let mut session = SessionStateSnapshot::new(session_id.to_string(), 0);
    session.messages.push(ChatMessage {
        message_id: "msg-model-client-compact-1".to_string(),
        role: MessageRole::User,
        content: "old model compaction request should be summarized by the model. ".repeat(160),
        created_at_ms: 1,
        metadata: JsonMap::new(),
    });
    session.messages.push(ChatMessage {
        message_id: "msg-model-client-compact-2".to_string(),
        role: MessageRole::Assistant,
        content: "old assistant response described planner, producer, validator, and committer. "
            .repeat(160),
        created_at_ms: 2,
        metadata: JsonMap::new(),
    });
    session.messages.push(ChatMessage {
        message_id: "msg-model-client-compact-3".to_string(),
        role: MessageRole::User,
        content: "recent user message should stay in suffix.".to_string(),
        created_at_ms: 3,
        metadata: JsonMap::new(),
    });
    session.messages.push(ChatMessage {
        message_id: "msg-model-client-compact-4".to_string(),
        role: MessageRole::Assistant,
        content: "recent assistant message should stay in suffix.".to_string(),
        created_at_ms: 4,
        metadata: JsonMap::new(),
    });
    assign_plain_model_semantics(&mut session);
    session
}

fn p7_large_context_compaction_config() -> AgentRuntimeConfig {
    AgentRuntimeConfig {
        model_context_tokens: 22_000,
        model_max_output_tokens: 4_000,
        prompt_compaction_trigger_headroom_tokens: u32::MAX,
        prompt_compaction_user_replay_tokens: 100,
        prompt_compaction_summary_max_tokens: 4_000,
        ..Default::default()
    }
}

fn p7_large_context_session(session_id: &str) -> SessionStateSnapshot {
    let mut session = SessionStateSnapshot::new(session_id.to_string(), 0);
    let large_user_context =
        "old prefix user details about model compaction pressure and chain fidelity. ".repeat(180);
    let large_assistant_context =
        "old prefix assistant implementation notes about planner producer validator committer. "
            .repeat(180);
    let messages = [
        (
            "msg-p7-old-user-1",
            MessageRole::User,
            format!("alpha {large_user_context}"),
        ),
        (
            "msg-p7-old-assistant-1",
            MessageRole::Assistant,
            format!("beta {large_assistant_context}"),
        ),
        (
            "msg-p7-old-user-2",
            MessageRole::User,
            format!("gamma {large_user_context}"),
        ),
        (
            "msg-p7-old-assistant-2",
            MessageRole::Assistant,
            format!("delta {large_assistant_context}"),
        ),
        (
            "msg-p7-recent-user",
            MessageRole::User,
            "recent user suffix must remain visible after compaction.".to_string(),
        ),
        (
            "msg-p7-recent-assistant",
            MessageRole::Assistant,
            "recent assistant suffix must remain visible after compaction.".to_string(),
        ),
    ];
    for (index, (message_id, role, content)) in messages.into_iter().enumerate() {
        session.messages.push(ChatMessage {
            message_id: message_id.to_string(),
            role,
            content,
            created_at_ms: (index + 1) as i64,
            metadata: JsonMap::new(),
        });
    }
    assign_plain_model_semantics(&mut session);
    session
}

fn hash_json_value(value: &Value) -> String {
    stable_text_hash(value.to_string().as_str())
}

fn model_message_role_to_chat_role(
    role: &crate::model::prepared_prompt::ModelMessageRoleV1,
) -> MessageRole {
    match role {
        crate::model::prepared_prompt::ModelMessageRoleV1::System => MessageRole::System,
        crate::model::prepared_prompt::ModelMessageRoleV1::User => MessageRole::User,
        crate::model::prepared_prompt::ModelMessageRoleV1::Assistant => MessageRole::Assistant,
        crate::model::prepared_prompt::ModelMessageRoleV1::Tool => MessageRole::Tool,
    }
}

fn common_model_message_prefix_len(
    left: &[crate::model::prepared_prompt::ModelMessageV1],
    right: &[crate::model::prepared_prompt::ModelMessageV1],
) -> usize {
    left.iter()
        .zip(right.iter())
        .take_while(|(left, right)| left.role == right.role && left.content == right.content)
        .count()
}

fn common_model_message_prefix_metrics(
    left: &[crate::model::prepared_prompt::ModelMessageV1],
    right: &[crate::model::prepared_prompt::ModelMessageV1],
) -> (usize, usize, u32) {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .fold(
            (0usize, 0usize, 0u32),
            |(count, bytes, tokens), (message, _)| {
                let encoded =
                    serde_json::to_string(message).expect("serialize common prefix message");
                (
                    count.saturating_add(1),
                    bytes.saturating_add(encoded.len()),
                    tokens.saturating_add(crate::model::prepared_prompt::estimate_text_tokens(
                        encoded.as_str(),
                    )),
                )
            },
        )
}

fn reusable_cache_prefix_message_len(
    messages: &[crate::model::prepared_prompt::ModelMessageV1],
) -> usize {
    messages.len()
}

fn hash_model_message_prefix(
    messages: &[crate::model::prepared_prompt::ModelMessageV1],
    prefix_len: usize,
) -> String {
    let value = messages
        .iter()
        .take(prefix_len)
        .map(|message| {
            serde_json::json!({
                "role": format!("{:?}", message.role),
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();
    hash_json_value(&Value::Array(value))
}

#[test]
fn model_assistant_semantics_preserve_empty_reasoning_content() {
    let generate_result = GenerateResult {
        content: String::new(),
        tool_calls: vec![ToolCallEnvelope {
            id: "call-1".to_string(),
            name: "bash".to_string(),
            args_json: "{\"command\":\"pwd\"}".to_string(),
        }],
        reasoning_content: Some(String::new()),
        input_tokens: None,
        total_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };

    let semantics = build_model_assistant_semantics(&generate_result);
    let ModelMessageSemanticsV1::Assistant {
        reasoning_content,
        tool_calls,
    } = semantics
    else {
        panic!("assistant semantics expected");
    };

    assert_eq!(reasoning_content.as_deref(), Some(""));
    assert_eq!(tool_calls[0].id, "call-1");
}

fn system_prompt_sections(content: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current_name = "preamble".to_string();
    let mut current_lines: Vec<String> = Vec::new();
    for line in content.lines() {
        if let Some(name) = line.strip_prefix("# ") {
            if !current_lines.is_empty() || current_name != "preamble" {
                sections.push((current_name, current_lines.join("\n")));
            }
            current_name = name.trim().to_string();
            current_lines = vec![line.to_string()];
        } else {
            current_lines.push(line.to_string());
        }
    }
    if !current_lines.is_empty() || current_name != "preamble" {
        sections.push((current_name, current_lines.join("\n")));
    }
    sections
}

fn changed_system_prompt_sections(left: &str, right: &str) -> Vec<String> {
    let left_sections = system_prompt_sections(left);
    let right_sections = system_prompt_sections(right);
    let max_len = left_sections.len().max(right_sections.len());
    (0..max_len)
        .filter_map(|index| {
            let left = left_sections.get(index);
            let right = right_sections.get(index);
            if left == right {
                return None;
            }
            Some(match (left, right) {
                (Some((left_name, _)), Some((right_name, _))) if left_name == right_name => {
                    left_name.clone()
                }
                (Some((left_name, _)), Some((right_name, _))) => {
                    format!("{left_name}->{right_name}")
                }
                (Some((left_name, _)), None) => format!("{left_name}-><removed>"),
                (None, Some((right_name, _))) => format!("<added>->{right_name}"),
                (None, None) => unreachable!(),
            })
        })
        .collect()
}

fn stable_system_prompt_root(content: &str) -> &str {
    content
}

fn detect_generate_request_cache_breaks(
    left: &GenerateDriverRequest,
    right: &GenerateDriverRequest,
) -> Value {
    let left_system = left.prepared_prompt.system_prompt.as_deref().unwrap_or("");
    let right_system = right.prepared_prompt.system_prompt.as_deref().unwrap_or("");
    let left_stable_system = stable_system_prompt_root(left_system);
    let right_stable_system = stable_system_prompt_root(right_system);
    let changed_system_sections =
        changed_system_prompt_sections(left_stable_system, right_stable_system);
    let system_break = left_stable_system != right_stable_system;
    let left_tools_hash =
        hash_json_value(&serde_json::to_value(&left.prepared_prompt.tool_definitions).unwrap());
    let right_tools_hash =
        hash_json_value(&serde_json::to_value(&right.prepared_prompt.tool_definitions).unwrap());
    let tools_break = left_tools_hash != right_tools_hash;
    let context_prefix_len = common_model_message_prefix_len(
        &left.prepared_prompt.messages,
        &right.prepared_prompt.messages,
    );
    let reusable_message_prefix_len =
        reusable_cache_prefix_message_len(left.prepared_prompt.messages.as_slice());
    let messages_break = context_prefix_len < reusable_message_prefix_len;
    json!({
        "schema": "prompt_cache_break_detection_v1",
        "hasBreak": system_break || tools_break || messages_break,
        "system": {
            "break": system_break,
            "reason": if system_break { Some("system_prompt_root_changed") } else { None },
            "changedSections": changed_system_sections,
        },
        "tools": {
            "break": tools_break,
            "reason": if tools_break { Some("tool_definitions_changed") } else { None },
            "leftHash": left_tools_hash,
            "rightHash": right_tools_hash,
        },
        "provider": {
            "break": false,
            "reason": "checked_by_model_client_payload_audit",
        },
        "messages": {
            "break": messages_break,
            "reason": if messages_break {
                Some("previous_request_messages_not_fully_reused")
            } else {
                None
            },
            "commonPrefixLen": context_prefix_len,
            "previousReusablePrefixMessageCount": reusable_message_prefix_len,
            "previousMessageCount": left.prepared_prompt.messages.len(),
            "currentMessageCount": right.prepared_prompt.messages.len(),
        },
    })
}

fn assert_model_tool_definitions_match_default_projection(
    tool_definitions: &[ModelToolDefinition],
) {
    let actual = tool_definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect::<Vec<_>>();
    let expected = vec!["read", "bash", "edit", "write", "task_output", "agent"];
    assert_eq!(actual, expected);
    for definition in tool_definitions
        .iter()
        .filter(|definition| matches!(definition.name.as_str(), "read" | "bash" | "edit" | "write"))
    {
        assert!(
            !definition.input_schema["properties"]
                .as_object()
                .expect("builtin tool properties")
                .contains_key("title"),
            "{} projected removed title argument",
            definition.name
        );
    }
    let edit = tool_definitions
        .iter()
        .find(|definition| definition.name == "edit")
        .expect("edit definition");
    assert_eq!(edit.input_schema["required"], json!(["path", "edits"]));
}

#[test]
fn generate_driver_request_projects_explicit_skill_metadata_without_extra_tools() {
    let skill_root = std::env::temp_dir().join(format!(
        "centaeris_agent_runtime_skill_catalog_{}_{}",
        std::process::id(),
        now_ms()
    ));
    let skill_dir = skill_root.join("external-research-tavily-cli");
    std::fs::create_dir_all(skill_dir.as_path()).expect("create skill dir");
    std::fs::create_dir_all(skill_dir.join("references")).expect("create references dir");
    std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: external-research-tavily-cli\ndescription: Use Tavily CLI research through bash without adding model tools.\nallowed-tools: [bash,read]\n---\n# Tavily CLI Research\nUse bash to run `tavily search --query <query> --json` and summarize stdout.\n",
        )
        .expect("write skill");
    std::fs::write(
        skill_dir.join("references").join("api.md"),
        "Large Tavily API reference that should remain a referenced asset.",
    )
    .expect("write reference");

    let store = AgentRuntimeTestStore::new();
    let config = AgentRuntimeConfig {
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let tool_layer = ToolLayer::new_with_skill_catalog_config(
        explicit_workspace_skill_catalog_config(skill_root.as_path(), skill_root.as_path()),
    )
    .with_cwd(skill_root.clone())
    .expect("set stage7 workspace root");
    assert_eq!(
        tool_layer.skill_index().entries().len(),
        1,
        "skill catalog snapshot: {:?}",
        tool_layer.skill_index().snapshot()
    );
    let skill_location = tool_layer.skill_index().entries()[0].skill_md_path.clone();
    let skill_read = tool_layer.execute(ToolInvocationRequest {
        tool_call_id: "call-read-explicit-skill".to_string(),
        tool_name: "read".to_string(),
        args_json: json!({"path": skill_location}).to_string(),
    });
    assert_eq!(skill_read.status, "ok", "skill read: {skill_read:#?}");
    assert!(skill_read.content.contains("tavily search --query"));
    assert!(!skill_read.content.contains("Large Tavily API reference"));
    let engine = AgentRuntime::new_for_test_with_tools(store.clone(), tool_layer, config);

    let request = engine
        .build_generate_driver_request(
            "chat-skill-guidance-request",
            "turn-skill-guidance-request",
            "Need Tavily CLI research for a bounded external evidence gap",
            0,
        )
        .expect("build generate request");

    assert_model_tool_definitions_match_default_projection(
        &request.prepared_prompt.tool_definitions,
    );

    let system_prompt = request
        .prepared_prompt
        .system_prompt
        .as_deref()
        .expect("system prompt");
    assert!(!system_prompt.contains("external-research-tavily-cli"));
    let context_messages_text = request
        .prepared_prompt
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(context_messages_text.contains("<available_skills>"));
    assert!(context_messages_text.contains("<name>external-research-tavily-cli</name>"));
    assert!(context_messages_text.contains("<location>"));
    assert!(!context_messages_text.contains("tavily search --query"));
    assert!(!context_messages_text.contains("Large Tavily API reference"));

    let _ = std::fs::remove_dir_all(skill_root);
}

#[test]
fn generate_driver_request_keeps_conversation_prefix_before_dynamic_context_for_cache_audit() {
    let workspace_root = temp_dir_path("prompt_cache_prefix_workspace");
    std::fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let mut session = SessionStateSnapshot::new("chat-cache-prefix-audit".to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig {
        max_message_chars: 10_000,
    });
    handler.push_user_message(
        &mut session,
        "第一轮问题：请读取仓库里的 prompt pipeline 状态。",
        JsonMap::new(),
    );
    handler.push_assistant_message(
        &mut session,
        "第一轮回答：当前需要先审计请求组装顺序。",
        JsonMap::new(),
    );
    let stable_context_window = session.context_window.clone();
    session_manager
        .save_session(&session)
        .expect("save audit session");

    let config = AgentRuntimeConfig {
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let tool_layer = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("bind workspace root");
    let engine = AgentRuntime::new_for_test_with_tools(store.clone(), tool_layer, config);

    let request = engine
        .build_generate_driver_request(
            "chat-cache-prefix-audit",
            "turn-cache-prefix-audit",
            "第二轮问题：继续审计缓存前缀。",
            0,
        )
        .expect("build generate request");

    assert!(request.prepared_prompt.system_prompt.is_some());
    assert_eq!(stable_context_window.len(), 2);
    assert_eq!(
        request.prepared_prompt.messages.len(),
        stable_context_window.len() + 2
    );
    for (index, expected) in stable_context_window.iter().enumerate() {
        let actual = &request.prepared_prompt.messages[index];
        assert_eq!(
            actual.role,
            crate::model::prepared_prompt::ModelMessageRoleV1::from(&expected.role)
        );
        assert_eq!(actual.content, expected.content);
    }

    let execution_context =
        &request.prepared_prompt.messages[request.prepared_prompt.messages.len().saturating_sub(2)];
    assert_eq!(
        execution_context.role,
        crate::model::prepared_prompt::ModelMessageRoleV1::User
    );
    let expected_execution_context = prompt_projection::build_execution_context_message(
        "chat-cache-prefix-audit",
        "turn-cache-prefix-audit",
        workspace_root
            .canonicalize()
            .expect("canonical workspace")
            .as_path(),
        "bash",
    );
    assert_eq!(execution_context, &expected_execution_context);

    let current_user_message = request
        .prepared_prompt
        .messages
        .last()
        .expect("current user message");
    assert_eq!(
        current_user_message.role,
        crate::model::prepared_prompt::ModelMessageRoleV1::User
    );
    assert_eq!(
        current_user_message.content,
        "第二轮问题：继续审计缓存前缀。"
    );

    let expected_context_tokens =
        request
            .prepared_prompt
            .messages
            .iter()
            .fold(0u32, |total, message| {
                total.saturating_add(crate::model::prepared_prompt::estimate_text_tokens(
                    serde_json::to_string(message)
                        .expect("serialize model message")
                        .as_str(),
                ))
            });
    let expected_system_tokens = request
        .prepared_prompt
        .system_prompt
        .as_deref()
        .map(crate::model::prepared_prompt::estimate_text_tokens)
        .unwrap_or_default();
    let serialized_tools = serde_json::to_string(&request.prepared_prompt.tool_definitions)
        .expect("serialize model tool definitions");
    let expected_tool_tokens =
        crate::model::prepared_prompt::estimate_text_tokens(serialized_tools.as_str());
    assert_eq!(
        request.context_token_estimate,
        expected_system_tokens
            .saturating_add(expected_context_tokens)
            .saturating_add(expected_tool_tokens),
        "request token estimate must include system, context, and tool definitions"
    );

    let audit_lines = request
        .prepared_prompt
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let label = if index < stable_context_window.len() {
                "stable_conversation_prefix"
            } else if message.content.starts_with("<environment_context>") {
                "dynamic_execution_context"
            } else if index == request.prepared_prompt.messages.len().saturating_sub(1) {
                "current_user_prefix"
            } else {
                "current_user"
            };
            format!(
                "{index}:{label}:{:?}:{}",
                message.role,
                message.content.lines().next().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    eprintln!("prompt_cache_prefix_audit\n{}", audit_lines.join("\n"));

    let persisted = session_manager
        .load_session("chat-cache-prefix-audit")
        .expect("load persisted session")
        .expect("persisted session");
    assert!(persisted
        .messages
        .iter()
        .all(|message| !message.content.contains("<environment_context>")));

    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn query_loop_image_tool_continuation_keeps_required_observation_after_tool_result() {
    let workspace_root = temp_dir_path("read_image_prepared_prompt_workspace");
    std::fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
    image::DynamicImage::new_rgb8(2, 3)
        .save_with_format(workspace_root.join("page.png"), image::ImageFormat::Png)
        .expect("write image fixture");
    let tool_layer = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("bind workspace root");
    let report = tool_layer.execute(ToolInvocationRequest {
        tool_call_id: "call-read-image".to_string(),
        tool_name: "read".to_string(),
        args_json: json!({"path": "page.png"}).to_string(),
    });
    assert_eq!(report.status, "ok", "report={report:#?}");
    assert_eq!(report.details["contentType"], "image/png");
    assert_eq!(report.details["widthPx"], 2);
    assert_eq!(report.details["heightPx"], 3);
    assert!(!report.details.to_string().contains("dataBase64"));

    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let mut session = SessionStateSnapshot::new("chat-read-image".to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig {
        max_message_chars: 10_000,
    });
    let mut user_metadata = JsonMap::new();
    user_metadata.insert(
        MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
        MESSAGE_SEMANTIC_USER_REQUEST.to_string(),
    );
    handler.push_user_message(&mut session, "Read and describe page.png.", user_metadata);
    handler.push_model_assistant_message(
        &mut session,
        "",
        JsonMap::new(),
        ModelMessageSemanticsV1::Assistant {
            reasoning_content: None,
            tool_calls: vec![ModelToolCallStateV1 {
                id: "call-read-image".to_string(),
                name: "read".to_string(),
                args_json: json!({"path": "page.png"}).to_string(),
            }],
        },
    );
    write_tool_results_to_context(&handler, &mut session, std::slice::from_ref(&report))
        .expect("write image tool result");
    session_manager
        .save_session(&session)
        .expect("save image session");

    let config = AgentRuntimeConfig {
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test_with_tools(store, tool_layer, config);
    let request = engine
        .build_generate_driver_request_with_runtime_scope(
            "chat-read-image",
            "turn-after-image",
            &TurnInput::ToolContinuation {
                objective: "Read and describe page.png.".to_string(),
            },
            1,
            PromptCompactionScopeV1::main(),
        )
        .expect("build request with image observation");

    assert_eq!(request.prepared_prompt.input_images.len(), 1);
    let image = &request.prepared_prompt.input_images[0];
    assert_eq!(image.content_type, "image/png");
    assert!(!image.data_base64.is_empty());
    let observation_message = request
        .prepared_prompt
        .messages
        .iter()
        .find(|message| message.message_id == image.message_id)
        .expect("synthetic image observation message");
    assert_eq!(
        observation_message.role,
        crate::model::prepared_prompt::ModelMessageRoleV1::User
    );
    assert_eq!(
        observation_message
            .content
            .match_indices(image.placeholder.as_str())
            .count(),
        1
    );
    assert_eq!(
        request
            .prepared_prompt
            .messages
            .last()
            .expect("image observation tail")
            .message_id,
        observation_message.message_id
    );
    let execution_context_index = request
        .prepared_prompt
        .messages
        .iter()
        .position(|message| message.content.starts_with("<environment_context>\n"))
        .expect("execution context");
    let user_anchor_index = request
        .prepared_prompt
        .messages
        .iter()
        .position(|message| message.content == "Read and describe page.png.")
        .expect("causal user anchor");
    assert!(execution_context_index < user_anchor_index);
    assert!(request.observations.iter().any(|observation| matches!(
        observation,
        ModelObservationV1::InputImage { image: observed }
            if observed.message_id == image.message_id
    )));

    let persisted = session_manager
        .load_session("chat-read-image")
        .expect("load image session")
        .expect("image session exists");
    assert!(!serde_json::to_string(&persisted)
        .expect("serialize persisted session")
        .contains(&image.data_base64));

    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn agents_file_is_ephemeral_user_context_and_part_of_cache_identity() {
    let workspace_root = temp_dir_path("agents_user_context_workspace");
    std::fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
    std::fs::write(
        workspace_root.join("AGENTS.md"),
        "Use exact protocol names.\n",
    )
    .expect("write AGENTS.md");
    let store = AgentRuntimeTestStore::new();
    let config = AgentRuntimeConfig {
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let tool_layer = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("bind workspace root");
    let engine = AgentRuntime::new_for_test_with_tools(store.clone(), tool_layer, config);

    let first = engine
        .build_generate_driver_request(
            "chat-agents-context",
            "turn-agents-context",
            "inspect the protocol",
            0,
        )
        .expect("build request with AGENTS.md");
    let agents_index = first
        .prepared_prompt
        .messages
        .iter()
        .position(|message| message.message_id.ends_with(":agents_context"))
        .expect("AGENTS context message");
    assert!(agents_index > 0);
    assert!(first.prepared_prompt.messages[agents_index - 1]
        .message_id
        .ends_with(":execution_context"));
    assert_eq!(
        first.prepared_prompt.messages[agents_index].content,
        prompt_projection::build_agents_context_message(
            "chat-agents-context",
            "turn-agents-context",
            workspace_root
                .canonicalize()
                .expect("canonical workspace")
                .as_path(),
            "Use exact protocol names.\n",
        )
        .content
    );
    assert_eq!(
        first
            .prepared_prompt
            .messages
            .last()
            .map(|message| message.content.as_str()),
        Some("inspect the protocol")
    );

    std::fs::write(
        workspace_root.join("AGENTS.md"),
        "Use canonical identities.\n",
    )
    .expect("update AGENTS.md");
    let second = engine
        .build_generate_driver_request(
            "chat-agents-context",
            "turn-agents-context",
            "inspect the protocol",
            0,
        )
        .expect("rebuild request with changed AGENTS.md");
    assert_ne!(
        first.provider_prompt_cache_key,
        second.provider_prompt_cache_key
    );

    let answer_now = engine
        .build_generate_driver_request_with_runtime_scope(
            "chat-agents-context",
            "turn-agents-answer-now",
            &TurnInput::AnswerNow {
                message: "answer now".to_string(),
                intervention: AgentRunInterventionV1::answer_now("intervention-1", "agent-run-1"),
                supplement_ids: vec![],
            },
            0,
            PromptCompactionScopeV1::main(),
        )
        .expect("AnswerNow request keeps AGENTS.md");
    assert!(answer_now.prepared_prompt.messages.iter().any(|message| {
        message.message_id.ends_with(":agents_context")
            && message.content.contains("Use canonical identities.")
    }));

    let persisted = SessionManager::new(store)
        .load_or_create_session("chat-agents-context")
        .expect("load session");
    assert!(persisted
        .messages
        .iter()
        .all(|message| !message.content.contains("# AGENTS.md instructions")));

    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn agents_file_rejects_invalid_utf8_and_oversize_content() {
    let workspace_root = temp_dir_path("agents_invalid_workspace");
    std::fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
    let tool_layer = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("bind workspace root");

    std::fs::write(workspace_root.join("AGENTS.md"), [0xff, 0xfe])
        .expect("write invalid UTF-8 AGENTS.md");
    assert!(tool_layer
        .read_agents_instructions()
        .expect_err("invalid UTF-8 must fail")
        .contains("valid UTF-8"));

    std::fs::write(workspace_root.join("AGENTS.md"), "x".repeat(32_769))
        .expect("write oversized AGENTS.md");
    assert!(tool_layer
        .read_agents_instructions()
        .expect_err("oversized AGENTS.md must fail")
        .contains("exceeds character limit"));

    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn agent_instructions_are_ephemeral_context_and_part_of_cache_identity() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(
        store.clone(),
        AgentRuntimeConfig {
            agent_instructions: "Be precise and cite evidence.".to_string(),
            enable_prompt_compaction: false,
            ..Default::default()
        },
    );
    let first = engine
        .build_generate_driver_request(
            "chat-agent-instructions-a",
            "turn-agent-instructions-a",
            "inspect the protocol",
            0,
        )
        .expect("build request with Agent instructions");
    let message = first
        .prepared_prompt
        .messages
        .iter()
        .find(|message| message.message_id.ends_with(":agent_instructions"))
        .expect("Agent instructions message");
    assert!(message.content.contains("Be precise and cite evidence."));
    assert_eq!(
        first
            .prepared_prompt
            .messages
            .last()
            .map(|message| message.content.as_str()),
        Some("inspect the protocol")
    );

    let changed = AgentRuntime::new_for_test(
        store.clone(),
        AgentRuntimeConfig {
            agent_instructions: "Prefer short answers.".to_string(),
            enable_prompt_compaction: false,
            ..Default::default()
        },
    )
    .build_generate_driver_request(
        "chat-agent-instructions-b",
        "turn-agent-instructions-b",
        "inspect the protocol",
        0,
    )
    .expect("build request with changed Agent instructions");
    assert_ne!(
        first.provider_prompt_cache_key,
        changed.provider_prompt_cache_key
    );

    let persisted = SessionManager::new(store)
        .load_or_create_session("chat-agent-instructions-a")
        .expect("load session");
    assert!(persisted
        .messages
        .iter()
        .all(|message| !message.content.contains("# Instructions for this Agent")));
}

#[test]
fn generate_driver_request_preserves_configured_model_output_limit() {
    let store = AgentRuntimeTestStore::new();
    let config = AgentRuntimeConfig {
        model_context_tokens: 1_000_000,
        model_max_output_tokens: 384_000,
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test(store, config);

    let request = engine
        .build_generate_driver_request(
            "chat-configured-model-output-limit",
            "turn-configured-model-output-limit",
            "hello",
            0,
        )
        .expect("build generate request");

    assert_eq!(request.prepared_prompt.max_output_tokens, 384_000);
}

#[test]
fn generate_driver_request_loud_fails_for_invalid_model_context_budget() {
    let store = AgentRuntimeTestStore::new();
    let config = AgentRuntimeConfig {
        model_context_tokens: 16,
        model_max_output_tokens: 16,
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test(store, config);

    let error = engine
        .build_generate_driver_request(
            "chat-invalid-model-context-budget",
            "turn-invalid-model-context-budget",
            "hello",
            0,
        )
        .expect_err("invalid model context budget must fail");
    assert!(error.contains("model_context_budget_invalid"));
}

#[test]
fn tool_continuation_loud_fails_without_tool_result_tail() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let mut session =
        SessionStateSnapshot::new("chat-tool-continuation-context-tail".to_string(), 0);
    MessageHandler::new(MessageHandlerConfig::default()).push_user_message(
        &mut session,
        "hello",
        JsonMap::new(),
    );
    session_manager
        .save_session(&session)
        .expect("save session");
    let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());

    let error = engine
        .build_generate_driver_request_with_runtime_scope(
            "chat-tool-continuation-context-tail",
            "turn-tool-continuation-context-tail",
            &TurnInput::ToolContinuation {
                objective: "continue".to_string(),
            },
            0,
            PromptCompactionScopeV1::main(),
        )
        .expect_err("tool continuation requires a tool result tail");
    assert!(error.contains("tool_continuation_requires_tool_result_tail"));
}

fn append_prompt_test_tool_group(
    handler: &MessageHandler,
    session: &mut SessionStateSnapshot,
    call_ids: &[&str],
) {
    handler.push_model_assistant_message(
        session,
        "running prompt test tools",
        JsonMap::new(),
        ModelMessageSemanticsV1::Assistant {
            reasoning_content: None,
            tool_calls: call_ids
                .iter()
                .map(|call_id| ModelToolCallStateV1 {
                    id: (*call_id).to_string(),
                    name: "read".to_string(),
                    args_json: "{}".to_string(),
                })
                .collect(),
        },
    );
    for call_id in call_ids {
        handler.push_model_tool_message(
            session,
            format!("result for {call_id}").as_str(),
            JsonMap::new(),
            ModelMessageSemanticsV1::ToolResult {
                tool_call_id: (*call_id).to_string(),
                tool_name: "read".to_string(),
                status: "success".to_string(),
                result_state: "successWithOutput".to_string(),
                error_kind: None,
                object_refs: Vec::new(),
                transition_reason: None,
            },
        );
    }
}

#[test]
fn query_loop_tool_continuation_loud_fails_without_reliable_user_anchor() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session_id = "chat-tool-continuation-anchor-missing";
    let mut session = SessionStateSnapshot::new(session_id.to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig::default());
    handler.push_user_message(&mut session, "untyped historical user", JsonMap::new());
    append_prompt_test_tool_group(&handler, &mut session, &["call-anchor-missing"]);
    session_manager
        .save_session(&session)
        .expect("save missing-anchor session");
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());

    let error = engine
        .build_generate_driver_request_with_runtime_scope(
            session_id,
            "turn-tool-continuation-anchor-missing",
            &TurnInput::ToolContinuation {
                objective: "continue".to_string(),
            },
            1,
            PromptCompactionScopeV1::main(),
        )
        .expect_err("untyped user history is not a reliable causal anchor");

    assert_eq!(error, "tool_continuation_reliable_user_anchor_missing");
}

#[test]
fn query_loop_tool_continuation_anchors_ephemeral_context_before_user_and_keeps_parallel_tool_tail()
{
    let workspace_root = temp_dir_path("tool_continuation_execution_context_workspace");
    std::fs::create_dir_all(workspace_root.as_path()).expect("create workspace root");
    let skill_catalog = workspace_root.join("explicit-skill-catalog");
    let skill_dir = skill_catalog.join("continuation-skill");
    std::fs::create_dir_all(skill_dir.as_path()).expect("create continuation skill");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: continuation-skill\ndescription: Use when continuing this test task.\n---\nRead the task state.\n",
    )
    .expect("write continuation skill");
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session_id = "chat-tool-continuation-execution-context";
    let mut session = SessionStateSnapshot::new(session_id.to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig::default());
    let mut user_metadata = JsonMap::new();
    user_metadata.insert(
        MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
        MESSAGE_SEMANTIC_USER_REQUEST.to_string(),
    );
    handler.push_user_message(&mut session, "inspect the workspace", user_metadata);
    append_prompt_test_tool_group(
        &handler,
        &mut session,
        &["call-read-context", "call-read-agents"],
    );
    session_manager
        .save_session(&session)
        .expect("save paired tool session");
    let tool_layer = ToolLayer::new_with_skill_catalog_config(
        explicit_workspace_skill_catalog_config(workspace_root.as_path(), skill_catalog.as_path()),
    )
    .with_cwd(workspace_root.clone())
    .expect("bind workspace root");
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        tool_layer,
        AgentRuntimeConfig::default(),
    );

    let request = engine
        .build_generate_driver_request_with_runtime_scope(
            session_id,
            "turn-tool-continuation-execution-context",
            &TurnInput::ToolContinuation {
                objective: "inspect the workspace".to_string(),
            },
            1,
            PromptCompactionScopeV1::main(),
        )
        .expect("build tool continuation request");

    let messages = request.prepared_prompt.messages;
    let anchor_index = messages
        .iter()
        .position(|message| message.content == "inspect the workspace")
        .expect("causal user anchor");
    let execution_context_index = messages
        .iter()
        .position(|message| message.content.starts_with("<environment_context>\n"))
        .expect("execution context");
    let skill_catalog_index = messages
        .iter()
        .position(|message| message.content.starts_with("<available_skills>\n"))
        .expect("skill catalog");
    assert!(execution_context_index < anchor_index);
    assert!(skill_catalog_index < anchor_index);
    assert_eq!(
        messages[messages.len() - 2].tool_call_id.as_deref(),
        Some("call-read-context")
    );
    assert_eq!(
        messages
            .last()
            .expect("parallel tool result tail")
            .tool_call_id
            .as_deref(),
        Some("call-read-agents")
    );
    assert!(messages.last().is_some_and(
        |message| message.role == crate::model::prepared_prompt::ModelMessageRoleV1::Tool
    ));
    let persisted = session_manager
        .load_session(session_id)
        .expect("load persisted session")
        .expect("persisted session");
    assert!(persisted.messages.iter().all(|message| {
        !message.content.contains("<environment_context>")
            && !message.content.contains("<available_skills>")
    }));

    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn query_loop_tool_continuation_anchor_covers_supplement_compaction_and_lifecycle_tail() {
    for case in ["supplement", "compaction", "lifecycle"] {
        let store = AgentRuntimeTestStore::new();
        let session_manager = SessionManager::new(store.clone());
        let session_id = format!("chat-tool-continuation-{case}");
        let mut session = SessionStateSnapshot::new(session_id.clone(), 0);
        let handler = MessageHandler::new(MessageHandlerConfig::default());
        let anchor_content = format!("{case} causal user input");
        let mut anchor_metadata = JsonMap::new();
        anchor_metadata.insert(
            MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
            if case == "supplement" {
                MESSAGE_SEMANTIC_TURN_SUPPLEMENT
            } else {
                MESSAGE_SEMANTIC_USER_REQUEST
            }
            .to_string(),
        );
        let anchor_message_id =
            handler.push_user_message(&mut session, anchor_content.as_str(), anchor_metadata);
        append_prompt_test_tool_group(&handler, &mut session, &[format!("call-{case}").as_str()]);

        if case == "compaction" {
            let compaction_id = "prompt_compaction:test:1";
            let mut summary_metadata = JsonMap::new();
            summary_metadata.insert("kind".to_string(), "context_compaction".to_string());
            summary_metadata.insert("compaction_id".to_string(), compaction_id.to_string());
            summary_metadata.insert(
                "first_kept_message_id".to_string(),
                anchor_message_id.clone(),
            );
            session.messages.push(ChatMessage {
                message_id: "msg:compaction:summary".to_string(),
                role: MessageRole::System,
                content: "# Stable summary\n\nExact bytes.".to_string(),
                created_at_ms: 1,
                metadata: summary_metadata,
            });
            session.model_semantics.insert(
                "msg:compaction:summary".to_string(),
                ModelMessageSemanticsV1::Plain,
            );
            let mut replay_metadata = JsonMap::new();
            replay_metadata.insert(
                "kind".to_string(),
                "prompt_compaction_user_replay".to_string(),
            );
            replay_metadata.insert("compaction_id".to_string(), compaction_id.to_string());
            replay_metadata.insert(
                MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
                MESSAGE_SEMANTIC_USER_REQUEST.to_string(),
            );
            session.messages.push(ChatMessage {
                message_id: "msg:compaction:replay".to_string(),
                role: MessageRole::User,
                content: "stable replay bytes".to_string(),
                created_at_ms: 1,
                metadata: replay_metadata,
            });
            session.model_semantics.insert(
                "msg:compaction:replay".to_string(),
                ModelMessageSemanticsV1::Plain,
            );
            handler.refresh_context_window(&mut session);
        }

        if case == "lifecycle" {
            let mut lifecycle_metadata = JsonMap::new();
            lifecycle_metadata.insert(
                LIFECYCLE_HOOK_CONTEXT_META_KEY.to_string(),
                "true".to_string(),
            );
            handler.push_system_message(
                &mut session,
                "[Lifecycle hook context]\nverified receipt",
                lifecycle_metadata,
            );
        }

        session_manager
            .save_session(&session)
            .expect("save anchor matrix session");
        let engine = AgentRuntime::new_for_test(
            store,
            AgentRuntimeConfig {
                agent_instructions: "Keep the runtime context anchored.".to_string(),
                enable_prompt_compaction: false,
                ..Default::default()
            },
        );
        let request = engine
            .build_generate_driver_request_with_runtime_scope(
                session_id.as_str(),
                format!("turn-tool-continuation-{case}").as_str(),
                &TurnInput::ToolContinuation {
                    objective: anchor_content.clone(),
                },
                1,
                PromptCompactionScopeV1::main(),
            )
            .expect("build anchor matrix continuation");
        let messages = request.prepared_prompt.messages;
        let runtime_index = messages
            .iter()
            .position(|message| {
                message
                    .content
                    .contains("Keep the runtime context anchored.")
            })
            .expect("ephemeral Agent instructions");
        let anchor_index = messages
            .iter()
            .position(|message| message.content == anchor_content)
            .expect("causal user anchor");
        assert!(runtime_index < anchor_index, "case={case}");

        if case == "compaction" {
            assert_eq!(messages[0].content, "# Stable summary\n\nExact bytes.");
            assert_eq!(messages[1].content, "stable replay bytes");
        } else if case == "lifecycle" {
            let lifecycle_index = messages
                .iter()
                .position(|message| message.content.contains("verified receipt"))
                .expect("lifecycle context");
            assert!(lifecycle_index < runtime_index, "case={case}");
            assert_eq!(
                messages[lifecycle_index].role,
                crate::model::prepared_prompt::ModelMessageRoleV1::User
            );
        }
        assert!(messages.last().is_some_and(|message| {
            message.role == crate::model::prepared_prompt::ModelMessageRoleV1::Tool
        }));
    }
}

#[test]
fn query_loop_tool_continuation_twenty_round_deterministic_eval() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let handler = MessageHandler::new(MessageHandlerConfig::default());
    let engine = AgentRuntime::new_for_test(
        store,
        AgentRuntimeConfig {
            agent_instructions: "Stable deterministic runtime context.".to_string(),
            ..Default::default()
        },
    );
    let mut compliant_rounds = 0usize;
    let mut duplicate_tool_rounds = 0usize;

    for round in 1..=20 {
        let session_id = format!("chat-tool-continuation-eval-{round}");
        let objective = format!("inspect deterministic fixture {round}");
        let call_id = format!("call-eval-{round}");
        let mut session = SessionStateSnapshot::new(session_id.clone(), 0);
        let mut user_metadata = JsonMap::new();
        user_metadata.insert(
            MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
            MESSAGE_SEMANTIC_USER_REQUEST.to_string(),
        );
        handler.push_user_message(&mut session, objective.as_str(), user_metadata);
        append_prompt_test_tool_group(&handler, &mut session, &[call_id.as_str()]);
        session_manager
            .save_session(&session)
            .expect("save deterministic continuation session");

        let request = engine
            .build_generate_driver_request_with_runtime_scope(
                session_id.as_str(),
                format!("turn-eval-{round}").as_str(),
                &TurnInput::ToolContinuation {
                    objective: objective.clone(),
                },
                1,
                PromptCompactionScopeV1::main(),
            )
            .expect("build deterministic continuation request");
        let messages = request.prepared_prompt.messages;
        let runtime_index = messages
            .iter()
            .position(|message| {
                message
                    .content
                    .contains("Stable deterministic runtime context.")
            })
            .expect("runtime context");
        let user_index = messages
            .iter()
            .position(|message| message.content == objective)
            .expect("causal user objective");
        let tail = messages.last().expect("tool result tail");
        let compliant = runtime_index < user_index
            && tail.role == crate::model::prepared_prompt::ModelMessageRoleV1::Tool
            && tail.tool_call_id.as_deref() == Some(call_id.as_str());
        compliant_rounds += usize::from(compliant);

        // The deterministic probe repeats the tool only when the projected tail
        // looks like a new user continuation—the exact false signal fixed here.
        duplicate_tool_rounds +=
            usize::from(tail.role == crate::model::prepared_prompt::ModelMessageRoleV1::User);
    }

    assert_eq!(compliant_rounds, 20);
    assert_eq!(duplicate_tool_rounds, 0);
    eprintln!(
        "CORE_01_20_ROUND compliant={compliant_rounds} duplicateToolRounds={duplicate_tool_rounds} auditedBaselineCompliant=18 auditedBaselineDuplicateToolRounds=2"
    );
}

#[test]
fn generate_driver_request_two_round_cache_audit_hashes_prompt_tools_and_context_prefix() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session_id = "chat-cache-two-round-audit";
    let mut session = SessionStateSnapshot::new(session_id.to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig {
        max_message_chars: 10_000,
    });
    handler.push_user_message(
        &mut session,
        "基线问题：先确认 prompt pipeline 的缓存风险。",
        JsonMap::new(),
    );
    handler.push_assistant_message(
        &mut session,
        "基线回答：需要比较 system、tools 和 messages 前缀。",
        JsonMap::new(),
    );
    session_manager
        .save_session(&session)
        .expect("save initial audit session");

    let config = AgentRuntimeConfig {
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test(store.clone(), config);

    let round1 = engine
        .build_generate_driver_request(
            session_id,
            "turn-cache-audit-1",
            "第一轮：输出当前缓存审计基线。",
            0,
        )
        .expect("build round1 request");

    let mut session = session_manager
        .load_or_create_session(session_id)
        .expect("load round1 session");
    handler.push_assistant_message(&mut session, "第一轮回答：审计基线已建立。", JsonMap::new());
    session_manager
        .save_session(&session)
        .expect("save round1 transcript");

    let round2 = engine
        .build_generate_driver_request(
            session_id,
            "turn-cache-audit-2",
            "第二轮：继续比较缓存前缀。",
            0,
        )
        .expect("build round2 request");

    let round1_system_hash = stable_text_hash(stable_system_prompt_root(
        round1
            .prepared_prompt
            .system_prompt
            .as_deref()
            .unwrap_or(""),
    ));
    let round2_system_hash = stable_text_hash(stable_system_prompt_root(
        round2
            .prepared_prompt
            .system_prompt
            .as_deref()
            .unwrap_or(""),
    ));
    let changed_system_sections = changed_system_prompt_sections(
        stable_system_prompt_root(
            round1
                .prepared_prompt
                .system_prompt
                .as_deref()
                .unwrap_or(""),
        ),
        stable_system_prompt_root(
            round2
                .prepared_prompt
                .system_prompt
                .as_deref()
                .unwrap_or(""),
        ),
    );
    let round1_tools_hash =
        hash_json_value(&serde_json::to_value(&round1.prepared_prompt.tool_definitions).unwrap());
    let round2_tools_hash =
        hash_json_value(&serde_json::to_value(&round2.prepared_prompt.tool_definitions).unwrap());
    let context_prefix_len = common_model_message_prefix_len(
        &round1.prepared_prompt.messages,
        &round2.prepared_prompt.messages,
    );
    let round1_context_prefix_hash = hash_model_message_prefix(
        round1.prepared_prompt.messages.as_slice(),
        context_prefix_len,
    );
    let round2_context_prefix_hash = hash_model_message_prefix(
        round2.prepared_prompt.messages.as_slice(),
        context_prefix_len,
    );
    let cache_break_detection = detect_generate_request_cache_breaks(&round1, &round2);
    let round1_reusable_prefix_len =
        reusable_cache_prefix_message_len(round1.prepared_prompt.messages.as_slice());

    assert_eq!(
        round1_system_hash, round2_system_hash,
        "default prompt-cache-enabled system prompt should keep a stable root"
    );
    assert_eq!(
        round1.prepared_prompt.system_prompt, round2.prepared_prompt.system_prompt,
        "cache-enabled system prompt must be byte-stable"
    );
    assert!(
        !round1
            .prepared_prompt
            .system_prompt
            .as_deref()
            .unwrap_or_default()
            .contains("# Current Objective"),
        "volatile current objective must not be rendered into system root"
    );
    assert!(
        changed_system_sections.is_empty(),
        "unexpected system prompt volatility: {changed_system_sections:?}"
    );
    assert_eq!(
        round1_tools_hash, round2_tools_hash,
        "tool definitions should remain stable across adjacent turns"
    );
    assert_eq!(
        round1_context_prefix_hash, round2_context_prefix_hash,
        "common context prefix hash should be deterministic"
    );
    assert!(
        context_prefix_len >= round1_reusable_prefix_len,
        "the full reusable message prefix from the previous request should remain reusable"
    );
    assert_eq!(
            cache_break_detection
                .get("hasBreak")
                .and_then(Value::as_bool),
            Some(false),
            "cache break audit should fail if volatile suffix prevents reusable prefix reuse: {cache_break_detection}"
        );

    eprintln!(
        "prompt_cache_two_round_audit\n{}",
        serde_json::json!({
            "round1": {
                "stableSystemPromptPrefixHash": round1_system_hash,
                "toolDefinitionsHash": round1_tools_hash,
                "contextMessageCount": round1.prepared_prompt.messages.len(),
            },
            "round2": {
                "stableSystemPromptPrefixHash": round2_system_hash,
                "toolDefinitionsHash": round2_tools_hash,
                "contextMessageCount": round2.prepared_prompt.messages.len(),
            },
            "comparison": {
                "systemPromptChanged": round1_system_hash != round2_system_hash,
                "changedSystemSections": changed_system_sections,
                "toolDefinitionsChanged": round1_tools_hash != round2_tools_hash,
                "contextMessagesCommonPrefixLen": context_prefix_len,
                "round1ReusablePrefixMessageCount": round1_reusable_prefix_len,
                "contextMessagesCommonPrefixHash": round1_context_prefix_hash,
                "cacheBreakDetection": cache_break_detection,
            }
        })
    );
}

#[test]
fn query_loop_cache_prefix_stays_reusable_across_legacy_80_message_threshold() {
    let mut message_count_boundaries = Vec::new();
    let mut legacy_80_cap = Value::Null;
    let mut token_budget_only = Value::Null;
    for message_count_before in [80, 4_096, 10_000] {
        let message_count_after = message_count_before + 1;
        let store = AgentRuntimeTestStore::new();
        let session_manager = SessionManager::new(store.clone());
        let session_id = format!("chat-cache-{message_count_before}-threshold");
        let mut session = SessionStateSnapshot::new(session_id.clone(), 0);
        session.messages = (0..message_count_before - 1)
            .map(|index| ChatMessage {
                message_id: format!("msg-history-{index}"),
                role: MessageRole::User,
                content: format!("small history message {index:02}"),
                created_at_ms: index as i64,
                metadata: JsonMap::new(),
            })
            .collect();
        assign_plain_model_semantics(&mut session);
        session_manager
            .save_session(&session)
            .expect("save cache boundary session");
        let config = AgentRuntimeConfig {
            model_context_tokens: 2_000_000,
            ..Default::default()
        };
        let input_limit = config.model_context_tokens - config.model_max_output_tokens;
        let engine = AgentRuntime::new_for_test(store, config);

        let started = std::time::Instant::now();
        let before = engine
            .build_generate_driver_request(
                &session_id,
                &format!("turn-cache-message-{message_count_before}"),
                &format!("message {message_count_before}"),
                0,
            )
            .expect("build request before message-count boundary");
        let after = engine
            .build_generate_driver_request(
                &session_id,
                &format!("turn-cache-message-{message_count_after}"),
                &format!("message {message_count_after}"),
                0,
            )
            .expect("build request after message-count boundary");
        let request_build_elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let metrics = common_model_message_prefix_metrics(
            &before.prepared_prompt.messages,
            &after.prepared_prompt.messages,
        );
        let saved = session_manager
            .load_session(&session_id)
            .expect("load cache boundary session")
            .expect("cache boundary session exists");
        let compaction_count = saved
            .messages
            .iter()
            .filter(|message| {
                message.metadata.get("kind").map(String::as_str) == Some("context_compaction")
            })
            .count();

        assert_eq!(metrics.0, message_count_before);
        assert!(metrics.1 > 0);
        assert!(metrics.2 > 0);
        assert_eq!(before.prepared_prompt.messages.len(), message_count_before);
        assert_eq!(after.prepared_prompt.messages.len(), message_count_after);
        assert!(after.context_token_estimate > before.context_token_estimate);
        assert!(after.context_token_estimate < input_limit);
        assert_eq!(saved.messages.len(), message_count_after);
        assert_eq!(saved.context_window.len(), message_count_after);
        assert_eq!(compaction_count, 0);
        assert_eq!(
            before.prepared_prompt.system_prompt,
            after.prepared_prompt.system_prompt
        );
        assert_eq!(
            before.prepared_prompt.tool_definitions,
            after.prepared_prompt.tool_definitions
        );
        assert_eq!(
            before.provider_prompt_cache_key,
            after.provider_prompt_cache_key
        );
        message_count_boundaries.push(json!({
            "messageCountBefore": message_count_before,
            "messageCountAfter": message_count_after,
            "retainedMessagesBefore": before.prepared_prompt.messages.len(),
            "retainedMessagesAfter": after.prepared_prompt.messages.len(),
            "commonPrefixMessages": metrics.0,
            "commonPrefixBytes": metrics.1,
            "commonPrefixTokens": metrics.2,
            "compactionCount": compaction_count,
            "requestBuildElapsedMs": request_build_elapsed_ms,
        }));
        if message_count_before == 80 {
            // Test-only plain-message baseline: the old cap dropped the first message at 81.
            let legacy_at_80 = &before.prepared_prompt.messages[..80];
            let legacy_at_81 = &after.prepared_prompt.messages[1..81];
            let legacy_metrics = common_model_message_prefix_metrics(legacy_at_80, legacy_at_81);
            assert_eq!(legacy_metrics, (0, 0, 0));
            legacy_80_cap = json!({
                "commonPrefixMessages": legacy_metrics.0,
                "commonPrefixBytes": legacy_metrics.1,
                "commonPrefixTokens": legacy_metrics.2,
                "retainedMessagesAt80": legacy_at_80.len(),
                "retainedMessagesAt81": legacy_at_81.len(),
                "compactionCount": 0,
            });
            token_budget_only = json!({
                "commonPrefixMessages": metrics.0,
                "commonPrefixBytes": metrics.1,
                "commonPrefixTokens": metrics.2,
                "retainedMessagesAt80": before.prepared_prompt.messages.len(),
                "retainedMessagesAt81": after.prepared_prompt.messages.len(),
                "compactionCount": compaction_count,
            });
        }
    }
    eprintln!(
        "query_loop_cache_80_threshold_metrics\n{}",
        json!({
            "measurementKind": "syntheticPreparedPromptPrefixes",
            "providerCacheHitRateMeasured": false,
            "legacy80Cap": legacy_80_cap,
            "tokenBudgetOnly": token_budget_only,
            "messageCountBoundaries": message_count_boundaries,
        })
    );
}

#[test]
fn generate_driver_request_keeps_compiled_profile_stable_when_cache_disabled() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session_id = "chat-cache-disabled-volatility-audit";
    let mut session = SessionStateSnapshot::new(session_id.to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig {
        max_message_chars: 10_000,
    });
    handler.push_user_message(
        &mut session,
        "基线问题：检查 system prompt 动态段。",
        JsonMap::new(),
    );
    handler.push_assistant_message(
        &mut session,
        "基线回答：动态段应当被审计出来。",
        JsonMap::new(),
    );
    session_manager
        .save_session(&session)
        .expect("save initial audit session");

    let config = AgentRuntimeConfig {
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test(store.clone(), config);

    let round1 = engine
        .build_generate_driver_request(
            session_id,
            "turn-cache-disabled-audit-1",
            "第一轮：定位 system prompt volatility。",
            0,
        )
        .expect("build round1 request");

    let mut session = session_manager
        .load_or_create_session(session_id)
        .expect("load round1 session");
    handler.push_user_message(
        &mut session,
        "第一轮：定位 system prompt volatility。",
        JsonMap::new(),
    );
    handler.push_assistant_message(
        &mut session,
        "第一轮回答：volatility 来自当前目标段。",
        JsonMap::new(),
    );
    session_manager
        .save_session(&session)
        .expect("save round1 transcript");

    let round2 = engine
        .build_generate_driver_request(
            session_id,
            "turn-cache-disabled-audit-2",
            "第二轮：继续定位 system prompt volatility。",
            0,
        )
        .expect("build round2 request");

    let changed_sections = changed_system_prompt_sections(
        round1
            .prepared_prompt
            .system_prompt
            .as_deref()
            .expect("round1 system prompt"),
        round2
            .prepared_prompt
            .system_prompt
            .as_deref()
            .expect("round2 system prompt"),
    );

    assert!(changed_sections.is_empty());

    eprintln!(
        "prompt_cache_disabled_volatility_audit\n{}",
        serde_json::json!({
            "changedSystemSections": changed_sections,
            "round1SystemPromptHash": stable_text_hash(round1.prepared_prompt.system_prompt.as_deref().unwrap_or("")),
            "round2SystemPromptHash": stable_text_hash(round2.prepared_prompt.system_prompt.as_deref().unwrap_or("")),
        })
    );
}

#[test]
fn skills_cli_smoke_projects_metadata_and_preserves_bash_output_and_events() {
    let skill_root = std::env::temp_dir().join(format!(
        "centaeris_agent_runtime_skills_cli_smoke_{}_{}",
        std::process::id(),
        now_ms()
    ));
    let skill_dir = skill_root.join("external-research-tavily-cli");
    std::fs::create_dir_all(skill_dir.as_path()).expect("create skill dir");
    std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: external-research-tavily-cli\ndescription: Use when a task needs Tavily CLI research through bash without adding model tools.\nallowed-tools: [bash,read]\n---\n# Tavily CLI Research\nUse bash to run `tavily search --query <query> --json` and summarize stdout.\n",
        )
        .expect("write skill");
    let store = AgentRuntimeTestStore::new();
    let config = AgentRuntimeConfig {
        enable_prompt_compaction: false,
        ..Default::default()
    };
    let tool_layer = ToolLayer::new_with_skill_catalog_config(
        explicit_workspace_skill_catalog_config(skill_root.as_path(), skill_root.as_path()),
    )
    .with_cwd(skill_root.clone())
    .expect("set stage7 workspace root");
    let engine = AgentRuntime::new_for_test_with_tools(store.clone(), tool_layer, config);

    let request = engine
        .build_generate_driver_request(
            "chat-skills-cli-smoke",
            "turn-skills-cli-smoke",
            "Use tavily cli to check the latest dental evidence",
            0,
        )
        .expect("build generate request");

    assert_model_tool_definitions_match_default_projection(
        &request.prepared_prompt.tool_definitions,
    );
    let system_prompt = request
        .prepared_prompt
        .system_prompt
        .as_deref()
        .expect("system prompt");
    assert!(!system_prompt.contains("external-research-tavily-cli"));
    assert!(!system_prompt.contains("tavily search --query"));
    let context_messages_text = request
        .prepared_prompt
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(context_messages_text.contains("<available_skills>"));
    assert!(context_messages_text.contains("<name>external-research-tavily-cli</name>"));
    assert!(!context_messages_text.contains("tavily search --query"));

    let report = ToolExecutionResult {
        tool_call_id: "call-tavily-cli".to_string(),
        tool_name: "bash".to_string(),
        status: "ok".to_string(),
        content: "{\"title\":\"Dental implant guideline\"}".to_string(),
        details: serde_json::json!({
            "schema": "bash_result_v1",
            "command": "tavily search --query \"dental implant guideline\" --json",
            "exitCode": 0,
            "timedOut": false,
            "stdout": "{\"title\":\"Dental implant guideline\",\"url\":\"https://example.test/guideline\",\"snippet\":\"Current guideline recommends risk review before surgery.\"}",
            "stderr": ""
        }),
        facts: Vec::new(),
        error: None,
        started_at_ms: 1,
        completed_at_ms: 11,
        latency_ms: 10,
        parallel_group: None,
        transition_reason: None,
    };

    let operations_json = project_tool_operations_json(std::slice::from_ref(&report));
    let operations: Vec<Value> =
        serde_json::from_str(operations_json.as_deref().expect("operations json"))
            .expect("parse operations");
    let operation = operations.first().expect("operation");
    assert!(operation.get("title").is_none());
    assert_eq!(
        operation.get("kind").and_then(Value::as_str),
        Some("command")
    );
    assert!(operation.get("commandPreview").is_none());
    assert!(operation
        .get("outputPreview")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("Dental implant guideline"));
    let events = events::build_runtime_event_tool_result_events(
        "chat-skills-cli-smoke",
        "turn-skills-cli-smoke",
        std::slice::from_ref(&report),
        operations_json.as_deref(),
    )
    .expect("build tool result events");
    let event_payload = serde_json::to_value(events.first().expect("tool result event"))
        .expect("project event payload");
    let event_operation = event_payload
        .get("payload")
        .and_then(|payload| payload.get("operations"))
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .expect("event operation");
    assert_eq!(
        event_operation.get("kind").and_then(Value::as_str),
        Some("command")
    );
    assert!(event_operation
        .get("outputPreview")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("Dental implant guideline"));

    let _ = std::fs::remove_dir_all(skill_root);
}

#[tokio::test]
async fn prompt_compaction_persists_minimal_markdown_commit() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session = model_prompt_compaction_session("chat-compact-commit");
    session_manager
        .save_session(&session)
        .expect("save session");

    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    config.prompt_compaction_summary_max_tokens = 1_200;
    let engine = AgentRuntime::new_for_test(store.clone(), config);

    engine
        .build_generate_driver_request_with_async_driver(
            "chat-compact-commit",
            "turn-compact-commit",
            "new user request",
            0,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .expect("build generate request with model compaction");

    let session_snapshot = session_manager
        .load_or_create_session("chat-compact-commit")
        .expect("load compacted session");
    let summary = session_snapshot
        .messages
        .iter()
        .find(|message| {
            message.metadata.get("kind").map(String::as_str) == Some("context_compaction")
        })
        .expect("prompt compaction summary");
    assert!(!summary.content.trim().is_empty());
    assert!(summary.metadata.keys().all(|key| matches!(
        key.as_str(),
        "kind" | "compaction_id" | "first_kept_message_id"
    )));
    assert!(session_snapshot
        .messages
        .iter()
        .any(|message| message.content.contains("old model compaction request")));
}
#[tokio::test]
async fn model_prompt_compaction_generate_request_uses_model_client_summary() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session = model_prompt_compaction_session("chat-model-client-compact");
    session_manager
        .save_session(&session)
        .expect("save model compaction session");

    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    config.model_context_tokens = 12_000;
    config.model_max_output_tokens = 4_096;
    config.prompt_compaction_summary_max_tokens = 2_000;
    let engine = AgentRuntime::new_for_test(store.clone(), config);
    let model_client =
        PromptCompactionModelClient::new(PromptCompactionModelClientBehavior::ValidSummary);
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig {
            timeout_ms: 60_000,
            max_output_tokens: Some(4_096),
            ..ModelSessionConfig::default()
        }),
    };
    let driver = ModelClientGenerateDriver::new(&model_client, &config_store);

    let request = engine
        .build_generate_driver_request_with_async_driver(
            "chat-model-client-compact",
            "turn-model-client-compact",
            "Continue after model compaction.",
            0,
            &driver,
        )
        .await
        .expect("build generate request with model compaction");

    let model_requests = model_client.requests();
    assert_eq!(model_requests.len(), 1);
    let compaction_request = &model_requests[0];
    assert!(compaction_request.turn_id.starts_with("turn-"));
    assert_ne!(compaction_request.turn_id, "turn-model-client-compact");
    assert_eq!(compaction_request.prepared_prompt.messages.len(), 1);
    assert_eq!(compaction_request.prepared_prompt.max_output_tokens, 2_000);
    assert_eq!(
        compaction_request.session_config.max_output_tokens,
        Some(2_000)
    );
    assert_eq!(compaction_request.session_config.timeout_ms, 300_000);
    assert!(compaction_request.prepared_prompt.messages[0]
        .content
        .contains("[model_compaction_prompt_v1]"));
    assert!(compaction_request
        .prepared_prompt
        .tool_definitions
        .is_empty());
    assert_eq!(
        compaction_request.prepared_prompt.tool_choice,
        ModelToolChoice::None
    );

    let saved_session = session_manager
        .load_or_create_session("chat-model-client-compact")
        .expect("load saved session");
    let summary_message = saved_session
        .messages
        .iter()
        .find(|message| {
            message.metadata.get("kind").map(String::as_str) == Some("context_compaction")
        })
        .expect("summary message appended");
    assert!(summary_message.content.starts_with("# Goal"));
    assert!(saved_session
        .messages
        .iter()
        .any(|message| message.content.contains("old model compaction request")));
    let stats_json = saved_session
        .metadata
        .get("prompt_compaction_stats_json")
        .expect("prompt compaction stats");
    let stats_value = serde_json::from_str::<Value>(stats_json).expect("stats json");
    assert_eq!(
        stats_value
            .get("decision")
            .and_then(|value| value.get("strategy"))
            .and_then(Value::as_str),
        Some("model")
    );
    assert!(request
        .prepared_prompt
        .messages
        .iter()
        .any(|message| message.content.starts_with("# Goal")));
    assert_eq!(request.prepared_prompt.max_output_tokens, 4_096);
}

#[tokio::test]
async fn prompt_compaction_uses_full_prepared_prompt_pressure_before_budget_validation() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let mut session = SessionStateSnapshot::new("chat-full-prompt-pressure".to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig::default());
    let pressure_text = "history payload for assembled prompt pressure ".repeat(60);
    handler.push_user_message(&mut session, pressure_text.as_str(), JsonMap::new());
    handler.push_assistant_message(&mut session, pressure_text.as_str(), JsonMap::new());
    handler.push_user_message(&mut session, pressure_text.as_str(), JsonMap::new());
    handler.push_assistant_message(&mut session, pressure_text.as_str(), JsonMap::new());
    assign_test_message_ids(&mut session, "chat-full-prompt-pressure");
    session_manager
        .save_session(&session)
        .expect("save session");

    let config = AgentRuntimeConfig {
        model_context_tokens: 6_000,
        model_max_output_tokens: 1_200,
        prompt_compaction_trigger_headroom_tokens: u32::MAX,
        prompt_compaction_user_replay_tokens: 100,
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test(store.clone(), config);

    let request = engine
        .build_generate_driver_request_with_async_driver(
            "chat-full-prompt-pressure",
            "turn-full-prompt-pressure",
            "continue after pressure compaction",
            0,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .expect("full prepared prompt pressure should compact before budget validation");

    let saved_session = session_manager
        .load_or_create_session("chat-full-prompt-pressure")
        .expect("load compacted session");
    let stats = serde_json::from_str::<Value>(
        saved_session
            .metadata
            .get("prompt_compaction_stats_json")
            .expect("prompt compaction stats"),
    )
    .expect("parse prompt compaction stats");
    let history_tokens = stats
        .get("before_token_estimate")
        .and_then(Value::as_u64)
        .expect("history token estimate");
    let prompt_tokens = stats
        .get("decision")
        .and_then(|decision| decision.get("pressure"))
        .and_then(|pressure| pressure.get("estimatedInputTokens"))
        .and_then(Value::as_u64)
        .expect("assembled prompt token estimate");
    assert!(history_tokens < 3_600);
    assert!(prompt_tokens >= 3_600);
    assert_eq!(stats.get("triggered").and_then(Value::as_bool), Some(true));
    assert!(request.context_token_estimate <= 4_800);
}

#[tokio::test]
async fn prompt_compaction_runs_before_tool_loop_continuation_generate() {
    const MODEL_CONTEXT_TOKENS: u32 = 12_500;
    const MODEL_MAX_OUTPUT_TOKENS: u32 = 4_096;
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let mut session = model_prompt_compaction_session("chat-tool-continuation-pressure");
    let handler = MessageHandler::new(MessageHandlerConfig::default());
    session
        .messages
        .iter_mut()
        .rfind(|message| message.role == MessageRole::User)
        .expect("causal user anchor")
        .metadata
        .insert(
            MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
            MESSAGE_SEMANTIC_USER_REQUEST.to_string(),
        );
    handler.push_model_assistant_message(
        &mut session,
        "I will read the large file.",
        JsonMap::new(),
        ModelMessageSemanticsV1::Assistant {
            reasoning_content: None,
            tool_calls: vec![
                ModelToolCallStateV1 {
                    id: "call-large-read".to_string(),
                    name: "read".to_string(),
                    args_json: json!({"path":"large.txt"}).to_string(),
                },
                ModelToolCallStateV1 {
                    id: "call-small-read".to_string(),
                    name: "read".to_string(),
                    args_json: json!({"path":"small.txt"}).to_string(),
                },
            ],
        },
    );
    handler.push_model_tool_message(
        &mut session,
        "large read result ".repeat(700).as_str(),
        JsonMap::new(),
        ModelMessageSemanticsV1::ToolResult {
            tool_call_id: "call-large-read".to_string(),
            tool_name: "read".to_string(),
            status: "success".to_string(),
            result_state: "successWithOutput".to_string(),
            error_kind: None,
            object_refs: Vec::new(),
            transition_reason: None,
        },
    );
    handler.push_model_tool_message(
        &mut session,
        "small read result",
        JsonMap::new(),
        ModelMessageSemanticsV1::ToolResult {
            tool_call_id: "call-small-read".to_string(),
            tool_name: "read".to_string(),
            status: "success".to_string(),
            result_state: "successWithOutput".to_string(),
            error_kind: None,
            object_refs: Vec::new(),
            transition_reason: None,
        },
    );
    let mut lifecycle_metadata = JsonMap::new();
    lifecycle_metadata.insert(
        LIFECYCLE_HOOK_CONTEXT_META_KEY.to_string(),
        "true".to_string(),
    );
    handler.push_system_message(
        &mut session,
        "[Lifecycle hook context]\npressure receipt",
        lifecycle_metadata,
    );
    session_manager
        .save_session(&session)
        .expect("save session");

    let config = AgentRuntimeConfig {
        model_context_tokens: MODEL_CONTEXT_TOKENS,
        model_max_output_tokens: MODEL_MAX_OUTPUT_TOKENS,
        prompt_compaction_trigger_headroom_tokens: u32::MAX,
        prompt_compaction_user_replay_tokens: 3_000,
        agent_instructions: "Stable runtime context.".to_string(),
        ..Default::default()
    };
    let engine = AgentRuntime::new_for_test(store.clone(), config);
    let request = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            "chat-tool-continuation-pressure",
            "turn-tool-continuation-pressure",
            &TurnInput::ToolContinuation {
                objective: "continue after the large read".to_string(),
            },
            1,
            PromptCompactionScopeV1::main(),
            None,
            None,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .expect("tool continuation should compact before generate");

    assert!(request.compression_stats_json.is_some());
    assert!(request
        .prepared_prompt
        .messages
        .iter()
        .any(|message| message.content.starts_with("# Goal")));
    let messages = request.prepared_prompt.messages.as_slice();
    let lifecycle_index = messages
        .iter()
        .position(|message| message.content.contains("pressure receipt"))
        .expect("lifecycle context survives compaction");
    assert_eq!(
        messages[lifecycle_index].role,
        crate::model::prepared_prompt::ModelMessageRoleV1::User
    );
    let runtime_index = messages
        .iter()
        .position(|message| message.content.contains("Stable runtime context."))
        .expect("runtime context");
    let anchor_index = messages
        .iter()
        .position(|message| message.content == "recent user message should stay in suffix.")
        .expect("true user anchor survives compaction");
    let assistant_index = messages
        .iter()
        .position(|message| {
            message
                .tool_calls
                .iter()
                .any(|call| call.id == "call-large-read")
        })
        .expect("parallel assistant tool call survives compaction");
    assert!(lifecycle_index < runtime_index);
    assert!(runtime_index < anchor_index);
    assert!(anchor_index < assistant_index);
    assert_eq!(
        messages[messages.len() - 2].tool_call_id.as_deref(),
        Some("call-large-read")
    );
    assert_eq!(
        messages
            .last()
            .and_then(|message| message.tool_call_id.as_deref()),
        Some("call-small-read")
    );
    let saved = session_manager
        .load_or_create_session("chat-tool-continuation-pressure")
        .expect("load compacted continuation");
    let persisted_tool_index = saved
        .messages
        .iter()
        .position(|message| {
            matches!(
                saved.model_semantics_for(message.message_id.as_str()),
                Ok(ModelMessageSemanticsV1::ToolResult { tool_call_id, .. })
                    if tool_call_id == "call-small-read"
            )
        })
        .expect("persisted second tool result");
    let persisted_lifecycle_index = saved
        .messages
        .iter()
        .position(|message| message.content.contains("pressure receipt"))
        .expect("persisted lifecycle context");
    assert!(persisted_tool_index < persisted_lifecycle_index);
    assert!(request.context_token_estimate <= MODEL_CONTEXT_TOKENS - MODEL_MAX_OUTPUT_TOKENS);
}

#[tokio::test]
async fn pre_compact_hook_blocks_before_model_compaction_provider_request() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session = model_prompt_compaction_session("chat-pre-compact-hook-block");
    session_manager
        .save_session(&session)
        .expect("save model compaction session");

    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    config.model_context_tokens = 12_000;
    let hook_engine = LifecycleHookEngineV1::new(vec![LifecycleHookHandlerV1 {
        id: "pre-compact-block".to_string(),
        event: LifecycleHookEventNameV1::PreCompact,
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
    }])
    .expect("valid hook engine");
    let engine = AgentRuntime::new_for_test(store.clone(), config).with_lifecycle_hooks(
        QueryLifecycleHookRuntime::new(
            hook_engine,
            Arc::new(EventOutputHookRunner {
                outputs: vec![(
                    LifecycleHookEventNameV1::PreCompact,
                    json!({ "blockReason": "compaction paused by project policy" }).to_string(),
                )],
            }),
            None,
        ),
    );
    let model_client =
        PromptCompactionModelClient::new(PromptCompactionModelClientBehavior::ValidSummary);
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let driver = ModelClientGenerateDriver::new(&model_client, &config_store);

    let _request = engine
        .build_generate_driver_request_with_async_driver(
            "chat-pre-compact-hook-block",
            "turn-pre-compact-hook-block",
            "Continue after hook block.",
            0,
            &driver,
        )
        .await
        .expect("build request should continue without compacting");

    assert!(
        model_client.requests().is_empty(),
        "PreCompact block must prevent summary provider call"
    );
    let saved_session = session_manager
        .load_or_create_session("chat-pre-compact-hook-block")
        .expect("load saved session");
    let stats_value = serde_json::from_str::<Value>(
        saved_session
            .metadata
            .get("prompt_compaction_stats_json")
            .expect("blocked compaction stats"),
    )
    .expect("stats json");
    assert_eq!(
        stats_value
            .get("decision")
            .and_then(|decision| decision.get("action"))
            .and_then(Value::as_str),
        Some("blocked")
    );
    assert_eq!(
        stats_value
            .get("decision")
            .and_then(|decision| decision.get("strategy"))
            .and_then(Value::as_str),
        Some("pre_compact_hook")
    );
}

#[tokio::test]
async fn post_compact_hook_receives_bounded_succeeded_outcome() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session = model_prompt_compaction_session("chat-post-compact-hook-payload");
    session_manager
        .save_session(&session)
        .expect("save model compaction session");

    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    config.model_context_tokens = 12_000;
    config.prompt_compaction_summary_max_tokens = 2_000;
    let captured_events = Arc::new(Mutex::new(Vec::new()));
    let hook_engine = LifecycleHookEngineV1::new(vec![LifecycleHookHandlerV1 {
        id: "post-compact-capture".to_string(),
        event: LifecycleHookEventNameV1::PostCompact,
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
    }])
    .expect("valid hook engine");
    let engine = AgentRuntime::new_for_test(store.clone(), config).with_lifecycle_hooks(
        QueryLifecycleHookRuntime::new(
            hook_engine,
            Arc::new(CapturingHookRunner {
                outputs: vec![(LifecycleHookEventNameV1::PostCompact, "{}".to_string())],
                events: captured_events.clone(),
            }),
            None,
        ),
    );
    let model_client =
        PromptCompactionModelClient::new(PromptCompactionModelClientBehavior::ValidSummary);
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let driver = ModelClientGenerateDriver::new(&model_client, &config_store);

    let _request = engine
        .build_generate_driver_request_with_async_driver(
            "chat-post-compact-hook-payload",
            "turn-post-compact-hook-payload",
            "Continue after compaction.",
            0,
            &driver,
        )
        .await
        .expect("build request should compact");

    let events = captured_events.lock().expect("captured events lock");
    let post_compact = events
        .iter()
        .find(|event| event.event == LifecycleHookEventNameV1::PostCompact)
        .expect("PostCompact event");
    assert_eq!(
        post_compact.payload.get("schema").and_then(Value::as_str),
        Some("prompt_compaction_post_compact_hook_v1")
    );
    assert_eq!(
        post_compact.payload.get("status").and_then(Value::as_str),
        Some("succeeded")
    );
    assert_eq!(
        post_compact
            .payload
            .as_object()
            .expect("PostCompact payload object")
            .len(),
        7
    );
    assert!(post_compact.payload.get("summaryMarkdown").is_none());
}

#[tokio::test]
async fn model_prompt_compaction_provider_failure_does_not_fallback_to_template() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session = model_prompt_compaction_session("chat-model-client-fail");
    session_manager
        .save_session(&session)
        .expect("save model compaction session");

    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    config.model_context_tokens = 12_000;
    config.prompt_compaction_summary_max_tokens = 2_000;
    let engine = AgentRuntime::new_for_test(store.clone(), config);
    let model_client =
        PromptCompactionModelClient::new(PromptCompactionModelClientBehavior::ProviderError);
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let driver = ModelClientGenerateDriver::new(&model_client, &config_store);

    let request = engine
        .build_generate_driver_request_with_async_driver(
            "chat-model-client-fail",
            "turn-model-client-fail",
            "Continue despite compaction provider failure.",
            0,
            &driver,
        )
        .await
        .expect("build generate request records compaction failure");

    let saved_session = session_manager
        .load_or_create_session("chat-model-client-fail")
        .expect("load saved session");
    let messages_text = saved_session
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!messages_text.contains("# Goal"));
    assert!(saved_session
        .metadata
        .contains_key(PROMPT_COMPACTION_FAILURE_META_KEY));
    assert!(request
        .prepared_prompt
        .messages
        .iter()
        .any(|message| message.content.contains("old model compaction request")));
    let events = store
        .list_events("chat-model-client-fail", 100, 0)
        .expect("list events");
    assert!(events
        .iter()
        .any(|event| event.event_type == "prompt.compaction.failed"));
}

#[tokio::test]
async fn query_loop_failed_compaction_preserves_history_at_token_budget_boundary() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session_id = "chat-token-budget-boundary";
    let turn_id = "turn-token-budget-boundary";
    let user_message = "Continue without dropping history.";
    let session = model_prompt_compaction_session(session_id);
    let original_messages = serde_json::to_value(&session.messages).expect("original messages");
    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    config.model_context_tokens = 100_000;
    config.prompt_compaction_summary_max_tokens = 2_000;
    config.enable_prompt_compaction = false;
    session_manager
        .save_session(&session)
        .expect("save session");
    let baseline = AgentRuntime::new_for_test(store.clone(), config.clone())
        .build_generate_driver_request(session_id, turn_id, user_message, 0)
        .expect("measure full prompt token cost");

    for over_budget_by in [0, 1] {
        session_manager
            .save_session(&session)
            .expect("restore original history");
        config.enable_prompt_compaction = true;
        config.model_context_tokens =
            baseline.context_token_estimate + config.model_max_output_tokens - over_budget_by;
        let engine = AgentRuntime::new_for_test(store.clone(), config.clone());
        let model_client =
            PromptCompactionModelClient::new(PromptCompactionModelClientBehavior::ProviderError);
        let config_store = StaticModelSessionConfigStore {
            config: Some(ModelSessionConfig::default()),
        };
        let driver = ModelClientGenerateDriver::new(&model_client, &config_store);
        let result = engine
            .build_generate_driver_request_with_async_driver(
                session_id,
                turn_id,
                user_message,
                0,
                &driver,
            )
            .await;

        assert_eq!(
            model_client.requests().len(),
            1,
            "token pressure must attempt compaction"
        );
        let expected_message_count = if over_budget_by == 0 {
            let request = result.expect("exactly fitting input survives failed compaction");
            assert_eq!(
                request.context_token_estimate,
                baseline.context_token_estimate
            );
            assert_eq!(
                request.prepared_prompt.messages,
                baseline.prepared_prompt.messages
            );
            session.messages.len() + 1
        } else {
            assert!(result
                .expect_err("one token over budget must reject, not truncate")
                .contains("model_context_budget_exceeded"));
            session.messages.len()
        };
        let saved = session_manager
            .load_or_create_session(session_id)
            .expect("load history after failed compaction");
        assert_eq!(saved.messages.len(), expected_message_count);
        assert_eq!(saved.context_window.len(), expected_message_count);
        assert_eq!(
            serde_json::to_value(&saved.messages[..session.messages.len()])
                .expect("saved original messages"),
            original_messages
        );
        assert_eq!(
            serde_json::to_value(&saved.context_window[..session.messages.len()])
                .expect("original messages in active context"),
            original_messages
        );
        for (message_id, semantics) in &session.model_semantics {
            assert_eq!(saved.model_semantics.get(message_id), Some(semantics));
        }
        assert!(saved
            .metadata
            .contains_key(PROMPT_COMPACTION_FAILURE_META_KEY));
        assert!(saved.messages.iter().all(|message| {
            message.metadata.get("kind").map(String::as_str) != Some("context_compaction")
        }));
    }
}

#[tokio::test]
async fn query_loop_over_budget_continuation_preserves_history_when_compaction_cannot_run() {
    for circuit_open in [false, true] {
        let store = AgentRuntimeTestStore::new();
        let session_id = "chat-compaction-unavailable-budget";
        let turn_id = "turn-compaction-unavailable-budget";
        let mut session = model_prompt_compaction_session(session_id);
        append_prompt_test_tool_group(
            &MessageHandler::new(MessageHandlerConfig::default()),
            &mut session,
            &["call-budget-boundary"],
        );
        let original_messages = serde_json::to_value(&session.messages).expect("original messages");
        let mut config = AgentRuntimeConfig::default();
        force_prompt_compaction_for_tests(&mut config);
        config.enable_prompt_compaction = circuit_open;
        let engine = AgentRuntime::new_for_test(store.clone(), config);
        if circuit_open {
            for _ in 0..PROMPT_COMPACTION_CIRCUIT_FAILURE_THRESHOLD {
                engine.record_prompt_compaction_failure(
                    &mut session,
                    turn_id,
                    "provider",
                    "failed",
                );
            }
            assert!(prompt_compaction_circuit_is_open(&session));
        }
        let session_manager = SessionManager::new(store);
        session_manager
            .save_session(&session)
            .expect("save session");
        let model_client =
            PromptCompactionModelClient::new(PromptCompactionModelClientBehavior::ProviderError);
        let config_store = StaticModelSessionConfigStore {
            config: Some(ModelSessionConfig::default()),
        };
        let driver = ModelClientGenerateDriver::new(&model_client, &config_store);
        let error = engine
            .build_generate_driver_request_with_async_driver_and_runtime_scope(
                session_id,
                turn_id,
                &TurnInput::ToolContinuation {
                    objective: "Continue without dropping history.".to_string(),
                },
                1,
                PromptCompactionScopeV1::main(),
                None,
                None,
                &driver,
            )
            .await
            .expect_err("over-budget continuation must reject without truncation");
        assert!(error.contains("model_context_budget_exceeded"));
        assert!(model_client.requests().is_empty());
        let saved = session_manager
            .load_or_create_session(session_id)
            .expect("load unchanged history");
        assert_eq!(
            serde_json::to_value(&saved.messages).unwrap(),
            original_messages
        );
        assert_eq!(
            serde_json::to_value(&saved.context_window).unwrap(),
            original_messages
        );
        assert_eq!(saved.model_semantics, session.model_semantics);
        assert_eq!(prompt_compaction_circuit_is_open(&saved), circuit_open);
    }
}

#[tokio::test]
async fn prompt_compaction_low_pressure_writes_skipped_stats() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let mut session = SessionStateSnapshot::new("chat-compact-skip".to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig {
        max_message_chars: 10_000,
    });
    handler.push_user_message(&mut session, "short request", JsonMap::new());
    assign_test_message_ids(&mut session, "chat-compact-skip");
    session_manager
        .save_session(&session)
        .expect("save session");

    let config = AgentRuntimeConfig::default();
    let engine = AgentRuntime::new_for_test(store.clone(), config);
    let driver = TestPromptCompactionAsyncDriver;
    let _request = engine
        .build_generate_driver_request_with_async_driver(
            "chat-compact-skip",
            "turn-compact-skip",
            "continue",
            0,
            &driver,
        )
        .await
        .expect("build request with skipped compaction");

    let saved_session = session_manager
        .load_or_create_session("chat-compact-skip")
        .expect("load skipped session");
    let stats = serde_json::from_str::<Value>(
        saved_session
            .metadata
            .get("prompt_compaction_stats_json")
            .expect("skipped compaction stats"),
    )
    .expect("skipped stats json");
    assert_eq!(
        stats.get("reason").and_then(Value::as_str),
        Some("insufficient_messages")
    );
}

#[tokio::test]
async fn query_loop_continues_until_natural_final_while_usage_remains_observational() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let model_client = UnboundedToolLoopModelClient {
        request_count: AtomicUsize::new(0),
        tool_turns: 55,
        request_roles: Mutex::new(vec![]),
        request_tail_roles: Mutex::new(vec![]),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let user_message = format!(
        "Keep working until the task is complete.\n{}",
        "evidence ".repeat(6_000)
    );

    let mut provider_usages = Vec::new();
    let response = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            AgentRunRequest {
                session_id: "chat-unbounded-tool-loop".to_string(),
                agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
                    agent_run_id: "agent-run-unbounded-tool-loop".to_string(),
                    execution_id: "execution-unbounded-tool-loop".to_string(),
                    authorization_digest: format!("sha256:{}", "a".repeat(64)),
                }),
                initial_turn_id: "turn-unbounded-tool-loop".to_string(),
                user_message,
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |_| {},
            &|| Ok(None),
            &mut |safe_point| {
                if let ToolSafePoint::ProviderUsage { usage, .. } = safe_point {
                    provider_usages.push(usage);
                }
                Ok(())
            },
        )
        .await
        .expect("long tool loop must reach its natural final");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(response.turn_responses.len(), 56);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 56);
    assert_eq!(execution_count.load(Ordering::SeqCst), 55);
    let usage = &response
        .turn_responses
        .last()
        .expect("long tool loop final response")
        .agent_run_resource_usage;
    assert_eq!(usage.provider_attempts, 56);
    assert_eq!(usage.completed_provider_rounds, 56);
    assert!(usage.estimated_input_tokens > 500_000);
    assert_eq!(usage.actual_input_tokens, 2_240_000);
    assert_eq!(usage.prompt_cache_hit_tokens, 2_128_000);
    assert_eq!(usage.prompt_cache_miss_tokens, 112_000);
    assert_eq!(usage.output_tokens, 5_600);
    assert_eq!(usage.tool_call_count, 55);
    assert!(store
        .list_checkpoints("chat-unbounded-tool-loop", 100, 0)
        .expect("list ordinary turn checkpoints")
        .is_empty());
    assert_eq!(provider_usages.len(), 56);
    let persisted_usage = provider_usages
        .iter()
        .try_fold(
            crate::runtime::contracts::ProviderTokenUsageV1::default(),
            |sum, item| sum.checked_add(item),
        )
        .expect("aggregate provider usage");
    assert_eq!(
        provider_usages.last().and_then(|item| item.input_tokens),
        Some(40_000)
    );
    assert_eq!(
        provider_usages.last().and_then(|item| item.output_tokens),
        Some(100)
    );
    assert_eq!(persisted_usage.input_tokens, Some(2_240_000));
    assert_eq!(persisted_usage.output_tokens, Some(5_600));
    let request_roles = model_client
        .request_roles
        .lock()
        .expect("request roles lock");
    assert_eq!(request_roles[0], vec![MessageRole::User]);
    assert_eq!(
        request_roles[1],
        vec![MessageRole::User, MessageRole::Assistant, MessageRole::Tool]
    );
    for (index, roles) in request_roles.iter().enumerate().skip(1) {
        assert_eq!(
            roles.last(),
            Some(&MessageRole::Tool),
            "request {index} must end in a real tool result: {roles:?}"
        );
        assert!(
            roles
                .iter()
                .filter(|role| **role == MessageRole::User)
                .count()
                <= 1,
            "request {index} must not contain a synthetic user message: {roles:?}"
        );
    }
    let request_tail_roles = model_client
        .request_tail_roles
        .lock()
        .expect("request tail roles lock");
    assert!(request_tail_roles
        .iter()
        .skip(1)
        .all(|role| role == &Some(MessageRole::Tool)));
}

#[tokio::test]
async fn query_loop_external_cancel_stops_before_another_provider_request() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let model_client = UnboundedToolLoopModelClient {
        request_count: AtomicUsize::new(0),
        tool_turns: usize::MAX,
        request_roles: Mutex::new(vec![]),
        request_tail_roles: Mutex::new(vec![]),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let cancellation_execution_count = execution_count.clone();
    let cancellation_probe = || {
        Ok((cancellation_execution_count.load(Ordering::SeqCst) >= 30)
            .then(|| "test_watchdog".to_string()))
    };
    let mut stream_events = Vec::new();

    let response = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_async(
            AgentRunRequest {
                session_id: "chat-cancelled-long-tool-loop".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-cancelled-long-tool-loop".to_string(),
                user_message: "Keep working until externally cancelled.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| stream_events.push(event),
            &cancellation_probe,
        )
        .await
        .expect("external cancellation should stop the loop cleanly");

    assert_eq!(
        response.stop,
        AgentRunStop::Cancelled("test_watchdog".to_string())
    );
    assert_eq!(response.turn_responses.len(), 30);
    assert!(response
        .turn_responses
        .iter()
        .all(|turn| turn.continuation == QueryContinuation::ExecuteTools));
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 30);
    assert_eq!(execution_count.load(Ordering::SeqCst), 30);
    assert!(!stream_events
        .iter()
        .any(|event| matches!(event, TurnUpdate::RuntimeError { .. })));
    assert_eq!(
        response
            .turn_responses
            .last()
            .expect("last completed cancelled turn")
            .agent_run_resource_usage
            .provider_attempts,
        30
    );
}

#[tokio::test]
async fn cancellation_drops_in_flight_provider_request_and_keeps_session_input() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());
    let dropped = Arc::new(AtomicBool::new(false));
    let model_client = PendingStreamModelClient {
        dropped: dropped.clone(),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let polls = AtomicUsize::new(0);
    let cancellation_probe = || {
        Ok((polls.fetch_add(1, Ordering::SeqCst) >= 2)
            .then(|| "agent_run_cancel_requested".to_string()))
    };
    let mut events = Vec::new();

    let response = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_async(
            AgentRunRequest {
                session_id: "chat-cancel-in-flight-provider".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-cancel-in-flight-provider".to_string(),
                user_message: "Keep this input after Stop.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| events.push(event),
            &cancellation_probe,
        )
        .await
        .expect("cancel in-flight provider request");

    assert_eq!(
        response.stop,
        AgentRunStop::Cancelled("agent_run_cancel_requested".to_string())
    );
    assert!(dropped.load(Ordering::SeqCst));
    assert!(events.iter().any(|event| {
        matches!(event, TurnUpdate::ModelDone { finish_reason: Some(reason), .. } if reason == "user_cancelled")
    }));
    assert!(!events
        .iter()
        .any(|event| matches!(event, TurnUpdate::RuntimeError { .. })));
    let session = engine
        .session_manager
        .load_or_create_session("chat-cancel-in-flight-provider")
        .expect("load cancelled session snapshot");
    assert!(session
        .messages
        .iter()
        .any(|message| message.content == "Keep this input after Stop."));
}

#[tokio::test]
async fn query_loop_tool_executes_only_after_model_response_completes() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let model_client = StreamExecutionBoundaryModelClient {
        request_count: AtomicUsize::new(0),
        execution_count: execution_count.clone(),
        turn_control: None,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut stream_events = Vec::new();

    let response = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_async(
            AgentRunRequest {
                session_id: "chat-stream-boundary".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-stream-boundary".to_string(),
                user_message: "Run the tool, then finish.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| stream_events.push(event),
            &|| Ok(None),
        )
        .await
        .expect("stream boundary run");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(response.turn_responses[0].tool_results.len(), 1);
    assert!(stream_events.iter().any(|event| matches!(
        event,
        TurnUpdate::ToolCallReady { call_id, .. } if call_id == "call-stream-boundary"
    )));
}

#[tokio::test]
async fn turn_supplement_waits_for_tool_batch_and_continues_the_same_loop() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let turn_control = TurnControl::new();
    let model_client = StreamExecutionBoundaryModelClient {
        request_count: AtomicUsize::new(0),
        execution_count: execution_count.clone(),
        turn_control: Some(turn_control.clone()),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut stream_events = Vec::new();

    let response = engine
        .process_turn_loop_online_with_model_client_stream_controlled_async(
            AgentRunRequest {
                session_id: "chat-turn-supplement-safe-point".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-supplement-safe-point".to_string(),
                user_message: "Run the tool, then follow my update.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| stream_events.push(event),
            &|| Ok(None),
            &turn_control,
        )
        .await
        .expect("same-loop supplement run");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(response.turn_responses.len(), 2);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert!(response
        .turn_responses
        .last()
        .expect("final response")
        .session_snapshot
        .messages
        .iter()
        .any(|message| {
            message.content == "Use the completed tool result, then change direction."
                && message
                    .metadata
                    .get(MESSAGE_SEMANTIC_KIND_META_KEY)
                    .map(String::as_str)
                    == Some(MESSAGE_SEMANTIC_TURN_SUPPLEMENT)
        }));
    let closed_error = turn_control
        .enqueue_supplement_with("late supplement".to_string(), || Ok(()))
        .expect_err("terminal task must reject late supplements");
    assert!(closed_error.contains("no longer accepting input"));
}

#[tokio::test]
async fn turn_supplement_replaces_a_toolless_final_at_the_provider_boundary() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());
    let turn_control = TurnControl::new();
    let model_client = NoToolSupplementBoundaryModelClient {
        request_count: AtomicUsize::new(0),
        turn_control: turn_control.clone(),
        enqueue_supplement: true,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };

    let response = engine
        .process_turn_loop_online_with_model_client_stream_controlled_async(
            AgentRunRequest {
                session_id: "chat-turn-supplement-provider-boundary".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-supplement-provider-boundary".to_string(),
                user_message: "Answer, but accept an update before finalizing.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |_| {},
            &|| Ok(None),
            &turn_control,
        )
        .await
        .expect("tool-free supplement run");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert!(response
        .turn_responses
        .last()
        .expect("final response")
        .session_snapshot
        .messages
        .iter()
        .any(|message| message.content == "final with newer constraint"));
}

#[tokio::test]
async fn answer_now_during_natural_final_does_not_abort_replace_or_restart_provider() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let turn_control = TurnControl::new();
    engine
        .persist_answer_now_requested(
            "chat-answer-now-natural",
            "turn-answer-now-natural",
            &AgentRunInterventionV1::answer_now("intervention-answer-now", "agent-run-answer-now"),
            "test.user",
        )
        .expect("persist requested answer-now");
    let model_client = AnswerNowBoundaryModelClient {
        request_count: AtomicUsize::new(0),
        with_tool: false,
        enqueue_intervention: true,
        execution_count,
        turn_control: turn_control.clone(),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut stream_events = Vec::new();

    let response = engine
        .process_turn_loop_online_with_model_client_stream_controlled_async(
            AgentRunRequest {
                session_id: "chat-answer-now-natural".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-answer-now-natural".to_string(),
                user_message: "Research, unless the current answer is enough.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| stream_events.push(event),
            &|| Ok(None),
            &turn_control,
        )
        .await
        .expect("natural final satisfies answer-now");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 1);
    assert!(!stream_events
        .iter()
        .any(|event| matches!(event, TurnUpdate::ReplaceContent { .. })));
}

#[tokio::test]
async fn answer_now_waits_for_tool_terminal_then_runs_one_toolless_convergence_request() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let turn_control = TurnControl::new();
    engine
        .persist_answer_now_requested(
            "chat-answer-now-tool",
            "turn-answer-now-tool",
            &AgentRunInterventionV1::answer_now("intervention-answer-now", "agent-run-answer-now"),
            "test.user",
        )
        .expect("persist requested answer-now");
    let model_client = AnswerNowBoundaryModelClient {
        request_count: AtomicUsize::new(0),
        with_tool: true,
        enqueue_intervention: true,
        execution_count: execution_count.clone(),
        turn_control: turn_control.clone(),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let mut stream_events = Vec::new();

    let response = engine
        .process_turn_loop_online_with_model_client_stream_controlled_async(
            AgentRunRequest {
                session_id: "chat-answer-now-tool".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-answer-now-tool".to_string(),
                user_message: "Use one tool, then answer.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |event| stream_events.push(event),
            &|| Ok(None),
            &turn_control,
        )
        .await
        .expect("answer-now tool boundary run");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert!(!stream_events
        .iter()
        .any(|event| matches!(event, TurnUpdate::ReplaceContent { .. })));
    assert!(response
        .turn_responses
        .last()
        .expect("convergence final")
        .session_snapshot
        .messages
        .iter()
        .any(|message| {
            message
                .metadata
                .get(MESSAGE_SEMANTIC_KIND_META_KEY)
                .map(String::as_str)
                == Some(MESSAGE_SEMANTIC_ANSWER_NOW)
        }));
}

#[tokio::test]
async fn answer_now_requested_fact_recovers_after_memory_loss() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let intervention = AgentRunInterventionV1::answer_now(
        "intervention-answer-now-recovered",
        "agent-run-answer-now-recovered",
    );
    let first_event = engine
        .persist_answer_now_requested(
            "chat-answer-now-recovered",
            "turn-answer-now-recovered",
            &intervention,
            "test.user",
        )
        .expect("persist requested intervention");
    let replayed_event = engine
        .persist_answer_now_requested(
            "chat-answer-now-recovered",
            "turn-answer-now-recovered",
            &intervention,
            "test.user",
        )
        .expect("idempotently persist requested intervention");
    assert_eq!(first_event.event_id, replayed_event.event_id);
    assert_eq!(
        engine
            .persist_answer_now_requested(
                "chat-answer-now-recovered",
                "turn-answer-now-recovered",
                &AgentRunInterventionV1::answer_now(
                    "intervention-answer-now-conflict",
                    "agent-run-answer-now-recovered",
                ),
                "test.user",
            )
            .expect_err("a second active intervention must be rejected"),
        "alreadyConverging"
    );

    let turn_control = TurnControl::new();
    let model_client = AnswerNowBoundaryModelClient {
        request_count: AtomicUsize::new(0),
        with_tool: true,
        enqueue_intervention: false,
        execution_count: execution_count.clone(),
        turn_control: turn_control.clone(),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let response = engine
        .process_turn_loop_online_with_model_client_stream_controlled_async(
            AgentRunRequest {
                session_id: "chat-answer-now-recovered".to_string(),
                agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
                    agent_run_id: "agent-run-answer-now-recovered".to_string(),
                    execution_id: "execution-answer-now-recovered".to_string(),
                    authorization_digest: format!("sha256:{}", "c".repeat(64)),
                }),
                initial_turn_id: "turn-answer-now-recovered".to_string(),
                user_message: "Use one tool, then answer immediately.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |_| {},
            &|| Ok(None),
            &turn_control,
        )
        .await
        .expect("recover requested answer-now from durable fact");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    let projected_interventions = response
        .turn_responses
        .iter()
        .flat_map(|turn| turn.runtime_events.iter())
        .filter_map(|event| {
            serde_json::to_value(event)
                .ok()
                .filter(|payload| {
                    payload.get("type").and_then(Value::as_str)
                        == Some("AgentRunInterventionChanged")
                })
                .and_then(|payload| {
                    payload
                        .get("payload")
                        .and_then(|value| value.get("status"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
        })
        .collect::<Vec<_>>();
    assert!(projected_interventions
        .windows(2)
        .any(|statuses| statuses == ["requested", "applied"]));
    let changes = store
        .list_events("chat-answer-now-recovered", 100, 0)
        .expect("list intervention events")
        .into_iter()
        .filter(|event| {
            event.event_type == crate::runtime::contracts::AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1
        })
        .map(|event| {
            serde_json::from_str::<AgentRunInterventionChangedV1>(event.payload_json.as_str())
                .expect("decode intervention event")
        })
        .collect::<Vec<_>>();
    assert_eq!(changes.len(), 2);
    assert!(changes
        .iter()
        .any(|change| change.status == AgentRunInterventionStatusV1::Requested));
    assert!(changes
        .iter()
        .any(|change| change.status == AgentRunInterventionStatusV1::Applied));
}

#[tokio::test]
async fn query_loop_runtime_job_wait_resumes_once_with_one_terminal_tool_result() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let captured_hook_events = Arc::new(Mutex::new(Vec::new()));
    let hook_engine = LifecycleHookEngineV1::new(vec![LifecycleHookHandlerV1 {
        id: "runtime-wait-post-tool".to_string(),
        event: LifecycleHookEventNameV1::PostToolUse,
        matcher: None,
        source: LifecycleHookSourceV1 {
            kind: LifecycleHookSourceKindV1::Project,
            name: "project".to_string(),
        },
        trusted: true,
        program: "unused".to_string(),
        args: vec![],
        cwd: None,
        timeout_ms: 1_000,
    }])
    .expect("valid lifecycle hook engine");
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        runtime_job_wait_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    )
    .with_lifecycle_hooks(QueryLifecycleHookRuntime::new(
        hook_engine,
        Arc::new(CapturingHookRunner {
            outputs: vec![(
                LifecycleHookEventNameV1::PostToolUse,
                json!({"additionalContext": [{"text": "runtime wait receipt context"}]})
                    .to_string(),
            )],
            events: captured_hook_events.clone(),
        }),
        None,
    ));
    let model_client = RuntimeJobWaitModelClient {
        request_count: AtomicUsize::new(0),
        follow_up_tool_result_count: AtomicUsize::new(0),
        expected_tool_result_fragment: "durable background evidence",
        expect_toolless_follow_up: false,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let agent_run_identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "agent-run-runtime-wait".to_string(),
        execution_id: "execution-runtime-wait".to_string(),
        authorization_digest: format!("sha256:{}", "a".repeat(64)),
    };

    let waiting = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-runtime-wait".to_string(),
                agent_run_identity: Some(agent_run_identity.clone()),
                initial_turn_id: "turn-runtime-wait".to_string(),
                user_message: "Wait for the durable result, then answer.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("enter runtime job wait");

    assert_eq!(waiting.stop, AgentRunStop::RuntimeJobWait);
    assert_eq!(waiting.turn_responses.len(), 1);
    assert!(waiting.turn_responses[0].tool_results.is_empty());
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        engine
            .pending_runtime_job_wait_identity("chat-runtime-wait")
            .expect("load pending runtime wait identity"),
        Some(("turn-runtime-wait".to_string(), agent_run_identity.clone()))
    );
    let wait_checkpoint = serde_json::from_str::<RuntimeAwaitJobCheckpointV1>(
        waiting.turn_responses[0]
            .checkpoint
            .as_ref()
            .expect("runtime wait checkpoint")
            .payload_json
            .as_str(),
    )
    .expect("runtime wait checkpoint");
    assert_eq!(wait_checkpoint.waits.len(), 1);
    let job_id = wait_checkpoint.waits[0].job_id.clone();

    complete_runtime_wait_test_job(&store, job_id, "chat-other-session");

    let scope_error = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-runtime-wait".to_string(),
                agent_run_identity: Some(agent_run_identity.clone()),
                initial_turn_id: "turn-runtime-wait-wrong-scope".to_string(),
                user_message: "Wait for the durable result, then answer.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: Some("turn-runtime-wait".to_string()),
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect_err("cross-session output reference must fail");
    assert!(scope_error.contains("runtime_job_wait_output_ref_scope_mismatch"));
    link_runtime_wait_test_object(&store, "chat-runtime-wait");

    let resumed = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-runtime-wait".to_string(),
                agent_run_identity: Some(agent_run_identity.clone()),
                initial_turn_id: "turn-runtime-wait-resumed".to_string(),
                user_message: "Wait for the durable result, then answer.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: Some("turn-runtime-wait".to_string()),
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("resume terminal runtime job");

    assert_eq!(resumed.stop, AgentRunStop::Finalized);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert!(engine
        .pending_runtime_job_wait_identity("chat-runtime-wait")
        .expect("load cleared runtime wait identity")
        .is_none());
    assert_eq!(
        model_client
            .follow_up_tool_result_count
            .load(Ordering::SeqCst),
        1
    );
    assert_eq!(
        resumed
            .turn_responses
            .iter()
            .flat_map(|response| response.tool_results.iter())
            .count(),
        1
    );

    let replay_error = engine
        .resume_turn_with_agent_run_identity_async(
            "chat-runtime-wait",
            "turn-runtime-wait",
            Some(&agent_run_identity),
        )
        .await
        .expect_err("consumed runtime wait cannot be resumed twice");
    assert!(replay_error.contains("runtime wait checkpoint missing"));
    assert_eq!(
        captured_hook_events
            .lock()
            .expect("captured hooks")
            .iter()
            .filter(|event| event.event == LifecycleHookEventNameV1::PostToolUse)
            .count(),
        1,
        "resolved wait replay must use the durable post-hook receipt"
    );
    let session = SessionManager::new(store.clone())
        .load_session("chat-runtime-wait")
        .expect("load session")
        .expect("runtime wait session");
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::Tool)
            .count(),
        1,
        "replaying the resolved checkpoint must not duplicate ToolResult"
    );
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| {
                message
                    .metadata
                    .get(LIFECYCLE_HOOK_CONTEXT_META_KEY)
                    .is_some_and(|value| value == "true")
                    && message.content.contains("runtime wait receipt context")
            })
            .count(),
        1,
        "resolved wait replay must not duplicate lifecycle hook context"
    );
}

#[test]
fn shared_runtime_job_scope_allows_global_or_exact_session_only() {
    assert!(loop_runtime::runtime_job_session_scope_matches(
        None, "chat-1"
    ));
    assert!(loop_runtime::runtime_job_session_scope_matches(
        Some("chat-1"),
        "chat-1"
    ));
    assert!(!loop_runtime::runtime_job_session_scope_matches(
        Some("chat-2"),
        "chat-1"
    ));
}

#[tokio::test]
async fn runtime_job_wait_restores_checkpoint_from_durable_pending_batch() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        runtime_job_wait_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let model_client = RuntimeJobWaitModelClient {
        request_count: AtomicUsize::new(0),
        follow_up_tool_result_count: AtomicUsize::new(0),
        expected_tool_result_fragment: "durable background evidence",
        expect_toolless_follow_up: false,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let agent_run_identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "agent-run-runtime-wait-restore".to_string(),
        execution_id: "execution-runtime-wait-restore".to_string(),
        authorization_digest: format!("sha256:{}", "f".repeat(64)),
    };

    let waiting = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-runtime-wait-restore".to_string(),
                agent_run_identity: Some(agent_run_identity.clone()),
                initial_turn_id: "turn-runtime-wait-restore".to_string(),
                user_message: "Start the durable background tool.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("enter runtime job wait");
    assert_eq!(waiting.stop, AgentRunStop::RuntimeJobWait);

    let session = SessionManager::new(store.clone())
        .load_session("chat-runtime-wait-restore")
        .expect("load session")
        .expect("runtime wait session");
    let pending = serde_json::from_str::<PendingRuntimeToolBatchV1>(
        session
            .metadata
            .get(RUNTIME_PENDING_TOOL_BATCH_META_KEY)
            .expect("pending runtime batch"),
    )
    .expect("decode pending batch");
    assert_eq!(pending.turn_id, "turn-runtime-wait-restore");
    store
        .save_checkpoint(CheckpointRecord {
            checkpoint_id: "checkpoint:runtime-wait-restore".to_string(),
            kind: crate::runtime::contracts::CheckpointKindV1::Wait,
            session_id: "chat-runtime-wait-restore".to_string(),
            turn_id: "turn-runtime-wait-restore".to_string(),
            status: "running".to_string(),
            done_reason: None,
            updated_at_ms: now_ms(),
            payload_json: "{}".to_string(),
        })
        .expect("simulate crash before wait checkpoint commit");

    let error = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-runtime-wait-restore".to_string(),
                agent_run_identity: Some(agent_run_identity),
                initial_turn_id: "turn-runtime-wait-restore-resume".to_string(),
                user_message: "Resume the same wait.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: Some("turn-runtime-wait-restore".to_string()),
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect_err("conflicting runtime wait checkpoint must loud-fail");

    assert!(error.contains("runtime_job_wait_checkpoint_identity_conflict"));
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn answer_now_abandons_only_the_runtime_job_waiter() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        runtime_job_wait_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let model_client = RuntimeJobWaitModelClient {
        request_count: AtomicUsize::new(0),
        follow_up_tool_result_count: AtomicUsize::new(0),
        expected_tool_result_fragment: "immediate answer",
        expect_toolless_follow_up: true,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let agent_run_identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "agent-run-runtime-wait".to_string(),
        execution_id: "execution-runtime-wait".to_string(),
        authorization_digest: format!("sha256:{}", "d".repeat(64)),
    };
    let waiting = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-runtime-wait".to_string(),
                agent_run_identity: Some(agent_run_identity.clone()),
                initial_turn_id: "turn-runtime-wait".to_string(),
                user_message: "Start the background investigation.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("enter runtime wait");
    assert_eq!(waiting.stop, AgentRunStop::RuntimeJobWait);
    let wait_checkpoint = serde_json::from_str::<RuntimeAwaitJobCheckpointV1>(
        waiting.turn_responses[0]
            .checkpoint
            .as_ref()
            .expect("runtime wait checkpoint")
            .payload_json
            .as_str(),
    )
    .expect("runtime wait checkpoint");
    let job_id = wait_checkpoint.waits[0].job_id.clone();

    engine
        .persist_answer_now_requested(
            "chat-runtime-wait",
            "turn-runtime-wait",
            &AgentRunInterventionV1::answer_now(
                "intervention-answer-now-runtime-wait",
                agent_run_identity.agent_run_id.clone(),
            ),
            "test.user",
        )
        .expect("persist answer-now while waiting");
    let resumed = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-runtime-wait".to_string(),
                agent_run_identity: Some(agent_run_identity),
                initial_turn_id: "turn-runtime-wait-answer-now".to_string(),
                user_message: "Answer with what is already available.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: Some("turn-runtime-wait".to_string()),
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("abandon waiter and converge");

    assert_eq!(resumed.stop, AgentRunStop::Finalized);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(resumed.turn_responses[0].tool_results.len(), 1);
    assert_eq!(
        resumed.turn_responses[0].tool_results[0].status,
        "cancelled"
    );
    assert_eq!(
        resumed.turn_responses[0].tool_results[0]
            .transition_reason
            .as_deref(),
        Some("cancelled_by_answer_now")
    );
    assert_eq!(
        store
            .get_runtime_job(job_id.as_str())
            .expect("load shared background job")
            .expect("background job")
            .status,
        RuntimeJobStatus::Queued,
        "answer-now abandons the waiter without cancelling the shared job"
    );
    assert!(resumed.turn_responses[0]
        .runtime_events
        .iter()
        .any(|event| {
            serde_json::to_value(event).ok().is_some_and(|payload| {
                payload.get("type").and_then(Value::as_str) == Some("RuntimeWaitChanged")
                    && payload
                        .get("payload")
                        .and_then(|value| value.get("status"))
                        .and_then(Value::as_str)
                        == Some("abandoned")
            })
        }));
    let applied = store
        .list_events("chat-runtime-wait", 100, 0)
        .expect("list runtime-wait intervention events")
        .into_iter()
        .filter(|event| {
            event.event_type == crate::runtime::contracts::AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1
        })
        .filter_map(|event| {
            serde_json::from_str::<AgentRunInterventionChangedV1>(event.payload_json.as_str()).ok()
        })
        .find(|change| change.status == AgentRunInterventionStatusV1::Applied)
        .expect("runtime-wait applied intervention");
    assert_eq!(
        applied.safe_boundary.as_deref(),
        Some("runtime_job_wait_boundary")
    );
}

#[tokio::test]
async fn agent_run_cancellation_abandons_runtime_job_waiter_before_terminal_commit() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        runtime_job_wait_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let model_client = RuntimeJobWaitModelClient {
        request_count: AtomicUsize::new(0),
        follow_up_tool_result_count: AtomicUsize::new(0),
        expected_tool_result_fragment: "run was cancelled",
        expect_toolless_follow_up: true,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let agent_run_identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "agent-run-cancel-runtime-wait".to_string(),
        execution_id: "execution-cancel-runtime-wait".to_string(),
        authorization_digest: format!("sha256:{}", "e".repeat(64)),
    };
    let waiting = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-cancel-runtime-wait".to_string(),
                agent_run_identity: Some(agent_run_identity.clone()),
                initial_turn_id: "turn-cancel-runtime-wait".to_string(),
                user_message: "Start a background job.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("enter runtime wait");
    assert_eq!(waiting.stop, AgentRunStop::RuntimeJobWait);
    let job_id = serde_json::from_str::<RuntimeAwaitJobCheckpointV1>(
        waiting.turn_responses[0]
            .checkpoint
            .as_ref()
            .expect("runtime wait checkpoint")
            .payload_json
            .as_str(),
    )
    .expect("runtime wait checkpoint")
    .waits[0]
        .job_id
        .clone();

    let mut safe_point_results = Vec::new();
    let cancelled = engine
        .process_turn_loop_online_with_model_client_stream_cancellable_and_tool_safe_point_async(
            AgentRunRequest {
                session_id: "chat-cancel-runtime-wait".to_string(),
                agent_run_identity: Some(agent_run_identity),
                initial_turn_id: "turn-cancel-runtime-wait".to_string(),
                user_message: "Start a background job.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |_| {},
            &|| Ok(Some("agent_run_cancel_requested".to_string())),
            &mut |safe_point| {
                if let ToolSafePoint::CompletedTurn(result) = safe_point {
                    safe_point_results.extend(result.tool_results);
                }
                Ok(())
            },
        )
        .await
        .expect("cancel runtime wait");

    assert_eq!(
        cancelled.stop,
        AgentRunStop::Cancelled("agent_run_cancel_requested".to_string())
    );
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 1);
    assert_eq!(safe_point_results.len(), 1);
    assert_eq!(safe_point_results[0].status, "cancelled");
    assert_eq!(
        safe_point_results[0].transition_reason.as_deref(),
        Some("agent_run_cancelled")
    );
    assert!(!cancelled.turn_responses[0]
        .session_snapshot
        .metadata
        .contains_key(RUNTIME_PENDING_TOOL_BATCH_META_KEY));
    assert_eq!(
        store
            .get_runtime_job(job_id.as_str())
            .expect("load shared background job")
            .expect("background job")
            .status,
        RuntimeJobStatus::Queued
    );
    let abandoned = store
        .list_events("chat-cancel-runtime-wait", 100, 0)
        .expect("list runtime events")
        .into_iter()
        .filter(|event| {
            event.event_type == crate::runtime::contracts::RUNTIME_WAIT_CHANGED_SCHEMA_V1
        })
        .filter_map(|event| {
            serde_json::from_str::<RuntimeWaitChangedV1>(event.payload_json.as_str()).ok()
        })
        .find(|change| change.status == RuntimeWaitStatusV1::Abandoned)
        .expect("abandoned runtime waiter event");
    assert_eq!(abandoned.transition_reason, "agent_run_cancelled");
}

#[tokio::test]
async fn query_loop_answer_now_closes_question_wait_before_toolless_convergence() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store.clone(), AgentRuntimeConfig::default());
    let agent_run_identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "agent-run-answer-now-question".to_string(),
        execution_id: "execution-answer-now-question".to_string(),
        authorization_digest: format!("sha256:{}", "a".repeat(64)),
    };
    let wait_turn_id = "turn-answer-now-question";
    let waiting = crate::runtime::contracts::RuntimeAwaitQuestionCheckpointV1::new(
        &agent_run_identity,
        wait_turn_id,
        "question-1",
    )
    .expect("question wait");
    store
        .save_checkpoint(CheckpointRecord {
            checkpoint_id: "checkpoint:answer-now-question".to_string(),
            kind: crate::runtime::contracts::CheckpointKindV1::Wait,
            session_id: "chat-answer-now-question".to_string(),
            turn_id: wait_turn_id.to_string(),
            status: "paused_question".to_string(),
            done_reason: Some("question".to_string()),
            updated_at_ms: now_ms(),
            payload_json: serde_json::to_string(&waiting).expect("serialize question checkpoint"),
        })
        .expect("persist question checkpoint");
    engine
        .persist_answer_now_requested(
            "chat-answer-now-question",
            wait_turn_id,
            &AgentRunInterventionV1::answer_now(
                "intervention-answer-now-question",
                agent_run_identity.agent_run_id.clone(),
            ),
            "test.user",
        )
        .expect("persist answer-now during question wait");
    let model_client = AnswerNowBoundaryModelClient {
        request_count: AtomicUsize::new(0),
        with_tool: false,
        enqueue_intervention: false,
        execution_count: Arc::new(AtomicUsize::new(0)),
        turn_control: TurnControl::new(),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };

    let resumed = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-answer-now-question".to_string(),
                agent_run_identity: Some(agent_run_identity),
                initial_turn_id: "turn-answer-now-question-resume".to_string(),
                user_message: "Answer now instead of waiting.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: Some(wait_turn_id.to_string()),
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("close question wait and converge");

    assert_eq!(resumed.stop, AgentRunStop::Finalized);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 1);
    assert_eq!(resumed.turn_responses.len(), 2);
    assert!(resumed.turn_responses[0].checkpoint.is_none());
    assert!(store
        .load_checkpoint_by_turn("chat-answer-now-question", wait_turn_id)
        .expect("load question checkpoint")
        .is_none());
    let applied = store
        .list_events("chat-answer-now-question", 100, 0)
        .expect("list question intervention events")
        .into_iter()
        .filter(|event| {
            event.event_type == crate::runtime::contracts::AGENT_RUN_INTERVENTION_CHANGED_SCHEMA_V1
        })
        .filter_map(|event| {
            serde_json::from_str::<AgentRunInterventionChangedV1>(event.payload_json.as_str()).ok()
        })
        .find(|change| change.status == AgentRunInterventionStatusV1::Applied)
        .expect("question applied intervention");
    assert_eq!(
        applied.safe_boundary.as_deref(),
        Some("question_wait_boundary")
    );
}

#[tokio::test]
async fn runtime_job_wait_rechecks_a_precompleted_job_before_yielding() {
    let store = AgentRuntimeTestStore::new();
    let agent_run_identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "agent-run-runtime-wait".to_string(),
        execution_id: "execution-runtime-wait".to_string(),
        authorization_digest: format!("sha256:{}", "b".repeat(64)),
    };
    let job_id = build_provider_poll_runtime_job_id(
        "chat-runtime-wait",
        "agent-run-runtime-wait",
        "turn-runtime-wait",
        "call-runtime-wait",
        "test.runtime_wait",
        "runtime_wait_test_tool",
        "runtime-wait-ticket",
    );
    let at_ms = now_ms();
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: RuntimeJobRecord {
                job_id: job_id.clone(),
                job_kind: PROVIDER_POLL_RUNTIME_JOB_KIND.to_string(),
                status: RuntimeJobStatus::Queued,
                run_at_ms: 0,
                lease_owner: None,
                lease_expires_at_ms: None,
                heartbeat_at_ms: None,
                retry_count: 0,
                max_retries: 4,
                backoff_policy: RuntimeBackoffPolicy::default(),
                idempotency_key: "provider.poll:chat-runtime-wait:agent-run-runtime-wait:test.runtime_wait:runtime_wait_test_tool:runtime-wait-ticket".to_string(),
                session_id: Some("chat-runtime-wait".to_string()),
                branch_id: None,
                checkpoint_id: None,
                payload_ref: Some(
                    build_provider_poll_payload_ref(&ProviderPollingRuntimePayload {
                        provider_id: "test.runtime_wait".to_string(),
                        tool_name: "runtime_wait_test_tool".to_string(),
                        poll_key: "runtime-wait-ticket".to_string(),
                        poll_args: json!({"ticket": "runtime-wait-ticket"}),
                        source_agent_run_id: agent_run_identity.agent_run_id.clone(),
                        source_turn_id: "turn-runtime-wait".to_string(),
                        source_tool_call_id: "call-runtime-wait".to_string(),
                        lease_ms: 30_000,
                    })
                    .expect("precompleted provider poll payload"),
                ),
                output_refs: Vec::new(),
                last_error: None,
                created_at_ms: at_ms,
                updated_at_ms: at_ms,
            },
        })
        .expect("schedule precompleted provider job");
    complete_runtime_wait_test_job(&store, job_id, "chat-runtime-wait");

    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        runtime_job_wait_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let model_client = RuntimeJobWaitModelClient {
        request_count: AtomicUsize::new(0),
        follow_up_tool_result_count: AtomicUsize::new(0),
        expected_tool_result_fragment: "durable background evidence",
        expect_toolless_follow_up: false,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let response = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-runtime-wait".to_string(),
                agent_run_identity: Some(agent_run_identity),
                initial_turn_id: "turn-runtime-wait".to_string(),
                user_message: "Use the completed background result.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("consume precompleted runtime job without yielding");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(response.turn_responses.len(), 2);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    let wait_statuses = response.turn_responses[0]
        .runtime_events
        .iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .filter(|payload| payload.get("type").and_then(Value::as_str) == Some("RuntimeWaitChanged"))
        .filter_map(|payload| {
            payload
                .get("payload")
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    assert_eq!(wait_statuses, vec!["waiting", "resumed"]);
}

#[tokio::test]
async fn cancellation_discards_queued_supplement_before_another_provider_or_tool() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        stream_execution_boundary_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let turn_control = TurnControl::new();
    let model_client = StreamExecutionBoundaryModelClient {
        request_count: AtomicUsize::new(0),
        execution_count: execution_count.clone(),
        turn_control: Some(turn_control.clone()),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let cancellation_execution_count = execution_count.clone();

    let response = engine
        .process_turn_loop_online_with_model_client_stream_controlled_async(
            AgentRunRequest {
                session_id: "chat-turn-supplement-cancel".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-supplement-cancel".to_string(),
                user_message: "Run one tool, then stop.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
            &mut |_| {},
            &|| {
                Ok((cancellation_execution_count.load(Ordering::SeqCst) >= 1)
                    .then(|| "test_cancel".to_string()))
            },
            &turn_control,
        )
        .await
        .expect("cancel queued supplement run");

    assert_eq!(
        response.stop,
        AgentRunStop::Cancelled("test_cancel".to_string())
    );
    assert_eq!(response.turn_responses.len(), 1);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 1);
    assert!(turn_control
        .enqueue_supplement_with("late supplement".to_string(), || Ok(()))
        .expect_err("cancelled task must reject supplements")
        .contains("no longer accepting input"));
}

#[tokio::test]
async fn four_independent_calls_execute_as_one_batch_before_one_follow_up_request() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        complete_batch_test_tool_layer(execution_count.clone()),
        AgentRuntimeConfig::default(),
    );
    let model_client = CompleteBatchModelClient {
        request_count: AtomicUsize::new(0),
        follow_up_tool_result_count: AtomicUsize::new(0),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };

    let response = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-complete-four-tool-batch".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-complete-four-tool-batch".to_string(),
                user_message: "Collect four facts, then verify once.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("complete four-tool batch run");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(response.turn_responses.len(), 2);
    assert_eq!(response.turn_responses[0].tool_results.len(), 4);
    assert_eq!(response.turn_responses[1].tool_results.len(), 0);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        model_client
            .follow_up_tool_result_count
            .load(Ordering::SeqCst),
        4
    );
    assert_eq!(execution_count.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn complete_turn_tool_success_stops_and_resume_replays_no_side_effects() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let config = AgentRuntimeConfig::default();
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        complete_turn_test_tool_layer(execution_count.clone(), true),
        config,
    );
    let model_client = CompleteTurnTestModelClient {
        request_count: AtomicUsize::new(0),
        include_sibling: false,
        sibling_first: false,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };

    let first = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-complete-turn".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-complete-turn".to_string(),
                user_message: "Complete this turn.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("complete-turn tool run");

    assert_eq!(first.stop, AgentRunStop::TerminalTool);
    assert_eq!(first.turn_responses.len(), 1);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 1);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert!(first.turn_responses[0].checkpoint.is_none());
    assert!(store
        .load_checkpoint_by_turn("chat-complete-turn", "turn-complete-turn")
        .expect("load ordinary turn checkpoint")
        .is_none());
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 1);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn complete_turn_tool_failure_continues_to_provider() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let config = AgentRuntimeConfig::default();
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        complete_turn_test_tool_layer(execution_count.clone(), false),
        config,
    );
    let model_client = CompleteTurnTestModelClient {
        request_count: AtomicUsize::new(0),
        include_sibling: false,
        sibling_first: false,
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };

    let response = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-complete-turn-failure".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-complete-turn-failure".to_string(),
                user_message: "Try the complete-turn tool.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("failed complete-turn tool continues");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(response.turn_responses.len(), 2);
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(execution_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        response.turn_responses[0].continuation,
        QueryContinuation::ExecuteTools
    );
}

#[tokio::test]
async fn complete_turn_tool_sibling_fails_before_route_or_execution() {
    let store = AgentRuntimeTestStore::new();
    let execution_count = Arc::new(AtomicUsize::new(0));
    let engine = AgentRuntime::new_for_test_with_tools(
        store.clone(),
        complete_turn_test_tool_layer(execution_count.clone(), true),
        AgentRuntimeConfig::default(),
    );
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };

    for sibling_first in [false, true] {
        let model_client = CompleteTurnTestModelClient {
            request_count: AtomicUsize::new(0),
            include_sibling: true,
            sibling_first,
        };
        let order = if sibling_first { "first" } else { "last" };
        let session_id = format!("chat-complete-turn-sibling-{order}");
        let turn_id = format!("turn-complete-turn-sibling-{order}");
        let error = engine
            .process_turn_loop_online_with_model_client_async(
                AgentRunRequest {
                    session_id: session_id.clone(),
                    agent_run_identity: None,
                    initial_turn_id: turn_id.clone(),
                    user_message: "Return an invalid sibling call.".to_string(),
                    runtime_scope: PromptCompactionScopeV1::main(),
                    resume_from_turn_id: None,
                    auto_continue_after_resume_wait: None,
                },
                &model_client,
                &config_store,
            )
            .await
            .expect_err("complete-turn sibling must loud-fail");

        assert!(error.contains("must be the only tool call"));
        assert!(store
            .load_checkpoint_by_turn(session_id.as_str(), turn_id.as_str())
            .expect("load rejected checkpoint")
            .is_none());
    }
    assert_eq!(execution_count.load(Ordering::SeqCst), 0);
}

#[test]
fn complete_turn_tool_rejects_success_status_with_error() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        complete_turn_test_tool_layer(Arc::new(AtomicUsize::new(0)), true),
        AgentRuntimeConfig::default(),
    );
    let generate_result = GenerateResult {
        content: String::new(),
        tool_calls: vec![ToolCallEnvelope {
            id: "call-false-success".to_string(),
            name: "complete_turn_test_tool".to_string(),
            args_json: "{}".to_string(),
        }],
        reasoning_content: None,
        input_tokens: None,
        total_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };
    let result = ToolExecutionResult {
        tool_call_id: "call-false-success".to_string(),
        tool_name: "complete_turn_test_tool".to_string(),
        status: "ok".to_string(),
        content: "ok".to_string(),
        details: json!({}),
        facts: Vec::new(),
        error: Some(crate::tool::ToolErrorInfo::from_unstructured_error(
            "reported error",
        )),
        started_at_ms: 1,
        completed_at_ms: 2,
        latency_ms: 1,
        parallel_group: None,
        transition_reason: Some("false_success".to_string()),
    };

    assert!(!engine
        .should_complete_turn_after_tool_success(&generate_result, &[result])
        .expect("evaluate false success"));
}

#[test]
fn complete_turn_tool_rejects_call_and_tool_identity_mismatches() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test_with_tools(
        store,
        complete_turn_test_tool_layer(Arc::new(AtomicUsize::new(0)), true),
        AgentRuntimeConfig::default(),
    );
    let generate_result = GenerateResult {
        content: String::new(),
        tool_calls: vec![ToolCallEnvelope {
            id: "call-identity".to_string(),
            name: "complete_turn_test_tool".to_string(),
            args_json: "{}".to_string(),
        }],
        reasoning_content: None,
        input_tokens: None,
        total_tokens: None,
        prompt_cache_hit_tokens: None,
        prompt_cache_miss_tokens: None,
    };

    for (tool_call_id, tool_name) in [
        ("call-mismatch", "complete_turn_test_tool"),
        ("call-identity", "banana"),
    ] {
        let result = ToolExecutionResult {
            tool_call_id: tool_call_id.to_string(),
            tool_name: tool_name.to_string(),
            status: "ok".to_string(),
            content: "ok".to_string(),
            details: json!({}),
            facts: Vec::new(),
            error: None,
            started_at_ms: 1,
            completed_at_ms: 2,
            latency_ms: 1,
            parallel_group: None,
            transition_reason: Some("identity_test".to_string()),
        };
        let error = engine
            .should_complete_turn_after_tool_success(&generate_result, &[result])
            .expect_err("identity mismatch must loud-fail");
        assert!(error.contains("call/result identity mismatch"));
    }
}

#[tokio::test]
async fn p7_model_prompt_compaction_online_loop_commits_summary_and_continues_main_response() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let session = p7_large_context_session("chat-p7-model-compact-success");
    session_manager
        .save_session(&session)
        .expect("save p7 model compaction session");

    let engine = AgentRuntime::new_for_test(store.clone(), p7_large_context_compaction_config());
    let model_client = P7PromptCompactionModelClient::new(P7PromptCompactionModelBehavior::Valid);
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };

    let response = engine
        .process_turn_loop_online_with_model_client_async(
            AgentRunRequest {
                session_id: "chat-p7-model-compact-success".to_string(),
                agent_run_identity: None,
                initial_turn_id: "turn-p7-model-compact-success".to_string(),
                user_message: "Continue the P7 compaction smoke.".to_string(),
                runtime_scope: PromptCompactionScopeV1::main(),
                resume_from_turn_id: None,
                auto_continue_after_resume_wait: None,
            },
            &model_client,
            &config_store,
        )
        .await
        .expect("run online loop with model compaction");

    assert_eq!(response.stop, AgentRunStop::Finalized);
    assert_eq!(response.turn_responses.len(), 1);
    assert_eq!(
        response.turn_responses[0]
            .agent_run_resource_usage
            .provider_attempts,
        2
    );
    assert_eq!(
        response.turn_responses[0]
            .agent_run_resource_usage
            .completed_provider_rounds,
        2,
        "prompt compaction and the main response share one root-run meter"
    );

    let model_requests = model_client.requests();
    assert_eq!(model_requests.len(), 2);
    let compaction_request = &model_requests[0];
    assert!(compaction_request.turn_id.starts_with("turn-"));
    assert_ne!(compaction_request.turn_id, model_requests[1].turn_id);
    assert_eq!(compaction_request.prepared_prompt.messages.len(), 1);
    assert!(compaction_request.prepared_prompt.messages[0]
        .content
        .contains("[model_compaction_prompt_v1]"));
    assert!(compaction_request
        .prepared_prompt
        .tool_definitions
        .is_empty());
    assert_eq!(
        compaction_request.prepared_prompt.tool_choice,
        ModelToolChoice::None
    );

    let main_request = &model_requests[1];
    assert_eq!(main_request.turn_id, "turn-p7-model-compact-success");
    assert!(main_request
        .prepared_prompt
        .messages
        .iter()
        .any(|message| message.content.starts_with("# Goal")));
    assert!(main_request.prepared_prompt.messages.iter().any(|message| {
        message
            .content
            .contains("recent user suffix must remain visible")
    }));

    let saved_session = session_manager
        .load_or_create_session("chat-p7-model-compact-success")
        .expect("load p7 model compaction session");
    let summary_message = saved_session
        .messages
        .iter()
        .find(|message| {
            message.metadata.get("kind").map(String::as_str) == Some("context_compaction")
        })
        .expect("summary message appended");
    assert!(summary_message.content.starts_with("# Goal"));
    assert!(saved_session
        .messages
        .iter()
        .any(|message| message.content == "p7 main response after compaction"));

    let stats_json = saved_session
        .metadata
        .get("prompt_compaction_stats_json")
        .expect("prompt compaction stats");
    let stats = serde_json::from_str::<Value>(stats_json).expect("parse stats");
    assert_eq!(
        stats
            .get("decision")
            .and_then(|value| value.get("action"))
            .and_then(Value::as_str),
        Some("compact")
    );
    assert_eq!(
        stats
            .get("decision")
            .and_then(|value| value.get("strategy"))
            .and_then(Value::as_str),
        Some("model")
    );
}

#[tokio::test]
async fn prompt_compaction_uses_context_pressure_and_preserves_large_usage_telemetry() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    session_manager
        .save_session(&p7_large_context_session("chat-prompt-compaction-usage"))
        .expect("save prompt compaction usage session");
    let engine = AgentRuntime::new_for_test(store.clone(), p7_large_context_compaction_config());
    let driver = TestPromptCompactionAsyncDriver;
    let mut usage = AgentRunResourceUsageV1 {
        provider_attempts: 40,
        completed_provider_rounds: 40,
        estimated_input_tokens: 900_000,
        actual_input_tokens: 800_000,
        prompt_cache_hit_tokens: 760_000,
        prompt_cache_miss_tokens: 40_000,
        output_tokens: 12_000,
        tool_call_count: 39,
    };

    let request = engine
        .build_generate_driver_request_with_async_driver_and_runtime_scope(
            "chat-prompt-compaction-usage",
            "turn-prompt-compaction-usage",
            &TurnInput::UserMessage("Continue after context compaction.".to_string()),
            0,
            PromptCompactionScopeV1::main(),
            Some(&mut usage),
            None,
            &driver,
        )
        .await
        .expect("context pressure should compact independently of cumulative usage");

    assert!(request.compression_stats_json.is_some());
    assert!(request
        .prepared_prompt
        .messages
        .iter()
        .any(|message| message.content.starts_with("# Goal")));
    assert_eq!(usage.provider_attempts, 41);
    assert_eq!(usage.completed_provider_rounds, 41);
    assert!(usage.estimated_input_tokens > 900_000);
    assert!(usage.actual_input_tokens > 800_000);
    assert_eq!(usage.prompt_cache_hit_tokens, 760_000);
    assert_eq!(usage.prompt_cache_miss_tokens, 40_000);
    assert_eq!(usage.output_tokens, 12_000);
    assert_eq!(usage.tool_call_count, 39);
}

#[tokio::test]
async fn p7_model_prompt_compaction_online_loop_rejects_empty_or_oversized_markdown() {
    for (case_name, behavior) in [
        ("empty", P7PromptCompactionModelBehavior::Empty),
        ("oversized", P7PromptCompactionModelBehavior::Oversized),
    ] {
        let store = AgentRuntimeTestStore::new();
        let session_manager = SessionManager::new(store.clone());
        let session_id = format!("chat-p7-model-compact-fail-{case_name}");
        let session = p7_large_context_session(session_id.as_str());
        session_manager
            .save_session(&session)
            .expect("save p7 failure session");

        let engine =
            AgentRuntime::new_for_test(store.clone(), p7_large_context_compaction_config());
        let model_client = P7PromptCompactionModelClient::new(behavior);
        let config_store = StaticModelSessionConfigStore {
            config: Some(ModelSessionConfig::default()),
        };

        let response = engine
            .process_turn_loop_online_with_model_client_async(
                AgentRunRequest {
                    session_id: session_id.clone(),
                    agent_run_identity: None,
                    initial_turn_id: format!("turn-p7-model-compact-fail-{case_name}"),
                    user_message: "Continue despite model compaction failure.".to_string(),
                    runtime_scope: PromptCompactionScopeV1::main(),
                    resume_from_turn_id: None,
                    auto_continue_after_resume_wait: None,
                },
                &model_client,
                &config_store,
            )
            .await
            .expect("run online loop with failing model compaction");

        assert_eq!(response.stop, AgentRunStop::Finalized);
        let model_requests = model_client.requests();
        assert_eq!(model_requests.len(), 2, "{case_name}");
        assert!(model_requests[0].turn_id.starts_with("turn-"));
        assert_ne!(model_requests[0].turn_id, model_requests[1].turn_id);
        assert_eq!(
            model_requests[0].prepared_prompt.tool_choice,
            ModelToolChoice::None,
            "{case_name}"
        );
        assert_eq!(
            model_requests[1].turn_id,
            format!("turn-p7-model-compact-fail-{case_name}")
        );
        assert!(model_requests[1]
            .prepared_prompt
            .messages
            .iter()
            .any(|message| {
                message
                    .content
                    .contains("old prefix user details about model compaction pressure")
            }));

        let saved_session = session_manager
            .load_or_create_session(session_id.as_str())
            .expect("load p7 failure session");
        let messages_text = saved_session
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !saved_session.messages.iter().any(|message| {
                message.metadata.get("kind").map(String::as_str) == Some("context_compaction")
            }),
            "{case_name}"
        );
        assert!(
            messages_text.contains("old prefix user details about model compaction pressure"),
            "{case_name}"
        );
        assert!(
            saved_session
                .metadata
                .contains_key(PROMPT_COMPACTION_FAILURE_META_KEY),
            "{case_name}"
        );
        assert!(saved_session
            .messages
            .iter()
            .any(|message| message.content == "p7 main response after compaction"));

        let events = store
            .list_events(session_id.as_str(), 100, 0)
            .expect("list p7 failure events");
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "prompt.compaction.failed"),
            "{case_name}"
        );
    }
}

#[tokio::test]
async fn prompt_compaction_generate_request_preserves_default_tool_projection() {
    let store = AgentRuntimeTestStore::new();
    let session_manager = SessionManager::new(store.clone());
    let mut session = SessionStateSnapshot::new("chat-compact-pollution-gate".to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig {
        max_message_chars: 10_000,
    });
    handler.push_user_message(
        &mut session,
        "Inspect the runtime evidence projection and compaction code.",
        JsonMap::new(),
    );
    handler.push_assistant_message(
        &mut session,
        "I found the current runtime uses Bash evidence projection.",
        JsonMap::new(),
    );
    handler.push_user_message(
        &mut session,
        "Continue with the prompt compaction hot context gate.",
        JsonMap::new(),
    );
    handler.push_assistant_message(
        &mut session,
        "The next step is checking generated prompt artifacts.",
        JsonMap::new(),
    );
    assign_test_message_ids(&mut session, "chat-compact-pollution-gate");
    session_manager
        .save_session(&session)
        .expect("save session");

    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    config.model_context_tokens = 6_200;
    config.prompt_compaction_summary_max_tokens = 1_200;
    let engine = AgentRuntime::new_for_test(store.clone(), config);
    let driver = TestPromptCompactionAsyncDriver;

    let request = engine
        .build_generate_driver_request_with_async_driver(
            "chat-compact-pollution-gate",
            "turn-compact-pollution-gate",
            "Summarize the current prompt compaction gate.",
            0,
            &driver,
        )
        .await
        .expect("build generate request");

    assert_model_tool_definitions_match_default_projection(
        &request.prepared_prompt.tool_definitions,
    );

    let system_prompt = request
        .prepared_prompt
        .system_prompt
        .as_deref()
        .expect("system prompt");
    assert!(system_prompt.starts_with("# Harness\n"));
    assert!(system_prompt.contains("Use only the tools supplied for the current turn"));

    let context_messages_text = request
        .prepared_prompt
        .messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n---\n");
    assert!(!context_messages_text.contains("evidenceLedger"));
    let cache_break_detection = detect_generate_request_cache_breaks(&request, &request);
    assert_eq!(
        cache_break_detection
            .get("hasBreak")
            .and_then(Value::as_bool),
        Some(false)
    );
    eprintln!("prompt_cache_break_detection\n{}", cache_break_detection);
}

#[tokio::test]
async fn prompt_compaction_circuit_breaker_skips_tool_continuations_and_recovers_next_root_agent_run(
) {
    let store = AgentRuntimeTestStore::new();
    let mut config = AgentRuntimeConfig::default();
    force_prompt_compaction_for_tests(&mut config);
    let engine = AgentRuntime::new_for_test(store.clone(), config);
    let mut session = SessionStateSnapshot::new("chat-compact-circuit".to_string(), 0);
    let handler = MessageHandler::new(MessageHandlerConfig {
        max_message_chars: 10_000,
    });
    append_compaction_pressure_history(&handler, &mut session);
    handler.push_user_message(&mut session, "old-user-alpha", JsonMap::new());
    handler.push_assistant_message(&mut session, "old-assistant-beta", JsonMap::new());
    handler.push_user_message(&mut session, "old-user-gamma", JsonMap::new());
    handler.push_assistant_message(&mut session, "recent-assistant-delta", JsonMap::new());
    let original_messages = session.messages.clone();

    for index in 0..PROMPT_COMPACTION_CIRCUIT_FAILURE_THRESHOLD {
        engine.record_prompt_compaction_failure(
            &mut session,
            "turn-compact-circuit",
            "test_failure",
            format!("failure-{index}").as_str(),
        );
    }
    let stats = engine
        .apply_prompt_compaction(&mut session, "turn-compact-circuit")
        .expect("apply prompt compaction");

    assert!(stats.stats_json.is_none());
    assert!(stats.runtime_events.is_empty());
    assert!(prompt_compaction_circuit_is_open(&session));
    assert_eq!(
        session
            .metadata
            .get(PROMPT_COMPACTION_FAILURE_COUNT_META_KEY)
            .and_then(|value| value.parse::<u32>().ok()),
        Some(PROMPT_COMPACTION_CIRCUIT_FAILURE_THRESHOLD)
    );
    assert_eq!(
        session
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        original_messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>()
    );

    engine
        .session_manager
        .save_session(&session)
        .expect("save open circuit session");
    let request = engine
        .build_generate_driver_request_with_async_driver(
            "chat-compact-circuit",
            "turn-after-compact-circuit",
            "retry compaction in a new root AgentRun",
            0,
            &TestPromptCompactionAsyncDriver,
        )
        .await
        .expect("new root AgentRun should retry prompt compaction");
    assert!(request
        .prepared_prompt
        .messages
        .iter()
        .any(|message| message.content.starts_with("# Goal")));
    let recovered_session = engine
        .session_manager
        .load_or_create_session("chat-compact-circuit")
        .expect("load recovered circuit session");
    assert!(!prompt_compaction_circuit_is_open(&recovered_session));
    assert!(!recovered_session
        .metadata
        .contains_key(PROMPT_COMPACTION_FAILURE_COUNT_META_KEY));
}

fn append_compaction_pressure_history(
    handler: &MessageHandler,
    session: &mut SessionStateSnapshot,
) {
    let historical_user = "earlier-user-context ".repeat(16);
    let historical_assistant = "earlier-assistant-context ".repeat(16);
    handler.push_user_message(session, historical_user.as_str(), JsonMap::new());
    handler.push_assistant_message(session, historical_assistant.as_str(), JsonMap::new());
    handler.push_user_message(session, historical_user.as_str(), JsonMap::new());
    handler.push_assistant_message(session, historical_assistant.as_str(), JsonMap::new());
}

fn assign_test_message_ids(session: &mut SessionStateSnapshot, namespace: &str) {
    let previous_semantics = std::mem::take(&mut session.model_semantics);
    let mut remapped_semantics = std::collections::BTreeMap::new();
    for (index, message) in session.messages.iter_mut().enumerate() {
        if message.role == MessageRole::User {
            message.metadata.insert(
                MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
                MESSAGE_SEMANTIC_USER_REQUEST.to_string(),
            );
        }
        let previous_id = message.message_id.clone();
        message.message_id = format!("msg:{namespace}:{}", index + 1);
        message.created_at_ms = i64::try_from(index + 1).expect("test message index fits i64");
        remapped_semantics.insert(
            message.message_id.clone(),
            previous_semantics
                .get(previous_id.as_str())
                .cloned()
                .unwrap_or(ModelMessageSemanticsV1::Plain),
        );
    }
    session.model_semantics = remapped_semantics;
    crate::runtime::context_window::refresh_session_context_window(session);
}

fn assign_plain_model_semantics(session: &mut SessionStateSnapshot) {
    for message in &mut session.messages {
        if message.role == MessageRole::User {
            message.metadata.insert(
                MESSAGE_SEMANTIC_KIND_META_KEY.to_string(),
                MESSAGE_SEMANTIC_USER_REQUEST.to_string(),
            );
        }
        session
            .model_semantics
            .entry(message.message_id.clone())
            .or_insert(ModelMessageSemanticsV1::Plain);
    }
    crate::runtime::context_window::refresh_session_context_window(session);
}

#[tokio::test]
async fn completed_turn_projection_requires_exact_identity_and_acknowledgement() {
    let store = AgentRuntimeTestStore::new();
    let engine = AgentRuntime::new_for_test(store, AgentRuntimeConfig::default());
    let session_id = "chat-completed-turn-projection";
    let step = process_resume_probe(&engine, session_id).await;
    let result = AgentRunResult {
        turn_responses: vec![step],
        stop: AgentRunStop::Finalized,
    };
    let identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "agent-run-completed-turn-projection".to_string(),
        execution_id: "execution-completed-turn-projection".to_string(),
        authorization_digest: format!("sha256:{}", "a".repeat(64)),
    };

    let projection = engine
        .prepare_completed_turn_projection(session_id, &identity, &result)
        .expect("prepare completed turn projection");
    assert_eq!(projection.final_turn_id, "turn-resume-probe");
    assert_eq!(projection.expected_tool_call_ids, Vec::<String>::new());
    assert_eq!(
        engine
            .prepare_completed_turn_projection(session_id, &identity, &result)
            .expect("idempotent prepare"),
        projection
    );

    let mut non_terminal = result.clone();
    non_terminal.stop = AgentRunStop::QuestionWait;
    assert!(engine
        .prepare_completed_turn_projection(session_id, &identity, &non_terminal)
        .expect_err("non-terminal turn must not produce a receipt")
        .contains("completed_turn_projection_requires_terminal_result"));

    let wrong_identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: "agent-run-other".to_string(),
        execution_id: "execution-other".to_string(),
        authorization_digest: format!("sha256:{}", "b".repeat(64)),
    };
    assert!(engine
        .load_completed_turn_projection(session_id, &wrong_identity)
        .expect_err("wrong identity must not read a receipt")
        .contains("completed_turn_projection_identity_mismatch"));
    assert!(engine
        .acknowledge_completed_turn_projection(session_id, &wrong_identity)
        .expect_err("wrong identity must not acknowledge a receipt")
        .contains("completed_turn_projection_identity_mismatch"));

    engine
        .acknowledge_completed_turn_projection(session_id, &identity)
        .expect("exact acknowledgement clears receipt");
    engine
        .acknowledge_completed_turn_projection(session_id, &identity)
        .expect("acknowledgement is idempotent");
    assert_eq!(
        engine
            .load_completed_turn_projection(session_id, &identity)
            .expect("load acknowledged receipt"),
        None
    );
}

async fn process_resume_probe(
    engine: &AgentRuntime<AgentRuntimeTestStore>,
    session_id: &str,
) -> TurnStepResult {
    engine
        .process_turn_with_stream_sink_async(
            ProcessTurnRequest {
                session_id: session_id.to_string(),
                agent_run_identity: None,
                turn_id: "turn-resume-probe".to_string(),
                input: TurnInput::UserMessage("continue".to_string()),
                generate_result: GenerateResult {
                    content: "resumed".to_string(),
                    tool_calls: vec![],
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            },
            None,
        )
        .await
        .expect("process resume probe")
}

#[test]
fn subagent_query_loop_request_marks_agent_compaction_scope() {
    let req = sample_subagent_worker_run_request();
    let identity = RuntimeAgentRunIdentityV1 {
        agent_run_id: req.job.job_id.clone(),
        execution_id: "execution-subagent-one".to_string(),
        authorization_digest: format!("sha256:{}", "1".repeat(64)),
    };

    let request = build_subagent_query_loop_request(
        &req,
        &AgentRuntimeSubagentRunnerConfig {
            agent_run_identity: Some(identity.clone()),
            ..AgentRuntimeSubagentRunnerConfig::default()
        },
    )
    .expect("build subagent query loop request");

    assert_eq!(request.session_id, "chat-parent:subagent:agent-one");
    assert_eq!(request.initial_turn_id, "turn-child");
    assert_eq!(request.runtime_scope.agent_scope, "subagent");
    assert_eq!(
        request.runtime_scope.parent_session_id.as_deref(),
        Some("chat-parent")
    );
    assert_eq!(
        request.runtime_scope.runtime_job_id.as_deref(),
        Some(req.job.job_id.as_str())
    );
    assert_eq!(request.agent_run_identity, Some(identity));
}

#[tokio::test]
async fn subagent_tool_call_uses_the_durable_commit_port_once() {
    let workspace_root = temp_dir_path("subagent_durable_tool_commit");
    std::fs::create_dir_all(workspace_root.as_path()).expect("create workspace");
    std::fs::write(
        workspace_root.join("fixture.txt"),
        "durable subagent fixture",
    )
    .expect("write fixture");
    let store = AgentRuntimeTestStore::new();
    let tool_layer = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("workspace tool layer");
    let runtime_config = AgentRuntimeConfig {
        allowed_tools: Some(vec!["read".to_string()]),
        ..AgentRuntimeConfig::default()
    };
    let engine = AgentRuntime::new_for_test_with_tools(store, tool_layer, runtime_config);
    let req = sample_subagent_worker_run_request();
    let agent_run_id = req.job.job_id.clone();
    let model_client = SubagentReadModelClient {
        request_count: AtomicUsize::new(0),
    };
    let config_store = StaticModelSessionConfigStore {
        config: Some(ModelSessionConfig::default()),
    };
    let runner_config = AgentRuntimeSubagentRunnerConfig {
        auto_continue_after_resume_wait: Some(false),
        agent_run_identity: Some(RuntimeAgentRunIdentityV1 {
            agent_run_id: agent_run_id.clone(),
            execution_id: "execution-subagent-durable-tool".to_string(),
            authorization_digest: format!("sha256:{}", "2".repeat(64)),
        }),
    };
    let mut durable_tool_calls = Vec::new();
    let mut durable_receipts = Vec::new();

    let outcome = engine
        .run_subagent_worker_with_model_client_async(
            req,
            &model_client,
            &config_store,
            &runner_config,
            None,
            Some(&mut |safe_point| {
                match safe_point {
                    ToolSafePoint::DurableToolCall {
                        session_id,
                        agent_run_id,
                        call,
                        ..
                    } => durable_tool_calls.push((session_id, agent_run_id, call.id)),
                    ToolSafePoint::DurableReceipt {
                        session_id,
                        agent_run_id,
                        call,
                        ..
                    } => durable_receipts.push((session_id, agent_run_id, call.id)),
                    _ => {}
                }
                Ok(())
            }),
        )
        .await;

    assert!(matches!(
        outcome,
        SubagentWorkerRunOutcome::Succeeded { ref summary, .. }
            if summary == "subagent read completed"
    ));
    assert_eq!(model_client.request_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        durable_tool_calls,
        vec![(
            "chat-parent:subagent:agent-one".to_string(),
            agent_run_id.clone(),
            "call-subagent-read".to_string(),
        )]
    );
    assert_eq!(
        durable_receipts,
        vec![(
            "chat-parent:subagent:agent-one".to_string(),
            agent_run_id,
            "call-subagent-read".to_string(),
        )]
    );

    let _ = std::fs::remove_dir_all(workspace_root);
}

fn sample_subagent_worker_run_request() -> SubagentWorkerRunRequest {
    let parent = AgentRunContext::root(
        "chat-parent",
        "turn-parent",
        "turn-parent",
        "main-agent-run-turn-parent",
        "main-agent",
        std::env::temp_dir().to_string_lossy(),
        1,
    );
    let child = AgentRunContext::child(
        &parent,
        "chat-parent:subagent:agent-one".to_string(),
        "turn-child",
        "agent-run-one",
        "agent-one",
        2,
    );
    let mut work_packet = SubAgentWorkPacket::new(
        child,
        TaskBrief {
            task_id: Some("agent-one".to_string()),
            objective: "Inspect one focused issue.".to_string(),
            success_criteria: vec!["Return concise findings.".to_string()],
            constraints: vec!["Do not expose internal ids.".to_string()],
            output_hint: Some("Findings".to_string()),
        },
        HotView {
            summary: "Parent context".to_string(),
            recent_message_ids: vec![],
            state_kv: JsonMap::new(),
        },
        OutputContract {
            response_mode: "bounded_agent_result".to_string(),
            expected_sections: vec!["summary".to_string()],
            require_artifact_refs: false,
            max_summary_chars: Some(1_000),
        },
        ContextTransferMode::Borrow,
    );
    work_packet.allowed_tools = vec!["read".to_string()];
    work_packet.delegated_tool_contracts =
        test_delegated_tool_contracts(work_packet.allowed_tools.as_slice());
    let content_json = json!({ "workPacket": work_packet });
    let job = build_subagent_run_job(SubagentRunJobRequest {
        session_id: "chat-parent".to_string(),
        parent_turn_id: "turn-parent".to_string(),
        tool_call_id: "tool-agent-one".to_string(),
        subagent_id: "agent-one".to_string(),
        work_packet_ref: "external_context:agent-one".to_string(),
        checkpoint_id: None,
        run_at_ms: 3,
        created_at_ms: 3,
        max_retries: 1,
    });
    SubagentWorkerRunRequest {
        lifecycle: SubagentLifecycleRecord {
            subagent_id: "agent-one".to_string(),
            parent_turn_id: "turn-parent".to_string(),
            session_id: "chat-parent".to_string(),
            status: SubagentLifecycleStatus::Running,
            job_id: job.job_id.clone(),
            work_packet_ref: "external_context:agent-one".to_string(),
            result_ref: None,
            last_error: None,
            updated_at_ms: 3,
        },
        job,
        work_packet: SubagentWorkPacketEnvelope {
            ref_id: "external_context:agent-one".to_string(),
            content_json,
        },
    }
}

#[test]
fn read_observability_preserves_structured_coverage() {
    let report = ToolExecutionResult {
        tool_call_id: "call-read-coverage".to_string(),
        tool_name: "read".to_string(),
        status: "ok".to_string(),
        content: "Read castle/index.html lines 1-50 of 1642; next offset 50".to_string(),
        details: serde_json::json!({
            "schema": "file_read_result_v1",
            "path": "castle/index.html",
            "startLine": 1,
            "endLine": 50,
            "totalLines": 1642,
            "nextOffset": 50,
            "truncatedBy": "lines",
            "truncated": true
        }),
        facts: Vec::new(),
        error: None,
        started_at_ms: 1,
        completed_at_ms: 2,
        latency_ms: 1,
        parallel_group: None,
        transition_reason: None,
    };

    let operations_json = project_tool_operations_json(&[report]);
    let operations: Vec<Value> =
        serde_json::from_str(operations_json.expect("operations json").as_str())
            .expect("parse operations");
    let operation = operations.first().expect("operation");

    assert!(operation.get("kind").is_none());
    assert!(operation.get("title").is_none());
    assert_eq!(
        operation.get("path").and_then(Value::as_str),
        Some("castle/index.html")
    );
    assert_eq!(operation.get("startLine").and_then(Value::as_u64), Some(1));
    assert_eq!(operation.get("endLine").and_then(Value::as_u64), Some(50));
    assert_eq!(
        operation.get("totalLines").and_then(Value::as_u64),
        Some(1642)
    );
    assert_eq!(
        operation.get("nextOffset").and_then(Value::as_u64),
        Some(50)
    );
    assert_eq!(
        operation.get("truncatedBy").and_then(Value::as_str),
        Some("lines")
    );
}

#[test]
fn tool_operation_projection_does_not_scan_result_aliases() {
    let report = ToolExecutionResult {
        tool_call_id: "call-alias".to_string(),
        tool_name: "read".to_string(),
        status: "ok".to_string(),
        content: "banana".to_string(),
        details: serde_json::json!({
            "filePath": "banana",
            "start_line": 7,
            "coveredRange": { "endLine": 9 }
        }),
        facts: Vec::new(),
        error: None,
        started_at_ms: 1,
        completed_at_ms: 2,
        latency_ms: 1,
        parallel_group: None,
        transition_reason: None,
    };

    let operations: Vec<Value> = serde_json::from_str(
        project_tool_operations_json(&[report])
            .expect("operations json")
            .as_str(),
    )
    .expect("parse operations");
    let operation = operations.first().expect("operation");
    assert!(operation.get("path").is_none());
    assert!(operation.get("startLine").is_none());
    assert!(operation.get("endLine").is_none());
}

#[test]
fn edit_observability_preserves_executor_diff_preview() {
    let report = ToolExecutionResult {
        tool_call_id: "call-edit".to_string(),
        tool_name: "edit".to_string(),
        status: "ok".to_string(),
        content: "Edited 1 file(s): src/lib.rs.".to_string(),
        details: serde_json::json!({
            "schema": "edit_result_v1",
            "filesChanged": 1,
            "addedLines": 1,
            "removedLines": 1,
            "diffPreview": "--- src/lib.rs\n+++ src/lib.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n-before\n+after",
            "operations": [
                {
                    "type": "update",
                    "path": "src/lib.rs",
                    "targetPath": null,
                    "previousFileHash": "sha256:old",
                    "fileHash": "sha256:new",
                    "addedLines": 1,
                    "removedLines": 1
                }
            ]
        }),
        facts: Vec::new(),
        error: None,
        started_at_ms: 1,
        completed_at_ms: 8,
        latency_ms: 7,
        parallel_group: None,
        transition_reason: Some("local_tool_exec".to_string()),
    };

    let operations_json = project_tool_operations_json(&[report]);
    let operations: Vec<Value> =
        serde_json::from_str(operations_json.expect("operations json").as_str())
            .expect("parse operations");
    let operation = operations.first().expect("operation");

    assert!(operation.get("kind").is_none());
    assert!(operation.get("title").is_none());
    assert_eq!(
        operation.get("path").and_then(Value::as_str),
        Some("src/lib.rs")
    );
    assert_eq!(operation.get("added").and_then(Value::as_u64), Some(1));
    assert_eq!(operation.get("removed").and_then(Value::as_u64), Some(1));
    let diff_preview = operation
        .get("diffPreview")
        .and_then(Value::as_str)
        .expect("diff preview");
    assert!(diff_preview.contains("--- src/lib.rs"));
    assert!(diff_preview.contains("-old"));
    assert!(diff_preview.contains("+new"));
    assert!(diff_preview.contains("-before"));
    assert!(diff_preview.contains("+after"));
}

#[test]
fn write_observability_preserves_executor_diff_preview() {
    let report = ToolExecutionResult {
        tool_call_id: "call-write".to_string(),
        tool_name: "write".to_string(),
        status: "ok".to_string(),
        content: "Wrote src/lib.rs.".to_string(),
        details: serde_json::json!({
            "schema": "write_result_v1",
            "path": "src/lib.rs",
            "created": false,
            "addedLines": 1,
            "removedLines": 1,
            "diffPreview": "--- src/lib.rs\n+++ src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new"
        }),
        facts: Vec::new(),
        error: None,
        started_at_ms: 1,
        completed_at_ms: 8,
        latency_ms: 7,
        parallel_group: None,
        transition_reason: Some("local_tool_exec".to_string()),
    };

    let operations_json = project_tool_operations_json(&[report]);
    let operations: Vec<Value> =
        serde_json::from_str(operations_json.expect("operations json").as_str())
            .expect("parse operations");
    let operation = operations.first().expect("operation");

    assert_eq!(operation.get("added").and_then(Value::as_u64), Some(1));
    assert_eq!(operation.get("removed").and_then(Value::as_u64), Some(1));
    let diff_preview = operation
        .get("diffPreview")
        .and_then(Value::as_str)
        .expect("diff preview");
    assert!(diff_preview.contains("-old"));
    assert!(diff_preview.contains("+new"));
}

#[test]
fn failed_edit_observability_does_not_project_applied_change_facts() {
    let report = ToolExecutionResult {
        tool_call_id: "call-edit-failed".to_string(),
        tool_name: "edit".to_string(),
        status: "error".to_string(),
        content: "file mutation rejected; read the existing file before editing it".to_string(),
        details: serde_json::json!({
            "schema": "file_tool_rejected_v1",
            "path": "src/lib.rs",
            "addedLines": 1,
            "removedLines": 1
        }),
        facts: Vec::new(),
        error: Some(ToolErrorInfo::new(
            crate::tool::ToolFailureKind::InvalidInput,
            "file mutation rejected; read the existing file before editing it",
            "File must be read before mutation",
        )),
        started_at_ms: 1,
        completed_at_ms: 2,
        latency_ms: 1,
        parallel_group: None,
        transition_reason: Some("local_tool_exec_error".to_string()),
    };

    let operations_json = project_tool_operations_json(&[report]);
    let operations: Vec<Value> =
        serde_json::from_str(operations_json.expect("operations json").as_str())
            .expect("parse operations");
    let operation = operations.first().expect("operation");

    assert!(operation.get("kind").is_none());
    assert!(operation.get("title").is_none());
    assert_eq!(
        operation.get("status").and_then(Value::as_str),
        Some("error")
    );
    assert_eq!(
        operation.get("resultState").and_then(Value::as_str),
        Some("failed")
    );
    assert_eq!(
        operation.get("path").and_then(Value::as_str),
        Some("src/lib.rs")
    );
    assert!(operation.get("diffPreview").is_none());
    assert!(operation.get("added").is_none());
    assert!(operation.get("removed").is_none());
}

#[test]
fn bash_rg_observability_keeps_command_result_identity() {
    let report = ToolExecutionResult {
        tool_call_id: "call-rg".to_string(),
        tool_name: "bash".to_string(),
        status: "ok".to_string(),
        content: "core/src/runtime/tool_observability.rs:91:pub struct ToolObservability {"
            .to_string(),
        details: serde_json::json!({
            "schema": "bash_result_v1",
            "command": "rg -n \"ToolObservability\" core/src",
            "exitCode": 0,
            "timedOut": false,
            "stdout": "core/src/runtime/tool_observability.rs:91:pub struct ToolObservability {",
            "stderr": ""
        }),
        facts: Vec::new(),
        error: None,
        started_at_ms: 1,
        completed_at_ms: 2,
        latency_ms: 1,
        parallel_group: None,
        transition_reason: None,
    };

    let operations_json = project_tool_operations_json(&[report]);
    let operations: Vec<Value> =
        serde_json::from_str(operations_json.expect("operations json").as_str())
            .expect("parse operations");
    let operation = operations.first().expect("operation");

    assert_eq!(
        operation.get("kind").and_then(Value::as_str),
        Some("command")
    );
    assert!(operation.get("title").is_none());
    assert!(operation.get("path").is_none());
    assert!(operation.get("startLine").is_none());
    assert!(operation.get("matchCount").is_none());
    assert!(operation
        .get("outputPreview")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .contains("ToolObservability"));
    assert!(operation.get("commandPreview").is_none());
}

#[test]
fn failed_bash_observability_keeps_command_failure_identity() {
    let report = ToolExecutionResult {
        tool_call_id: "call-bash-failed".to_string(),
        tool_name: "bash".to_string(),
        status: "error".to_string(),
        content: "Git for Windows Bash is unavailable".to_string(),
        details: serde_json::json!({
            "schema": "bash_result_v1",
            "command": "ls -la ./docs 2>/dev/null | head -20",
            "executed": false,
            "exitCode": -1,
            "timedOut": false,
            "stdout": "",
            "stderr": ""
        }),
        facts: Vec::new(),
        error: Some(
            ToolErrorInfo::new(
                crate::tool::ToolFailureKind::SandboxUnavailable,
                "Git for Windows Bash is unavailable",
                "Sandbox unavailable",
            )
            .with_diagnostic("execution-host:banana"),
        ),
        started_at_ms: 1,
        completed_at_ms: 2,
        latency_ms: 1,
        parallel_group: None,
        transition_reason: Some("local_tool_exec_error".to_string()),
    };

    let operations_json = project_tool_operations_json(&[report]);
    let operations: Vec<Value> =
        serde_json::from_str(operations_json.expect("operations json").as_str())
            .expect("parse operations");
    let operation = operations.first().expect("operation");

    assert_eq!(
        operation.get("kind").and_then(Value::as_str),
        Some("command")
    );
    assert_eq!(
        operation.get("status").and_then(Value::as_str),
        Some("error")
    );
    assert_eq!(
        operation.get("resultState").and_then(Value::as_str),
        Some("failed")
    );
    assert_eq!(operation.get("path").and_then(Value::as_str), None);
    assert_eq!(operation.get("matchCount").and_then(Value::as_u64), None);
    let output_preview = operation
        .get("outputPreview")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(output_preview.contains("Git for Windows Bash"));
    assert!(!output_preview.contains("D:/missing/bash.exe"));
}

#[tokio::test]
async fn tool_failure_emits_only_tool_result_session_event() {
    let workspace_root = temp_dir_path("tool_failure_single_result_event_workspace");
    std::fs::create_dir_all(&workspace_root).expect("create workspace");
    let store = AgentRuntimeTestStore::new();
    let tool_layer = ToolLayer::new()
        .with_cwd(workspace_root.clone())
        .expect("tool layer workspace root");
    let engine =
        AgentRuntime::new_for_test_with_tools(store, tool_layer, AgentRuntimeConfig::default());

    let response = engine
        .process_turn_with_stream_sink_async(
            ProcessTurnRequest {
                session_id: "chat-tool-failure-single-event".to_string(),
                agent_run_identity: None,
                turn_id: "turn-tool-failure-single-event".to_string(),
                input: TurnInput::UserMessage("read the missing file".to_string()),
                generate_result: GenerateResult {
                    content: "I will inspect the requested file.".to_string(),
                    tool_calls: vec![ToolCallEnvelope {
                        id: "call-missing-read".to_string(),
                        name: "read".to_string(),
                        args_json: json!({ "path": "missing.txt" }).to_string(),
                    }],
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            },
            None,
        )
        .await
        .expect("tool failure should remain a normal ToolResult continuation");

    let session_events = response
        .runtime_events
        .iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .collect::<Vec<_>>();
    let failed_tool_result = session_events.iter().find(|event| {
        event.get("type").and_then(Value::as_str) == Some("ToolResult")
            && event.get("status").and_then(Value::as_str) == Some("error")
    });
    assert!(
        failed_tool_result.is_some(),
        "failed tool result must remain visible"
    );
    assert!(
        session_events
            .iter()
            .all(|event| event.get("type").and_then(Value::as_str) != Some("Error")),
        "tool-scoped failures must not emit a duplicate narrative Error event"
    );

    let _ = std::fs::remove_dir_all(workspace_root);
}

#[test]
fn canonical_runtime_event_facts_and_tool_receipts_share_core_schema() {
    let final_event = RuntimeEventProjection {
        event_id: "event-final".to_string(),
        version: crate::runtime::event::RUNTIME_EVENT_VERSION.to_string(),
        event_type: "Final".to_string(),
        at_ms: 3,
        session_id: "chat-1".to_string(),
        turn_id: "turn-1".to_string(),
        task_id: "turn-1".to_string(),
        parent_task_id: "turn-1".to_string(),
        status: "done".to_string(),
        visibility: crate::runtime::event::RuntimeEventVisibility::User,
        tool_name: None,
        process_state: Some(RuntimeProcessState::Synthesizing),
        payload: json!({"content": "answer"}),
        meta: json!({}),
    };
    let final_record = canonical_session_record_from_runtime_event(&final_event, "task-1")
        .expect("canonical final")
        .expect("durable final");
    assert_eq!(final_record.event_id, final_event.event_id);
    assert_eq!(final_record.event_type, SessionRecordType::AssistantMessage);
    assert_eq!(final_record.payload["modelMarkdown"], "answer");

    let compaction_event = RuntimeEventProjection {
        event_id: "event-compaction".to_string(),
        version: crate::runtime::event::RUNTIME_EVENT_VERSION.to_string(),
        event_type: "PromptCompaction".to_string(),
        at_ms: 2,
        session_id: "chat-1".to_string(),
        turn_id: "turn-1".to_string(),
        task_id: "prompt_compaction".to_string(),
        parent_task_id: "turn-1".to_string(),
        status: "done".to_string(),
        visibility: crate::runtime::event::RuntimeEventVisibility::Internal,
        tool_name: None,
        process_state: Some(RuntimeProcessState::Compressing),
        payload: json!({
            "summary": "上下文已压缩",
            "detail": null,
            "compaction": {
                "compactionId": "compact-1",
                "summaryMessageId": "summary-1",
                "summaryMarkdown": "# Summary\n\nExact bytes.",
                "firstKeptMessageId": "message-3"
            }
        }),
        meta: json!({}),
    };
    let compaction_record =
        canonical_session_record_from_runtime_event(&compaction_event, "task-1")
            .expect("canonical compaction")
            .expect("durable compaction");
    assert_eq!(compaction_record.event_type, SessionRecordType::Compaction);
    assert_eq!(
        compaction_record.payload["summaryMarkdown"],
        "# Summary\n\nExact bytes."
    );
    assert_eq!(compaction_record.payload["firstKeptMessageId"], "message-3");

    let call = ToolCallEnvelope {
        id: "call-1".to_string(),
        name: "read".to_string(),
        args_json: json!({"path": "README.md"}).to_string(),
    };
    let result = ToolExecutionResult {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status: "ok".to_string(),
        content: "contents".to_string(),
        details: json!({"path": "README.md"}),
        facts: Vec::new(),
        error: None,
        started_at_ms: 1,
        completed_at_ms: 2,
        latency_ms: 1,
        parallel_group: None,
        transition_reason: None,
    };
    let call_record = canonical_tool_call_record(
        "chat-1",
        "turn-1",
        "task-1",
        &call,
        "centaeris.builtin",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "README.md",
        2,
    )
    .expect("canonical tool call");
    let result_record =
        canonical_tool_result_record("chat-1", "turn-1", "task-1", &call, &result, 2)
            .expect("canonical tool result");
    let records = [call_record, result_record];
    assert_eq!(records[0].event_type, SessionRecordType::ToolCall);
    assert_eq!(records[1].event_type, SessionRecordType::ToolResult);
    assert_eq!(records[0].payload["displayTarget"], "README.md");
    assert_eq!(records[1].payload["callId"], "call-1");
    assert_eq!(records[1].payload["modelContent"], "contents");
    assert_eq!(records[1].payload["latencyMs"], 1);
    assert!(crate::session::session_record_projects_to_agent_run_stream(
        records[1].event_type
    ));

    let crate::session::SessionStreamProjection::SessionEvent { event, .. } =
        crate::session::project_committed_session_record(&records[1], 1)
            .expect("project durable tool result");
    assert_eq!(event.payload["latencyMs"], 1);
}

#[test]
fn canonical_model_request_record_embeds_ordered_observations() {
    let read_contract = crate::tool::list_tool_contracts()
        .into_iter()
        .find(|contract| contract.name == "read")
        .expect("read contract");
    let mcp_contract = crate::tool::ToolContract {
        name: "banana_search".to_string(),
        category: "external.mcp".to_string(),
        summary: "Search canonical legal sources.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"],
            "additionalProperties": false
        }),
        concurrency_safe: true,
        turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
        provider_id: Some("mcp:legal:source:2026-07-28".to_string()),
        schema_hash: None,
        scopes: vec![],
        dynamic: true,
    };
    let tool_definitions = [&read_contract, &mcp_contract]
        .into_iter()
        .map(|contract| crate::tool::ModelToolDefinition {
            name: contract.name.clone(),
            description: contract.summary.clone(),
            input_schema: contract.input_schema.clone(),
        })
        .collect::<Vec<_>>();
    let digest = crate::extension::composition::empty_composition_digest("test").expect("digest");
    let composition_environment = crate::extension::composition::AgentCompositionEnvironmentV1 {
        tool_contracts: vec![read_contract, mcp_contract],
        skill_catalog_digest: digest.clone(),
        plugin_activation_digest: digest.clone(),
        hook_composition_digest: digest.clone(),
        execution_profile_digest: digest,
        policy_version: "test.v1".to_string(),
        model_binding_override: None,
    };
    let context_message = crate::model::prepared_prompt::ModelMessageV1 {
        message_id: "msg:chat-1:turn-1:execution_context".to_string(),
        role: crate::model::prepared_prompt::ModelMessageRoleV1::User,
        content: "<environment_context><cwd>D:/workspace</cwd></environment_context>".to_string(),
        tool_calls: vec![],
        tool_call_id: None,
        reasoning_content: None,
    };
    let prepared_prompt = crate::model::prepared_prompt::PreparedPromptV1::new(
        Some("stable system prompt".to_string()),
        vec![context_message.clone()],
        tool_definitions,
        crate::tool::ModelToolChoice::Auto,
        32_768,
    )
    .expect("prepared prompt");
    let context_token_estimate =
        crate::model::prepared_prompt::estimate_text_tokens("stable system prompt")
            .saturating_add(crate::model::prepared_prompt::estimate_text_tokens(
                serde_json::to_string(&context_message)
                    .expect("serialize context message")
                    .as_str(),
            ))
            .saturating_add(crate::model::prepared_prompt::estimate_text_tokens(
                serde_json::to_string(&prepared_prompt.tool_definitions)
                    .expect("serialize tool definitions")
                    .as_str(),
            ));
    let request = ModelClientRequest {
        session_id: "chat-1".to_string(),
        turn_id: "turn-1".to_string(),
        loop_index: 0,
        provider_prompt_cache_key: Some("cache-key".to_string()),
        provider_prompt_cache_retention: Some("24h".to_string()),
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate,
        prepared_prompt,
        session_config: ModelSessionConfig::default(),
    };
    let incomplete_observation_error = ModelRequestStartedV1::from_request(
        ModelRequestPurposeV1::Main,
        &request,
        vec![
            ModelObservationV1::SystemPrompt {
                content: "stable system prompt".to_string(),
            },
            ModelObservationV1::ToolCatalog {
                tool_definitions: request.prepared_prompt.tool_definitions.clone(),
            },
        ],
        composition_environment
            .resolve_request(&request)
            .expect("composition"),
    )
    .expect_err("main request must checkpoint every prepared message");
    assert!(incomplete_observation_error.contains("coverage or order mismatch"));
    let started = ModelRequestStartedV1::from_request(
        ModelRequestPurposeV1::Main,
        &request,
        vec![
            ModelObservationV1::SystemPrompt {
                content: "stable system prompt".to_string(),
            },
            ModelObservationV1::ContextMessage {
                message: context_message,
            },
            ModelObservationV1::ToolCatalog {
                tool_definitions: request.prepared_prompt.tool_definitions.clone(),
            },
        ],
        composition_environment
            .resolve_request(&request)
            .expect("composition"),
    )
    .expect("model request boundary");
    let records = canonical_model_request_started_records("task-1", &started, 1)
        .expect("canonical model request records");

    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].event_type,
        SessionRecordType::ModelRequestStarted
    );
    assert!(!crate::session::session_record_projects_to_agent_run_stream(records[0].event_type));
    assert_eq!(records[0].payload["purpose"], "main");
    assert_eq!(records[0].payload["maxOutputTokens"], 32_768);
    assert_eq!(
        records[0].payload["observations"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(
        records[0].payload["observations"][0]["kind"],
        "system_prompt"
    );
    assert_eq!(records[0].payload["observations"][1]["kind"], "message");
    assert_eq!(
        records[0].payload["observations"][2]["kind"],
        "tool_catalog"
    );
    assert_eq!(
        records[0].payload["contextTokenBreakdown"]["systemPromptTokens"],
        crate::model::prepared_prompt::estimate_text_tokens("stable system prompt")
    );
    let breakdown = &records[0].payload["contextTokenBreakdown"];
    assert!(breakdown["systemToolTokens"].as_u64().unwrap_or_default() > 0);
    assert!(breakdown["mcpToolTokens"].as_u64().unwrap_or_default() > 0);
    assert_eq!(
        breakdown["mcpTools"][0]["providerId"],
        "mcp:legal:source:2026-07-28"
    );
    assert_eq!(breakdown["mcpTools"][0]["name"], "banana_search");
    assert_eq!(
        breakdown["mcpTools"][0]["tokens"],
        breakdown["mcpToolTokens"]
    );
    assert_eq!(
        [
            "systemPromptTokens",
            "systemToolTokens",
            "mcpToolTokens",
            "skillsTokens",
            "messageTokens",
        ]
        .into_iter()
        .map(|key| breakdown[key].as_u64().expect("breakdown token count"))
        .sum::<u64>(),
        u64::from(context_token_estimate)
    );
    assert!(records[0].payload.get("messageRefs").is_none());

    let mut active_records = crate::session::started_agent_run_records(
        "chat-1",
        "turn-1",
        "task-1",
        "test context usage",
        0,
    )
    .expect("agent run records")
    .to_vec();
    active_records.extend(records);
    let projection = crate::session::reduce_events("chat-1", active_records.iter())
        .expect("main request projection");
    assert_eq!(
        projection.context_token_estimate(),
        Some(u64::from(context_token_estimate))
    );
    assert_eq!(
        projection
            .context_token_breakdown()
            .map(|breakdown| breakdown.total_tokens()),
        Some(context_token_estimate)
    );
    assert_eq!(projection.context_token_estimate_updated_at_ms(), Some(1));
    assert!(!projection.is_compacting());

    active_records.push(
        crate::session::provider_usage_record(
            "chat-1",
            "turn-1",
            "task-1",
            &ProviderTokenUsageV1 {
                input_tokens: Some(20),
                output_tokens: Some(2),
                total_tokens: Some(22),
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
            2,
        )
        .expect("provider usage record"),
    );
    let projection =
        crate::session::reduce_events("chat-1", active_records.iter()).expect("usage projection");
    assert_eq!(projection.latest_provider_usage_updated_at_ms(), Some(2));
    assert_eq!(
        projection.latest_provider_usage_context_token_estimate(),
        Some(u64::from(context_token_estimate))
    );

    let compaction_prompt = crate::model::prepared_prompt::ModelMessageV1 {
        message_id: "msg:chat-1:compaction".to_string(),
        role: crate::model::prepared_prompt::ModelMessageRoleV1::User,
        content: "compact context".to_string(),
        tool_calls: vec![],
        tool_call_id: None,
        reasoning_content: None,
    };
    let compaction_context_token_estimate = crate::model::prepared_prompt::estimate_text_tokens(
        serde_json::to_string(&compaction_prompt)
            .expect("serialize compaction prompt")
            .as_str(),
    );
    let compaction_request = ModelClientRequest {
        session_id: "chat-1".to_string(),
        turn_id: "turn-compaction".to_string(),
        loop_index: 0,
        provider_prompt_cache_key: None,
        provider_prompt_cache_retention: None,
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate: compaction_context_token_estimate,
        prepared_prompt: crate::model::prepared_prompt::PreparedPromptV1::new(
            None,
            vec![compaction_prompt.clone()],
            vec![],
            crate::tool::ModelToolChoice::None,
            4_096,
        )
        .expect("compaction prompt"),
        session_config: ModelSessionConfig::default(),
    };
    let compaction_started = ModelRequestStartedV1::from_request(
        ModelRequestPurposeV1::Compaction,
        &compaction_request,
        vec![ModelObservationV1::CompactionPrompt {
            message: compaction_prompt,
        }],
        empty_agent_composition_environment()
            .resolve_request(&compaction_request)
            .expect("compaction composition"),
    )
    .expect("compaction request boundary");
    active_records.extend(
        canonical_model_request_started_records("task-1", &compaction_started, 2)
            .expect("canonical compaction records"),
    );
    let projection = crate::session::reduce_events("chat-1", active_records.iter())
        .expect("compaction projection");
    assert_eq!(
        projection.context_token_estimate(),
        Some(u64::from(context_token_estimate))
    );
    assert!(projection.is_compacting());

    active_records.push(
        crate::session::interrupted_agent_run_record(
            "chat-1",
            "turn-1",
            "task-1",
            "cancelled",
            "stopped",
            false,
            3,
        )
        .expect("terminal record"),
    );
    let projection = crate::session::reduce_events("chat-1", active_records.iter())
        .expect("terminal projection");
    assert!(!projection.is_compacting());
}
