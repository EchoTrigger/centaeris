//! Accepted terminal work is delivered from the durable outbox, independently
//! of a particular Worker/batch response. Failed items do not stop their siblings.
use crate::runtime_rpc_transport::{EventWriter, SessionWritePermit};
use crate::{agent_runtime, message_log, sessions, subagent_scheduler};
use centaeris_core::runtime::persist_subagent_result_projection_from_scheduler_events_async;
use centaeris_core::runtime::subagent::{
    load_subagent_work_packet_async, subagent_work_packet_runtime_binding,
    terminal_subagent_event_from_job, SUBAGENT_RUN_JOB_KIND,
};
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
        let delivered = deliver_terminal_job(writer, &record.job_id).await;
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
    job_id: &str,
) -> Result<Option<SessionWritePermit>, String> {
    deliver_terminal_job_in_scope(writer, job_id, None).await
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
            deliver_terminal_job_in_scope(writer, job_id, Some(child)).await?
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
    job_id: &str,
    deleting_child: Option<&str>,
) -> Result<Option<SessionWritePermit>, String> {
    let jobs = agent_runtime::native_job_store_handle()?;
    let job = jobs
        .get_runtime_job(job_id)
        .await
        .map_err(|error| error.diagnostic_message())?
        .ok_or_else(|| format!("terminal delivery Job missing: {job_id}"))?;
    if !job.status.is_terminal() {
        return Err(format!("terminal delivery Job is not terminal: {job_id}"));
    }
    let parent = job
        .session_id
        .as_deref()
        .ok_or("terminal delivery parent missing")?;
    let Some(session_writer) = writer.pin_session(parent)? else {
        return Ok(None);
    };
    let _work = writer.begin_background_work(parent)?;
    let packet = match load_subagent_work_packet_async(&jobs, &job).await {
        Ok(packet) => Some(packet),
        Err(JobStoreError::Rejected(_)) => None,
        Err(error) => return Err(error.diagnostic_message()),
    };
    let event = terminal_subagent_event_from_job(&job, packet.as_ref())?;
    let binding = packet
        .as_ref()
        .and_then(|packet| subagent_work_packet_runtime_binding(packet, &job).ok());
    if binding.is_none() {
        let native = sessions::listed_children_for_job(parent, job_id)?;
        if native.len() != 1 || native[0].id != event.child_session_id {
            return Err(format!("terminal delivery requires one authoritative child binding: {job_id}"));
        }
    }
    if let Some(binding) = &binding {
        if message_log::project_agent_run(job_id)?.is_none() {
            let cwd = sessions::cwd_for_session_id(parent)?;
            agent_runtime::materialize_agent_session_blocking(
                parent, &binding.child_session_id, &binding.child_turn_id,
                job_id, &binding.description, &cwd, job.created_at_ms,
            )?;
        }
    }
    let run = message_log::project_agent_run(job_id)?.ok_or("terminal delivery child Run missing")?;
    let child_id = run.session_id.as_str();
    let child_binding = sessions::find_agent_runtime_binding(child_id)?;
    if event.child_session_id != child_id
        || child_binding.as_ref().is_none_or(|(owner, _)| owner != parent)
        || binding.as_ref().is_some_and(|binding| binding.child_turn_id != run.turn_id)
    {
        return Err("terminal delivery child Run binding mismatch".into());
    }
    let _child_work = if deleting_child == Some(child_id) {
        None
    } else {
        Some(writer.begin_background_work(child_id)?)
    };
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
    subagent_scheduler::persist_agent_session_terminal_projection(
        &jobs,
        writer,
        job_id,
        child_id,
        &run.turn_id,
        &subagent_scheduler::ShutdownJobs::default(),
    )
    .await?;
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
    Ok(Some(session_writer))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod deletion_tests;
