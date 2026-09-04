use std::collections::HashMap;

use crate::model::{GenerateResult, ToolCallEnvelope};
use crate::runtime::contracts::RuntimeProcessState;
use crate::runtime::event::RuntimeEventVisibility;
use crate::runtime::subagent::{
    SubagentLifecycleStatus, SubagentSchedulerEvent, SubagentSchedulerEventKind,
};
use crate::tool::layer::ToolExecutionResult;
use crate::tool::permission::{PermissionDecision, PermissionNormalizedInput};
use crate::tool::RiskLevel;

use super::subagent::{
    build_runtime_event_subagent_event, build_runtime_event_subagent_event_from_scheduler_event,
    build_runtime_event_subagent_tool_group_events_from_tool_results, SubagentEventKind,
    SubagentEventPayload,
};
use super::{
    build_runtime_event_final_event, build_runtime_event_status_event,
    build_runtime_event_tool_call_events as build_runtime_event_tool_call_events_from_runtime,
    build_runtime_event_tool_result_events as build_runtime_event_tool_result_events_from_runtime,
};
use serde_json::Value;

#[test]
fn status_event_identity_is_bounded_for_long_structured_components() {
    let component = "x".repeat(512);
    let event = build_runtime_event_status_event(
        component.as_str(),
        component.as_str(),
        component.as_str(),
        component.as_str(),
        "Working",
        "running",
        None,
        None,
    );

    assert!(event.event_id.len() <= crate::session::SESSION_EVENT_ID_MAX_BYTES);
    event.validate().expect("bounded runtime event");
}

fn build_runtime_event_tool_call_events(
    session_id: &str,
    turn_id: &str,
    tool_calls: &[ToolCallEnvelope],
    permission_preview: Option<&HashMap<String, PermissionDecision>>,
) -> Vec<crate::runtime::event::RuntimeEventProjection> {
    build_runtime_event_tool_call_events_from_runtime(
        session_id,
        turn_id,
        tool_calls,
        permission_preview,
    )
    .expect("build ToolCall events")
}

fn build_runtime_event_tool_result_events(
    session_id: &str,
    turn_id: &str,
    tool_calls: Option<&[ToolCallEnvelope]>,
    tool_results: &[ToolExecutionResult],
    tool_operations_json: Option<&str>,
) -> Vec<crate::runtime::event::RuntimeEventProjection> {
    let _ = tool_calls;
    build_runtime_event_tool_result_events_from_runtime(
        session_id,
        turn_id,
        tool_results,
        tool_operations_json,
    )
    .expect("build ToolResult events")
}

#[test]
fn tool_call_event_omits_permission_decision_without_preview() {
    let events = build_runtime_event_tool_call_events(
        "chat-test",
        "turn-test",
        &[ToolCallEnvelope {
            id: "call-1".to_string(),
            name: "bash".to_string(),
            args_json: "{\"command\":\"python manage.py migrate\"}".to_string(),
        }],
        None,
    );

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].visibility, RuntimeEventVisibility::User);
    let payload: serde_json::Value = serde_json::to_value(&events[0]).expect("parse event payload");
    assert_eq!(
        payload
            .get("processState")
            .and_then(serde_json::Value::as_str),
        Some(RuntimeProcessState::Executing.as_str())
    );
    assert!(payload
        .get("meta")
        .and_then(|item| item.get("permissionDecision"))
        .is_none_or(Value::is_null));
    assert!(payload
        .get("payload")
        .and_then(|item| item.get("argsPreview"))
        .is_none());
}

#[test]
fn tool_call_event_uses_blocked_permission_preview_when_present() {
    let mut preview = HashMap::new();
    preview.insert(
        "call-1".to_string(),
        PermissionDecision::new(
            false,
            RiskLevel::Restricted,
            "recursive deletion of a protected root is prohibited",
            "bash_recursive_delete_protected_root",
            "test_permission_preview",
            PermissionNormalizedInput {
                tool_name: "bash".to_string(),
                command_name: Some("python".to_string()),
                path: None,
                task_id: None,
            },
        ),
    );

    let events = build_runtime_event_tool_call_events(
        "chat-test",
        "turn-test",
        &[ToolCallEnvelope {
            id: "call-1".to_string(),
            name: "bash".to_string(),
            args_json: "{\"command\":\"python manage.py migrate\"}".to_string(),
        }],
        Some(&preview),
    );

    let payload: serde_json::Value = serde_json::to_value(&events[0]).expect("parse event payload");
    assert_eq!(
        payload
            .get("meta")
            .and_then(|item| item.get("permissionReason"))
            .and_then(serde_json::Value::as_str),
        Some("recursive deletion of a protected root is prohibited")
    );
    assert_eq!(
        payload
            .get("meta")
            .and_then(|item| item.get("permissionDecision"))
            .and_then(|item| item.get("reasonType"))
            .and_then(serde_json::Value::as_str),
        Some("bash_recursive_delete_protected_root")
    );
    assert_eq!(
        payload
            .get("meta")
            .and_then(|item| item.get("permissionDecision"))
            .and_then(|item| item.get("decision"))
            .and_then(serde_json::Value::as_str),
        Some("blocked")
    );
}

#[test]
fn bash_tool_call_event_prefers_description_and_keeps_command() {
    let events = build_runtime_event_tool_call_events(
        "chat-test",
        "turn-test",
        &[ToolCallEnvelope {
            id: "call-1".to_string(),
            name: "bash".to_string(),
            args_json: r#"{"command":"cargo test","description":"Run focused tests"}"#.to_string(),
        }],
        None,
    );

    let event = serde_json::to_value(&events[0]).expect("serialize ToolCall event");
    assert_eq!(event["payload"]["displayTarget"], "Run focused tests");
    assert_eq!(event["payload"]["command"], "cargo test");
    assert_eq!(event["payload"]["description"], "Run focused tests");
}

#[test]
fn bash_tool_call_event_bounds_a_command_title_without_losing_the_command() {
    let command = "x".repeat(300);
    let events = build_runtime_event_tool_call_events(
        "chat-test",
        "turn-test",
        &[ToolCallEnvelope {
            id: "call-1".to_string(),
            name: "bash".to_string(),
            args_json: serde_json::json!({"command": command.clone()}).to_string(),
        }],
        None,
    );

    let event = serde_json::to_value(&events[0]).expect("serialize ToolCall event");
    assert_eq!(
        event["payload"]["displayTarget"]
            .as_str()
            .expect("display target")
            .chars()
            .count(),
        256
    );
    assert_eq!(event["payload"]["command"], command);
    assert!(event["payload"].get("description").is_none());
}

#[test]
fn tool_result_event_keeps_evidence_refs_internal() {
    let raw_tail = "Z".repeat(2_000);
    let details = serde_json::json!({
        "summary": "large evidence captured",
        "output": format!("preview-only-{raw_tail}"),
        "externalContextStore": {
            "persisted": true,
            "linked": true,
            "objectId": "external_context:tool_evidence_1",
            "rawOutputObjectized": true
        },
        "evidenceRollupStore": {
            "persisted": true,
            "linked": true,
            "objectId": "external_context:tool_evidence_rollup_1"
        }
    });
    let events = build_runtime_event_tool_result_events(
        "chat-evidence-event",
        "turn-evidence-event",
        None,
        &[ToolExecutionResult {
            tool_call_id: "call-evidence".to_string(),
            tool_name: "bash".to_string(),
            status: "ok".to_string(),
            content: "large evidence captured".to_string(),
            details,
            facts: Vec::new(),
            error: None,
            started_at_ms: 10,
            completed_at_ms: 20,
            latency_ms: 10,
            parallel_group: None,
            transition_reason: Some("parallel_exec".to_string()),
        }],
        None,
    );

    assert_eq!(events.len(), 2);
    let user_event = events
        .iter()
        .find(|event| event.visibility == RuntimeEventVisibility::User)
        .expect("user tool result event");
    let user_payload: serde_json::Value =
        serde_json::to_value(user_event).expect("parse user payload");
    assert_eq!(
        user_payload
            .get("processState")
            .and_then(serde_json::Value::as_str),
        Some(RuntimeProcessState::Executing.as_str())
    );
    let payload = user_payload.get("payload").expect("tool result payload");
    assert!(payload.get("evidenceObjectId").is_none());
    assert!(payload.get("evidenceRollupObjectId").is_none());
    assert!(payload.get("turnTranscriptObjectId").is_none());
    assert!(payload
        .get("outputByteLength")
        .and_then(serde_json::Value::as_u64)
        .is_some());
    assert_eq!(
        payload
            .get("resultState")
            .and_then(serde_json::Value::as_str),
        Some("successWithOutput")
    );
    assert!(payload.get("outputTruncated").is_none());
    assert!(!serde_json::to_string(user_event)
        .expect("serialize user event")
        .contains(raw_tail.as_str()));

    let internal_event = events
        .iter()
        .find(|event| event.visibility == RuntimeEventVisibility::Internal)
        .expect("internal evidence refs event");
    let internal_payload: serde_json::Value =
        serde_json::to_value(internal_event).expect("parse internal payload");
    assert_eq!(
        internal_payload
            .get("type")
            .and_then(serde_json::Value::as_str),
        Some("ToolEvidenceRefs")
    );
    let refs_payload = internal_payload
        .get("payload")
        .expect("tool evidence refs payload");
    assert_eq!(
        refs_payload
            .get("evidenceObjectId")
            .and_then(serde_json::Value::as_str),
        Some("external_context:tool_evidence_1")
    );
    assert_eq!(
        refs_payload
            .get("evidenceRollupObjectId")
            .and_then(serde_json::Value::as_str),
        Some("external_context:tool_evidence_rollup_1")
    );
    assert!(refs_payload.get("turnTranscriptObjectId").is_none());
    assert!(refs_payload
        .get("outputBytes")
        .and_then(serde_json::Value::as_u64)
        .is_some());
    assert_eq!(
        refs_payload
            .get("outputTruncated")
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(!serde_json::to_string(internal_event)
        .expect("serialize internal event")
        .contains(raw_tail.as_str()));
}

#[test]
fn tool_result_event_projects_validated_model_input_images() {
    let events = build_runtime_event_tool_result_events(
        "chat-image-event",
        "turn-image-event",
        None,
        &[ToolExecutionResult {
            tool_call_id: "call-image".to_string(),
            tool_name: "read".to_string(),
            status: "ok".to_string(),
            content: "Observed image generated.png".to_string(),
            details: serde_json::json!({
                "modelInputImages": [{
                    "sourceKind": "executionFile",
                    "image": {
                        "path": "generated.png",
                        "contentType": "image/png",
                        "sha256": format!("sha256:{}", "0".repeat(64)),
                        "byteLength": 4,
                        "widthPx": 1,
                        "heightPx": 1,
                        "placeholder": "[Image observation: call-image]"
                    }
                }]
            }),
            facts: Vec::new(),
            error: None,
            started_at_ms: 10,
            completed_at_ms: 20,
            latency_ms: 10,
            parallel_group: None,
            transition_reason: Some("serial_exec".to_string()),
        }],
        None,
    );

    let event = serde_json::to_value(&events[0]).expect("tool result event");
    assert_eq!(
        event["payload"]["modelInputImages"][0]["image"]["path"],
        "generated.png"
    );
}

#[test]
fn tool_result_event_projects_bounded_network_host_diagnostics() {
    let events = build_runtime_event_tool_result_events(
        "chat-network-event",
        "turn-network-event",
        None,
        &[ToolExecutionResult {
            tool_call_id: "call-network".to_string(),
            tool_name: "bash".to_string(),
            status: "error".to_string(),
            content: "Command failed with exit code 5.\nstderr: response 403".to_string(),
            details: serde_json::json!({
                "runtimeDiagnostics": [{
                    "source": "networkProxy",
                    "stream": "internal",
                    "severity": "warning",
                    "code": "network_policy_denied",
                    "message": "private address blocked",
                    "details": {
                        "targetHost": "localhost",
                        "targetPort": 443,
                        "networkPolicyMode": "publicInternet"
                    }
                }]
            }),
            facts: Vec::new(),
            error: None,
            started_at_ms: 10,
            completed_at_ms: 20,
            latency_ms: 10,
            parallel_group: None,
            transition_reason: Some("serial_exec".to_string()),
        }],
        None,
    );

    assert_eq!(events.len(), 1);
    let event: Value = serde_json::to_value(&events[0]).expect("event");
    assert_eq!(
        event["payload"]["networkDiagnostics"][0]["details"]["targetHost"],
        "localhost"
    );
    assert_eq!(
        event["payload"]["networkDiagnostics"][0]["details"]["networkPolicyMode"],
        "publicInternet"
    );
}

#[test]
fn tool_result_event_reconstruction_is_stable() {
    let report = ToolExecutionResult {
        tool_call_id: "call-stable".to_string(),
        tool_name: "read".to_string(),
        status: "ok".to_string(),
        content: "done".to_string(),
        details: serde_json::json!({"path": "docs/README.md"}),
        facts: Vec::new(),
        error: None,
        started_at_ms: 10,
        completed_at_ms: 20,
        latency_ms: 10,
        parallel_group: None,
        transition_reason: Some("serial_exec".to_string()),
    };
    let first = build_runtime_event_tool_result_events(
        "chat-stable",
        "turn-stable",
        None,
        std::slice::from_ref(&report),
        None,
    );
    let replay =
        build_runtime_event_tool_result_events("chat-stable", "turn-stable", None, &[report], None);

    assert_eq!(first, replay);
    assert_eq!(first[0].at_ms, 20);
    assert!(first[0].event_id.starts_with("evt_v1_tool_result:sha256:"));
}

#[test]
fn tool_result_event_identity_has_unambiguous_tuple_boundaries() {
    let report = |tool_call_id: &str| ToolExecutionResult {
        tool_call_id: tool_call_id.to_string(),
        tool_name: "read".to_string(),
        status: "ok".to_string(),
        content: "done".to_string(),
        details: serde_json::json!({}),
        facts: Vec::new(),
        error: None,
        started_at_ms: 10,
        completed_at_ms: 20,
        latency_ms: 10,
        parallel_group: None,
        transition_reason: Some("serial_exec".to_string()),
    };
    let first =
        build_runtime_event_tool_result_events("chat", "initial", None, &[report("2:x")], None);
    let second =
        build_runtime_event_tool_result_events("chat", "initial:2", None, &[report("x")], None);

    assert_ne!(first[0].event_id, second[0].event_id);
}

#[test]
fn tool_result_event_carries_file_fact_from_file_tool_output() {
    let details = serde_json::json!({
        "schema": "write_result_v1",
        "path": "docs/example.md",
        "fileHash": "sha256:abc",
        "fileFact": {
            "schema": "file_write_fact_v1",
            "toolName": "write",
            "path": "docs/example.md",
            "fileHash": "sha256:abc"
        }
    });
    let events = build_runtime_event_tool_result_events(
        "chat-file-fact",
        "turn-file-fact",
        None,
        &[ToolExecutionResult {
            tool_call_id: "call-write".to_string(),
            tool_name: "write".to_string(),
            status: "ok".to_string(),
            content: "Wrote docs/example.md.".to_string(),
            details,
            facts: Vec::new(),
            error: None,
            started_at_ms: 10,
            completed_at_ms: 20,
            latency_ms: 10,
            parallel_group: None,
            transition_reason: Some("local_tool_exec".to_string()),
        }],
        None,
    );

    assert_eq!(events.len(), 1);
    let parsed: serde_json::Value = serde_json::to_value(&events[0]).expect("parse payload");
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("fileFact"))
            .and_then(|item| item.get("schema"))
            .and_then(serde_json::Value::as_str),
        Some("file_write_fact_v1")
    );
}

#[test]
fn subagent_event_builder_emits_protocol_payload() {
    let mut payload = SubagentEventPayload::new("sub-1", "turn-test");
    payload.role = Some("worker".to_string());
    payload.title = Some("Read docs".to_string());
    payload.summary = Some("Collected design notes".to_string());
    payload.work_packet_ref = Some("ctx:work-packet:1".to_string());
    payload.source_event_ids = vec!["evt-tool-1".to_string(), " ".to_string()];
    payload.result_envelope = Some(serde_json::json!({
        "summary": "Docs reviewed",
        "findings": ["runtime is local-first"]
    }));

    let event = build_runtime_event_subagent_event(
        "chat-test",
        "turn-test",
        "task-sub-1",
        "turn-test",
        SubagentEventKind::Result,
        &payload,
    );

    assert_eq!(event.event_type, "SubagentResult");
    assert_eq!(event.visibility, RuntimeEventVisibility::User);
    let parsed: serde_json::Value = serde_json::to_value(&event).expect("parse event payload");
    assert_eq!(
        parsed.get("type").and_then(serde_json::Value::as_str),
        Some("SubagentResult")
    );
    assert_eq!(
        parsed.get("status").and_then(serde_json::Value::as_str),
        Some("done")
    );
    assert_eq!(
        parsed
            .get("processState")
            .and_then(serde_json::Value::as_str),
        Some(RuntimeProcessState::Reviewing.as_str())
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("subagentId"))
            .and_then(serde_json::Value::as_str),
        Some("sub-1")
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("sourceEventIds"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        parsed
            .get("meta")
            .and_then(|item| item.get("protocol"))
            .and_then(serde_json::Value::as_str),
        Some("subagent.v1")
    );
}

#[test]
fn subagent_scheduler_event_projects_to_session_event_subagent_progress() {
    let event = build_runtime_event_subagent_event_from_scheduler_event(
        "chat-test",
        "turn-test",
        &SubagentSchedulerEvent {
            kind: SubagentSchedulerEventKind::Running,
            subagent_id: "subagent:turn-test:tool-1".to_string(),
            child_session_id: "session-agent-tool-1".to_string(),
            parent_turn_id: "turn-test".to_string(),
            job_id: crate::runtime::keys::runtime_job::subagent_run_job_id("abc"),
            work_packet_ref: Some("external_context:work_packet_1".to_string()),
            result_ref: None,
            worker_id: Some("worker-1".to_string()),
            status: SubagentLifecycleStatus::Running,
            summary: "Subagent worker started processing the work packet.".to_string(),
            description: Some("Worker is executing the delegated packet.".to_string()),
            started_at_ms: Some(12_000),
            completed_at_ms: None,
            at_ms: 12_345,
        },
    );

    let parsed: serde_json::Value = serde_json::to_value(&event).expect("parse scheduler event");
    assert_eq!(
        parsed.get("type").and_then(serde_json::Value::as_str),
        Some("SubagentProgress")
    );
    assert_eq!(
        parsed.get("status").and_then(serde_json::Value::as_str),
        Some("running")
    );
    assert_eq!(
        parsed.get("at").and_then(serde_json::Value::as_i64),
        Some(12_345)
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("workPacketRef"))
            .and_then(serde_json::Value::as_str),
        Some("external_context:work_packet_1")
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("sessionState"))
            .and_then(serde_json::Value::as_str),
        Some("waiting_background")
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("title"))
            .and_then(serde_json::Value::as_str),
        Some("Worker is executing the delegated packet.")
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("description"))
            .and_then(serde_json::Value::as_str),
        Some("Worker is executing the delegated packet.")
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("startedAtMs"))
            .and_then(serde_json::Value::as_i64),
        Some(12_000)
    );
}

#[test]
fn subagent_scheduler_terminal_events_project_to_result_failed_and_cancelled() {
    let cases = [
        (
            SubagentSchedulerEventKind::Succeeded,
            SubagentLifecycleStatus::Succeeded,
            "SubagentResult",
            "done",
            None,
        ),
        (
            SubagentSchedulerEventKind::Failed,
            SubagentLifecycleStatus::Failed,
            "SubagentFailed",
            "error",
            Some(false),
        ),
        (
            SubagentSchedulerEventKind::Cancelled,
            SubagentLifecycleStatus::Cancelled,
            "SubagentCancelled",
            "done",
            None,
        ),
    ];

    for (kind, status, expected_type, expected_status, expected_retryable) in cases {
        let event = build_runtime_event_subagent_event_from_scheduler_event(
            "chat-test",
            "turn-test",
            &SubagentSchedulerEvent {
                kind: kind.clone(),
                subagent_id: "subagent:turn-test:tool-1".to_string(),
                child_session_id: "session-agent-tool-1".to_string(),
                parent_turn_id: "turn-test".to_string(),
                job_id: crate::runtime::keys::runtime_job::subagent_run_job_id("abc"),
                work_packet_ref: None,
                result_ref: if kind == SubagentSchedulerEventKind::Succeeded {
                    Some("checkpoint:turn-agent-result".to_string())
                } else {
                    None
                },
                worker_id: Some("worker-1".to_string()),
                status,
                summary: "scheduler terminal state".to_string(),
                description: None,
                started_at_ms: None,
                completed_at_ms: Some(12_346),
                at_ms: 12_346,
            },
        );
        let parsed: serde_json::Value =
            serde_json::to_value(&event).expect("parse scheduler event");
        assert_eq!(
            parsed.get("type").and_then(serde_json::Value::as_str),
            Some(expected_type)
        );
        assert_eq!(
            parsed.get("status").and_then(serde_json::Value::as_str),
            Some(expected_status)
        );
        assert_eq!(
            parsed
                .get("payload")
                .and_then(|item| item.get("retryable"))
                .and_then(serde_json::Value::as_bool),
            expected_retryable
        );
        assert!(parsed
            .get("payload")
            .and_then(|item| item.get("description"))
            .is_none());
        assert!(parsed
            .get("payload")
            .and_then(|item| item.get("startedAtMs"))
            .is_none());
        assert_eq!(
            parsed
                .get("payload")
                .and_then(|item| item.get("completedAtMs"))
                .and_then(serde_json::Value::as_i64),
            Some(12_346)
        );
        assert_eq!(
            parsed
                .get("payload")
                .and_then(|item| item.get("sessionState"))
                .and_then(serde_json::Value::as_str),
            Some("attention_pending")
        );
        assert_eq!(
            parsed
                .get("payload")
                .and_then(|item| item.get("taskNotification"))
                .and_then(|item| item.get("schema"))
                .and_then(serde_json::Value::as_str),
            Some("task_notification_v1")
        );
        assert_eq!(
            parsed
                .get("payload")
                .and_then(|item| item.get("taskNotification"))
                .and_then(|item| item.get("status"))
                .and_then(serde_json::Value::as_str),
            Some(expected_status)
        );
        if kind == SubagentSchedulerEventKind::Succeeded {
            assert_eq!(
                parsed
                    .get("payload")
                    .and_then(|item| item.get("producedRefs"))
                    .and_then(serde_json::Value::as_array)
                    .and_then(|items| items.first())
                    .and_then(|item| item.get("resultRef"))
                    .and_then(serde_json::Value::as_str),
                Some("checkpoint:turn-agent-result")
            );
            assert_eq!(
                parsed
                    .get("payload")
                    .and_then(|item| item.get("taskNotification"))
                    .and_then(|item| item.get("outputRef"))
                    .and_then(|item| item.get("resultRef"))
                    .and_then(serde_json::Value::as_str),
                Some("checkpoint:turn-agent-result")
            );
            assert_eq!(
                parsed
                    .get("payload")
                    .and_then(|item| item.get("childSessionId"))
                    .and_then(serde_json::Value::as_str),
                Some("session-agent-tool-1")
            );
            assert_eq!(
                parsed
                    .get("payload")
                    .and_then(|item| item.get("taskNotification"))
                    .and_then(|item| item.get("outputRef"))
                    .and_then(|item| item.get("runtimeJobId"))
                    .and_then(serde_json::Value::as_str),
                Some(crate::runtime::keys::runtime_job::subagent_run_job_id("abc").as_str())
            );
        } else {
            assert!(parsed
                .get("payload")
                .and_then(|item| item.get("taskNotification"))
                .and_then(|item| item.get("outputRef"))
                .is_some_and(serde_json::Value::is_null));
        }
    }
}

#[test]
fn subagent_tool_group_event_uses_explicit_trace_and_source_events() {
    let report = ToolExecutionResult {
        tool_call_id: "tc-sub-tool".to_string(),
        tool_name: crate::runtime::keys::runtime_job::SUBAGENT_RUN.to_string(),
        status: "ok".to_string(),
        content: "worker done".to_string(),
        details: serde_json::json!({
            "resultEnvelope": {
                "summary": "worker done"
            },
            "subagentTrace": {
                "subagentId": "subagent:turn-test:tc-sub-tool",
                "toolGroups": [
                    {
                        "toolGroupId": "tg-read-docs",
                        "status": "done",
                        "title": "Read docs",
                        "summary": "Read 3 files",
                        "stats": {
                            "toolCount": 3,
                            "fileCount": 3
                        },
                        "details": [
                            {
                                "toolName": "read",
                                "summary": "docs/README.md"
                            }
                        ]
                    }
                ]
            }
        }),
        facts: Vec::new(),
        error: None,
        started_at_ms: 10,
        completed_at_ms: 30,
        latency_ms: 20,
        parallel_group: Some("serial".to_string()),
        transition_reason: Some("subagent_worker".to_string()),
    };
    let tool_events = build_runtime_event_tool_result_events(
        "chat-test",
        "turn-test",
        None,
        std::slice::from_ref(&report),
        None,
    );
    let events = build_runtime_event_subagent_tool_group_events_from_tool_results(
        "chat-test",
        "turn-test",
        std::slice::from_ref(&report),
        tool_events.as_slice(),
    );
    let replay = build_runtime_event_subagent_tool_group_events_from_tool_results(
        "chat-test",
        "turn-test",
        &[report],
        tool_events.as_slice(),
    );

    assert_eq!(events.len(), 1);
    assert_eq!(events, replay);
    let parsed: serde_json::Value =
        serde_json::to_value(&events[0]).expect("parse subagent tool group");
    assert_eq!(
        parsed.get("type").and_then(serde_json::Value::as_str),
        Some("SubagentToolGroup")
    );
    assert_eq!(
        parsed.get("status").and_then(serde_json::Value::as_str),
        Some("done")
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("toolGroupId"))
            .and_then(serde_json::Value::as_str),
        Some("tg-read-docs")
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("sourceEventIds"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        parsed
            .get("payload")
            .and_then(|item| item.get("details"))
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn final_event_includes_provider_prompt_cache_usage_when_available() {
    let generate_result = GenerateResult {
        content: "done".to_string(),
        tool_calls: vec![],
        reasoning_content: None,
        input_tokens: Some(100),
        total_tokens: Some(120),
        prompt_cache_hit_tokens: Some(90),
        prompt_cache_miss_tokens: Some(10),
    };

    let event = build_runtime_event_final_event(
        "chat-cache-usage",
        "turn-cache-usage",
        "done",
        Some(&generate_result),
    );
    let payload = serde_json::to_value(&event).expect("final event payload json");
    let usage = payload
        .get("meta")
        .and_then(|meta| meta.get("modelUsage"))
        .expect("model usage meta");

    assert_eq!(
        usage.get("promptCacheHitTokens").and_then(Value::as_i64),
        Some(90)
    );
    assert_eq!(
        usage.get("promptCacheMissTokens").and_then(Value::as_i64),
        Some(10)
    );
    assert_eq!(
        usage.get("promptCacheTotalTokens").and_then(Value::as_i64),
        Some(100)
    );
    assert_eq!(
        usage.get("promptCacheHitRate").and_then(Value::as_f64),
        Some(0.9)
    );
}
