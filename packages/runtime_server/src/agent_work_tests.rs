use super::*;
use std::sync::{Arc, Mutex};

use axum::{body::Bytes, extract::State, http::StatusCode, routing::post, Router};
use centaeris_core::tool::ToolTurnBehavior;
use serde_json::Value;

#[path = "agent_work_runtime_tests.rs"]
mod runtime;

#[path = "agent_work_return_runtime_tests.rs"]
mod returns;

type CapturedRequests = Arc<Mutex<Vec<Value>>>;

fn authorization() -> WorkspaceAgentRunAuthorization {
    serde_json::from_str(include_str!(
        "../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap()
}

fn request(args: Value) -> DynamicToolProviderRequest {
    DynamicToolProviderRequest {
        tool_call_id: "dispatch-one".into(),
        tool_name: "dispatch_work".into(),
        args_json: args.to_string(),
        cancellation_probe: None,
        contract: DynamicToolRegistry::from_contracts(vec![work_tool_contract()])
            .unwrap()
            .find_contract("dispatch_work")
            .unwrap(),
    }
}

fn args() -> Value {
    json!({"objective": "  Complete text 中文 🔎\n", "session_refs": ["work-session"], "file_refs": ["input-report"]})
}

fn accepted(args: &Value) -> Value {
    let authority = authorization();
    json!({"schema": "workspace.agent_work.validated.v1", "agentId": authority.agent_id,
        "sessionId": authority.session_id, "agentRunId": authority.agent_run_id,
        "toolCallId": "dispatch-one", "authorizationDigest": authority.digest().unwrap(),
        "inputDigest": serde_json::from_value::<WorkArgs>(args.clone()).unwrap().digest().unwrap()})
}

async fn api(
    response: Value,
    status: StatusCode,
) -> (String, CapturedRequests, tokio::task::JoinHandle<()>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let handler = post(
        |State((response, status, captured)): State<(Value, StatusCode, CapturedRequests)>,
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
    );
    let app = Router::new()
        .route("/internal/agent-work/validate", handler.clone())
        .route(
            "/internal/agent-work/returns/confirm/validate",
            handler.clone(),
        )
        .route("/internal/agent-work/query", handler)
        .with_state((response, status, captured.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, captured, task)
}

#[test]
fn first_party_work_contract_continues_and_ordinary_tools_do_not_include_it() {
    let contract = work_tool_contract();
    assert_eq!(contract.name, "dispatch_work");
    assert_eq!(contract.provider_id, PROVIDER_ID);
    assert_eq!(contract.turn_behavior, ToolTurnBehavior::ContinueTurn);
    assert!(contract.scopes.is_empty());
    assert!(!crate::workspace_tools::workspace_tool_contracts()
        .iter()
        .any(|tool| tool.name == "dispatch_work"));
    assert_eq!(
        contract.input_schema["required"],
        json!(["objective", "session_refs", "file_refs"])
    );
    assert!(contract.input_schema["properties"]
        .get("recipient_session_id")
        .is_none());
}

fn query_request() -> DynamicToolProviderRequest {
    DynamicToolProviderRequest {
        tool_call_id: "query-one".into(),
        tool_name: "get_work_request".into(),
        args_json:
            json!({"source_agent_run_id":"source-run", "source_tool_call_id":"dispatch-one"})
                .to_string(),
        cancellation_probe: None,
        contract: DynamicToolRegistry::from_contracts(vec![work_query_tool_contract()])
            .unwrap()
            .find_contract("get_work_request")
            .unwrap(),
    }
}

fn queried(status: &str) -> Value {
    let auth = authorization();
    let mut record = json!({"schema":"workspace.agent_work.query_result.v1", "agentId":auth.agent_id,
        "sessionId":auth.session_id, "agentRunId":auth.agent_run_id, "toolCallId":"query-one",
        "authorizationDigest":auth.digest().unwrap(), "sourceAgentRunId":"source-run",
        "sourceToolCallId":"dispatch-one", "status":status, "source":null, "operation":null, "work":null});
    if status != "notRecorded" {
        record["source"] = json!({"sourceAgentRunId":"source-run", "sourceTurnId":"resolved-core-turn",
            "sourceToolCallId":"dispatch-one", "sourceEventId":"source-result", "projectsToAgentRunStream":false});
    }
    if status == "admitted" {
        record["operation"] = json!({"operationId":"accepted-op", "command":"submitMessage", "status":"accepted",
            "sessionId":"child-session", "agentRunId":"child-run", "turnId":"child-turn"});
        record["work"] =
            json!({"available":true, "agentRunStatus":"failed", "returns":[], "output":null});
    }
    record
}

#[test]
fn query_contract_uses_exact_model_arguments_and_keeps_dispatch_digest() {
    let contract = work_query_tool_contract();
    assert_eq!(contract.name, "get_work_request");
    assert_eq!(contract.provider_id, PROVIDER_ID);
    assert_eq!(contract.turn_behavior, ToolTurnBehavior::ContinueTurn);
    assert!(contract.concurrency_safe);
    assert!(contract.scopes.is_empty());
    assert_eq!(
        contract.input_schema["required"],
        json!(["source_agent_run_id", "source_tool_call_id"])
    );
    assert_eq!(contract.input_schema["additionalProperties"], false);
    assert!(!crate::workspace_tools::workspace_tool_contracts()
        .iter()
        .any(|tool| tool.name == "get_work_request"));
    let before = DynamicToolRegistry::from_contracts(vec![work_tool_contract()]).unwrap();
    let after = DynamicToolRegistry::from_contracts(vec![work_tool_contract(), contract]).unwrap();
    assert_eq!(
        before.find_contract("dispatch_work").unwrap(),
        after.find_contract("dispatch_work").unwrap()
    );
}

fn confirmation_authorization() -> WorkspaceAgentRunAuthorization {
    let mut auth = authorization();
    auth.user_id = "42".into();
    auth
}

fn confirm_request(args: Value) -> DynamicToolProviderRequest {
    DynamicToolProviderRequest {
        tool_call_id: "confirm-one".into(),
        tool_name: "confirm_work_return".into(),
        args_json: args.to_string(),
        cancellation_probe: None,
        contract: DynamicToolRegistry::from_contracts(vec![work_confirmation_tool_contract()])
            .unwrap()
            .find_contract("confirm_work_return")
            .unwrap(),
    }
}

fn confirmation_args() -> Value {
    json!({"notice_id":"work-return:notice", "attempt_id":"accepted-attempt"})
}

fn confirmation_validated() -> Value {
    let auth = confirmation_authorization();
    json!({"schema":"workspace.agent_work.return_confirm.validated.v1", "agentId":auth.agent_id,
        "sessionId":auth.session_id, "agentRunId":auth.agent_run_id, "toolCallId":"confirm-one",
        "authorizationDigest":auth.digest().unwrap(), "noticeId":"work-return:notice", "attemptId":"accepted-attempt",
        "inputDigest":serde_json::from_value::<WorkConfirmationArgs>(confirmation_args()).unwrap().digest().unwrap(),
        "noticeIdentity":{"agentId":auth.agent_id,"coordinationSessionId":auth.session_id,"userId":42,"workspaceId":auth.workspace_id,
            "sourceAgentRunId":"original-source", "sourceTurnId":"original-turn", "sourceToolCallId":"dispatch-one",
            "sourceEventId":"source-result", "operationId":"original-work-op", "workSessionId":"child-session",
            "workAgentRunId":"child-run", "factKind":"sessionTerminal", "factRef":"terminal-fact", "factDigest":"sha256:fact"}})
}

#[test]
fn confirmation_contract_has_two_exact_arguments_and_continue_turn() {
    let contract = work_confirmation_tool_contract();
    assert_eq!(contract.name, "confirm_work_return");
    assert_eq!(contract.provider_id, PROVIDER_ID);
    assert_eq!(contract.turn_behavior, ToolTurnBehavior::ContinueTurn);
    assert!(!contract.concurrency_safe);
    assert!(contract.scopes.is_empty());
    assert_eq!(
        contract.input_schema["required"],
        json!(["notice_id", "attempt_id"])
    );
    assert_eq!(contract.input_schema["additionalProperties"], false);
    assert!(!crate::workspace_tools::workspace_tool_contracts()
        .iter()
        .any(|tool| tool.name == "confirm_work_return"));
}

#[tokio::test]
async fn confirmation_provider_forwards_owned_identity_without_handling_facts() {
    let expected = confirmation_validated();
    let (url, captured, server) = api(expected.clone(), StatusCode::OK).await;
    let auth = confirmation_authorization();
    let provider = WorkspaceAgentWorkProvider::new(
        url,
        "synthetic-token".into(),
        &auth,
        auth.digest().unwrap(),
    )
    .unwrap();
    let result = provider
        .execute(confirm_request(confirmation_args()))
        .await
        .unwrap();
    assert_eq!(result.details, expected);
    assert_eq!(
        serde_json::from_str::<Value>(&result.content).unwrap(),
        expected
    );
    assert!(!result.is_error && result.facts.is_empty() && result.transition_reason.is_none());
    assert_eq!(
        captured.lock().unwrap().as_slice(),
        &[
            json!({"schema":"workspace.agent_work.return_confirm.validate.v1",
        "agentRunId":auth.agent_run_id,"authorizationDigest":auth.digest().unwrap(),"coordinationSessionId":auth.session_id,
        "toolCallId":"confirm-one","noticeId":"work-return:notice","attemptId":"accepted-attempt"})
        ]
    );
    server.abort();
}

#[tokio::test]
async fn confirmation_provider_rejects_aliases_paths_and_unknown_fields_before_rpc() {
    let (url, captured, server) = api(confirmation_validated(), StatusCode::OK).await;
    let auth = confirmation_authorization();
    let provider = WorkspaceAgentWorkProvider::new(
        url,
        "synthetic-token".into(),
        &auth,
        auth.digest().unwrap(),
    )
    .unwrap();
    for args in [
        json!({"noticeId":"notice","attemptId":"attempt"}),
        json!({"notice_id":"", "attempt_id":"attempt"}),
        json!({"notice_id":"notice","attempt_id":"/path"}),
        json!({"notice_id":"notice","attempt_id":"attempt","agent_run_id":"other"}),
    ] {
        assert!(provider.execute(confirm_request(args)).await.is_err());
    }
    assert!(captured.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn confirmation_provider_rejects_wrong_run_notice_attempt_source_and_extra_response_fields() {
    for field in [
        "agentRunId",
        "noticeId",
        "attemptId",
        "inputDigest",
        "authorizationDigest",
        "toolCallId",
        "extra",
    ] {
        let mut response = confirmation_validated();
        response[field] = json!("wrong");
        let (url, _, server) = api(response, StatusCode::OK).await;
        let auth = confirmation_authorization();
        let provider = WorkspaceAgentWorkProvider::new(
            url,
            "synthetic-token".into(),
            &auth,
            auth.digest().unwrap(),
        )
        .unwrap();
        assert!(
            provider
                .execute(confirm_request(confirmation_args()))
                .await
                .is_err(),
            "{field}"
        );
        server.abort();
    }
    let mut response = confirmation_validated();
    response["noticeIdentity"]["coordinationSessionId"] = json!("other-session");
    let (url, _, server) = api(response, StatusCode::OK).await;
    let auth = confirmation_authorization();
    let provider = WorkspaceAgentWorkProvider::new(
        url,
        "synthetic-token".into(),
        &auth,
        auth.digest().unwrap(),
    )
    .unwrap();
    assert!(provider
        .execute(confirm_request(confirmation_args()))
        .await
        .is_err());
    server.abort();
}

#[tokio::test]
async fn query_provider_returns_source_and_admission_without_delivery_facts() {
    for status in ["notRecorded", "pending", "admitted"] {
        let expected = queried(status);
        let (url, captured, server) = api(expected.clone(), StatusCode::OK).await;
        let auth = authorization();
        let provider = WorkspaceAgentWorkProvider::new(
            url,
            "synthetic-token".into(),
            &auth,
            auth.digest().unwrap(),
        )
        .unwrap();
        let result = provider.execute(query_request()).await.unwrap();
        assert_eq!(result.details, expected);
        assert_eq!(
            serde_json::from_str::<Value>(&result.content).unwrap(),
            expected
        );
        assert!(result.facts.is_empty());
        assert!(result.transition_reason.is_none());
        assert!(!result.is_error);
        let sent = captured.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0],
            json!({"schema":"workspace.agent_work.query.v1", "agentRunId":auth.agent_run_id,
            "authorizationDigest":auth.digest().unwrap(), "coordinationSessionId":auth.session_id,
            "toolCallId":"query-one", "sourceAgentRunId":"source-run", "sourceToolCallId":"dispatch-one"})
        );
        server.abort();
    }
}

#[tokio::test]
async fn query_provider_reads_owned_final_work_output_as_untrusted_content() {
    let mut expected = queried("admitted");
    expected["work"]["agentRunStatus"] = json!("completed");
    expected["work"]["output"] = json!({"schema":"workspace.agent_work.output.v1",
        "source":"untrustedWorkOutput", "sessionId":"child-session", "agentRunId":"child-run",
        "eventId":"retained-final", "throughSequence":42, "body":"Real delegated result"});
    let (url, _, server) = api(expected.clone(), StatusCode::OK).await;
    let auth = authorization();
    let provider = WorkspaceAgentWorkProvider::new(
        url,
        "synthetic-token".into(),
        &auth,
        auth.digest().unwrap(),
    )
    .unwrap();
    let result = provider
        .execute(query_request())
        .await
        .expect("get_work_request must return the retained result");
    assert_eq!(
        serde_json::from_str::<Value>(&result.content).unwrap(),
        expected
    );
    assert!(result.facts.is_empty());
    server.abort();
}

#[tokio::test]
async fn query_provider_rejects_missing_or_foreign_work_output() {
    let mut valid = queried("admitted");
    valid["work"]["output"] = json!({"schema":"workspace.agent_work.output.v1",
        "source":"untrustedWorkOutput", "sessionId":"child-session", "agentRunId":"child-run",
        "eventId":"retained-final", "throughSequence":42, "body":"Real delegated result"});
    let mut cases = Vec::new();
    let mut missing = valid.clone();
    missing["work"].as_object_mut().unwrap().remove("output");
    cases.push(missing);
    for field in [
        "schema",
        "source",
        "sessionId",
        "agentRunId",
        "eventId",
        "body",
    ] {
        let mut forged = valid.clone();
        forged["work"]["output"][field] = json!(if ["eventId", "body"].contains(&field) {
            ""
        } else {
            "foreign"
        });
        cases.push(forged);
    }
    let mut no_boundary = valid.clone();
    no_boundary["work"]["output"]["throughSequence"] = json!(0);
    cases.push(no_boundary);
    let mut unknown = valid;
    unknown["work"]["output"]["extra"] = json!(true);
    cases.push(unknown);
    for response in cases {
        let (url, _, server) = api(response, StatusCode::OK).await;
        let auth = authorization();
        let provider = WorkspaceAgentWorkProvider::new(
            url,
            "synthetic-token".into(),
            &auth,
            auth.digest().unwrap(),
        )
        .unwrap();
        assert!(provider.execute(query_request()).await.is_err());
        server.abort();
    }
}

#[tokio::test]
async fn query_provider_rejects_forged_identity_incoherent_state_and_extra_arguments() {
    let mut cases = vec![(queried("pending"), StatusCode::FORBIDDEN)];
    let mut missing = queried("notRecorded");
    missing.as_object_mut().unwrap().remove("source");
    cases.push((missing, StatusCode::OK));
    for field in [
        "schema",
        "agentId",
        "sessionId",
        "agentRunId",
        "toolCallId",
        "authorizationDigest",
        "sourceAgentRunId",
        "sourceToolCallId",
        "status",
    ] {
        let mut response = queried("pending");
        response[field] = json!("foreign");
        cases.push((response, StatusCode::OK));
    }
    for field in ["sourceAgentRunId", "sourceToolCallId"] {
        let mut response = queried("pending");
        response["source"][field] = json!("foreign");
        cases.push((response, StatusCode::OK));
    }
    let mut incoherent = queried("notRecorded");
    incoherent["source"] = queried("pending")["source"].clone();
    cases.push((incoherent, StatusCode::OK));
    let mut unavailable = queried("admitted");
    unavailable["work"]["available"] = json!(false);
    cases.push((unavailable, StatusCode::OK));
    let mut extra = queried("pending");
    extra["source"]["recipientSessionId"] = json!("foreign");
    cases.push((extra, StatusCode::OK));
    for (response, status) in cases {
        let (url, _, server) = api(response, status).await;
        let auth = authorization();
        let provider = WorkspaceAgentWorkProvider::new(
            url,
            "synthetic-token".into(),
            &auth,
            auth.digest().unwrap(),
        )
        .unwrap();
        assert!(provider.execute(query_request()).await.is_err());
        server.abort();
    }
    let (url, captured, server) = api(queried("pending"), StatusCode::OK).await;
    let auth = authorization();
    let provider = WorkspaceAgentWorkProvider::new(
        url,
        "synthetic-token".into(),
        &auth,
        auth.digest().unwrap(),
    )
    .unwrap();
    for args in [
        json!({"source_agent_run_id":"source-run", "source_tool_call_id":"dispatch-one", "source_turn_id":"guess"}),
        json!({"source_agent_run_id":"source-run", "source_tool_call_id":"/path"}),
    ] {
        let mut request = query_request();
        request.args_json = args.to_string();
        assert!(provider.execute(request).await.is_err());
    }
    assert!(captured.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn work_provider_preserves_complete_body_refs_and_infers_own_context() {
    let body = format!("  中文 🔎\n{}Tail  ", "complete evidence\n".repeat(3000));
    let input =
        json!({"objective":body,"session_refs":["work-session"],"file_refs":["input-report"]});
    let (url, captured, server) = api(accepted(&input), StatusCode::OK).await;
    let auth = authorization();
    let provider = WorkspaceAgentWorkProvider::new(
        url,
        "synthetic-internal-token".into(),
        &auth,
        auth.digest().unwrap(),
    )
    .unwrap();
    let result = provider.execute(request(input.clone())).await.unwrap();
    assert!(!result.is_error);
    assert!(result
        .content
        .contains("awaiting Session commit, not admitted"));
    assert!(
        result.facts.is_empty(),
        "validation must never publish a second delivery fact"
    );
    let sent = captured.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["objective"], input["objective"]);
    assert_eq!(sent[0]["sessionRefs"], input["session_refs"]);
    assert_eq!(sent[0]["fileRefs"], input["file_refs"]);
    assert_eq!(sent[0]["coordinationSessionId"], auth.session_id);
    assert_eq!(sent[0]["agentRunId"], auth.agent_run_id);
    assert!(sent[0].get("recipientSessionId").is_none());
    server.abort();
}

#[tokio::test]
async fn work_provider_rejects_unknown_identity_recipient_and_invalid_utf8_budget_without_dispatch()
{
    let (url, captured, server) = api(json!({"accepted":true}), StatusCode::OK).await;
    let auth = authorization();
    let provider = WorkspaceAgentWorkProvider::new(
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
        json!({"objective":"x","session_refs":[],"file_refs":[],"recipient_session_id":"other"}),
        json!({"objective":" ","session_refs":[],"file_refs":[]}),
        json!({"objective":"中".repeat(21846),"session_refs":[],"file_refs":[]}),
        json!({"objective":"x","session_refs":vec!["unknown";9],"file_refs":[]}),
        json!({"objective":"x","session_refs":[],"file_refs":["/mnt/data/report.txt"]}),
    ] {
        assert!(provider.execute(request(input)).await.is_err());
    }
    assert!(captured.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn work_provider_rejects_fake_success_foreign_response_and_http_failure() {
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
        let provider = WorkspaceAgentWorkProvider::new(
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
