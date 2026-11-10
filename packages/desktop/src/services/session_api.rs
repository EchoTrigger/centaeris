//! Application-owned Session operations and the native SDK host adapter.

use crate::agent_runs::{
    AgentRunAttachRequest, AgentRunAttachResponse, AgentRunCancelRequest, AgentRunCancelResponse,
    AgentRunListRequest, AgentRunListResponse, AgentRunSummary,
};
use crate::agent_runtime::{
    AgentContextCompactRequest, AgentInputRequest, AgentQuestionAnswerRequest,
    AgentSupplementRequest,
};
use crate::errors::RuntimeHostError;
use crate::runtime_rpc_transport::{EventWriter, RuntimeServerClientHub};
use crate::sessions::{SessionGetRequest, SessionItemResponse};
use crate::{agent_runs, agent_runtime, sessions};
#[cfg(test)]
use centaeris_agent::OperationId;
use centaeris_agent::SessionHost;
pub(crate) use centaeris_agent::{
    AgentAnswerNowResponse, AgentClient, AgentContextCompactResponse, AgentInputResponse, AgentRun,
    AgentRunInterventionV1, AgentSupplementResponse, CancelReceipt, QuestionAnswer,
    RunSubscription, SessionCreateCommandRequest, SessionDataResponse, SessionEvent, SessionPrompt,
};
use futures::future::BoxFuture;
use std::sync::Arc;

/// Registers one native viewer in the product host's existing lifecycle.
pub(crate) fn connect_native(
    clients: &Arc<RuntimeServerClientHub>,
    viewer_id: &str,
) -> Result<AgentClient, RuntimeHostError> {
    let (writer, events) = clients
        .connect_native()?
        .ok_or_else(|| RuntimeHostError::new("agent_draining", "Agent host is draining"))?;
    if let Err(error) =
        writer.register_client(crate::runtime_server::RuntimeClientKind::Sdk, viewer_id)
    {
        clients.disconnect(&writer)?;
        return Err(error);
    }
    let disconnected = writer.disconnect_token()?;
    let host = Arc::new(LocalSessionHost {
        api: LocalSessionApi::new(writer),
        clients: Arc::clone(clients),
    });
    Ok(AgentClient::new(host, events, disconnected))
}

struct LocalSessionHost {
    api: LocalSessionApi,
    clients: Arc<RuntimeServerClientHub>,
}

impl SessionHost for LocalSessionHost {
    fn create_session(
        &self,
        request: SessionCreateCommandRequest,
    ) -> Result<String, RuntimeHostError> {
        let _request = self.api.events.request_permit(false)?;
        let _action = self.api.events.start_action()?;
        Ok(self.api.create(request)?.id)
    }

    fn load_session(&self, session_id: &str) -> Result<SessionDataResponse, RuntimeHostError> {
        let _request = self.api.events.request_permit(false)?;
        self.api.events.require_registered()?;
        self.api.load(SessionGetRequest {
            session_id: session_id.to_owned(),
        })
    }

    fn send(
        &self,
        session_id: &str,
        prompt: SessionPrompt,
    ) -> Result<AgentInputResponse, RuntimeHostError> {
        let _request = self.api.events.request_permit(false)?;
        let _action = self.api.events.start_action()?;
        self.api.send(AgentInputRequest {
            operation_id: prompt.operation_id,
            session_id: Some(session_id.to_owned()),
            message: prompt.message,
            tail_policy: prompt.tail_policy,
            rewrite_target_message_id: prompt.rewrite_target_message_id,
            rewrite_expected_tail_message_id: prompt.rewrite_expected_tail_message_id,
            auto_continue_after_resume_wait: prompt.auto_continue_after_resume_wait,
            attachments: prompt.attachments,
        })
    }

    fn subscribe(
        &self,
        session_id: &str,
        run_id: Option<&str>,
    ) -> Result<RunSubscription, RuntimeHostError> {
        let _request = self.api.events.request_permit(false)?;
        let viewer_id = self.api.events.registration()?.viewer_id;
        let attached = self.api.subscribe(AgentRunAttachRequest {
            session_id: Some(session_id.to_owned()),
            agent_run_id: run_id.map(str::to_owned),
            viewer_id: Some(viewer_id),
        })?;
        Ok(RunSubscription {
            agent_run: attached.agent_run.map(Into::into),
            viewer_id: attached.viewer_id,
            transition_reason: attached.transition_reason,
            attached_viewer_count: attached.attached_viewer_count,
        })
    }

    fn cancel(
        &self,
        session_id: &str,
        run_id: &str,
        reason: Option<String>,
    ) -> Result<CancelReceipt, RuntimeHostError> {
        let _request = self.api.events.request_permit(true)?;
        self.api.events.require_registered()?;
        let cancelled = self.api.cancel(AgentRunCancelRequest {
            session_id: Some(session_id.to_owned()),
            agent_run_id: Some(run_id.to_owned()),
            reason,
        })?;
        Ok(CancelReceipt {
            agent_run: cancelled.agent_run.map(Into::into),
            cancel_accepted: cancelled.cancel_accepted,
        })
    }

    fn supplement(
        &self,
        session_id: &str,
        run_id: &str,
        message: String,
    ) -> Result<AgentSupplementResponse, RuntimeHostError> {
        let _request = self.api.events.request_permit(false)?;
        let _action = self.api.events.start_action()?;
        self.api.supplement(AgentSupplementRequest {
            session_id: session_id.to_owned(),
            agent_run_id: run_id.to_owned(),
            message,
        })
    }

    fn answer_now(
        &self,
        session_id: &str,
        intervention: AgentRunInterventionV1,
    ) -> Result<AgentAnswerNowResponse, RuntimeHostError> {
        let _request = self.api.events.request_permit(false)?;
        let _action = self.api.events.start_action()?;
        self.api
            .validate_session_run(session_id, &intervention.agent_run_id)?;
        self.api.answer_now(intervention)
    }

    fn answer_question<'a>(
        &'a self,
        session_id: &'a str,
        answer: QuestionAnswer,
    ) -> BoxFuture<'a, Result<AgentInputResponse, RuntimeHostError>> {
        Box::pin(async move {
            let _request = self.api.events.request_permit(false)?;
            let _action = self.api.events.start_action()?;
            self.api
                .answer_question(AgentQuestionAnswerRequest {
                    session_id: Some(session_id.to_owned()),
                    question_id: answer.question_id,
                    answers: answer.answers,
                    answer_text: answer.answer_text,
                    auto_continue_after_resume_wait: answer.auto_continue_after_resume_wait,
                })
                .await
        })
    }

    fn compact_context<'a>(
        &'a self,
        session_id: &'a str,
    ) -> BoxFuture<'a, Result<AgentContextCompactResponse, RuntimeHostError>> {
        Box::pin(async move {
            let _request = self.api.events.request_permit(false)?;
            let _action = self.api.events.start_action()?;
            self.api
                .compact_context(AgentContextCompactRequest {
                    session_id: session_id.to_owned(),
                })
                .await
        })
    }

    fn list_runs(
        &self,
        session_id: &str,
        include_terminal: bool,
    ) -> Result<Vec<AgentRun>, RuntimeHostError> {
        let _request = self.api.events.request_permit(false)?;
        self.api.events.require_registered()?;
        let runs = self.api.list_runs(AgentRunListRequest {
            session_id: Some(session_id.to_owned()),
            include_terminal: Some(include_terminal),
        })?;
        Ok(runs.agent_runs.into_iter().map(Into::into).collect())
    }

    fn run(&self, run_id: &str) -> Result<AgentRun, RuntimeHostError> {
        let _request = self.api.events.request_permit(false)?;
        self.api.events.require_registered()?;
        self.api.run(run_id).map(Into::into)
    }

    fn disconnect(&self) -> Result<(), RuntimeHostError> {
        self.clients.disconnect(&self.api.events)
    }
}

impl From<AgentRunSummary> for AgentRun {
    fn from(run: AgentRunSummary) -> Self {
        Self {
            agent_run_id: run.agent_run_id,
            session_id: run.session_id,
            turn_id: run.turn_id,
            agent_run_kind: run.agent_run_kind,
            cwd: run.cwd,
            status: run.status,
            started_at_ms: run.started_at_ms,
            updated_at_ms: run.updated_at_ms,
            completed_at_ms: run.completed_at_ms,
            last_event_at_ms: run.last_event_at_ms,
            stall_reason: run.stall_reason,
            watchdog: run.watchdog,
            error: run.error,
        }
    }
}

#[derive(Clone)]
pub(crate) struct LocalSessionApi {
    events: EventWriter,
}

impl LocalSessionApi {
    pub(crate) fn new(events: EventWriter) -> Self {
        Self { events }
    }

    pub fn create(
        &self,
        request: SessionCreateCommandRequest,
    ) -> Result<SessionItemResponse, RuntimeHostError> {
        sessions::create_command(request)
            .map_err(|error| RuntimeHostError::new(error.code(), error.message()))
    }

    pub fn load(
        &self,
        request: SessionGetRequest,
    ) -> Result<SessionDataResponse, RuntimeHostError> {
        sessions::get(request).map_err(|error| RuntimeHostError::new("session_failed", error))
    }

    pub fn send(&self, request: AgentInputRequest) -> Result<AgentInputResponse, RuntimeHostError> {
        agent_runtime::input(self.events.clone(), request)
            .map_err(|error| RuntimeHostError::new(error.code(), error.message()))
    }

    /// Attach this connection's registered viewer to a Run. Events continue on
    /// the connection's existing `session/update` channel.
    pub fn subscribe(
        &self,
        request: AgentRunAttachRequest,
    ) -> Result<AgentRunAttachResponse, RuntimeHostError> {
        if let (Some(session_id), Some(run_id)) = (&request.session_id, &request.agent_run_id) {
            self.validate_session_run(session_id, run_id)?;
        }
        let viewer_id = request.viewer_id.clone().unwrap_or_default();
        self.events.with_registered_viewer(&viewer_id, || {
            agent_runs::attach(request)
                .map_err(|error| RuntimeHostError::new("agent_task_failed", error))
        })
    }

    pub fn run(&self, agent_run_id: &str) -> Result<AgentRunSummary, RuntimeHostError> {
        let run = crate::message_log::project_agent_run(agent_run_id)
            .map_err(|error| RuntimeHostError::new("agent_task_failed", error))?
            .ok_or_else(|| {
                RuntimeHostError::new("agent_run_not_found", "AgentRun was not found")
            })?;
        Ok(agent_runs::into_summary(&run))
    }

    fn validate_session_run(
        &self,
        session_id: &str,
        agent_run_id: &str,
    ) -> Result<(), RuntimeHostError> {
        let session_id = session_id.trim();
        let agent_run_id = agent_run_id.trim();
        if session_id.is_empty() || agent_run_id.is_empty() {
            return Ok(());
        }
        let run = crate::message_log::project_agent_run(agent_run_id)
            .map_err(|error| RuntimeHostError::new("agent_task_failed", error))?;
        if run.is_some_and(|run| run.session_id != session_id) {
            return Err(RuntimeHostError::invalid_request(
                "AgentRun belongs to a different Session",
            ));
        }
        Ok(())
    }

    pub fn cancel(
        &self,
        request: AgentRunCancelRequest,
    ) -> Result<AgentRunCancelResponse, RuntimeHostError> {
        if let (Some(session_id), Some(run_id)) = (&request.session_id, &request.agent_run_id) {
            self.validate_session_run(session_id, run_id)?;
        }
        agent_runtime::cancel_agent_run(self.events.clone(), request)
            .map_err(|error| RuntimeHostError::new("agent_task_failed", error))
    }

    pub fn list_runs(
        &self,
        request: AgentRunListRequest,
    ) -> Result<AgentRunListResponse, RuntimeHostError> {
        agent_runs::list(request).map_err(|error| RuntimeHostError::new("agent_task_failed", error))
    }

    pub fn supplement(
        &self,
        request: AgentSupplementRequest,
    ) -> Result<AgentSupplementResponse, RuntimeHostError> {
        agent_runtime::supplement(self.events.clone(), request)
            .map_err(|error| RuntimeHostError::new("session_supplement_failed", error))
    }

    pub fn answer_now(
        &self,
        intervention: AgentRunInterventionV1,
    ) -> Result<AgentAnswerNowResponse, RuntimeHostError> {
        agent_runtime::answer_now(self.events.clone(), intervention)
            .map_err(|error| RuntimeHostError::new("session_answer_now_failed", error))
    }

    pub async fn answer_question(
        &self,
        request: AgentQuestionAnswerRequest,
    ) -> Result<AgentInputResponse, RuntimeHostError> {
        agent_runtime::question_answer_async(self.events.clone(), request)
            .await
            .map_err(|error| RuntimeHostError::new("session_answer_question_failed", error))
    }

    pub async fn compact_context(
        &self,
        request: AgentContextCompactRequest,
    ) -> Result<AgentContextCompactResponse, RuntimeHostError> {
        agent_runtime::compact_context(self.events.clone(), request)
            .await
            .map_err(|error| RuntimeHostError::new("context_compaction_failed", error))
    }
}

#[cfg(all(test, feature = "native-job-store"))]
mod control_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_operation_ids_enforce_the_wire_identity_contract() {
        for invalid in ["", "has spaces", "路径", "slash/value"] {
            assert!(OperationId::try_from(invalid).is_err());
        }
        assert!(OperationId::try_from("x".repeat(129)).is_err());
        let valid = OperationId::try_from("request-1:part_2.3").unwrap();
        assert_eq!(serde_json::to_value(&valid).unwrap(), "request-1:part_2.3");
        assert_eq!(
            serde_json::from_value::<OperationId>(serde_json::json!(valid)).unwrap(),
            valid
        );
    }

    #[tokio::test]
    async fn native_event_overflow_closes_the_connection_and_drop_releases_its_viewer() {
        let clients = Arc::new(RuntimeServerClientHub::default());
        let mut native = connect_native(&clients, "native-overflow").unwrap();
        for sequence in 0..=128 {
            clients
                .broadcaster()
                .emit("session/update", serde_json::json!({"sequence": sequence}))
                .unwrap();
        }
        assert!(native.next_event().await.is_none());
        assert!(connect_native(&clients, "native-overflow").is_err());
        drop(native);
        let mut reconnected = connect_native(&clients, "native-overflow").unwrap();
        clients
            .broadcaster()
            .emit("session/update", serde_json::json!({"sequence":129}))
            .unwrap();
        assert_eq!(
            reconnected.next_event().await.unwrap().params["sequence"],
            129
        );
    }
}
