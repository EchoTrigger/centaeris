//! The hosted Run's leased execution owner. Django remains the platform authority.
mod lease;
pub(crate) mod outbox;

use crate::agent_run_execution::Failure;
use crate::http::agent_runs::finalization::{FinalizeRequest, Platform, TerminalState};
use crate::*;
use centaeris_core::session::reliability::{
    ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest, RenewRuntimeJobLeaseRequest,
    StartRuntimeJobRequest, YieldRuntimeJobRequest,
};
use serde::Deserialize;
use tokio::sync::watch;

#[derive(Clone)]
struct Shutdown(watch::Sender<bool>);
impl Default for Shutdown {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl Shutdown {
    fn is_stopped(&self) -> bool {
        *self.0.borrow()
    }
    async fn wait(&self, duration: Duration) -> bool {
        let mut changed = self.0.subscribe();
        if *changed.borrow() {
            return true;
        }
        tokio::select! { _ = changed.changed() => true, _ = tokio::time::sleep(duration) => false }
    }
    fn stop(&self) {
        self.0.send_replace(true);
    }
}

#[derive(Clone)]
struct Config {
    lease: Duration,
    recheck_ms: i64,
    mode: String,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let lease = env::var("RUNTIME_JOB_LEASE_MS")
            .unwrap_or_else(|_| "60000".into())
            .parse::<u64>()
            .map_err(|_| "RUNTIME_JOB_LEASE_MS is invalid")?;
        if !(1000..=300000).contains(&lease) {
            return Err("RUNTIME_JOB_LEASE_MS must be between 1000 and 300000".into());
        }
        let recheck_ms = env::var("AGENT_RUN_LIFECYCLE_RECHECK_MS")
            .unwrap_or_else(|_| "30000".into())
            .parse::<i64>()
            .map_err(|_| "AGENT_RUN_LIFECYCLE_RECHECK_MS is invalid")?;
        if recheck_ms <= 0 {
            return Err("AGENT_RUN_LIFECYCLE_RECHECK_MS must be positive".into());
        }
        let mode = env::var("AGENT_RUN_EXECUTION_MODE").unwrap_or_else(|_| "perRun".into());
        if !matches!(mode.as_str(), "perRun" | "retained") {
            return Err("AGENT_RUN_EXECUTION_MODE is invalid".into());
        }
        Ok(Self {
            lease: Duration::from_millis(lease),
            recheck_ms,
            mode,
        })
    }
}

#[derive(Clone, Copy)]
enum Lane {
    Execute,
    Advance,
    Cleanup,
}
impl Lane {
    fn stage(self) -> Option<&'static str> {
        match self {
            Self::Execute => None,
            Self::Advance => Some("advance"),
            Self::Cleanup => Some("cleanup"),
        }
    }
}

pub(crate) struct Executor {
    shutdown: Shutdown,
    dispatchers: Vec<tokio::task::JoinHandle<()>>,
    _platform: Platform,
}
impl Drop for Executor {
    fn drop(&mut self) {
        self.shutdown.stop();
    }
}
impl Executor {
    pub(crate) async fn shutdown(&mut self) {
        self.shutdown.stop();
        for task in self.dispatchers.drain(..) {
            let _ = task.await;
        }
    }
}

pub(crate) fn start(state: HttpServerState) -> Result<Executor, String> {
    let config = Config::from_env()?;
    let platform = Platform::new().map_err(|_| "lifecycle platform client unavailable")?;
    let mut executor = Executor {
        shutdown: Shutdown::default(),
        dispatchers: vec![],
        _platform: platform.clone(),
    };
    for lane in [Lane::Execute, Lane::Advance, Lane::Cleanup] {
        let mut state = state.clone();
        match lane {
            Lane::Execute => {}
            Lane::Advance => state.job_store = Arc::new(state.job_store.execution_control_view()),
            Lane::Cleanup => state.job_store = Arc::new(state.job_store.cleanup_view()),
        }
        let config = config.clone();
        let platform = platform.clone();
        let shutdown = executor.shutdown.clone();
        executor
            .dispatchers
            .push(state.runtime.handle().spawn(dispatch(
                state.clone(),
                platform,
                config,
                lane,
                shutdown,
            )));
    }
    let store = state.job_store.clone();
    executor.dispatchers.push(
        state
            .runtime
            .spawn(outbox::run(store.clone(), executor.shutdown.clone())),
    );
    executor.dispatchers.push(
        state
            .runtime
            .spawn(outbox::reconcile(store.clone(), executor.shutdown.clone())),
    );
    let shutdown = executor.shutdown.clone();
    executor.dispatchers.push(state.runtime.spawn(async move {
        while !shutdown.is_stopped() {
            let store = store.clone();
            let result = tokio::task::spawn_blocking(move || {
                store.reclaim_expired_runtime_job_leases(now_ms()?)
            })
            .await;
            if !matches!(result, Ok(Ok(_))) {
                eprintln!("Run lease reconciliation unavailable");
            }
            shutdown.wait(Duration::from_secs(5)).await;
        }
    }));
    Ok(executor)
}

async fn dispatch(
    state: HttpServerState,
    platform: Platform,
    config: Config,
    lane: Lane,
    shutdown: Shutdown,
) {
    let mut active = tokio::task::JoinSet::new();
    let mut sequence = 0_usize;
    while !shutdown.is_stopped() {
        while let Some(result) = active.try_join_next() {
            if result.is_err() {
                eprintln!("Run lifecycle task failed; lease remains reclaimable");
            }
        }
        let request = ClaimDueRuntimeJobsRequest {
            now_ms: now_ms().unwrap_or_default(),
            worker_id: format!(
                "runtime:{}:{}",
                std::process::id(),
                lane.stage().unwrap_or("execute")
            ),
            job_id: None,
            job_kind: Some(
                if lane.stage().is_none() && sequence % 2 == 1 {
                    "worker.noop"
                } else {
                    AGENT_RUN_LIFECYCLE_JOB_KIND
                }
                .into(),
            ),
            session_id: None,
            limit: 1,
            lease_ms: config.lease.as_millis() as u64,
        };
        let store = state.job_store.clone();
        sequence = sequence.wrapping_add(1);
        let claimed = tokio::task::spawn_blocking(move || match lane.stage() {
            Some(stage) => store.claim_control_jobs(request, stage),
            None => store.claim_worker_jobs(request),
        })
        .await;
        match claimed {
            Ok(Ok(jobs)) if !jobs.is_empty() => {
                for job in jobs {
                    if shutdown.is_stopped() {
                        break;
                    }
                    let state = state.clone();
                    let platform = platform.clone();
                    let config = config.clone();
                    let shutdown = shutdown.clone();
                    active.spawn_blocking(move || {
                        run_claimed(state, platform, config, lane, job, shutdown)
                    });
                }
            }
            Ok(Ok(_)) if lane.stage().is_none() => {
                let store = state.job_store.clone();
                let result = tokio::task::spawn_blocking(move || {
                    store.wait_for_runtime_jobs(
                        &[AGENT_RUN_LIFECYCLE_JOB_KIND.into(), "worker.noop".into()],
                        Duration::from_secs(20),
                    )
                })
                .await;
                if !matches!(result, Ok(Ok(_))) {
                    shutdown.wait(Duration::from_secs(1)).await;
                }
            }
            Ok(Ok(_)) => {
                shutdown.wait(Duration::from_millis(250)).await;
            }
            _ => {
                eprintln!("Run claim unavailable");
                shutdown.wait(Duration::from_secs(1)).await;
            }
        }
    }
    // Running blocking attempts observe shutdown through their lease guard. They
    // leave uncertain work to durable lease reclaim instead of declaring completion.
}

fn owner(job: &RuntimeJobRecord) -> Result<&str, Failure> {
    job.lease_owner
        .as_deref()
        .ok_or_else(|| Failure::rejected(409, "agent_run_lifecycle_lease_lost"))
}

struct Binding<'a> {
    run: &'a str,
    digest: &'a str,
}
fn binding(job: &RuntimeJobRecord) -> Result<Binding<'_>, Failure> {
    let invalid = || Failure::rejected(409, "agent_run_lifecycle_binding_invalid");
    let run = job
        .job_id
        .strip_prefix("agent_run.lifecycle:")
        .filter(|run| !run.is_empty())
        .ok_or_else(invalid)?;
    let prefix = format!("{}:", job.job_id);
    let digest = job
        .idempotency_key
        .strip_prefix(&prefix)
        .ok_or_else(invalid)?;
    if job.job_kind != AGENT_RUN_LIFECYCLE_JOB_KIND
        || job.payload_ref.as_deref() != Some(format!("record:agent_run:{run}").as_str())
        || job.session_id.as_deref().is_none_or(str::is_empty)
        || agent_run_authorization::validate_sha256("authorizationDigest", digest).is_err()
    {
        return Err(invalid());
    }
    Ok(Binding { run, digest })
}

enum Next {
    Complete,
    Yield { at: i64, reason: String },
}

fn run_claimed(
    state: HttpServerState,
    platform: Platform,
    config: Config,
    lane: Lane,
    job: RuntimeJobRecord,
    shutdown: Shutdown,
) {
    let result = (|| -> Result<(), Failure> {
        let owner = owner(&job)?;
        if job.job_kind != "worker.noop" {
            binding(&job)?;
        }
        match lane.stage() {
            Some(_) => state.job_store.start_control_job(&job.job_id, owner)?,
            None => state.job_store.start_runtime_job(StartRuntimeJobRequest {
                job_id: job.job_id.clone(),
                lease_owner: owner.into(),
                started_at_ms: now_ms()?,
            })?,
        }
        let lease = lease::Lease::acquire(state.job_store.clone(), &job, config.lease, shutdown)?;
        let next = if job.job_kind == "worker.noop" {
            Ok(Next::Complete)
        } else if let Some(stage) = lane.stage() {
            advance_control(&state, &platform, &config, &job, stage, &lease)
        } else {
            advance(&state, &platform, &config, &job, &lease)
        };
        let next = match next {
            Ok(next) => next,
            Err(error) => {
                lease.require()?;
                if let Some(reason) =
                    wait_reason(&error).or_else(|| lane.stage().map(|_| "control_retry_pending"))
                {
                    Next::Yield {
                        at: now_ms()?.saturating_add(1000),
                        reason: reason.into(),
                    }
                } else {
                    lease.handoff()?;
                    return fail(&state, &platform, &job, error);
                }
            }
        };
        lease.handoff()?;
        match next {
            Next::Complete => state
                .job_store
                .complete_runtime_job(CompleteRuntimeJobRequest {
                    job_id: job.job_id.clone(),
                    lease_owner: owner.into(),
                    output_refs: vec![],
                    completed_at_ms: now_ms()?,
                })?,
            Next::Yield { at, reason } => {
                let now = now_ms()?;
                state.job_store.yield_runtime_job(YieldRuntimeJobRequest {
                    job_id: job.job_id.clone(),
                    lease_owner: owner.into(),
                    yielded_at_ms: now,
                    run_at_ms: at.max(now),
                    transition_reason: reason,
                })?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        eprintln!("Run lifecycle attempt stopped; jobId={}", job.job_id);
    }
}

#[derive(Deserialize)]
#[serde(
    tag = "disposition",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Resolution {
    Ready {
        schema: String,
        agent_run_start: Box<AgentRunStart>,
    },
    Terminal {
        schema: String,
        terminal_state: TerminalState,
        agent_run_cleanup: Box<contract::AgentRunCleanup>,
    },
    Released {
        schema: String,
        terminal_state: TerminalState,
        release_receipt_id: String,
    },
}

fn resolve(
    platform: &Platform,
    config: &Config,
    job: &RuntimeJobRecord,
) -> Result<Resolution, Failure> {
    let binding = binding(job)?;
    let resolved: Resolution = platform.post("/internal/agent-run-lifecycle/resolve", json!({"schema":"runtime.agent_run_lifecycle.resolve.v1","jobId":job.job_id,"leaseOwner":owner(job)?,"agentRunId":binding.run,"authorizationDigest":binding.digest,"executionMode":config.mode})).map_err(|error|error.failure())?;
    let schema = match &resolved {
        Resolution::Ready { schema, .. }
        | Resolution::Terminal { schema, .. }
        | Resolution::Released { schema, .. } => schema,
    };
    if schema != "runtime.agent_run_lifecycle.resolved.v1" {
        return Err(Failure::rejected(
            409,
            "agent_run_lifecycle_resolve_response_invalid",
        ));
    }
    Ok(resolved)
}

fn transition(platform: &Platform, run: &str, state: &str, reason: &str) -> Result<(), Failure> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Transition {
        agent_run_id: String,
        state: String,
    }
    let result: Transition = platform.post("/internal/agent-runs/transition", json!({"schema":"runtime.agent_run.transition.v1","agentRunId":run,"state":state,"transitionReason":reason})).map_err(|error|error.failure())?;
    if result.agent_run_id != run
        || (result.state != state
            && !(state == "failed"
                && reason == "agent_run_lifecycle_dead_lettered"
                && result.state == "completed"))
    {
        return Err(Failure::rejected(
            409,
            "agent_run_transition_response_invalid",
        ));
    }
    Ok(())
}

fn advance(
    state: &HttpServerState,
    platform: &Platform,
    config: &Config,
    job: &RuntimeJobRecord,
    lease: &lease::Lease,
) -> Result<Next, Failure> {
    let resolved = resolve(platform, config, job)?;
    let Resolution::Ready {
        agent_run_start, ..
    } = resolved
    else {
        return finish(state, platform, config, job, resolved, lease, Lane::Execute);
    };
    let binding = binding(job)?;
    if agent_run_start.agent_run_id != binding.run
        || agent_run_start.authorization_digest != binding.digest
    {
        return Err(Failure::rejected(
            409,
            "agent_run_lifecycle_resolve_response_invalid",
        ));
    }
    let deadline = agent_run_start.authorization.deadline_at_ms();
    let request = AgentRunStepRequest {
        schema: contract::AGENT_RUN_STEP_SCHEMA.into(),
        job_id: job.job_id.clone(),
        lease_owner: owner(job)?.into(),
        agent_run_start: *agent_run_start,
    };
    let key = env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
        .map_err(|_| "execution signing key unavailable")?;
    let request = request.validate(key.as_bytes())?;
    transition(
        platform,
        binding.run,
        "running",
        "agent_run_lifecycle_step_started",
    )?;
    lease.require()?;
    let outcome = agent_run_execution::step(state.clone(), request)?;
    lease.require()?;
    if outcome.disposition == "waiting" {
        transition(platform, binding.run, "running", &outcome.transition_reason)?;
        return Ok(Next::Yield {
            at: retry_at(&outcome, now_ms()?, config.recheck_ms, deadline),
            reason: outcome.transition_reason,
        });
    }
    let terminal = outcome.terminal_state.ok_or("terminal state missing")?;
    if config.mode == "perRun" {
        let request = FinalizeRequest {
            schema: "runtime.agent_run.finalize.v1".into(),
            job_id: job.job_id.clone(),
            lease_owner: owner(job)?.into(),
            agent_run_id: binding.run.into(),
            authorization_digest: binding.digest.into(),
            terminal_state: Some(terminal_state(terminal)?),
            agent_run_cleanup: None,
        };
        http::agent_runs::finalization::finish(cleanup_state(state), request)
            .map_err(cleanup_pending)?;
        Ok(Next::Complete)
    } else {
        transition(
            platform,
            binding.run,
            terminal,
            "runtime_session_terminal_committed",
        )?;
        finish(
            state,
            platform,
            config,
            job,
            resolve(platform, config, job)?,
            lease,
            Lane::Execute,
        )
    }
}

fn terminal_state(state: &str) -> Result<TerminalState, Failure> {
    match state {
        "completed" => Ok(TerminalState::Completed),
        "failed" => Ok(TerminalState::Failed),
        "cancelled" => Ok(TerminalState::Cancelled),
        _ => Err(Failure::rejected(409, "agent_run_terminal_invalid")),
    }
}

fn retry_at(outcome: &AgentRunStepOutcome, now: i64, recheck: i64, deadline: Option<i64>) -> i64 {
    let at = match outcome.transition_reason.as_str() {
        "execution_recovery_checkpoint_committed" => outcome.retry_at_ms.unwrap_or(now).max(now),
        "execution_host_admission_wait" => now.saturating_add(1000),
        "runtime_job_wait" => now.saturating_add(5 * 60 * 1000),
        _ => now.saturating_add(recheck),
    };
    deadline.map_or(at, |deadline| at.min(deadline.max(now)))
}

fn cleanup_pending(error: Failure) -> Failure {
    if wait_reason(&error).is_some() {
        error
    } else {
        Failure::rejected(503, "retained_cleanup_pending")
    }
}

fn wait_reason(error: &Failure) -> Option<&'static str> {
    match error {
        Failure::Rejected { status: 409, code }
            if matches!(
                code.as_str(),
                "environment_controller_busy" | "environment_controller_owned"
            ) =>
        {
            Some("environment_controller_wait")
        }
        Failure::Rejected { status: 409, code } if code == "environment_host_busy" => {
            Some("execution_host_admission_wait")
        }
        Failure::Rejected { status: 409, code } if code == "execution_resident_capacity_wait" => {
            Some("execution_resident_capacity_wait")
        }
        Failure::Rejected { code, .. } if code == "retained_cleanup_pending" => {
            Some("retained_cleanup_pending")
        }
        _ => None,
    }
}

fn finish(
    state: &HttpServerState,
    platform: &Platform,
    config: &Config,
    job: &RuntimeJobRecord,
    resolved: Resolution,
    lease: &lease::Lease,
    lane: Lane,
) -> Result<Next, Failure> {
    let binding = binding(job)?;
    lease.require()?;
    match resolved {
        Resolution::Released {
            release_receipt_id,
            terminal_state,
            ..
        } => {
            let _committed_terminal = terminal_state;
            if !http::agent_runs::finalization::valid_identity(&release_receipt_id) {
                return Err(Failure::rejected(409, "agent_run_release_response_invalid"));
            }
        }
        Resolution::Terminal {
            terminal_state,
            agent_run_cleanup,
            ..
        } => {
            let cleanup = agent_run_cleanup;
            if cleanup.agent_run_id != binding.run || cleanup.authorization_digest != binding.digest
            {
                return Err(Failure::rejected(409, "agent_run_cleanup_binding_invalid"));
            }
            let mut request = FinalizeRequest {
                schema: "runtime.agent_run.finalize.v1".into(),
                job_id: job.job_id.clone(),
                lease_owner: owner(job)?.into(),
                agent_run_id: binding.run.into(),
                authorization_digest: binding.digest.into(),
                terminal_state: None,
                agent_run_cleanup: None,
            };
            if config.mode == "perRun" && matches!(lane, Lane::Execute) {
                request.agent_run_cleanup = Some(cleanup);
                http::agent_runs::finalization::finish(cleanup_state(state), request)
                    .map_err(cleanup_pending)?;
            } else {
                let epoch = cleanup
                    .environment_authorization
                    .payload
                    .controller
                    .as_ref()
                    .filter(|controller| controller.controller_purpose == "cleanup")
                    .ok_or("cleanup controller missing")?
                    .controller_epoch;
                let receipt = agent_run_cleanup::teardown(
                    cleanup_state(state),
                    job.job_id.clone(),
                    owner(job)?.into(),
                    *cleanup,
                )
                .map_err(cleanup_pending)?;
                lease.require()?;
                request.terminal_state = Some(terminal_state);
                platform
                    .release(&request, epoch, &receipt)
                    .map_err(|error| cleanup_pending(error.failure()))?;
            }
        }
        Resolution::Ready { .. } => {
            return Err(Failure::rejected(409, "control_execution_forbidden"))
        }
    }
    Ok(Next::Complete)
}

fn advance_control(
    state: &HttpServerState,
    platform: &Platform,
    config: &Config,
    job: &RuntimeJobRecord,
    stage: &str,
    lease: &lease::Lease,
) -> Result<Next, Failure> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct ControlResolution {
        schema: String,
        agent_run_control: contract::AgentRunControl,
    }
    let binding = binding(job)?;
    let resolved: ControlResolution = platform.post("/internal/agent-run-lifecycle/control",json!({"schema":"runtime.agent_run_lifecycle.control.resolve.v1","jobId":job.job_id,"leaseOwner":owner(job)?,"agentRunId":binding.run,"authorizationDigest":binding.digest})).map_err(|error|error.failure())?;
    if resolved.schema != "runtime.agent_run_lifecycle.control.resolved.v1"
        || resolved.agent_run_control.agent_run_id != binding.run
    {
        return Err(Failure::rejected(409, "agent_run_control_response_invalid"));
    }
    lease.require()?;
    let result = http::agent_runs::control::execute(
        state,
        &job.job_id,
        owner(job)?,
        &resolved.agent_run_control,
    )?;
    lease.require()?;
    let terminal = result["terminalState"]
        .as_str()
        .ok_or("control terminal missing")?;
    transition(
        platform,
        binding.run,
        terminal,
        "runtime_session_terminal_committed",
    )?;
    if result["disposition"] == "released" {
        Ok(Next::Complete)
    } else if stage == "cleanup" {
        finish(
            state,
            platform,
            config,
            job,
            resolve(platform, config, job)?,
            lease,
            Lane::Cleanup,
        )
    } else {
        Ok(Next::Yield {
            at: now_ms()?,
            reason: "control_cleanup_pending".into(),
        })
    }
}

fn fail(
    state: &HttpServerState,
    platform: &Platform,
    job: &RuntimeJobRecord,
    error: Failure,
) -> Result<(), Failure> {
    let (reason, retryable) = match error {
        Failure::Step {
            reason, retryable, ..
        } => (reason, retryable),
        Failure::Rejected { status: 503, .. } => ("dependency_unavailable", true),
        Failure::Rejected { code, .. }
            if matches!(
                code.as_str(),
                "agent_run_lifecycle_lease_lost" | "agent_run_execute_lease_required"
            ) =>
        {
            return Ok(())
        }
        _ => ("agent_run_lifecycle_failed", false),
    };
    let disposition = job_protocol::fail_owned_job(
        job_protocol::FailRequest {
            schema: "runtime.job.fail.v1".into(),
            job_id: job.job_id.clone(),
            lease_owner: owner(job)?.into(),
            failed_at_ms: now_ms()?,
            error: reason.into(),
            retryable,
        },
        state.job_store.as_ref(),
    )
    .map_err(|(status, code)| Failure::rejected(status, code))?;
    if job.job_kind == AGENT_RUN_LIFECYCLE_JOB_KIND
        && disposition
            != centaeris_core::session::reliability::RuntimeJobFailureDisposition::RetryScheduled
    {
        transition(
            platform,
            binding(job)?.run,
            "failed",
            if retryable {
                "agent_run_lifecycle_dead_lettered"
            } else {
                "runtime_step_failed"
            },
        )?;
    }
    Ok(())
}

fn cleanup_state(state: &HttpServerState) -> HttpServerState {
    let mut cleanup = state.clone();
    cleanup.job_store = Arc::new(state.job_store.cleanup_view());
    cleanup
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waiting_uses_runtime_recovery_deadline_and_authorization_deadline() {
        let outcome = AgentRunStepOutcome {
            disposition: "waiting",
            terminal_state: None,
            transition_reason: "execution_recovery_checkpoint_committed".into(),
            retry_at_ms: Some(8000),
        };
        assert_eq!(retry_at(&outcome, 1000, 30000, None), 8000);
        assert_eq!(retry_at(&outcome, 1000, 30000, Some(4000)), 4000);
        assert_eq!(retry_at(&outcome, 9000, 30000, None), 9000);
        assert_eq!(retry_at(&outcome, 9000, 30000, Some(4000)), 9000);
    }
    #[test]
    fn runtime_job_wait_retains_timed_compensation_and_host_admission_rechecks_promptly() {
        let mut outcome = AgentRunStepOutcome {
            disposition: "waiting",
            terminal_state: None,
            transition_reason: "runtime_job_wait".into(),
            retry_at_ms: None,
        };
        assert_eq!(retry_at(&outcome, 1000, 30000, None), 301000);
        outcome.transition_reason = "execution_host_admission_wait".into();
        assert_eq!(retry_at(&outcome, 1000, 30000, None), 2000);
        outcome.transition_reason = "question_wait".into();
        assert_eq!(retry_at(&outcome, 1000, 30000, None), 31000);
    }
}
