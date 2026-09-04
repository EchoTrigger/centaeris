use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::session::external_context::{
    ExternalContextObject, ExternalContextObjectLink, ExternalContextStorePort,
};
use crate::session::reliability::{
    runtime_job_retry_delay_ms, CancelRuntimeJobRequest, ClaimDueRuntimeJobsRequest,
    CompleteRuntimeJobRequest, CreateDeadLetterRequest, DeadLetterRecord, DeadLetterReplayPolicy,
    DeadLetterStatus, DeadLetterStorePort, FailRuntimeJobRequest, RuntimeJobFailureDisposition,
    RuntimeJobRecord, RuntimeJobStorePort,
};
use crate::session::store::{
    CreateDeadLetterAndFailJobRequest, RuntimeStoreTransactionPort,
    UpsertExternalContextLinkAndCompleteJobRequest,
};
use crate::tool::layer::{extract_dynamic_tool_pending_poll, ToolInvocationRequest, ToolLayer};
use crate::tool::ToolErrorInfo;

pub const PROVIDER_POLL_RUNTIME_JOB_KIND: &str = "provider.poll";
const PROVIDER_POLL_PAYLOAD_REF_PREFIX: &str = "provider-polling-json:";
const PROVIDER_POLL_PROGRESS_PREFIX: &str = "provider-polling-progress-json:";
const PROVIDER_POLL_MAX_CONSECUTIVE_ERROR_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone)]
pub enum ProviderPollingToolLayerResolution {
    Ready(Box<ToolLayer>),
    Failed(ToolErrorInfo),
    Stopped { reason: String },
}

pub type ProviderPollingToolLayerResolver =
    Arc<dyn Fn(&RuntimeJobRecord) -> ProviderPollingToolLayerResolution + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderPollingRuntimePayload {
    pub provider_id: String,
    pub tool_name: String,
    pub poll_key: String,
    #[serde(default = "default_poll_args")]
    pub poll_args: Value,
    pub source_agent_run_id: String,
    pub source_turn_id: String,
    pub source_tool_call_id: String,
    pub lease_ms: u64,
}

impl ProviderPollingRuntimePayload {
    fn validate(&self) -> Result<(), String> {
        for (field, value) in [
            ("providerId", self.provider_id.as_str()),
            ("toolName", self.tool_name.as_str()),
            ("pollKey", self.poll_key.as_str()),
            ("sourceAgentRunId", self.source_agent_run_id.as_str()),
            ("sourceTurnId", self.source_turn_id.as_str()),
            ("sourceToolCallId", self.source_tool_call_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("provider poll payload {field} is required"));
            }
        }
        if self.lease_ms == 0 {
            return Err("provider poll payload leaseMs must be positive".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderPollingProgress {
    pending_attempts: u32,
    consecutive_error_attempts: u32,
    last_transition: String,
}

#[derive(Debug, Clone)]
pub struct ProviderPollingSchedulerConfig {
    pub worker_id: String,
    pub tick_ms: u64,
    pub lease_ms: u64,
    pub claim_limit: usize,
    pub max_jobs_per_tick: usize,
}

impl Default for ProviderPollingSchedulerConfig {
    fn default() -> Self {
        Self {
            worker_id: format!("provider-poll-worker-{}", std::process::id()),
            tick_ms: 1_000,
            lease_ms: 30_000,
            claim_limit: 4,
            max_jobs_per_tick: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProviderPollingSchedulerStats {
    started: bool,
    tick_count: u64,
    reclaimed_leases: u64,
    success_count: u64,
    requeued_count: u64,
    dead_letter_count: u64,
    cancelled_count: u64,
    failure_count: u64,
    last_run_at_ms: i64,
    last_success_at_ms: i64,
    last_duration_ms: u64,
    last_claimed_jobs: usize,
    last_error: Option<String>,
}

#[derive(Debug)]
struct SchedulerRuntimeState {
    running: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<Mutex<ProviderPollingSchedulerStats>>,
}

pub struct StoreBackedProviderPollingScheduler<
    S: RuntimeJobStorePort
        + DeadLetterStorePort
        + ExternalContextStorePort
        + RuntimeStoreTransactionPort
        + Clone
        + Send
        + Sync
        + 'static,
> {
    store: S,
    tool_layer_resolver: ProviderPollingToolLayerResolver,
    config: ProviderPollingSchedulerConfig,
    state: SchedulerRuntimeState,
}

impl<
        S: RuntimeJobStorePort
            + DeadLetterStorePort
            + ExternalContextStorePort
            + RuntimeStoreTransactionPort
            + Clone
            + Send
            + Sync
            + 'static,
    > std::fmt::Debug for StoreBackedProviderPollingScheduler<S>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreBackedProviderPollingScheduler")
            .field("config", &self.config)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl<
        S: RuntimeJobStorePort
            + DeadLetterStorePort
            + ExternalContextStorePort
            + RuntimeStoreTransactionPort
            + Clone
            + Send
            + Sync
            + 'static,
    > StoreBackedProviderPollingScheduler<S>
{
    pub fn new(store: S, tool_layer: ToolLayer, config: ProviderPollingSchedulerConfig) -> Self {
        Self::new_with_tool_layer_resolver(
            store,
            Arc::new(move |_job| {
                ProviderPollingToolLayerResolution::Ready(Box::new(tool_layer.clone()))
            }),
            config,
        )
    }

    pub fn new_with_tool_layer_resolver(
        store: S,
        tool_layer_resolver: ProviderPollingToolLayerResolver,
        config: ProviderPollingSchedulerConfig,
    ) -> Self {
        Self {
            store,
            tool_layer_resolver,
            config,
            state: SchedulerRuntimeState {
                running: Arc::new(AtomicBool::new(false)),
                handle: Mutex::new(None),
                stats: Arc::new(Mutex::new(ProviderPollingSchedulerStats::default())),
            },
        }
    }
    fn run_loop(
        store: S,
        tool_layer_resolver: ProviderPollingToolLayerResolver,
        config: ProviderPollingSchedulerConfig,
        running: Arc<AtomicBool>,
        stats: Arc<Mutex<ProviderPollingSchedulerStats>>,
    ) {
        let async_runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(err) => {
                if let Ok(mut guard) = stats.lock() {
                    guard.started = false;
                    guard.last_error = Some(format!(
                        "provider poll scheduler async runtime creation failed: {err}"
                    ));
                }
                running.store(false, Ordering::SeqCst);
                return;
            }
        };
        while running.load(Ordering::SeqCst) {
            let tick_started_at_ms = now_ms();
            let tick_started_at = std::time::Instant::now();
            let mut tick_error: Option<String> = None;
            let mut claimed_count = 0usize;
            let mut reclaimed = 0usize;
            match store.reclaim_expired_runtime_job_leases(tick_started_at_ms) {
                Ok(value) => reclaimed = value,
                Err(err) => {
                    tick_error = Some(format!(
                        "reclaim expired provider poll leases failed: {err}"
                    ))
                }
            }

            let claim_limit = config.claim_limit.min(config.max_jobs_per_tick).max(1);
            match store.claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
                now_ms: tick_started_at_ms,
                worker_id: config.worker_id.clone(),
                job_id: None,
                job_kind: Some(PROVIDER_POLL_RUNTIME_JOB_KIND.to_string()),
                session_id: None,
                limit: claim_limit,
                lease_ms: config.lease_ms,
            }) {
                Ok(jobs) => {
                    claimed_count = jobs.len();
                    for job in jobs.into_iter().take(config.max_jobs_per_tick) {
                        let tool_layer = match tool_layer_resolver(&job) {
                            ProviderPollingToolLayerResolution::Ready(tool_layer) => tool_layer,
                            ProviderPollingToolLayerResolution::Failed(error) => {
                                match handle_provider_poll_failure(store.clone(), &job, &error) {
                                    Ok(ProviderPollingJobOutcome::Requeued) => {
                                        if let Ok(mut guard) = stats.lock() {
                                            guard.requeued_count =
                                                guard.requeued_count.saturating_add(1);
                                        }
                                    }
                                    Ok(ProviderPollingJobOutcome::DeadLettered) => {
                                        if let Ok(mut guard) = stats.lock() {
                                            guard.dead_letter_count =
                                                guard.dead_letter_count.saturating_add(1);
                                        }
                                    }
                                    Ok(ProviderPollingJobOutcome::Completed) => {}
                                    Ok(ProviderPollingJobOutcome::Cancelled) => {
                                        if let Ok(mut guard) = stats.lock() {
                                            guard.cancelled_count =
                                                guard.cancelled_count.saturating_add(1);
                                        }
                                    }
                                    Err(fail_err) => {
                                        tick_error = Some(fail_err);
                                        if let Ok(mut guard) = stats.lock() {
                                            guard.failure_count =
                                                guard.failure_count.saturating_add(1);
                                        }
                                    }
                                }
                                continue;
                            }
                            ProviderPollingToolLayerResolution::Stopped { reason } => {
                                match cancel_provider_poll_job(store.clone(), &job, reason.as_str())
                                {
                                    Ok(ProviderPollingJobOutcome::Cancelled) => {
                                        if let Ok(mut guard) = stats.lock() {
                                            guard.cancelled_count =
                                                guard.cancelled_count.saturating_add(1);
                                        }
                                    }
                                    Ok(_) => {}
                                    Err(cancel_err) => {
                                        tick_error = Some(cancel_err);
                                        if let Ok(mut guard) = stats.lock() {
                                            guard.failure_count =
                                                guard.failure_count.saturating_add(1);
                                        }
                                    }
                                }
                                continue;
                            }
                        };
                        match async_runtime.block_on(run_provider_poll_job(
                            store.clone(),
                            &tool_layer,
                            &job,
                        )) {
                            Ok(ProviderPollingJobOutcome::Completed) => {
                                if let Ok(mut guard) = stats.lock() {
                                    guard.success_count = guard.success_count.saturating_add(1);
                                    guard.last_success_at_ms = now_ms();
                                }
                            }
                            Ok(ProviderPollingJobOutcome::Requeued) => {
                                if let Ok(mut guard) = stats.lock() {
                                    guard.requeued_count = guard.requeued_count.saturating_add(1);
                                }
                            }
                            Ok(ProviderPollingJobOutcome::DeadLettered) => {
                                if let Ok(mut guard) = stats.lock() {
                                    guard.dead_letter_count =
                                        guard.dead_letter_count.saturating_add(1);
                                }
                            }
                            Ok(ProviderPollingJobOutcome::Cancelled) => {
                                if let Ok(mut guard) = stats.lock() {
                                    guard.cancelled_count = guard.cancelled_count.saturating_add(1);
                                }
                            }
                            Err(err) => {
                                tick_error = Some(err);
                                if let Ok(mut guard) = stats.lock() {
                                    guard.failure_count = guard.failure_count.saturating_add(1);
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    tick_error = Some(format!("claim due provider poll jobs failed: {err}"));
                }
            }

            if let Ok(mut guard) = stats.lock() {
                guard.tick_count = guard.tick_count.saturating_add(1);
                guard.reclaimed_leases = guard.reclaimed_leases.saturating_add(reclaimed as u64);
                guard.last_run_at_ms = tick_started_at_ms;
                guard.last_duration_ms = tick_started_at.elapsed().as_millis() as u64;
                guard.last_claimed_jobs = claimed_count;
                guard.last_error = tick_error;
            }

            thread::sleep(Duration::from_millis(config.tick_ms.max(10)));
        }
    }
}

impl<
        S: RuntimeJobStorePort
            + DeadLetterStorePort
            + ExternalContextStorePort
            + RuntimeStoreTransactionPort
            + Clone
            + Send
            + Sync
            + 'static,
    > StoreBackedProviderPollingScheduler<S>
{
    pub fn start(&self) -> Result<(), String> {
        if self.state.running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        if let Ok(mut guard) = self.state.stats.lock() {
            guard.started = true;
            guard.last_error = None;
        }

        let store = self.store.clone();
        let tool_layer_resolver = self.tool_layer_resolver.clone();
        let config = self.config.clone();
        let running = self.state.running.clone();
        let stats = self.state.stats.clone();
        let handle = thread::Builder::new()
            .name("provider_poll_scheduler".to_string())
            .spawn(move || Self::run_loop(store, tool_layer_resolver, config, running, stats))
            .map_err(|err| {
                self.state.running.store(false, Ordering::SeqCst);
                format!("spawn provider poll scheduler thread failed: {err}")
            })?;

        if let Ok(mut slot) = self.state.handle.lock() {
            if let Some(previous) = slot.take() {
                let _ = previous.join();
            }
            *slot = Some(handle);
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<(), String> {
        self.state.running.store(false, Ordering::SeqCst);
        if let Ok(mut slot) = self.state.handle.lock() {
            if let Some(handle) = slot.take() {
                let _ = handle.join();
            }
        }
        if let Ok(mut guard) = self.state.stats.lock() {
            guard.started = false;
        }
        Ok(())
    }

    pub fn status_snapshot(&self) -> Result<String, String> {
        let running = self.state.running.load(Ordering::SeqCst);
        let snapshot = self
            .state
            .stats
            .lock()
            .map_err(|err| format!("lock provider poll scheduler stats failed: {err}"))?
            .clone();
        Ok(json!({
            "started": running && snapshot.started,
            "workerId": self.config.worker_id,
            "tickMs": self.config.tick_ms,
            "leaseMs": self.config.lease_ms,
            "claimLimit": self.config.claim_limit,
            "maxJobsPerTick": self.config.max_jobs_per_tick,
            "tickCount": snapshot.tick_count,
            "reclaimedLeases": snapshot.reclaimed_leases,
            "successCount": snapshot.success_count,
            "requeuedCount": snapshot.requeued_count,
            "deadLetterCount": snapshot.dead_letter_count,
            "cancelledCount": snapshot.cancelled_count,
            "failureCount": snapshot.failure_count,
            "lastRunAtMs": snapshot.last_run_at_ms,
            "lastSuccessAtMs": snapshot.last_success_at_ms,
            "lastDurationMs": snapshot.last_duration_ms,
            "lastClaimedJobs": snapshot.last_claimed_jobs,
            "lastError": snapshot.last_error,
        })
        .to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderPollingJobOutcome {
    Completed,
    Requeued,
    DeadLettered,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderPollingFailureAction {
    Retry(ProviderPollingProgress),
    DeadLetter(&'static str),
}

pub fn build_provider_poll_payload_ref(
    payload: &ProviderPollingRuntimePayload,
) -> Result<String, String> {
    payload.validate()?;
    serde_json::to_string(payload)
        .map(|json| format!("{PROVIDER_POLL_PAYLOAD_REF_PREFIX}{json}"))
        .map_err(|err| format!("serialize provider polling payload failed: {err}"))
}

pub fn build_provider_poll_runtime_job_id(
    session_id: &str,
    source_agent_run_id: &str,
    turn_id: &str,
    tool_call_id: &str,
    provider_id: &str,
    tool_name: &str,
    poll_key: &str,
) -> String {
    let preimage = json!({
        "sessionId": session_id,
        "sourceAgentRunId": source_agent_run_id,
        "turnId": turn_id,
        "toolCallId": tool_call_id,
        "providerId": provider_id,
        "toolName": tool_name,
        "pollKey": poll_key,
    });
    let hash = stable_fnv1a64(preimage.to_string().as_str());
    format!("provider_poll_job:{hash:016x}")
}

pub fn parse_provider_poll_payload_ref(
    payload_ref: Option<&str>,
) -> Result<ProviderPollingRuntimePayload, String> {
    let raw = payload_ref
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "provider poll payload_ref is required".to_string())?;
    let json = raw
        .strip_prefix(PROVIDER_POLL_PAYLOAD_REF_PREFIX)
        .ok_or_else(|| "provider poll payload_ref prefix is invalid".to_string())?;
    let payload = serde_json::from_str::<ProviderPollingRuntimePayload>(json)
        .map_err(|err| format!("decode provider poll payload_ref failed: {err}"))?;
    payload.validate()?;
    Ok(payload)
}

fn default_poll_args() -> Value {
    Value::Object(serde_json::Map::new())
}

fn stable_fnv1a64(input: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn provider_polling_progress(job: &RuntimeJobRecord) -> Result<ProviderPollingProgress, String> {
    let Some(raw) = job.last_error.as_deref() else {
        return Ok(ProviderPollingProgress::default());
    };
    let encoded = raw
        .strip_prefix(PROVIDER_POLL_PROGRESS_PREFIX)
        .ok_or_else(|| {
            format!(
                "provider poll job has unsupported progress state: job_id={}",
                job.job_id
            )
        })?;
    serde_json::from_str(encoded)
        .map_err(|error| format!("decode provider poll progress failed: {error}"))
}

fn encode_provider_polling_progress(progress: &ProviderPollingProgress) -> Result<String, String> {
    serde_json::to_string(progress)
        .map(|encoded| format!("{PROVIDER_POLL_PROGRESS_PREFIX}{encoded}"))
        .map_err(|error| format!("encode provider poll progress failed: {error}"))
}

fn provider_polling_failure_action(
    job: &RuntimeJobRecord,
    error: &ToolErrorInfo,
) -> Result<ProviderPollingFailureAction, String> {
    if !error.retryable {
        return Ok(ProviderPollingFailureAction::DeadLetter(
            "provider_poll_permanent_error",
        ));
    }
    let mut progress = provider_polling_progress(job)?;
    progress.consecutive_error_attempts = progress.consecutive_error_attempts.saturating_add(1);
    progress.last_transition = format!("retryable_error:{}", error.kind.as_str());
    if progress.consecutive_error_attempts >= PROVIDER_POLL_MAX_CONSECUTIVE_ERROR_ATTEMPTS {
        return Ok(ProviderPollingFailureAction::DeadLetter(
            "provider_poll_retry_exhausted",
        ));
    }
    Ok(ProviderPollingFailureAction::Retry(progress))
}

fn provider_polling_pending_progress(
    job: &RuntimeJobRecord,
    poll_key: &str,
) -> Result<ProviderPollingProgress, String> {
    let mut progress = provider_polling_progress(job)?;
    progress.pending_attempts = progress.pending_attempts.saturating_add(1);
    progress.consecutive_error_attempts = 0;
    progress.last_transition = format!("pending:{poll_key}");
    Ok(progress)
}

async fn run_provider_poll_job<
    S: RuntimeJobStorePort
        + DeadLetterStorePort
        + ExternalContextStorePort
        + RuntimeStoreTransactionPort,
>(
    store: S,
    tool_layer: &ToolLayer,
    job: &RuntimeJobRecord,
) -> Result<ProviderPollingJobOutcome, String> {
    let payload = match parse_provider_poll_payload_ref(job.payload_ref.as_deref()) {
        Ok(payload) => payload,
        Err(error) => {
            return dead_letter_provider_poll_job(
                store,
                job,
                job.retry_count.saturating_add(1),
                "provider_poll_payload_invalid",
                error.as_str(),
            )
        }
    };
    let lease_owner = provider_poll_job_lease_owner(job, "run provider poll job")?.to_string();
    let args_json = serde_json::to_string(&payload.poll_args)
        .map_err(|err| format!("serialize provider poll args failed: {err}"))?;
    let report = tool_layer
        .execute_async(ToolInvocationRequest {
            tool_call_id: payload.source_tool_call_id.clone(),
            tool_name: payload.tool_name.clone(),
            args_json,
        })
        .await;
    if report.status != "ok" {
        let error = report.error.clone().unwrap_or_else(|| {
            ToolErrorInfo::from_unstructured_error("provider poll execution failed")
        });
        return handle_provider_poll_failure(store, job, &error);
    }

    if let Some(extracted) = extract_dynamic_tool_pending_poll(&report.details) {
        let pending = extracted?;
        let progress = match provider_polling_pending_progress(job, pending.spec.poll_key.as_str())
        {
            Ok(progress) => progress,
            Err(error) => {
                return dead_letter_provider_poll_job(
                    store,
                    job,
                    job.retry_count.saturating_add(1),
                    "provider_poll_progress_invalid",
                    error.as_str(),
                )
            }
        };
        if progress.pending_attempts >= job.max_retries.max(1) {
            return dead_letter_provider_poll_job(
                store,
                job,
                job.retry_count.saturating_add(1),
                "provider_poll_pending_exhausted",
                format!(
                    "provider poll remained pending after {} attempts poll_key={}",
                    progress.pending_attempts, pending.spec.poll_key
                )
                .as_str(),
            );
        }
        let next_run_at_ms = pending.spec.next_poll_at_ms.unwrap_or_else(|| {
            now_ms().saturating_add(runtime_job_retry_delay_ms(
                &job.backoff_policy,
                progress.pending_attempts,
                job.job_id.as_str(),
                now_ms(),
            ))
        });
        store.fail_runtime_job(FailRuntimeJobRequest {
            job_id: job.job_id.clone(),
            lease_owner: lease_owner.clone(),
            failed_at_ms: now_ms(),
            last_error: encode_provider_polling_progress(&progress)?,
            next_run_at_ms: Some(next_run_at_ms),
            disposition: RuntimeJobFailureDisposition::RetryScheduled,
        })?;
        return Ok(ProviderPollingJobOutcome::Requeued);
    }

    let mut output_refs = vec![];
    let mut completed_object = None;
    let mut completed_link = None;
    if let Some(extracted) = extract_external_context_object_from_tool_output(&report.details) {
        let mut object = extracted?;
        if !report.facts.is_empty() {
            object
                .metadata
                .as_object_mut()
                .ok_or_else(|| {
                    "provider poll external object metadata must be an object".to_string()
                })?
                .insert(
                    "toolExecutionFacts".to_string(),
                    serde_json::to_value(&report.facts)
                        .map_err(|error| format!("encode provider poll facts failed: {error}"))?,
                );
        }
        let session_id = provider_poll_job_session_id(job, "link provider poll external context")?;
        let linked_at_ms = report.completed_at_ms.max(object.updated_at_ms);
        let object_id = object.object_id.clone();
        completed_link = Some(ExternalContextObjectLink {
            session_id: session_id.to_string(),
            turn_id: Some(payload.source_turn_id.clone()),
            tool_call_id: Some(payload.source_tool_call_id.clone()),
            object_id: object_id.clone(),
            source_provider_id: object.source_provider_id.clone(),
            source_tool_name: object.source_tool_name.clone(),
            linked_at_ms,
        });
        completed_object = Some(object);
        output_refs.push(object_id);
    }
    if completed_object.is_none() && !report.facts.is_empty() {
        return Err("provider poll facts require a durable external object".to_string());
    }

    store.upsert_external_context_link_and_complete_job(
        UpsertExternalContextLinkAndCompleteJobRequest {
            object: completed_object,
            link: completed_link,
            complete_job: CompleteRuntimeJobRequest {
                job_id: job.job_id.clone(),
                lease_owner,
                output_refs,
                completed_at_ms: now_ms(),
            },
        },
    )?;
    Ok(ProviderPollingJobOutcome::Completed)
}

fn handle_provider_poll_failure<
    S: RuntimeJobStorePort
        + DeadLetterStorePort
        + ExternalContextStorePort
        + RuntimeStoreTransactionPort,
>(
    store: S,
    job: &RuntimeJobRecord,
    error: &ToolErrorInfo,
) -> Result<ProviderPollingJobOutcome, String> {
    let attempt = job.retry_count.saturating_add(1);
    let progress = match provider_polling_failure_action(job, error) {
        Ok(ProviderPollingFailureAction::Retry(progress)) => progress,
        Ok(ProviderPollingFailureAction::DeadLetter(reason)) => {
            return dead_letter_provider_poll_job(
                store,
                job,
                attempt,
                reason,
                error.model_message.as_str(),
            )
        }
        Err(progress_error) => {
            return dead_letter_provider_poll_job(
                store,
                job,
                attempt,
                "provider_poll_progress_invalid",
                progress_error.as_str(),
            )
        }
    };
    let lease_owner = provider_poll_job_lease_owner(job, "fail provider poll job")?.to_string();
    let failed_at_ms = now_ms();
    let next_run_at_ms = failed_at_ms.saturating_add(runtime_job_retry_delay_ms(
        &job.backoff_policy,
        progress.consecutive_error_attempts,
        job.job_id.as_str(),
        failed_at_ms,
    ));
    store.fail_runtime_job(FailRuntimeJobRequest {
        job_id: job.job_id.clone(),
        lease_owner,
        failed_at_ms,
        last_error: encode_provider_polling_progress(&progress)?,
        next_run_at_ms: Some(next_run_at_ms),
        disposition: RuntimeJobFailureDisposition::RetryScheduled,
    })?;
    Ok(ProviderPollingJobOutcome::Requeued)
}

fn cancel_provider_poll_job<S: RuntimeJobStorePort>(
    store: S,
    job: &RuntimeJobRecord,
    reason: &str,
) -> Result<ProviderPollingJobOutcome, String> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err("stop provider poll job requires a reason".to_string());
    }
    store.cancel_runtime_job(CancelRuntimeJobRequest {
        job_id: job.job_id.clone(),
        reason: reason.to_string(),
        cancelled_at_ms: now_ms(),
        expected_status: Some(job.status.clone()),
    })?;
    Ok(ProviderPollingJobOutcome::Cancelled)
}

fn dead_letter_provider_poll_job<
    S: RuntimeJobStorePort
        + DeadLetterStorePort
        + ExternalContextStorePort
        + RuntimeStoreTransactionPort,
>(
    store: S,
    job: &RuntimeJobRecord,
    attempt: u32,
    failure_reason: &str,
    last_error: &str,
) -> Result<ProviderPollingJobOutcome, String> {
    let lease_owner =
        provider_poll_job_lease_owner(job, "dead letter provider poll job")?.to_string();
    let failed_at_ms = now_ms();
    let dead_letter = CreateDeadLetterRequest {
        dead_letter: DeadLetterRecord {
            dead_letter_id: format!("dead_letter:{}", job.job_id),
            original_job_id: job.job_id.clone(),
            job_kind: job.job_kind.clone(),
            status: DeadLetterStatus::Open,
            session_id: job.session_id.clone(),
            branch_id: job.branch_id.clone(),
            checkpoint_id: job.checkpoint_id.clone(),
            payload_ref: job.payload_ref.clone(),
            idempotency_key: job.idempotency_key.clone(),
            failure_reason: failure_reason.to_string(),
            last_error: last_error.to_string(),
            attempts: attempt,
            first_failed_at_ms: failed_at_ms,
            last_failed_at_ms: failed_at_ms,
            replay_policy: DeadLetterReplayPolicy::default(),
            replayed_job_id: None,
            dismissed_by: None,
            dismissed_reason: None,
            updated_at_ms: failed_at_ms,
        },
    };
    store.create_dead_letter_and_fail_job(CreateDeadLetterAndFailJobRequest {
        dead_letter,
        fail_job: FailRuntimeJobRequest {
            job_id: job.job_id.clone(),
            lease_owner,
            failed_at_ms,
            last_error: last_error.to_string(),
            next_run_at_ms: None,
            disposition: RuntimeJobFailureDisposition::DeadLettered,
        },
    })?;
    Ok(ProviderPollingJobOutcome::DeadLettered)
}

fn provider_poll_job_lease_owner<'a>(
    job: &'a RuntimeJobRecord,
    action: &str,
) -> Result<&'a str, String> {
    required_provider_poll_job_field(job, "lease_owner", job.lease_owner.as_deref(), action)
}

fn provider_poll_job_session_id<'a>(
    job: &'a RuntimeJobRecord,
    action: &str,
) -> Result<&'a str, String> {
    required_provider_poll_job_field(job, "session_id", job.session_id.as_deref(), action)
}

fn required_provider_poll_job_field<'a>(
    job: &RuntimeJobRecord,
    field_name: &str,
    value: Option<&'a str>,
    action: &str,
) -> Result<&'a str, String> {
    value
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .ok_or_else(|| {
            format!(
                "{action} requires runtime job {field_name}: job_id={}",
                job.job_id
            )
        })
}

fn extract_external_context_object_from_tool_output(
    details: &Value,
) -> Option<Result<ExternalContextObject, String>> {
    let external_object = details
        .get("result")
        .and_then(|result| result.get("externalObject"))
        .or_else(|| details.get("externalObject"))?;
    let mode = external_object
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !mode.eq_ignore_ascii_case("externalObject") {
        return None;
    }
    let object_value = external_object.get("object").cloned()?;
    Some(
        serde_json::from_value::<ExternalContextObject>(object_value)
            .map_err(|err| format!("decode provider poll external object failed: {err}")),
    )
}

fn now_ms() -> i64 {
    crate::runtime::contracts::current_timestamp_ms()
}

#[cfg(test)]
mod tests {
    use super::{
        build_provider_poll_payload_ref, encode_provider_polling_progress,
        parse_provider_poll_payload_ref, provider_polling_failure_action,
        provider_polling_pending_progress, run_provider_poll_job, ProviderPollingFailureAction,
        ProviderPollingProgress, ProviderPollingRuntimePayload, PROVIDER_POLL_PAYLOAD_REF_PREFIX,
        PROVIDER_POLL_RUNTIME_JOB_KIND,
    };
    use crate::runtime::contracts::TimestampMs;
    use crate::session::external_context::*;
    use crate::session::reliability::*;
    use crate::session::store::*;
    use crate::tool::layer::{
        DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse,
        ToolExecutionFact, ToolLayer,
    };
    use crate::tool::{DynamicToolContract, DynamicToolRegistry, ToolErrorInfo, ToolFailureKind};
    use serde_json::{json, Value};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    #[derive(Debug, Clone, Copy)]
    struct NoopProviderPollStore;

    impl RuntimeJobStorePort for NoopProviderPollStore {
        fn schedule_runtime_job(
            &self,
            _req: ScheduleRuntimeJobRequest,
        ) -> Result<ScheduleRuntimeJobResult, String> {
            panic!("unexpected provider polling store call")
        }
        fn get_runtime_job(&self, _job_id: &str) -> Result<Option<RuntimeJobRecord>, String> {
            panic!("unexpected provider polling store call")
        }
        fn list_runtime_jobs(
            &self,
            _req: ListRuntimeJobsRequest,
        ) -> Result<Vec<RuntimeJobRecord>, String> {
            panic!("unexpected provider polling store call")
        }
        fn claim_due_runtime_jobs(
            &self,
            _req: ClaimDueRuntimeJobsRequest,
        ) -> Result<Vec<RuntimeJobRecord>, String> {
            panic!("unexpected provider polling store call")
        }
        fn start_runtime_job(&self, _req: StartRuntimeJobRequest) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn renew_runtime_job_lease(&self, _req: RenewRuntimeJobLeaseRequest) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn yield_runtime_job(&self, _req: YieldRuntimeJobRequest) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn wake_runtime_job(
            &self,
            _req: WakeRuntimeJobRequest,
        ) -> Result<WakeRuntimeJobDisposition, String> {
            panic!("unexpected provider polling store call")
        }
        fn complete_runtime_job(&self, _req: CompleteRuntimeJobRequest) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn fail_runtime_job(&self, _req: FailRuntimeJobRequest) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn cancel_runtime_job(&self, _req: CancelRuntimeJobRequest) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn reclaim_expired_runtime_job_leases(
            &self,
            _now_ms: TimestampMs,
        ) -> Result<usize, String> {
            panic!("unexpected provider polling store call")
        }
    }
    impl DeadLetterStorePort for NoopProviderPollStore {
        fn create_dead_letter(
            &self,
            _req: CreateDeadLetterRequest,
        ) -> Result<CreateDeadLetterResult, String> {
            panic!("unexpected provider polling store call")
        }
        fn get_dead_letter(
            &self,
            _dead_letter_id: &str,
        ) -> Result<Option<DeadLetterRecord>, String> {
            panic!("unexpected provider polling store call")
        }
        fn list_dead_letters(
            &self,
            _req: ListDeadLettersRequest,
        ) -> Result<Vec<DeadLetterRecord>, String> {
            panic!("unexpected provider polling store call")
        }
        fn mark_dead_letter_replaying(
            &self,
            _req: MarkDeadLetterReplayingRequest,
        ) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn mark_dead_letter_replayed(
            &self,
            _req: MarkDeadLetterReplayedRequest,
        ) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn replay_dead_letter(
            &self,
            _req: ReplayDeadLetterRequest,
        ) -> Result<ReplayDeadLetterResult, String> {
            panic!("unexpected provider polling store call")
        }
        fn dismiss_dead_letter(&self, _req: DismissDeadLetterRequest) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
    }
    impl ExternalContextStorePort for NoopProviderPollStore {
        fn upsert_external_context_object(
            &self,
            _object: ExternalContextObject,
        ) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn load_external_context_object(
            &self,
            _object_id: &str,
        ) -> Result<Option<ExternalContextObject>, String> {
            panic!("unexpected provider polling store call")
        }
        fn link_external_context_object(
            &self,
            _link: ExternalContextObjectLink,
        ) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }
        fn load_external_context_object_link(
            &self,
            _session_id: &str,
            _object_id: &str,
            _turn_id: &str,
            _tool_call_id: &str,
        ) -> Result<Option<ExternalContextObjectLink>, String> {
            panic!("unexpected provider polling store call")
        }
        fn list_external_context_objects(
            &self,
            _req: ListExternalContextObjectsRequest,
        ) -> Result<Vec<ExternalContextObjectIndexEntry>, String> {
            panic!("unexpected provider polling store call")
        }
    }
    impl RuntimeStoreTransactionPort for NoopProviderPollStore {
        fn save_wait_checkpoint(&self, _req: SaveWaitCheckpointRequest) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }

        fn consume_wait_checkpoint(
            &self,
            _req: ConsumeWaitCheckpointRequest,
        ) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }

        fn upsert_external_context_and_schedule_job(
            &self,
            _req: UpsertExternalContextAndScheduleJobRequest,
        ) -> Result<ScheduleRuntimeJobResult, String> {
            panic!("unexpected provider polling store call")
        }

        fn upsert_external_context_link_and_complete_job(
            &self,
            _req: UpsertExternalContextLinkAndCompleteJobRequest,
        ) -> Result<(), String> {
            panic!("unexpected provider polling store call")
        }

        fn create_dead_letter_and_fail_job(
            &self,
            _req: CreateDeadLetterAndFailJobRequest,
        ) -> Result<CreateDeadLetterResult, String> {
            panic!("unexpected provider polling store call")
        }
    }

    #[derive(Debug)]
    struct CompletedPollProvider;

    impl DynamicToolProvider for CompletedPollProvider {
        fn provider_id(&self) -> &str {
            "ragflow.clinic"
        }

        fn execute<'a>(
            &'a self,
            req: DynamicToolProviderRequest,
        ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
        {
            Box::pin(async move {
                let args =
                    serde_json::from_str::<Value>(req.args_json.as_str()).unwrap_or(Value::Null);
                Ok(DynamicToolProviderResponse {
                    content: "provider poll completed".to_string(),
                    details: json!({
                        "externalObject": {
                            "mode": "externalObject",
                            "pointer": {
                                "objectId": "external_context:provider_poll_done",
                                "objectKind": "externalKnowledge",
                                "source": "ragflow_clinic_search",
                                "recency": "warm",
                                "trust": "raw",
                                "reason": "provider poll completed",
                                "updatedAtMs": 5_200
                            },
                            "object": {
                                "schemaVersion": "external_context.v1",
                                "objectId": "external_context:provider_poll_done",
                                "objectKind": "externalKnowledge",
                                "sourceProviderId": "ragflow.clinic",
                                "sourceToolName": req.tool_name,
                                "title": "Provider poll completed",
                                "content": format!("poll result for {}", args.get("ticket").and_then(Value::as_str).unwrap_or("unknown")),
                                "metadata": {
                                    "args": args
                                },
                                "updatedAtMs": 5_200
                            }
                        }
                    }),
                    is_error: false,
                    facts: vec![ToolExecutionFact::CitationRecorded(json!({
                        "citationId": format!("citation:{}", "a".repeat(64)),
                        "inputRef": "input_provider_poll",
                        "ownerRef": "source_provider_poll",
                        "ownerKind": "sourceObject",
                        "displayName": "provider-poll.txt",
                        "evidenceKind": "workspaceSource",
                        "ownerSha256": format!("sha256:{}", "b".repeat(64)),
                        "sourceToolCallId": req.tool_call_id,
                        "sourceToolName": req.tool_name,
                        "locator": {"startLine": 1, "endLine": 1},
                    }))],
                    transition_reason: Some("provider_poll_completed".to_string()),
                })
            })
        }
    }

    fn dynamic_registry() -> Arc<DynamicToolRegistry> {
        Arc::new(
            DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
                name: "ragflow_clinic_search".to_string(),
                category: "external.context".to_string(),
                summary: "Search clinic knowledge".to_string(),
                input_schema: json!({
                    "type": "object",
                    "additionalProperties": true
                }),
                provider_id: "ragflow.clinic".to_string(),
                scopes: vec!["knowledge.read".to_string()],
                concurrency_safe: true,
                turn_behavior: crate::tool::ToolTurnBehavior::ContinueTurn,
            }])
            .expect("build provider poll dynamic registry"),
        )
    }

    fn sample_runtime_job(
        job_id: &str,
        run_at_ms: i64,
        payload_ref: String,
        max_retries: u32,
    ) -> RuntimeJobRecord {
        RuntimeJobRecord {
            job_id: job_id.to_string(),
            job_kind: PROVIDER_POLL_RUNTIME_JOB_KIND.to_string(),
            status: RuntimeJobStatus::Queued,
            run_at_ms,
            lease_owner: None,
            lease_expires_at_ms: None,
            heartbeat_at_ms: None,
            retry_count: 0,
            max_retries,
            backoff_policy: RuntimeBackoffPolicy::default(),
            idempotency_key: format!("idem:{job_id}"),
            session_id: Some("chat-provider-poll".to_string()),
            branch_id: None,
            checkpoint_id: None,
            payload_ref: Some(payload_ref),
            output_refs: vec![],
            last_error: None,
            created_at_ms: run_at_ms,
            updated_at_ms: run_at_ms,
        }
    }

    #[test]
    fn provider_poll_errors_have_a_separate_finite_retry_budget() {
        let mut job =
            sample_runtime_job("provider-poll-job-retry-budget", 0, "payload".into(), 1_200);
        job.retry_count = 420;
        job.last_error = Some(
            encode_provider_polling_progress(&ProviderPollingProgress {
                pending_attempts: 420,
                consecutive_error_attempts: 0,
                last_transition: "pending:ticket".to_string(),
            })
            .expect("encode progress"),
        );
        let error = ToolErrorInfo::new(
            ToolFailureKind::TimedOut,
            "provider poll timed out",
            "Provider poll timed out",
        )
        .with_retryable(true);

        for expected_attempt in 1..3 {
            let ProviderPollingFailureAction::Retry(progress) =
                provider_polling_failure_action(&job, &error).expect("retry decision")
            else {
                panic!("retryable provider error should remain retryable");
            };
            assert_eq!(progress.pending_attempts, 420);
            assert_eq!(progress.consecutive_error_attempts, expected_attempt);
            job.last_error = Some(encode_provider_polling_progress(&progress).expect("progress"));
        }
        assert_eq!(
            provider_polling_failure_action(&job, &error).expect("exhaustion decision"),
            ProviderPollingFailureAction::DeadLetter("provider_poll_retry_exhausted")
        );

        let pending = provider_polling_pending_progress(&job, "ticket")
            .expect("pending progress after retryable error");
        assert_eq!(pending.pending_attempts, 421);
        assert_eq!(pending.consecutive_error_attempts, 0);
    }

    #[test]
    fn provider_poll_permanent_error_is_terminal_on_first_failure() {
        let job = sample_runtime_job("provider-poll-job-permanent", 0, "payload".into(), 1_200);
        let error = ToolErrorInfo::new(
            ToolFailureKind::ProviderError,
            "document processing failed",
            "Document processing failed",
        );
        assert_eq!(
            provider_polling_failure_action(&job, &error).expect("permanent decision"),
            ProviderPollingFailureAction::DeadLetter("provider_poll_permanent_error")
        );
    }

    #[test]
    fn provider_poll_payload_requires_exact_agent_run_binding() {
        let missing_binding = format!(
            "{PROVIDER_POLL_PAYLOAD_REF_PREFIX}{}",
            json!({
                "providerId": "ragflow.clinic",
                "toolName": "ragflow_clinic_search",
                "pollKey": "ticket",
                "pollArgs": {},
                "sourceTurnId": "turn-provider-poll",
                "sourceToolCallId": "tc-provider-poll",
                "leaseMs": 30_000
            })
        );
        assert!(
            parse_provider_poll_payload_ref(Some(missing_binding.as_str()))
                .expect_err("sourceAgentRunId is required")
                .contains("sourceAgentRunId")
        );

        let unknown_field = format!(
            "{PROVIDER_POLL_PAYLOAD_REF_PREFIX}{}",
            json!({
                "providerId": "ragflow.clinic",
                "toolName": "ragflow_clinic_search",
                "pollKey": "ticket",
                "pollArgs": {},
                "sourceAgentRunId": "agent-run-provider-poll",
                "sourceTurnId": "turn-provider-poll",
                "sourceToolCallId": "tc-provider-poll",
                "leaseMs": 30_000,
                "agentRunId": "unsupported-alias"
            })
        );
        assert!(
            parse_provider_poll_payload_ref(Some(unknown_field.as_str()))
                .expect_err("unknown aliases must fail")
                .contains("unknown field")
        );
    }

    #[tokio::test]
    async fn provider_poll_job_requires_lease_owner_before_runtime_transition() {
        let store = NoopProviderPollStore;
        let payload_ref = build_provider_poll_payload_ref(&ProviderPollingRuntimePayload {
            provider_id: "ragflow.clinic".to_string(),
            tool_name: "ragflow_clinic_search".to_string(),
            poll_key: "ticket-done".to_string(),
            poll_args: json!({ "ticket": "ticket-done" }),
            source_agent_run_id: "agent-run-provider-poll".to_string(),
            source_turn_id: "turn-provider-poll".to_string(),
            source_tool_call_id: "tc-provider-poll".to_string(),
            lease_ms: 30_000,
        })
        .expect("build provider poll payload ref");
        let mut job = sample_runtime_job("provider-poll-job-missing-lease", 0, payload_ref, 4);
        job.status = RuntimeJobStatus::Leased;
        job.lease_owner = None;

        let tool_layer = ToolLayer::new_with_dynamic_tool_registry(dynamic_registry());
        let err = run_provider_poll_job(store, &tool_layer, &job)
            .await
            .expect_err("missing provider poll lease owner must fail");
        assert!(err.contains("lease_owner"));
        assert!(err.contains("provider-poll-job-missing-lease"));
    }

    #[tokio::test]
    async fn provider_poll_external_context_link_requires_session_id() {
        let store = NoopProviderPollStore;
        let payload_ref = build_provider_poll_payload_ref(&ProviderPollingRuntimePayload {
            provider_id: "ragflow.clinic".to_string(),
            tool_name: "ragflow_clinic_search".to_string(),
            poll_key: "ticket-done".to_string(),
            poll_args: json!({ "ticket": "ticket-done" }),
            source_agent_run_id: "agent-run-provider-poll".to_string(),
            source_turn_id: "turn-provider-poll".to_string(),
            source_tool_call_id: "tc-provider-poll".to_string(),
            lease_ms: 30_000,
        })
        .expect("build provider poll payload ref");
        let mut job = sample_runtime_job("provider-poll-job-missing-chat", 0, payload_ref, 4);
        job.status = RuntimeJobStatus::Leased;
        job.lease_owner = Some("provider-poll-worker-test".to_string());
        job.session_id = None;

        let mut tool_layer = ToolLayer::new_with_dynamic_tool_registry(dynamic_registry());
        tool_layer
            .register_dynamic_tool_provider(Arc::new(CompletedPollProvider))
            .expect("provider binding");
        let err = run_provider_poll_job(store, &tool_layer, &job)
            .await
            .expect_err("missing provider poll chat session must fail");
        assert!(err.contains("session_id"));
        assert!(err.contains("provider-poll-job-missing-chat"));
    }
}
