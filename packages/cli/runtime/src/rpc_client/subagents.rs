use std::sync::mpsc::{self, Receiver};

use centaeris_core::runtime::subagent::{
    list_session_subagent_tasks, request_subagent_task_cancellation, SessionSubagentTasks,
    SubagentTaskScope,
};
use centaeris_core::session::reliability::{
    RequestSubagentJobCancellation, SubagentJobCancellationReceipt,
};
use serde_json::Value;

use super::embedded::EmbeddedRuntime;
use super::{Runtime, RuntimeClient, RUNTIME_CLOSED};
use crate::protocol::HostCommandRequest;

/// The result of one native CLI runtime operation.
pub type NativeSubagentResponse<T> = Receiver<Result<T, String>>;

impl RuntimeClient {
    /// Loads the current and most recently finished attempts for each Agent in a parent Session.
    ///
    /// # Errors
    /// Returns an error if the in-process runtime can no longer accept requests.
    pub fn list_subagents_async(
        &self,
        parent_session_id: String,
        scope: SubagentTaskScope,
    ) -> Result<NativeSubagentResponse<Vec<SessionSubagentTasks>>, String> {
        if !self.connected {
            return Err(RUNTIME_CLOSED.into());
        }
        let (tx, rx) = mpsc::channel();
        match &self.runtime {
            Runtime::Embedded(runtime) => runtime.list_subagents(parent_session_id, scope, tx)?,
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test { .. } => {
                let _ = tx.send(Ok(Vec::new()));
            }
        }
        Ok(rx)
    }

    /// Requests cancellation of the exact displayed task under its parent Session's writer permit.
    ///
    /// # Errors
    /// Returns an error if the in-process runtime can no longer accept requests.
    pub fn cancel_subagent_async(
        &self,
        parent_session_id: String,
        task_id: String,
    ) -> Result<NativeSubagentResponse<SubagentJobCancellationReceipt>, String> {
        if !self.connected {
            return Err(RUNTIME_CLOSED.into());
        }
        let (tx, rx) = mpsc::channel();
        match &self.runtime {
            Runtime::Embedded(runtime) => {
                runtime.cancel_subagent(parent_session_id, task_id, tx)?
            }
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test { .. } => {
                let _ = tx.send(Err(
                    "Subagent cancellation requires the local job store".into()
                ));
            }
        }
        Ok(rx)
    }

    /// Runs the existing transcript paging consumer against one verified durable child Session.
    ///
    /// A result of `None` means the child Session has yet to be materialized. The consumer can
    /// only call transcript reads for the bound child and acquires no Session writer permit.
    ///
    /// # Errors
    /// Returns an error if the in-process runtime can no longer accept requests.
    pub fn read_subagent_transcript_async<T, Read>(
        &self,
        parent_session_id: String,
        child_session_id: String,
        read: Read,
    ) -> Result<NativeSubagentResponse<Option<T>>, String>
    where
        T: Send + 'static,
        Read: FnOnce(&mut dyn FnMut(&str, Value) -> Result<Value, String>) -> Result<T, String>
            + Send
            + 'static,
    {
        if !self.connected {
            return Err(RUNTIME_CLOSED.into());
        }
        let (tx, rx) = mpsc::channel();
        match &self.runtime {
            Runtime::Embedded(runtime) => {
                runtime.read_subagent_transcript(parent_session_id, child_session_id, read, tx)?;
            }
            #[cfg(any(test, feature = "test-support"))]
            Runtime::Test { requests, .. } => {
                let requests = requests.clone();
                std::thread::Builder::new()
                    .name("centaeris-test-child-transcript".into())
                    .spawn(move || {
                        let mut request = |method: &str, params: Value| {
                            let (tx, rx) = mpsc::channel();
                            requests
                                .as_ref()
                                .ok_or("test client has no request handler")?
                                .send((serde_json::json!({"method": method, "params": params}), tx))
                                .map_err(|error| error.to_string())?;
                            rx.recv_timeout(super::REQUEST_TIMEOUT)
                                .map_err(|error| error.to_string())?
                                .map_err(Into::into)
                        };
                        let _ = tx.send(read(&mut request).map(Some));
                    })
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(rx)
    }
}

impl EmbeddedRuntime {
    fn list_subagents(
        &self,
        parent: String,
        scope: SubagentTaskScope,
        tx: mpsc::Sender<Result<Vec<SessionSubagentTasks>, String>>,
    ) -> Result<(), String> {
        let (executor, _) = self.owner.as_ref().ok_or(RUNTIME_CLOSED)?;
        let writer = self.writer.clone();
        executor.spawn(async move {
            let result = async {
                let _request = writer
                    .request_permit(false)
                    .map_err(|error| error.to_string())?;
                writer
                    .require_registered()
                    .map_err(|error| error.to_string())?;
                let jobs = crate::agent_runtime::agent_runtime_job_store()?;
                list_session_subagent_tasks(jobs.as_ref(), &parent, scope)
                    .await
                    .map_err(|error| error.diagnostic_message())
            }
            .await;
            let _ = tx.send(result);
        });
        Ok(())
    }

    fn cancel_subagent(
        &self,
        parent: String,
        task_id: String,
        tx: mpsc::Sender<Result<SubagentJobCancellationReceipt, String>>,
    ) -> Result<(), String> {
        let (executor, _) = self.owner.as_ref().ok_or(RUNTIME_CLOSED)?;
        let writer = self.writer.clone();
        executor.spawn(async move {
            let result = async {
                let _request = writer
                    .request_permit(true)
                    .map_err(|error| error.to_string())?;
                writer
                    .require_registered()
                    .map_err(|error| error.to_string())?;
                let _action = writer.start_action().map_err(|error| error.to_string())?;
                let _parent_writer = writer
                    .pin_session(&parent)?
                    .ok_or_else(|| format!("Session {parent} is open in another window"))?;
                let jobs = crate::agent_runtime::agent_runtime_job_store()?;
                request_subagent_task_cancellation(
                    jobs.as_ref(),
                    RequestSubagentJobCancellation {
                        job_id: task_id,
                        session_id: parent,
                        requested_at_ms: chrono::Utc::now().timestamp_millis(),
                    },
                )
                .await
                .map_err(|error| error.diagnostic_message())
            }
            .await;
            let _ = tx.send(result);
        });
        Ok(())
    }

    fn read_subagent_transcript<T, Read>(
        &self,
        parent: String,
        child: String,
        read: Read,
        tx: mpsc::Sender<Result<Option<T>, String>>,
    ) -> Result<(), String>
    where
        T: Send + 'static,
        Read: FnOnce(&mut dyn FnMut(&str, Value) -> Result<Value, String>) -> Result<T, String>
            + Send
            + 'static,
    {
        let (executor, host) = self.owner.as_ref().ok_or(RUNTIME_CLOSED)?;
        let handle = executor.handle().clone();
        let writer = self.writer.clone();
        let app = host.app().clone();
        executor.spawn_blocking(move || {
            let result = (|| {
                let Some((owner, _)) = crate::sessions::find_agent_runtime_binding(&child)? else {
                    return Ok(None);
                };
                if owner != parent {
                    return Err("Child transcript does not belong to the current Session".into());
                }
                let mut request = |method: &str, payload: Value| {
                    if !matches!(
                        method,
                        "transcript/page" | "transcript/patches" | "transcript/content-range"
                    ) || payload["request"]["sessionId"].as_str() != Some(child.as_str())
                    {
                        return Err("Invalid child transcript read".into());
                    }
                    handle
                        .block_on(app.request(
                            writer.clone(),
                            HostCommandRequest {
                                command: method.to_owned(),
                                payload,
                            },
                        ))
                        .map_err(|error| error.to_string())
                };
                read(&mut request).map(Some)
            })();
            let _ = tx.send(result);
        });
        Ok(())
    }
}

#[cfg(all(test, feature = "native-job-store"))]
mod tests {
    use super::*;
    use crate::subagent_scheduler::queue_tests::{object, packet};
    use centaeris_core::runtime::subagent::{build_subagent_run_job, SubagentRunJobRequest};
    use centaeris_core::session::external_context::ExternalContextStorePort;
    use centaeris_core::session::reliability::{
        RuntimeJobStatus, RuntimeJobStorePort, ScheduleRuntimeJobRequest,
    };
    use centaeris_runtime_sqlite::SqliteRuntimeStore;

    #[tokio::test]
    async fn native_subagent_projection_and_cancellation_follow_exact_attempts() {
        let profile = tempfile::tempdir().expect("test store directory");
        let store =
            SqliteRuntimeStore::new(profile.path().join("runtime.sqlite3")).expect("test store");
        let cwd = profile.path().to_string_lossy();
        let submit = |parent: &str, name: &str, agent: &str, at: i64| {
            let work_ref = format!("packet:{name}");
            let job = build_subagent_run_job(SubagentRunJobRequest {
                session_id: parent.into(),
                parent_turn_id: "parent-turn".into(),
                tool_call_id: name.into(),
                subagent_id: agent.into(),
                work_packet_ref: work_ref.clone(),
                checkpoint_id: None,
                run_at_ms: at,
                created_at_ms: at,
                max_retries: 0,
            });
            let mut packet = packet(parent, &format!("session-{agent}"), &job, &cwd);
            packet["workPacket"]["run_context"]["agentRef"]["agentId"] = agent.into();
            packet["workPacket"]["task_brief"]["output_hint"] = name.into();
            store
                .upsert_external_context_object(object(&work_ref, packet.to_string()))
                .expect("persist delegated description");
            store
                .schedule_runtime_job(ScheduleRuntimeJobRequest { job })
                .expect("schedule delegation")
                .job
                .job_id
        };
        let a = submit("parent", "Task A", "agent-a", 1);
        let b = submit("parent", "Task B", "agent-a", 2);
        let sibling = submit("parent", "Sibling", "agent-b", 3);
        let foreign = submit("foreign", "Foreign", "agent-c", 4);
        let jobs = store.job_store(8).expect("native Job store");

        let listed = list_session_subagent_tasks(&jobs, "parent", SubagentTaskScope::Active)
            .await
            .expect("list parent attempts");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].active.as_ref().unwrap().task_id, a);
        assert!(listed[0].last_terminal.is_none());
        assert_eq!(listed[1].active.as_ref().unwrap().task_id, sibling);

        let cancel = |task: &str| RequestSubagentJobCancellation {
            session_id: "parent".into(),
            job_id: task.into(),
            requested_at_ms: 10,
        };
        assert!(request_subagent_task_cancellation(&jobs, cancel(&foreign))
            .await
            .is_err());
        let receipt = request_subagent_task_cancellation(&jobs, cancel(&a))
            .await
            .expect("cancel displayed attempt");
        assert!(receipt.accepted);
        assert_eq!(receipt.status, RuntimeJobStatus::Cancelled);

        let listed =
            list_session_subagent_tasks(&jobs, "parent", SubagentTaskScope::IncludeTerminal)
                .await
                .expect("recover from durable jobs");
        let running = listed[0].active.as_ref().unwrap();
        let terminal = listed[0].last_terminal.as_ref().unwrap();
        assert_eq!(running.child_session_id, terminal.child_session_id);
        assert_eq!(running.task_id, b);
        assert_eq!(running.description, "Task B");
        assert_eq!(terminal.task_id, a);
        assert_eq!(terminal.status, RuntimeJobStatus::Cancelled);
        assert_eq!(terminal.description, "Task A");
        assert!(
            !request_subagent_task_cancellation(&jobs, cancel(&a))
                .await
                .expect("reject stale stop")
                .accepted
        );
        for id in [&b, &sibling, &foreign] {
            assert_eq!(
                store.get_runtime_job(id).unwrap().unwrap().status,
                RuntimeJobStatus::Queued
            );
        }
        jobs.close().await.expect("close native Job store");
    }
}
