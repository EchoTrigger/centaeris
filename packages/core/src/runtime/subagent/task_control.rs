use std::cmp::Reverse;
use std::collections::BTreeMap;

use super::{
    delegation_id_suffix, load_subagent_work_packet_async, subagent_id_from_job,
    subagent_work_packet_runtime_binding, SUBAGENT_RUN_JOB_KIND,
};
use crate::runtime::contracts::TimestampMs;
use crate::runtime::keys::external_context::subagent_work_packet_ref;
use crate::session::reliability::{
    ListRuntimeJobsRequest, RequestSubagentJobCancellation, RuntimeJobRecord, RuntimeJobStatus,
    SubagentJobCancellationReceipt,
};
use crate::session::store::{
    job::{JobStoreError, JobWriteOutcome},
    AsyncJobStorePort,
};

/// A delegation attempt selected from the parent Session's durable Jobs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentTask {
    pub agent_id: String,
    pub child_session_id: String,
    pub task_id: String,
    pub parent_agent_run_id: String,
    pub description: String,
    pub status: RuntimeJobStatus,
    pub submitted_at_ms: TimestampMs,
    pub updated_at_ms: TimestampMs,
    pub cancellation_requested: bool,
}

/// The first outstanding attempt and most recently finished attempt of one Agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionSubagentTasks {
    pub active: Option<SubagentTask>,
    pub last_terminal: Option<SubagentTask>,
}

/// Selects active tasks alone for status polling or includes the latest terminal history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentTaskScope {
    Active,
    IncludeTerminal,
}

#[derive(Default)]
struct SelectedJobs {
    active: Option<RuntimeJobRecord>,
    terminal: Option<RuntimeJobRecord>,
}

/// Lists a parent Session's delegations, using bounded Job pages and only the selected packets.
///
/// # Errors
/// Returns storage or identity validation failures from the Job store.
pub async fn list_session_subagent_tasks(
    store: &dyn AsyncJobStorePort,
    parent_session_id: &str,
    scope: SubagentTaskScope,
) -> Result<Vec<SessionSubagentTasks>, JobStoreError> {
    const PAGE_SIZE: usize = 128;
    let mut selected = BTreeMap::<String, SelectedJobs>::new();
    let mut offset = 0;
    loop {
        let jobs = store
            .list_runtime_jobs(ListRuntimeJobsRequest {
                session_id: Some(parent_session_id.to_owned()),
                job_kind: Some(SUBAGENT_RUN_JOB_KIND.to_owned()),
                statuses: match scope {
                    SubagentTaskScope::IncludeTerminal => Vec::new(),
                    SubagentTaskScope::Active => vec![
                        RuntimeJobStatus::Queued,
                        RuntimeJobStatus::Leased,
                        RuntimeJobStatus::Running,
                    ],
                },
                limit: PAGE_SIZE,
                offset,
                ..Default::default()
            })
            .await?;
        let count = jobs.len();
        for job in jobs {
            let agent_id = subagent_id_from_job(&job)?;
            let entry = selected.entry(agent_id).or_default();
            if job.status.is_terminal() {
                if entry.terminal.as_ref().is_none_or(|previous| {
                    (job.updated_at_ms, &job.job_id) > (previous.updated_at_ms, &previous.job_id)
                }) {
                    entry.terminal = Some(job);
                }
            } else if entry
                .active
                .as_ref()
                .is_none_or(|previous| active_rank(&job) < active_rank(previous))
            {
                entry.active = Some(job);
            }
        }
        if count < PAGE_SIZE {
            break;
        }
        offset += count;
    }
    let mut agents = Vec::with_capacity(selected.len());
    for (agent_id, jobs) in selected {
        if delegated_agent_retired(store, &agent_id).await? {
            continue;
        }
        let active = match jobs.active {
            Some(job) => Some(read_task(store, job, &agent_id).await?),
            None => None,
        };
        let last_terminal = match jobs.terminal {
            Some(job) => Some(read_task(store, job, &agent_id).await?),
            None => None,
        };
        agents.push(SessionSubagentTasks {
            active,
            last_terminal,
        });
    }
    Ok(agents)
}

pub(crate) async fn delegated_agent_retired(
    store: &dyn AsyncJobStorePort,
    agent_id: &str,
) -> Result<bool, JobStoreError> {
    let Some(suffix) = delegation_id_suffix(agent_id, "agent-") else {
        return Ok(false);
    };
    Ok(store
        .load_external_context_object(&subagent_work_packet_ref(suffix))
        .await?
        .is_some_and(|packet| !packet.metadata["agentRetiredAtMs"].is_null()))
}

fn active_rank(job: &RuntimeJobRecord) -> (Reverse<u8>, TimestampMs, TimestampMs, &str) {
    let active = match job.status {
        RuntimeJobStatus::Running => 2,
        RuntimeJobStatus::Leased => 1,
        _ => 0,
    };
    (
        Reverse(active),
        job.run_at_ms,
        job.created_at_ms,
        &job.job_id,
    )
}

async fn read_task(
    store: &dyn AsyncJobStorePort,
    job: RuntimeJobRecord,
    agent_id: &str,
) -> Result<SubagentTask, JobStoreError> {
    let packet = load_subagent_work_packet_async(store, &job).await?;
    let binding = subagent_work_packet_runtime_binding(&packet, &job)?;
    if binding.subagent_id != agent_id {
        return Err(JobStoreError::Rejected(
            "delegated Agent task identity mismatch".into(),
        ));
    }
    let cancellation_requested = !job.status.is_terminal()
        && store
            .subagent_job_cancellation_requested(&job.job_id)
            .await?;
    Ok(SubagentTask {
        agent_id: binding.subagent_id,
        child_session_id: binding.child_session_id,
        task_id: job.job_id,
        parent_agent_run_id: binding.parent_agent_run_id,
        description: binding.description,
        status: job.status,
        submitted_at_ms: job.created_at_ms,
        updated_at_ms: job.updated_at_ms,
        cancellation_requested,
    })
}

/// Requests cancellation of one exact delegation and settles ambiguous Job writes.
///
/// The host must hold its parent Session mutation authority throughout this operation.
///
/// # Errors
/// Returns a Job store failure if the request fails or its outcome cannot be settled.
pub async fn request_subagent_task_cancellation(
    store: &dyn AsyncJobStorePort,
    request: RequestSubagentJobCancellation,
) -> Result<SubagentJobCancellationReceipt, JobStoreError> {
    let job_id = request.job_id.clone();
    let session_id = request.session_id.clone();
    let error = match store.request_subagent_job_cancellation(request).await {
        Ok(receipt) => return Ok(receipt),
        Err(error)
            if error.requires_settlement()
                || matches!(
                    error,
                    JobStoreError::Storage {
                        outcome: JobWriteOutcome::Committed,
                        ..
                    }
                ) =>
        {
            error
        }
        Err(error) => return Err(error),
    };
    store.settle_job_operations().await?;
    let accepted = store.subagent_job_cancellation_requested(&job_id).await?;
    let job = store
        .get_runtime_job(&job_id)
        .await?
        .filter(|job| {
            job.job_kind == SUBAGENT_RUN_JOB_KIND && job.session_id.as_deref() == Some(&session_id)
        })
        .ok_or_else(|| JobStoreError::Rejected("subagent job not found in session".into()))?;
    if accepted || job.status.is_terminal() {
        Ok(SubagentJobCancellationReceipt {
            accepted,
            status: job.status,
        })
    } else {
        Err(error)
    }
}
