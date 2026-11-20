//! Accepted terminal work is delivered from the durable outbox, independently
//! of a particular Worker/batch response. Failed items do not stop their siblings.
use crate::runtime_rpc_transport::{EventWriter, SessionWritePermit};
use crate::subagent_scheduler::ChildTaskAdmission;
use crate::{agent_runtime, message_log, sessions, subagent_scheduler};
use centaeris_core::runtime::persist_subagent_result_projection_from_scheduler_events_async;
use centaeris_core::runtime::subagent::{
    load_subagent_work_packet_async, subagent_work_packet_runtime_binding,
    terminal_subagent_event_from_job, SUBAGENT_RUN_JOB_KIND,
};
use centaeris_core::session::reliability::{RuntimeJobOutboxRecord, RuntimeJobStatus};
use centaeris_core::session::store::{job::JobStoreError, AsyncJobStorePort};

struct TerminalDeliveryScan {
    path: std::path::PathBuf,
    after: Option<(String, String)>,
}

// Serialize scans and retain attempted keys, including failures. A new process
// starts at the beginning; later pages are reached in subsequent bounded scans.
static TERMINAL_DELIVERY_SCAN: tokio::sync::Mutex<Option<TerminalDeliveryScan>> =
    tokio::sync::Mutex::const_new(None);
const TERMINAL_DELIVERY_PAGE_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryPoint {
    BeforeParent,
    AfterParent,
    BeforeChildTerminal,
    AfterChild,
    BeforeAcknowledgement,
}

pub(crate) fn checkpoint(point: DeliveryPoint, job_id: &str) {
    #[cfg(test)]
    if std::env::var("CENTAERIS_JOB_DELIVERY_TEST_EXIT")
        .ok()
        .as_deref()
        == Some(&format!("{point:?}"))
    {
        use std::io::Write;
        println!("delivery boundary reached: {point:?} {job_id}");
        std::io::stdout().flush().unwrap();
        std::process::exit(81);
    }
    let _ = (point, job_id);
}

pub(crate) async fn reconcile_terminal_jobs(writer: &EventWriter) -> Result<(), String> {
    let jobs = agent_runtime::native_job_store_handle()?;
    let path = agent_runtime::agent_runtime_store_db_path();
    let mut scan_guard = TERMINAL_DELIVERY_SCAN.lock().await;
    let scan = scan_guard.get_or_insert_with(|| TerminalDeliveryScan {
        path: path.clone(),
        after: None,
    });
    if scan.path != path {
        scan.path = path;
        scan.after = None;
    }
    let pending = jobs
        .pending_terminal_deliveries_after(
            TERMINAL_DELIVERY_PAGE_SIZE,
            Some(SUBAGENT_RUN_JOB_KIND.into()),
            scan.after.clone(),
        )
        .await
        .map_err(|error| error.diagnostic_message())?;
    let reached_end = pending.len() < TERMINAL_DELIVERY_PAGE_SIZE;
    let mut errors = Vec::new();
    for record in pending {
        scan.after = Some((record.job_id.clone(), record.event_type.clone()));
        let delivered = deliver_terminal_job(writer, &record).await;
        match delivered {
            Ok(Some(_session_writer)) => {
                checkpoint(DeliveryPoint::BeforeAcknowledgement, &record.job_id);
                if let Err(error) = jobs
                    .acknowledge_terminal_delivery(
                        record,
                        centaeris_core::runtime::contracts::current_timestamp_ms(),
                    )
                    .await
                {
                    errors.push(error.diagnostic_message());
                }
            }
            Ok(None) => {}
            Err(error) => errors.push(error),
        }
    }
    if reached_end {
        // Revisit failures and records inserted or regenerated behind the cursor.
        scan.after = None;
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

async fn deliver_terminal_job(
    writer: &EventWriter,
    record: &RuntimeJobOutboxRecord,
) -> Result<Option<SessionWritePermit>, String> {
    deliver_terminal_job_in_scope(writer, record, None).await
}

/// The deletion owner has closed this child's admission and drained its work.
/// Deliver the same durable terminal before its authoritative binding disappears.
pub(crate) async fn finish_child_delivery_before_deletion(
    writer: &EventWriter,
    job_id: &str,
    child: &str,
) -> Result<(), String> {
    let jobs = agent_runtime::native_job_store_handle()?;
    if let Some(record) = jobs
        .pending_terminal_delivery(job_id)
        .await
        .map_err(|error| error.diagnostic_message())?
    {
        let Some(_session_writer) =
            deliver_terminal_job_in_scope(writer, &record, Some(child)).await?
        else {
            return Err(format!("terminal delivery parent is not owned: {job_id}"));
        };
        jobs.acknowledge_terminal_delivery(
            record,
            centaeris_core::runtime::contracts::current_timestamp_ms(),
        )
        .await
        .map_err(|error| error.diagnostic_message())?;
    }
    Ok(())
}

async fn deliver_terminal_job_in_scope(
    writer: &EventWriter,
    record: &RuntimeJobOutboxRecord,
    deleting_child: Option<&str>,
) -> Result<Option<SessionWritePermit>, String> {
    let jobs = agent_runtime::native_job_store_handle()?;
    let job_id = record.job_id.as_str();
    let Some(job) = jobs
        .get_runtime_job(job_id)
        .await
        .map_err(|error| error.diagnostic_message())?
    else {
        if deleting_child.is_none()
            && jobs
                .pending_terminal_delivery(job_id)
                .await
                .map_err(|error| error.diagnostic_message())?
                .is_none()
        {
            return Ok(None);
        }
        return Err(format!("terminal delivery Job missing: {job_id}"));
    };
    if !job.status.is_terminal() {
        return Err(format!("terminal delivery Job is not terminal: {job_id}"));
    }
    let parent = job
        .session_id
        .as_deref()
        .ok_or("terminal delivery parent missing")?;
    let packet = match load_subagent_work_packet_async(&jobs, &job).await {
        Ok(packet) => Some(packet),
        Err(JobStoreError::Rejected(_)) => None,
        Err(error) => return Err(error.diagnostic_message()),
    };
    let event = terminal_subagent_event_from_job(&job, packet.as_ref())?;
    let binding = packet
        .as_ref()
        .and_then(|packet| subagent_work_packet_runtime_binding(packet, &job).ok());
    let child_id = event.child_session_id.as_str();
    if binding
        .as_ref()
        .is_some_and(|binding| binding.child_session_id != child_id)
        || deleting_child.is_some_and(|deleting| deleting != child_id)
    {
        return Err("terminal delivery child Session binding mismatch".into());
    }
    let (admission, deletion_writer, _deletion_parent_work) = if deleting_child.is_some() {
        let Some(permit) = writer.pin_session(parent)? else {
            return Ok(None);
        };
        (
            None,
            Some(permit),
            Some(writer.begin_background_work(parent)?),
        )
    } else {
        let Some(admission) = ChildTaskAdmission::acquire(writer, parent, child_id)? else {
            return Ok(None);
        };
        (Some(admission), None, None)
    };
    // The scan may have read this terminal before deletion processed it. Only
    // reusing the current outbox generation can authorize creating a child Run.
    if admission.is_some()
        && jobs
            .pending_terminal_delivery(job_id)
            .await
            .map_err(|error| error.diagnostic_message())?
            .as_ref()
            != Some(record)
    {
        return Ok(None);
    }
    if binding.is_none() && deleting_child.is_none() {
        let native = sessions::listed_children_for_job(parent, job_id)?;
        if native.len() != 1 || native[0].id != event.child_session_id {
            return Err(format!(
                "terminal delivery requires one authoritative child binding: {job_id}"
            ));
        }
    }
    if let (Some(admission), Some(binding)) = (&admission, &binding) {
        if !admission
            .materialize_if_current(&jobs, &job, binding, RuntimeJobStatus::is_terminal)
            .await?
        {
            return Ok(None);
        }
    }
    let run = message_log::project_agent_run(job_id)?;
    if run.is_none() && deleting_child.is_none() {
        return Err("terminal delivery child Run missing".into());
    }
    let child_binding = sessions::find_agent_runtime_binding(child_id)?;
    if child_binding
        .as_ref()
        .is_none_or(|(owner, _)| owner != parent)
        || run.as_ref().is_some_and(|run| {
            run.session_id != child_id
                || binding
                    .as_ref()
                    .is_some_and(|binding| binding.child_turn_id != run.turn_id)
        })
    {
        return Err("terminal delivery child Run binding mismatch".into());
    }
    checkpoint(DeliveryPoint::BeforeParent, job_id);
    let snapshots = agent_runtime::agent_runtime_store_actor()?;
    persist_subagent_result_projection_from_scheduler_events_async(
        &snapshots,
        &jobs,
        parent,
        std::slice::from_ref(&event),
    )
    .await
    .map_err(|error| error.diagnostic_message())?;
    checkpoint(DeliveryPoint::AfterParent, job_id);
    // Deletion can finish the authoritative parent delivery for a queued
    // follow-up whose child Run was never materialized.
    if let Some(run) = run {
        subagent_scheduler::persist_agent_session_terminal_projection(
            &jobs,
            writer,
            job_id,
            child_id,
            &run.turn_id,
            &subagent_scheduler::ShutdownJobs::default(),
        )
        .await?;
    }
    checkpoint(DeliveryPoint::AfterChild, job_id);
    if let Some(binding) = binding {
        let bridge = centaeris_core::runtime::build_subagent_scheduler_runtime_event(
            parent,
            &event.parent_turn_id,
            &event,
        );
        agent_runtime::emit_background_turn_update_blocking(
            writer,
            &binding.parent_agent_run_id,
            parent,
            centaeris_core::runtime::TurnUpdate::RuntimeEvent { event: bridge },
        )?;
    }
    Ok(admission
        .map(ChildTaskAdmission::into_session_writer)
        .or(deletion_writer))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod deletion_tests;
