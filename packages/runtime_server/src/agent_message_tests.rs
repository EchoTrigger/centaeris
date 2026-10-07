use super::*;
use std::sync::{Arc, Mutex};

use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Router};
use centaeris_core::tool::ToolTurnBehavior;
use serde_json::Value;

#[path = "agent_message_runtime_tests.rs"]
mod runtime;

type CapturedRequests = Arc<Mutex<Vec<Value>>>;

fn authorization() -> WorkspaceAgentRunAuthorization {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap()
}

fn request(args: Value) -> DynamicToolProviderRequest {
    DynamicToolProviderRequest {
        tool_call_id: "message-one".into(),
        tool_name: "send_message".into(),
        args_json: args.to_string(),
        cancellation_probe: None,
        contract: DynamicToolRegistry::from_contracts(vec![message_tool_contract()])
            .unwrap()
            .find_contract("send_message")
            .unwrap(),
    }
}

fn args() -> Value {
    json!({"body": "  Complete text 中文 🔎\n", "session_refs": ["work-session"], "file_refs": ["input-report"]})
}

fn accepted(args: &Value) -> Value {
    let authority = authorization();
    json!({"schema": "workspace.agent_message.validated.v1", "agentId": authority.agent_id,
        "sessionId": authority.session_id, "agentRunId": authority.agent_run_id,
        "toolCallId": "message-one", "authorizationDigest": authority.digest().unwrap(),
        "inputDigest": serde_json::from_value::<MessageArgs>(args.clone()).unwrap().digest().unwrap()})
}

async fn api(
    response: Value,
    status: StatusCode,
) -> (String, CapturedRequests, tokio::task::JoinHandle<()>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/internal/agent-messages/validate",
            post(
                |State((response, status, captured)): State<(
                    Value,
                    StatusCode,
                    CapturedRequests,
                )>,
                 body: Bytes| async move {
                    captured
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    (
                        status,
                        [("content-type", "application/json")],
                        response.to_string(),
                    )
                },
            ),
        )
        .with_state((response, status, captured.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, captured, task)
}

#[test]
fn first_party_message_contract_continues_and_ordinary_tools_do_not_include_it() {
    let contract = message_tool_contract();
    assert_eq!(contract.name, "send_message");
    assert_eq!(contract.provider_id, PROVIDER_ID);
    assert_eq!(contract.turn_behavior, ToolTurnBehavior::ContinueTurn);
    assert!(contract.scopes.is_empty());
    assert!(!crate::workspace_tools::workspace_tool_contracts()
        .iter()
        .any(|tool| tool.name == "send_message"));
    assert_eq!(
        contract.input_schema["required"],
        json!(["body", "session_refs", "file_refs"])
    );
    assert!(contract.input_schema["properties"]
        .get("recipient_session_id")
        .is_none());
}

#[tokio::test]
async fn message_provider_preserves_complete_body_refs_and_infers_own_context() {
    let body = format!("  中文 🔎\n{}Tail  ", "complete evidence\n".repeat(3000));
    let input = json!({"body":body,"session_refs":["work-session"],"file_refs":["input-report"]});
    let (url, captured, server) = api(accepted(&input), StatusCode::OK).await;
    let auth = authorization();
    let provider = WorkspaceAgentMessageProvider::new(
        url,
        "synthetic-internal-token".into(),
        &auth,
        auth.digest().unwrap(),
    )
    .unwrap();
    let result = provider.execute(request(input.clone())).await.unwrap();
    assert!(!result.is_error);
    assert!(
        result.facts.is_empty(),
        "validation must never publish a second delivery fact"
    );
    let sent = captured.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["body"], input["body"]);
    assert_eq!(sent[0]["sessionRefs"], input["session_refs"]);
    assert_eq!(sent[0]["fileRefs"], input["file_refs"]);
    assert_eq!(sent[0]["coordinationSessionId"], auth.session_id);
    assert_eq!(sent[0]["agentRunId"], auth.agent_run_id);
    assert!(sent[0].get("recipientSessionId").is_none());
    server.abort();
}

#[tokio::test]
async fn message_provider_rejects_unknown_identity_recipient_and_invalid_utf8_budget_without_dispatch(
) {
    let (url, captured, server) = api(json!({"accepted":true}), StatusCode::OK).await;
    let auth = authorization();
    let provider = WorkspaceAgentMessageProvider::new(
        url,
        "synthetic-internal-token".into(),
        &auth,
        auth.digest().unwrap(),
    )
    .unwrap();
    let mut foreign = request(args());
    foreign.contract.provider_id = Some("test.messages".into());
    assert!(provider.execute(foreign).await.is_err());
    for input in [
        json!({"body":"x","session_refs":[],"file_refs":[],"recipient_session_id":"other"}),
        json!({"body":" ","session_refs":[],"file_refs":[]}),
        json!({"body":"中".repeat(21846),"session_refs":[],"file_refs":[]}),
        json!({"body":"x","session_refs":vec!["unknown";9],"file_refs":[]}),
        json!({"body":"x","session_refs":[],"file_refs":["/mnt/data/report.txt"]}),
    ] {
        assert!(provider.execute(request(input)).await.is_err());
    }
    assert!(captured.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn message_provider_rejects_fake_success_foreign_response_and_http_failure() {
    let input = args();
    let mut cases = vec![
        (json!({"accepted": true}), StatusCode::OK),
        (accepted(&input), StatusCode::FORBIDDEN),
        (accepted(&input), StatusCode::INTERNAL_SERVER_ERROR),
    ];
    for field in [
        "schema",
        "agentId",
        "sessionId",
        "agentRunId",
        "toolCallId",
        "authorizationDigest",
        "inputDigest",
    ] {
        let mut response = accepted(&input);
        response[field] = json!("foreign");
        cases.push((response, StatusCode::OK));
    }
    let mut extra = accepted(&input);
    extra["recipientSessionId"] = json!("foreign");
    cases.push((extra, StatusCode::OK));
    for (response, status) in cases {
        let (url, captured, server) = api(response, status).await;
        let auth = authorization();
        let provider = WorkspaceAgentMessageProvider::new(
            url,
            "synthetic-internal-token".into(),
            &auth,
            auth.digest().unwrap(),
        )
        .unwrap();
        assert!(provider.execute(request(input.clone())).await.is_err());
        assert_eq!(captured.lock().unwrap().len(), 1);
        server.abort();
    }
}
