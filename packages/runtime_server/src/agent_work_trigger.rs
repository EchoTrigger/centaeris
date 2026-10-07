//! A committed dispatch result can wake the API materializer without delaying Core.
use std::sync::{Arc, OnceLock};

use centaeris_core::session::{SessionCommitReceipt, SessionRecordType};
use serde_json::json;
use tokio::sync::Semaphore;

use crate::contract::AgentRunStart;

static TRIGGER_SLOTS: OnceLock<Result<Arc<Semaphore>, String>> = OnceLock::new();

pub(crate) struct WorkCommitTrigger {
    runtime: tokio::runtime::Handle,
    http: reqwest::Client,
    endpoint: String,
    internal_token: String,
    slots: Arc<Semaphore>,
}

impl WorkCommitTrigger {
    pub(crate) fn new(
        runtime: tokio::runtime::Handle,
        api_url: &str,
        internal_token: &str,
    ) -> Result<Self, String> {
        let slots = TRIGGER_SLOTS
            .get_or_init(|| {
                crate::execution_capacity::ExecutionCapacity::from_env()
                    .map(|capacity| Arc::new(Semaphore::new(capacity.global)))
            })
            .clone()?;
        Ok(Self {
            runtime,
            http: crate::agent_work::work_api_client()?,
            endpoint: format!(
                "{}/internal/agent-work/materialize",
                api_url.trim_end_matches('/')
            ),
            internal_token: internal_token.to_owned(),
            slots,
        })
    }

    pub(crate) fn after_commit(&self, start: &AgentRunStart, receipt: &SessionCommitReceipt) {
        if start.native_coordination_session_id.as_deref()
            != Some(start.authorization.session_id.as_str())
        {
            return;
        }
        for record in &receipt.records {
            let event = &record.event;
            if event.session_id != start.authorization.session_id
                || event.agent_run_id.as_deref() != Some(start.agent_run_id.as_str())
                || event.event_type != SessionRecordType::ToolResult
                || event.payload["toolName"] != "dispatch_work"
                || event.payload["resultState"] != "successWithOutput"
            {
                continue;
            }
            let Ok(permit) = self.slots.clone().try_acquire_owned() else {
                eprintln!(
                    "agent_work_trigger_deferred eventId={} reason=capacity",
                    event.event_id
                );
                continue;
            };
            let http = self.http.clone();
            let endpoint = self.endpoint.clone();
            let token = self.internal_token.clone();
            let event_id = event.event_id.clone();
            self.runtime.spawn(async move {
                let _permit = permit;
                match http
                    .post(endpoint)
                    .header("X-Internal-Token", token)
                    .json(&json!({"schema": "workspace.agent_work.materialize.v1", "sourceEventId": event_id}))
                    .send()
                    .await
                {
                    Ok(response) if matches!(response.status().as_u16(), 200 | 201) => {}
                    Ok(response) => eprintln!(
                        "agent_work_trigger_deferred eventId={event_id} status={}", response.status().as_u16()
                    ),
                    Err(_) => eprintln!("agent_work_trigger_deferred eventId={event_id} reason=http"),
                }
            });
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_limits(mut self, http: reqwest::Client, slots: Arc<Semaphore>) -> Self {
        self.http = http;
        self.slots = slots;
        self
    }
}

#[cfg(test)]
#[path = "agent_work_trigger_tests.rs"]
mod tests;
