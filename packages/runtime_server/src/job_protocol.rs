use std::collections::{HashMap, HashSet};
use std::env;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::postgres_store::PostgresRuntimeStore;

use centaeris_core::session::reliability::{
    agent_run_lifecycle_job_id, runtime_job_retry_delay_ms, ClaimDueRuntimeJobsRequest,
    CompleteRuntimeJobRequest, CreateDeadLetterRequest, DeadLetterRecord, DeadLetterReplayPolicy,
    DeadLetterStatus, FailRuntimeJobRequest, RenewRuntimeJobLeaseRequest, RuntimeBackoffPolicy,
    RuntimeJobFailureDisposition, RuntimeJobOutboxPort, RuntimeJobRecord, RuntimeJobStatus,
    RuntimeJobStorePort, ScheduleRuntimeJobRequest, StartRuntimeJobRequest, WakeRuntimeJobRequest,
    YieldRuntimeJobRequest, AGENT_RUN_LIFECYCLE_JOB_KIND,
};
use centaeris_core::session::store::{
    CreateDeadLetterAndFailJobRequest, RuntimeStoreTransactionPort,
};
use centaeris_core::session::store::{RuntimeJobWaiter, RuntimeJobWaiterCursor, RuntimeStore};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

const TERMINAL_EVENT_TYPE: &str = "runtime_job.terminal";
const SESSION_ID_PREFIX: &str = "session_";
const PYTHON_WORKER_JOB_KINDS: [&str; 2] = ["agent_run.lifecycle", "worker.noop"];

fn python_worker_job_kind(value: &str) -> bool {
    PYTHON_WORKER_JOB_KINDS.contains(&value)
}

pub fn handle(
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    store: &PostgresRuntimeStore,
) -> Option<(u16, Vec<u8>)> {
    if !path.starts_with("/internal/jobs") && !path.starts_with("/internal/job-outbox") {
        return None;
    }
    let token = match env::var("INTERNAL_API_TOKEN") {
        Ok(value) if !value.is_empty() => value,
        _ => return Some(response(500, json!({"error":"internal_token_unavailable"}))),
    };
    if Some(token.as_str()) != headers.get("x-internal-token").map(String::as_str) {
        return Some(response(401, json!({"error":"unauthorized"})));
    }
    let result = match (method, path) {
        ("POST", "/internal/jobs/schedule") => schedule(body, store),
        ("POST", "/internal/jobs/claim") => claim(body, store),
        ("POST", "/internal/jobs/wait") => wait(body, store),
        ("POST", "/internal/jobs/start") => start(body, store),
        ("POST", "/internal/jobs/heartbeat") => heartbeat(body, store),
        ("POST", "/internal/jobs/yield") => yield_job(body, store),
        ("POST", "/internal/jobs/complete") => complete(body, store),
        ("POST", "/internal/jobs/fail") => fail(body, store),
        ("POST", "/internal/jobs/reconcile") => reconcile(body, store),
        ("POST", "/internal/job-outbox/pending") => pending_outbox(body, store),
        ("POST", "/internal/job-outbox/published") => publish_outbox(body, store),
        ("POST", "/internal/job-outbox/wake-waiter") => wake_waiter(body, store),
        ("POST", "/internal/job-outbox/reconcile-waiters") => reconcile_waiters(body, store),
        ("GET", _) if path.starts_with("/internal/jobs/") => get_job(path, store),
        _ => Err((404, "job_protocol_not_found")),
    };
    Some(match result {
        Ok(value) => response(200, value),
        Err((status, error)) => response(status, json!({"error":error})),
    })
}

type ProtocolResult = Result<Value, (u16, &'static str)>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScheduleRequest {
    schema: String,
    job_id: String,
    job_kind: String,
    run_at_ms: i64,
    max_retries: u32,
    idempotency_key: String,
    session_id: Option<String>,
    payload_ref: Option<String>,
    workspace_id: Option<String>,
}

fn schedule(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: ScheduleRequest = decode(body)?;
    if !valid_schedule_request(&request) {
        return Err((400, "job_schedule_invalid"));
    }
    let now = now_ms().map_err(|_| (500, "clock_unavailable"))?;
    let result = store
        .schedule_worker_job(
            ScheduleRuntimeJobRequest {
                job: RuntimeJobRecord {
                    job_id: request.job_id,
                    job_kind: request.job_kind,
                    status: RuntimeJobStatus::Queued,
                    run_at_ms: request.run_at_ms,
                    lease_owner: None,
                    lease_expires_at_ms: None,
                    heartbeat_at_ms: None,
                    retry_count: 0,
                    max_retries: request.max_retries,
                    backoff_policy: RuntimeBackoffPolicy::default(),
                    idempotency_key: request.idempotency_key,
                    session_id: request.session_id,
                    branch_id: None,
                    checkpoint_id: None,
                    payload_ref: request.payload_ref,
                    output_refs: vec![],
                    last_error: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                },
            },
            request.workspace_id.as_deref(),
        )
        .map_err(|_| (409, "job_schedule_conflict"))?;
    Ok(json!({"disposition":result.disposition,"job":result.job}))
}

fn valid_schedule_request(request: &ScheduleRequest) -> bool {
    request.schema == "runtime.job.schedule.v1"
        && (if request.job_kind == AGENT_RUN_LIFECYCLE_JOB_KIND {
            request
                .workspace_id
                .as_deref()
                .is_some_and(|value| bounded(value, 1, 64))
        } else {
            request.workspace_id.is_none()
        })
        && runtime_job_id(&request.job_id)
        && kind(&request.job_kind)
        && request.max_retries <= 10
        && bounded(&request.idempotency_key, 1, 160)
        && request
            .session_id
            .as_deref()
            .is_none_or(|value| identifier(value, SESSION_ID_PREFIX))
        && request.payload_ref.as_deref().is_none_or(safe_ref)
}

fn get_job(path: &str, store: &PostgresRuntimeStore) -> ProtocolResult {
    let job_id = path.trim_start_matches("/internal/jobs/");
    if !runtime_job_id(job_id) {
        return Err((400, "job_id_invalid"));
    }
    let job = store
        .get_runtime_job(job_id)
        .map_err(|_| (500, "job_store_failed"))?
        .ok_or((404, "job_not_found"))?;
    Ok(json!({"job":job}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimRequest {
    schema: String,
    worker_id: String,
    job_id: Option<String>,
    job_kind: String,
    now_ms: i64,
    lease_ms: u64,
    limit: usize,
}

fn claim(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: ClaimRequest = decode(body)?;
    if request.schema != "runtime.job.claim.v1"
        || !bounded(&request.worker_id, 16, 160)
        || !request.worker_id.starts_with("worker:")
        || request
            .job_id
            .as_deref()
            .is_some_and(|value| !runtime_job_id(value))
        || !python_worker_job_kind(request.job_kind.as_str())
        || !(1_000..=300_000).contains(&request.lease_ms)
        || !(1..=32).contains(&request.limit)
    {
        return Err((400, "job_claim_invalid"));
    }
    let jobs = store
        .claim_worker_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: request.now_ms,
            worker_id: request.worker_id,
            job_id: request.job_id,
            job_kind: Some(request.job_kind),
            session_id: None,
            limit: request.limit,
            lease_ms: request.lease_ms,
        })
        .map_err(|error| {
            if error == "execution claim busy" {
                (503, "execution_claim_busy")
            } else {
                (409, "job_claim_rejected")
            }
        })?;
    Ok(json!({"jobs":jobs}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WaitRequest {
    schema: String,
    job_kinds: Vec<String>,
    wait_ms: u64,
}

fn wait(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: WaitRequest = decode(body)?;
    if !valid_wait_request(&request) {
        return Err((400, "job_wait_invalid"));
    }
    let result = store
        .wait_for_runtime_jobs(
            request.job_kinds.as_slice(),
            Duration::from_millis(request.wait_ms),
        )
        .map_err(|_| (503, "job_wait_unavailable"))?;
    Ok(json!({
        "schema": "runtime.job.wait.result.v1",
        "disposition": if result.ready { "ready" } else { "timeout" },
        "nextRunAtMs": result.next_run_at_ms,
    }))
}

fn valid_wait_request(request: &WaitRequest) -> bool {
    request.schema == "runtime.job.wait.v1"
        && (1..=20_000).contains(&request.wait_ms)
        && !request.job_kinds.is_empty()
        && request.job_kinds.len() <= PYTHON_WORKER_JOB_KINDS.len()
        && request
            .job_kinds
            .iter()
            .all(|job_kind| python_worker_job_kind(job_kind))
        && request.job_kinds.iter().collect::<HashSet<_>>().len() == request.job_kinds.len()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LeaseTransitionRequest {
    schema: String,
    job_id: String,
    lease_owner: String,
    at_ms: i64,
}

fn start(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: LeaseTransitionRequest = decode(body)?;
    validate_lease_request(&request, "runtime.job.start.v1")?;
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: request.job_id,
            lease_owner: request.lease_owner,
            started_at_ms: request.at_ms,
        })
        .map_err(|_| (409, "job_start_rejected"))?;
    Ok(json!({"ok":true}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HeartbeatRequest {
    schema: String,
    job_id: String,
    lease_owner: String,
    heartbeat_at_ms: i64,
    lease_ms: u64,
}

fn heartbeat(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: HeartbeatRequest = decode(body)?;
    if request.schema != "runtime.job.heartbeat.v1"
        || !runtime_job_id(&request.job_id)
        || !bounded(&request.lease_owner, 16, 160)
        || !(1_000..=300_000).contains(&request.lease_ms)
    {
        return Err((400, "job_heartbeat_invalid"));
    }
    store
        .renew_runtime_job_lease(RenewRuntimeJobLeaseRequest {
            job_id: request.job_id,
            lease_owner: request.lease_owner,
            heartbeat_at_ms: request.heartbeat_at_ms,
            lease_ms: request.lease_ms,
        })
        .map_err(|_| (409, "job_heartbeat_rejected"))?;
    Ok(json!({"ok":true}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct YieldRequest {
    schema: String,
    job_id: String,
    lease_owner: String,
    yielded_at_ms: i64,
    run_at_ms: i64,
    transition_reason: String,
}

fn yield_job(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: YieldRequest = decode(body)?;
    if !valid_yield_request(&request) {
        return Err((400, "job_yield_invalid"));
    }
    let run_at_ms = request.run_at_ms;
    let transition_reason = request.transition_reason.clone();
    store
        .yield_runtime_job(YieldRuntimeJobRequest {
            job_id: request.job_id,
            lease_owner: request.lease_owner,
            yielded_at_ms: request.yielded_at_ms,
            run_at_ms,
            transition_reason: request.transition_reason,
        })
        .map_err(|_| (409, "job_yield_rejected"))?;
    Ok(json!({
        "ok": true,
        "runAtMs": run_at_ms,
        "transitionReason": transition_reason,
    }))
}

fn valid_yield_request(request: &YieldRequest) -> bool {
    request.schema == "runtime.job.yield.v1"
        && runtime_job_id(&request.job_id)
        && bounded(&request.lease_owner, 16, 160)
        && request.run_at_ms >= request.yielded_at_ms
        && reason(&request.transition_reason)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteRequest {
    schema: String,
    job_id: String,
    lease_owner: String,
    completed_at_ms: i64,
    output_refs: Vec<String>,
}

fn complete(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: CompleteRequest = decode(body)?;
    if request.schema != "runtime.job.complete.v1"
        || !runtime_job_id(&request.job_id)
        || !bounded(&request.lease_owner, 16, 160)
        || request.output_refs.len() > 32
        || request.output_refs.iter().any(|value| !safe_ref(value))
    {
        return Err((400, "job_complete_invalid"));
    }
    let job = store
        .get_runtime_job(request.job_id.as_str())
        .map_err(|_| (500, "job_store_failed"))?
        .ok_or((404, "job_not_found"))?;
    if job.job_kind == "provider.poll"
        || (matches!(job.job_kind.as_str(), "agent_run.lifecycle" | "worker.noop")
            && !request.output_refs.is_empty())
    {
        return Err((409, "job_complete_output_contract_mismatch"));
    }
    if job.status == RuntimeJobStatus::Succeeded {
        return if job.output_refs == request.output_refs {
            Ok(json!({"ok":true}))
        } else {
            Err((409, "job_complete_output_contract_mismatch"))
        };
    }
    store
        .complete_runtime_job(CompleteRuntimeJobRequest {
            job_id: request.job_id,
            lease_owner: request.lease_owner,
            output_refs: request.output_refs,
            completed_at_ms: request.completed_at_ms,
        })
        .map_err(|_| (409, "job_complete_rejected"))?;
    Ok(json!({"ok":true}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FailRequest {
    schema: String,
    job_id: String,
    lease_owner: String,
    failed_at_ms: i64,
    error: String,
    retryable: bool,
}

fn fail(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: FailRequest = decode(body)?;
    if request.schema != "runtime.job.fail.v1"
        || !runtime_job_id(&request.job_id)
        || !bounded(&request.lease_owner, 16, 160)
        || !reason(&request.error)
    {
        return Err((400, "job_fail_invalid"));
    }
    let job = store
        .get_runtime_job(&request.job_id)
        .map_err(|_| (500, "job_store_failed"))?
        .ok_or((404, "job_not_found"))?;
    let attempt = job.retry_count.saturating_add(1);
    let fail_job = if !request.retryable {
        FailRuntimeJobRequest {
            job_id: request.job_id.clone(),
            lease_owner: request.lease_owner.clone(),
            failed_at_ms: request.failed_at_ms,
            last_error: request.error.clone(),
            next_run_at_ms: None,
            disposition: RuntimeJobFailureDisposition::Failed,
        }
    } else if attempt <= job.max_retries {
        let next = request
            .failed_at_ms
            .saturating_add(runtime_job_retry_delay_ms(
                &job.backoff_policy,
                attempt,
                &job.job_id,
                request.failed_at_ms,
            ));
        FailRuntimeJobRequest {
            job_id: request.job_id,
            lease_owner: request.lease_owner,
            failed_at_ms: request.failed_at_ms,
            last_error: request.error.clone(),
            next_run_at_ms: Some(next),
            disposition: RuntimeJobFailureDisposition::RetryScheduled,
        }
    } else {
        FailRuntimeJobRequest {
            job_id: request.job_id.clone(),
            lease_owner: request.lease_owner,
            failed_at_ms: request.failed_at_ms,
            last_error: request.error.clone(),
            next_run_at_ms: None,
            disposition: RuntimeJobFailureDisposition::DeadLettered,
        }
    };
    let disposition = fail_job.disposition.clone();
    if disposition == RuntimeJobFailureDisposition::DeadLettered {
        store
            .create_dead_letter_and_fail_job(CreateDeadLetterAndFailJobRequest {
                dead_letter: CreateDeadLetterRequest {
                    dead_letter: DeadLetterRecord {
                        dead_letter_id: format!("dead_letter:{}", job.job_id),
                        original_job_id: job.job_id,
                        job_kind: job.job_kind,
                        status: DeadLetterStatus::Open,
                        session_id: job.session_id,
                        branch_id: job.branch_id,
                        checkpoint_id: job.checkpoint_id,
                        payload_ref: job.payload_ref,
                        idempotency_key: job.idempotency_key,
                        failure_reason: "retry_exhausted".to_string(),
                        last_error: request.error,
                        attempts: attempt,
                        first_failed_at_ms: request.failed_at_ms,
                        last_failed_at_ms: request.failed_at_ms,
                        replay_policy: DeadLetterReplayPolicy::default(),
                        replayed_job_id: None,
                        dismissed_by: None,
                        dismissed_reason: None,
                        updated_at_ms: request.failed_at_ms,
                    },
                },
                fail_job,
            })
            .map_err(|_| (409, "job_fail_rejected"))?;
    } else {
        store
            .fail_runtime_job(fail_job)
            .map_err(|_| (409, "job_fail_rejected"))?;
    }
    Ok(json!({"disposition":disposition}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReconcileRequest {
    schema: String,
    now_ms: i64,
}

fn reconcile(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: ReconcileRequest = decode(body)?;
    if request.schema != "runtime.job.reconcile.v1" {
        return Err((400, "job_reconcile_invalid"));
    }
    let reclaimed = store
        .reclaim_expired_runtime_job_leases(request.now_ms)
        .map_err(|_| (500, "job_reconcile_failed"))?;
    // Late waiters are recovered from durable wait checkpoints by reconcile_waiters.
    // Replaying acknowledged notifications would turn completed history into work.
    Ok(json!({"reclaimed":reclaimed}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingRequest {
    schema: String,
    limit: usize,
}

fn pending_outbox(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: PendingRequest = decode(body)?;
    if request.schema != "runtime.job.outbox.pending.v1" || !(1..=256).contains(&request.limit) {
        return Err((400, "job_outbox_request_invalid"));
    }
    let events = store
        .list_pending_runtime_job_outbox(request.limit)
        .map_err(|_| (500, "job_outbox_failed"))?;
    Ok(json!({"events":events}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublishedRequest {
    schema: String,
    job_id: String,
    event_type: String,
    generation: u32,
    published_at_ms: i64,
}

fn publish_outbox(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: PublishedRequest = decode(body)?;
    if request.schema != "runtime.job.outbox.published.v1"
        || !runtime_job_id(&request.job_id)
        || request.event_type != TERMINAL_EVENT_TYPE
    {
        return Err((400, "job_outbox_publish_invalid"));
    }
    let disposition = store
        .mark_runtime_job_outbox_published(
            &request.job_id,
            &request.event_type,
            request.generation,
            request.published_at_ms,
        )
        .map_err(|_| (409, "job_outbox_publish_rejected"))?;
    Ok(json!({"disposition":disposition}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WakeWaiterRequest {
    schema: String,
    job_id: String,
    generation: u32,
    after: Option<RuntimeJobWaiterCursor>,
}

const WAITER_SCAN_LIMIT: usize = 256;
const WAITER_SCAN_BUDGET: Duration = Duration::from_millis(200);

fn validate_waiter_cursor(
    after: Option<&RuntimeJobWaiterCursor>,
) -> Result<(), (u16, &'static str)> {
    if after.is_some_and(|cursor| {
        cursor.checkpoint_id.trim().is_empty() || cursor.tool_call_id.trim().is_empty()
    }) {
        return Err((400, "job_waiter_cursor_invalid"));
    }
    Ok(())
}

fn wake_waiter(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: WakeWaiterRequest = decode(body)?;
    if request.schema != "runtime.job.waiter_wake.v1" || !runtime_job_id(&request.job_id) {
        return Err((400, "job_waiter_wake_invalid"));
    }
    validate_waiter_cursor(request.after.as_ref())?;
    let _delivery_generation = request.generation;
    let source = store
        .get_runtime_job(&request.job_id)
        .map_err(|_| (500, "job_store_failed"))?
        .ok_or((404, "job_not_found"))?;
    if !source.status.is_terminal() {
        return Err((409, "job_waiter_wake_source_not_terminal"));
    }
    let (checked, waiters, next) =
        process_waiter_page(store, Some(source), request.after.as_ref())?;
    Ok(
        json!({"disposition": if waiters.is_empty() {"no_waiter"} else {"woken"}, "checked":checked, "waiters":waiters, "next":next}),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReconcileWaitersRequest {
    schema: String,
    after: Option<RuntimeJobWaiterCursor>,
}

fn reconcile_waiters(body: &[u8], store: &PostgresRuntimeStore) -> ProtocolResult {
    let request: ReconcileWaitersRequest = decode(body)?;
    if request.schema != "runtime.job.waiters.reconcile.v1" {
        return Err((400, "job_waiters_reconcile_invalid"));
    }
    validate_waiter_cursor(request.after.as_ref())?;
    let (checked, waiters, next) = process_waiter_page(store, None, request.after.as_ref())?;
    Ok(json!({"disposition":"reconciled", "checked":checked, "waiters":waiters, "next":next}))
}

type WaiterPage = (usize, Vec<Value>, Option<RuntimeJobWaiterCursor>);

fn process_waiter_page(
    store: &PostgresRuntimeStore,
    source: Option<RuntimeJobRecord>,
    after: Option<&RuntimeJobWaiterCursor>,
) -> Result<WaiterPage, (u16, &'static str)> {
    let started = Instant::now();
    let page = store
        .list_runtime_job_waiters(
            source.as_ref().map(|job| job.job_id.as_str()),
            after,
            WAITER_SCAN_LIMIT,
        )
        .map_err(|_| (500, "job_waiter_index_failed"))?;
    let mut sources = HashMap::new();
    if let Some(source) = source {
        sources.insert(source.job_id.clone(), source);
    }
    let mut seen = HashSet::new();
    let mut waiters = Vec::new();
    let mut checked = 0;
    let mut last = None;
    for relation in &page {
        // This is a scheduling budget between operations, not transaction
        // cancellation. Always finish one relation so a slow call can progress.
        if checked > 0 && started.elapsed() >= WAITER_SCAN_BUDGET {
            break;
        }
        if !sources.contains_key(&relation.source_job_id) {
            let source = store
                .get_runtime_job(&relation.source_job_id)
                .map_err(|_| (500, "job_store_failed"))?
                .ok_or((409, "job_waiter_source_missing"))?;
            sources.insert(relation.source_job_id.clone(), source);
        }
        let source = sources.get(&relation.source_job_id).expect("source loaded");
        if source.job_kind != relation.source_job_kind {
            return Err((409, "job_waiter_binding_mismatch"));
        }
        if source.status.is_terminal() {
            if let Some(waiter) =
                wake_lifecycle_waiter(&relation.session_id, relation, source, &mut seen, store)?
            {
                waiters.push(waiter);
            }
        }
        checked += 1;
        last = Some(relation.cursor.clone());
    }
    let next = if checked < page.len() || page.len() == WAITER_SCAN_LIMIT {
        last
    } else {
        None
    };
    Ok((checked, waiters, next))
}
fn wake_lifecycle_waiter(
    checkpoint_session_id: &str,
    wait: &RuntimeJobWaiter,
    source: &RuntimeJobRecord,
    seen_lifecycle_jobs: &mut HashSet<String>,
    store: &PostgresRuntimeStore,
) -> Result<Option<Value>, (u16, &'static str)> {
    if source
        .session_id
        .as_deref()
        .is_some_and(|session_id| session_id != checkpoint_session_id)
    {
        return Err((409, "job_waiter_binding_mismatch"));
    }
    let lifecycle_job_id = agent_run_lifecycle_job_id(wait.agent_run_id.as_str())
        .map_err(|_| (409, "job_waiter_binding_mismatch"))?;
    if !seen_lifecycle_jobs.insert(format!("{}:{}", lifecycle_job_id, source.job_id)) {
        return Ok(None);
    }
    let lifecycle = store
        .get_runtime_job(lifecycle_job_id.as_str())
        .map_err(|_| (500, "job_store_failed"))?
        .ok_or((409, "agent_run_lifecycle_job_missing"))?;
    if lifecycle.job_kind != AGENT_RUN_LIFECYCLE_JOB_KIND
        || lifecycle.session_id.as_deref() != Some(checkpoint_session_id)
    {
        return Err((409, "agent_run_lifecycle_job_binding_mismatch"));
    }
    let disposition = store
        .wake_runtime_job(WakeRuntimeJobRequest {
            job_id: lifecycle_job_id.clone(),
            source_job_id: source.job_id.clone(),
            woken_at_ms: source.updated_at_ms,
            transition_reason: "runtime_job_terminal".to_string(),
        })
        .map_err(|_| (409, "agent_run_lifecycle_wake_rejected"))?;
    Ok(Some(json!({
        "agentRunId": wait.agent_run_id,
        "lifecycleJobId": lifecycle_job_id,
        "sourceJobId": source.job_id,
        "disposition": disposition,
    })))
}

fn validate_lease_request(
    request: &LeaseTransitionRequest,
    schema: &str,
) -> Result<(), (u16, &'static str)> {
    if request.schema != schema
        || !runtime_job_id(&request.job_id)
        || !bounded(&request.lease_owner, 16, 160)
    {
        return Err((400, "job_transition_invalid"));
    }
    Ok(())
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, (u16, &'static str)> {
    serde_json::from_slice(body).map_err(|_| (400, "invalid_json"))
}

fn response(status: u16, body: Value) -> (u16, Vec<u8>) {
    (
        status,
        serde_json::to_vec(&body).expect("JSON response serialization must succeed"),
    )
}

fn bounded(value: &str, min: usize, max: usize) -> bool {
    let length = value.len();
    length >= min && length <= max && !value.chars().any(char::is_control)
}

fn identifier(value: &str, prefix: &str) -> bool {
    bounded(value, prefix.len() + 1, 160)
        && value.starts_with(prefix)
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | ':' | '.')
        })
}

fn runtime_job_id(value: &str) -> bool {
    bounded(value, 1, 160)
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | ':' | '.')
        })
}

fn kind(value: &str) -> bool {
    bounded(value, 1, 120)
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        })
}

fn safe_ref(value: &str) -> bool {
    bounded(value, 3, 512)
        && ["storage:", "external_context:", "artifact:", "record:"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
        && !value.contains("..")
}

fn reason(value: &str) -> bool {
    bounded(value, 1, 160)
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-' | '.' | ':')
        })
}

fn now_ms() -> Result<i64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before epoch: {error}"))?;
    i64::try_from(duration.as_millis()).map_err(|_| "timestamp overflow".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_refs_and_reasons_are_bounded() {
        assert!(safe_ref("storage:source:1"));
        assert!(!safe_ref("storage:../secret"));
        assert!(reason("unknown_job_kind"));
        assert!(!reason("provider body\nsecret"));
        assert!(kind("worker.noop"));
        assert!(!kind("banana/exec"));
        assert!(runtime_job_id("job_worker_noop_1"));
        assert!(runtime_job_id("subagent.run:0123456789abcdef"));
        assert!(!runtime_job_id("banana/exec"));
    }

    #[test]
    fn runtime_job_schedule_requires_canonical_session_identity() {
        let mut request = serde_json::from_value::<ScheduleRequest>(json!({
            "schema": "runtime.job.schedule.v1",
            "jobId": "agent_run.lifecycle:agent_run_1",
            "jobKind": "agent_run.lifecycle",
            "runAtMs": 100,
            "maxRetries": 10,
            "idempotencyKey": "agent_run.lifecycle:agent_run_1:digest",
            "sessionId": "session_1",
            "payloadRef": "record:agent_run:agent_run_1",
            "workspaceId": "workspace_1",
        }))
        .expect("decode exact schedule request");
        assert!(valid_schedule_request(&request));

        request.workspace_id = None;
        assert!(!valid_schedule_request(&request));
        request.workspace_id = Some("workspace_1".to_string());

        request.session_id = Some("banana".to_string());
        assert!(!valid_schedule_request(&request));
    }

    #[test]
    fn runtime_job_yield_contract_is_exact_and_bounded() {
        let request: YieldRequest = serde_json::from_value(json!({
            "schema": "runtime.job.yield.v1",
            "jobId": "subagent.run:0123456789abcdef",
            "leaseOwner": "worker-yield-owner",
            "yieldedAtMs": 100,
            "runAtMs": 200,
            "transitionReason": "waiting_for_durable_input",
        }))
        .expect("decode exact yield request");
        assert!(valid_yield_request(&request));

        let unknown_field = serde_json::from_value::<YieldRequest>(json!({
            "schema": "runtime.job.yield.v1",
            "jobId": "subagent.run:0123456789abcdef",
            "leaseOwner": "worker-yield-owner",
            "yieldedAtMs": 100,
            "runAtMs": 200,
            "transitionReason": "waiting_for_durable_input",
            "banana": true,
        }));
        assert!(unknown_field.is_err());

        let reversed_time = YieldRequest {
            run_at_ms: 99,
            ..request
        };
        assert!(!valid_yield_request(&reversed_time));
    }

    #[test]
    fn runtime_job_outbox_ack_requires_generation() {
        let exact = serde_json::from_value::<PublishedRequest>(json!({
            "schema": "runtime.job.outbox.published.v1",
            "jobId": "subagent.run:0123456789abcdef",
            "eventType": "runtime_job.terminal",
            "generation": 3,
            "publishedAtMs": 100,
        }));
        assert!(exact.is_ok());
        let missing_generation = serde_json::from_value::<PublishedRequest>(json!({
            "schema": "runtime.job.outbox.published.v1",
            "jobId": "subagent.run:0123456789abcdef",
            "eventType": "runtime_job.terminal",
            "publishedAtMs": 100,
        }));
        assert!(missing_generation.is_err());
    }

    #[test]
    fn runtime_job_failure_requires_explicit_retryability() {
        let request = serde_json::from_value::<FailRequest>(json!({
            "schema": "runtime.job.fail.v1",
            "jobId": "run.lifecycle:run_1",
            "leaseOwner": "worker_1",
            "failedAtMs": 100,
            "error": "runtime_step_failed",
            "retryable": false,
        }))
        .expect("decode exact failure request");
        assert!(!request.retryable);
        assert!(serde_json::from_value::<FailRequest>(json!({
            "schema": "runtime.job.fail.v1",
            "jobId": "run.lifecycle:run_1",
            "leaseOwner": "worker_1",
            "failedAtMs": 100,
            "error": "runtime_step_failed",
        }))
        .is_err());
        assert!(serde_json::from_value::<FailRequest>(json!({
            "schema": "runtime.job.fail.v1",
            "jobId": "run.lifecycle:run_1",
            "leaseOwner": "worker_1",
            "failedAtMs": 100,
            "error": "runtime_step_failed",
            "retryable": false,
            "banana": true,
        }))
        .is_err());
    }

    #[test]
    fn runtime_job_claim_requires_exact_python_worker_kind() {
        for job_kind in PYTHON_WORKER_JOB_KINDS {
            let request = serde_json::from_value::<ClaimRequest>(json!({
                "schema": "runtime.job.claim.v1",
                "workerId": "worker:python-slot-0",
                "jobId": null,
                "jobKind": job_kind,
                "nowMs": 100,
                "leaseMs": 60_000,
                "limit": 1,
            }))
            .expect("decode exact worker claim");
            assert!(python_worker_job_kind(request.job_kind.as_str()));
        }
        for job_kind in [
            "provider.poll",
            "subagent.run",
            "knowledge.process",
            "banana",
        ] {
            assert!(!python_worker_job_kind(job_kind));
        }
        assert!(serde_json::from_value::<ClaimRequest>(json!({
            "schema": "runtime.job.claim.v1",
            "workerId": "worker:python-slot-0",
            "jobId": null,
            "jobKind": null,
            "nowMs": 100,
            "leaseMs": 60_000,
            "limit": 1,
        }))
        .is_err());
    }

    #[test]
    fn runtime_job_wait_contract_is_exact_bounded_and_v1() {
        let request = serde_json::from_value::<WaitRequest>(json!({
            "schema": "runtime.job.wait.v1",
            "jobKinds": PYTHON_WORKER_JOB_KINDS,
            "waitMs": 20_000,
        }))
        .expect("decode exact worker wait request");
        assert!(valid_wait_request(&request));

        for invalid in [
            json!({"schema":"runtime.job.wait.v2","jobKinds":["worker.noop"],"waitMs":20_000}),
            json!({"schema":"runtime.job.wait.v1","jobKinds":["worker.noop","worker.noop"],"waitMs":20_000}),
            json!({"schema":"runtime.job.wait.v1","jobKinds":["provider.poll"],"waitMs":20_000}),
            json!({"schema":"runtime.job.wait.v1","jobKinds":["worker.noop"],"waitMs":20_001}),
        ] {
            let invalid = serde_json::from_value::<WaitRequest>(invalid)
                .expect("decode structurally valid wait request");
            assert!(!valid_wait_request(&invalid));
        }
        assert!(serde_json::from_value::<WaitRequest>(json!({
            "schema":"runtime.job.wait.v1",
            "jobKinds":["worker.noop"],
            "waitMs":20_000,
            "banana":true,
        }))
        .is_err());
    }
}
