//! A leased execution attempt shared by the native dispatcher and HTTP adapter.
use crate::*;

#[derive(Debug)]
pub(crate) enum Failure {
    Rejected {
        status: u16,
        code: String,
    },
    Step {
        status: u16,
        run: String,
        class: &'static str,
        retryable: bool,
        reason: &'static str,
    },
    Internal(String),
}

impl From<String> for Failure {
    fn from(error: String) -> Self {
        Self::Internal(error)
    }
}

impl From<&str> for Failure {
    fn from(error: &str) -> Self {
        Self::Internal(error.into())
    }
}

impl Failure {
    pub(crate) fn rejected(status: u16, code: &str) -> Self {
        Self::Rejected {
            status,
            code: code.into(),
        }
    }

    pub(crate) fn response(self) -> Result<Response<Body>, String> {
        match self {
            Self::Rejected { status, code } => json_error_response(status, &code),
            Self::Step {
                status,
                run,
                class,
                retryable,
                reason,
            } => {
                let mut response = agent_run_step_failure_response(&run, class, retryable, reason)?;
                *response.status_mut() =
                    axum::http::StatusCode::from_u16(status).map_err(|e| e.to_string())?;
                Ok(response)
            }
            Self::Internal(error) => Err(error),
        }
    }
}

pub(crate) fn control_failure(run: &str, error: &str) -> Option<Failure> {
    (error.starts_with("run_execution_control_probe_failed:")
        || error == "native_model_grant_unavailable")
        .then(|| Failure::Step {
            status: 503,
            run: run.into(),
            class: "execution_control_unavailable",
            retryable: true,
            reason: "execution_control_unavailable",
        })
}

fn step_failure<T>(
    run: &str,
    class: &'static str,
    retryable: bool,
    reason: &'static str,
) -> Result<T, Failure> {
    Err(Failure::Step {
        status: 500,
        run: run.into(),
        class,
        retryable,
        reason,
    })
}

pub(crate) fn step(
    mut state: HttpServerState,
    agent_run_step: AgentRunStepRequest,
) -> Result<AgentRunStepOutcome, Failure> {
    let session_id = &agent_run_step.agent_run_start.authorization.session_id;
    let execution_store = state.job_store.hosted_execution_view(
        session_id,
        session_id,
        RuntimeJobLeaseFence {
            job_id: agent_run_step.job_id.clone(),
            job_kind: AGENT_RUN_LIFECYCLE_JOB_KIND.into(),
            lease_owner: agent_run_step.lease_owner.clone(),
        },
    )?;
    let store = Arc::new(
        state
            .runtime
            .block_on(async { RuntimeStoreActor::start(execution_store) })
            .map_err(|error| error.to_string())?,
    );
    let runtime = state.runtime.clone();
    state.store = store.clone();
    let outcome = step_with_store(state, agent_run_step);
    runtime
        .block_on(store.close())
        .map_err(|error| error.to_string())?;
    outcome
}

fn step_with_store(
    state: HttpServerState,
    agent_run_step: AgentRunStepRequest,
) -> Result<AgentRunStepOutcome, Failure> {
    let HttpServerState {
        runtime,
        store,
        job_store,
        tool_layers,
        tool_concurrency_registry,
        native_model,
        ..
    } = state;
    if job_store
        .require_execute_lease(&agent_run_step.job_id, &agent_run_step.lease_owner)
        .is_err()
    {
        return Err(Failure::rejected(409, "agent_run_execute_lease_required"));
    }
    if let Err(error) = validate_agent_run_lifecycle_job(
        agent_run_step.job_id.as_str(),
        agent_run_step.lease_owner.as_str(),
        &agent_run_step.agent_run_start,
        job_store.as_ref(),
    ) {
        return Err(Failure::rejected(409, error.as_str()));
    }
    let agent_run_id = agent_run_step.agent_run_start.agent_run_id.clone();
    let failure_agent_run_start = agent_run_step.agent_run_start.clone();
    let failure_job_id = agent_run_step.job_id.clone();
    let failure_lease_owner = agent_run_step.lease_owner.clone();
    let failure_runtime = runtime.clone();
    let failure_store = store.clone();
    let failure_job_store = job_store.clone();
    let execution = catch_agent_run_step_panic(|| {
        execute_agent_run(
            agent_run_step.agent_run_start,
            runtime,
            store,
            job_store,
            agent_run_step.job_id,
            agent_run_step.lease_owner,
            tool_layers.clone(),
            tool_concurrency_registry,
            native_model,
        )
    });
    if let Ok(Err(error)) = &execution {
        if let Some(failure) = control_failure(&agent_run_id, error) {
            return Err(failure);
        }
    }
    let outcome = match execution {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) if error == "agent_run_lifecycle_lease_lost" => {
            return Err(Failure::rejected(409, error.as_str()));
        }
        Ok(Err(error)) if session_delivery::pending(&error) => {
            failure_diagnostics::report(agent_run_id.as_str(), "session_record_commit", &error);
            session_delivery_waiting()
        }
        Ok(Err(error)) => match terminalize_agent_run_failure(
            failure_runtime.as_ref(),
            failure_store.as_ref(),
            failure_job_store.as_ref(),
            &failure_agent_run_start,
            failure_job_id.as_str(),
            failure_lease_owner.as_str(),
            AgentRunFailure {
                internal_error: error.as_str(),
                transition_reason: "runtime_internal_error",
            },
        ) {
            Ok(outcome) => outcome,
            Err(terminal_error) => {
                let retryable = terminal_error == "runtime_completed_projection_requires_recovery";
                eprintln!(
                    "AgentRun failure terminalization failed: {terminal_error}; rootCause={error}"
                );
                return step_failure(
                    agent_run_id.as_str(),
                    if retryable {
                        "recovery_required"
                    } else {
                        "runtime_internal"
                    },
                    retryable,
                    "runtime_failure_terminalization_failed",
                );
            }
        },
        Err(error) => {
            match catch_agent_run_step_panic(|| {
                terminalize_agent_run_failure(
                    failure_runtime.as_ref(),
                    failure_store.as_ref(),
                    failure_job_store.as_ref(),
                    &failure_agent_run_start,
                    failure_job_id.as_str(),
                    failure_lease_owner.as_str(),
                    AgentRunFailure {
                        internal_error: error.as_str(),
                        transition_reason: "runtime_panic",
                    },
                )
            }) {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(terminal_error)) => {
                    let retryable =
                        terminal_error == "runtime_completed_projection_requires_recovery";
                    eprintln!(
                        "AgentRun panic terminalization failed: {terminal_error}; rootCause={error}"
                    );
                    return step_failure(
                        agent_run_id.as_str(),
                        if retryable {
                            "recovery_required"
                        } else {
                            "runtime_panic"
                        },
                        retryable,
                        "runtime_failure_terminalization_failed",
                    );
                }
                Err(terminal_error) => {
                    eprintln!(
                        "AgentRun panic terminalization panicked: {terminal_error}; rootCause={error}"
                    );
                    return step_failure(
                        agent_run_id.as_str(),
                        "runtime_panic",
                        false,
                        "runtime_failure_terminalization_failed",
                    );
                }
            }
        }
    };
    outcome.validate()?;
    Ok(outcome)
}
