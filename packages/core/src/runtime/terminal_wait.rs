//! Terminating an owning run abandons its continuation, without resuming tools.
use super::contracts::{
    CheckpointKindV1, CheckpointRecord, EventVisibility, RuntimeAwaitJobCheckpointV1, RuntimeEvent,
    RuntimeWaitChangedV1, RuntimeWaitStatusV1, RUNTIME_WAIT_CHANGED_SCHEMA_V1,
};
use crate::session::store::ConsumeWaitCheckpointRequest;

/// The caller must establish the owner's durable terminal state and commit
/// consumption atomically with that state (or an idempotent terminal repair).
pub fn abandon_terminal_runtime_job_wait(
    checkpoint: CheckpointRecord,
    session_id: &str,
    agent_run_id: &str,
    at_ms: i64,
) -> Result<ConsumeWaitCheckpointRequest, String> {
    if checkpoint.kind != CheckpointKindV1::Wait
        || checkpoint.status != "waiting"
        || checkpoint.done_reason.as_deref() != Some("runtime_job")
        || checkpoint.session_id != session_id
    {
        return Err("terminal wait checkpoint scope mismatch".into());
    }
    let wait: RuntimeAwaitJobCheckpointV1 = serde_json::from_str(&checkpoint.payload_json)
        .map_err(|error| format!("decode terminal runtime-job wait failed: {error}"))?;
    wait.validate()?;
    if wait.agent_run_id != agent_run_id || wait.turn_id != checkpoint.turn_id {
        return Err("terminal wait owner identity mismatch".into());
    }
    let changed = RuntimeWaitChangedV1 {
        schema: RUNTIME_WAIT_CHANGED_SCHEMA_V1.into(),
        continuation_id: wait.continuation_id.clone(),
        agent_run_id: agent_run_id.into(),
        status: RuntimeWaitStatusV1::Abandoned,
        transition_reason: "agent_run_terminal".into(),
        at_ms,
    };
    changed.validate()?;
    let event = RuntimeEvent {
        event_id: format!("runtime_wait:{}:abandoned", wait.continuation_id),
        session_id: session_id.into(),
        task_id: Some(checkpoint.turn_id.clone()),
        event_type: RUNTIME_WAIT_CHANGED_SCHEMA_V1.into(),
        at_ms,
        visibility: EventVisibility::User,
        payload_json: serde_json::to_string(&changed).map_err(|e| e.to_string())?,
    };
    Ok(ConsumeWaitCheckpointRequest {
        checkpoint,
        events: vec![event],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::contracts::{RuntimeAgentRunIdentityV1, RuntimeJobWaitV1};

    fn checkpoint() -> CheckpointRecord {
        let digest = format!("sha256:{}", "a".repeat(64));
        let wait = RuntimeAwaitJobCheckpointV1::new(
            &RuntimeAgentRunIdentityV1 {
                agent_run_id: "run_terminal".into(),
                execution_id: "execution_terminal".into(),
                authorization_digest: digest.clone(),
            },
            "turn_terminal",
            vec![RuntimeJobWaitV1 {
                tool_call_id: "call_terminal".into(),
                source_tool_name: "read_result".into(),
                tool_definition_digest: digest,
                job_id: "source:terminal".into(),
                job_kind: "subagent.run".into(),
            }],
        )
        .unwrap();
        CheckpointRecord {
            checkpoint_id: "checkpoint:terminal".into(),
            kind: CheckpointKindV1::Wait,
            session_id: "session_terminal".into(),
            turn_id: wait.turn_id.clone(),
            status: "waiting".into(),
            done_reason: Some("runtime_job".into()),
            updated_at_ms: 1,
            payload_json: serde_json::to_string(&wait).unwrap(),
        }
    }

    #[test]
    fn terminal_wait_abandons_without_resuming_tools() {
        let cp = checkpoint();
        let request =
            abandon_terminal_runtime_job_wait(cp.clone(), "session_terminal", "run_terminal", 42)
                .unwrap();
        assert_eq!(request.checkpoint, cp);
        assert_eq!(request.events.len(), 1);
        let event = &request.events[0];
        assert_eq!(event.event_type, RUNTIME_WAIT_CHANGED_SCHEMA_V1);
        let changed: RuntimeWaitChangedV1 = serde_json::from_str(&event.payload_json).unwrap();
        assert_eq!(changed.status, RuntimeWaitStatusV1::Abandoned);
        assert_eq!(changed.transition_reason, "agent_run_terminal");
        assert_eq!(changed.at_ms, 42);
        assert_eq!(
            event.event_id,
            format!("runtime_wait:{}:abandoned", changed.continuation_id)
        );
    }

    #[test]
    fn terminal_wait_rejects_unrelated_owner_or_checkpoint() {
        assert!(abandon_terminal_runtime_job_wait(
            checkpoint(),
            "another_session",
            "run_terminal",
            42
        )
        .is_err());
        assert!(abandon_terminal_runtime_job_wait(
            checkpoint(),
            "session_terminal",
            "another_run",
            42
        )
        .is_err());
        let mut cp = checkpoint();
        cp.turn_id = "another_turn".into();
        assert!(
            abandon_terminal_runtime_job_wait(cp, "session_terminal", "run_terminal", 42).is_err()
        );
        let mut cp = checkpoint();
        cp.kind = CheckpointKindV1::Recovery;
        assert!(
            abandon_terminal_runtime_job_wait(cp, "session_terminal", "run_terminal", 42).is_err()
        );
    }
}
