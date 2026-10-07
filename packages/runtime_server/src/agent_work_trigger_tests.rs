use super::*;
use axum::{body::Bytes, extract::State, routing::post, Router};
use centaeris_core::session::{CommittedSessionRecord, SessionLogRecord};
use serde_json::Value;
use std::sync::Mutex;
use std::time::Duration;

fn start() -> AgentRunStart {
    let authorization: Value = serde_json::from_str(include_str!(
        "../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap();
    serde_json::from_value(json!({
        "schema":"workspace.agent_run.start.v2", "initialInput":{"type":"userMessage"}, "agentRunId":authorization["agentRunId"],
        "turnId":"trigger-turn", "prompt":"Dispatch", "agentInstructions":"",
        "modelContextTokens":1000, "modelMaxOutputTokens":100,
        "authorizationDigest":"synthetic", "authorizationSignature":"synthetic",
        "coordinationSessionId":authorization["sessionId"],
        "nativeCoordinationSessionId":authorization["sessionId"],
        "tailAction":{"type":"append"}, "authorization":authorization
    }))
    .unwrap()
}

fn record(start: &AgentRunStart, id: &str, name: &str, state: &str) -> CommittedSessionRecord {
    CommittedSessionRecord {
        sequence: 1,
        event: SessionLogRecord {
            schema_version: centaeris_core::session::SESSION_EVENT_SCHEMA_VERSION.into(),
            event_version: centaeris_core::session::SESSION_EVENT_VERSION,
            event_type: SessionRecordType::ToolResult,
            event_id: id.into(),
            session_id: start.authorization.session_id.clone(),
            agent_run_id: Some(start.agent_run_id.clone()),
            turn_id: Some(start.turn_id.clone()),
            created_at_ms: 1,
            payload: json!({"toolName":name, "resultState":state}),
        },
    }
}

#[tokio::test]
async fn mixed_receipt_uses_all_success_event_ids_and_ignores_other_facts() {
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let app = Router::new()
        .route(
            "/internal/agent-work/materialize",
            post(
                |State(captured): State<Arc<Mutex<Vec<Value>>>>, body: Bytes| async move {
                    captured
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    // The trigger needs only the status; no response schema or body is consumed.
                    (axum::http::StatusCode::CREATED, "unused response")
                },
            ),
        )
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let slots = Arc::new(Semaphore::new(2));
    let trigger = WorkCommitTrigger::new(tokio::runtime::Handle::current(), &url, "synthetic")
        .unwrap()
        .with_test_limits(
            reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            slots.clone(),
        );
    let start = start();
    let mut foreign_run = record(&start, "foreign-run", "dispatch_work", "successWithOutput");
    foreign_run.event.agent_run_id = Some("different-run".into());
    let mut foreign_session = record(
        &start,
        "foreign-session",
        "dispatch_work",
        "successWithOutput",
    );
    foreign_session.event.session_id = "different-session".into();
    let mut terminal = record(&start, "terminal", "dispatch_work", "successWithOutput");
    terminal.event.event_type = SessionRecordType::AgentRunCompleted;
    let receipt = SessionCommitReceipt {
        records: vec![
            record(
                &start,
                "committed-one",
                "dispatch_work",
                "successWithOutput",
            ),
            record(&start, "failed", "dispatch_work", "failed"),
            record(&start, "denied", "dispatch_work", "denied"),
            record(&start, "aborted", "dispatch_work", "aborted"),
            record(&start, "no-matches", "dispatch_work", "successNoMatches"),
            record(&start, "query", "get_work_request", "successWithOutput"),
            foreign_run,
            foreign_session,
            terminal,
            record(
                &start,
                "committed-two",
                "dispatch_work",
                "successWithOutput",
            ),
            record(&start, "empty", "dispatch_work", "successNoOutput"),
        ],
    };
    for kind in [None, Some("different-session".into())] {
        let mut excluded = start.clone();
        excluded.native_coordination_session_id = kind;
        trigger.after_commit(&excluded, &receipt);
    }
    trigger.after_commit(&start, &SessionCommitReceipt { records: vec![] });
    assert_eq!(slots.available_permits(), 2);
    trigger.after_commit(&start, &receipt);
    let _drained = tokio::time::timeout(Duration::from_secs(5), slots.acquire_many(2))
        .await
        .unwrap()
        .unwrap();
    let mut ids: Vec<String> = captured
        .lock()
        .unwrap()
        .iter()
        .map(|body| {
            assert_eq!(body["schema"], "workspace.agent_work.materialize.v1");
            assert_eq!(body.as_object().unwrap().len(), 2);
            body["sourceEventId"].as_str().unwrap().to_owned()
        })
        .collect();
    ids.sort();
    assert_eq!(ids, ["committed-one", "committed-two"]);
    server.abort();
}
