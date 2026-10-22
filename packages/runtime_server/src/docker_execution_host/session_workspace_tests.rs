//! Observable workspace API behavior shared by all execution providers.
use super::*;
use serde_json::{json, Value};
use std::net::TcpListener;

fn fixture(responses: Vec<(u16, Value)>) -> (DockerExecutionHostRunner, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 8192);
            }
            let headers = String::from_utf8(headers).unwrap();
            assert!(headers
                .starts_with("POST /internal/agent-runs/session-workspace/resolve HTTP/1.1\r\n"));
            let header = |name: &str| {
                headers.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case(name).then(|| value.trim())
                })
            };
            assert_eq!(header("x-internal-token"), Some("fixture-internal-token"));
            let length: usize = header("content-length").unwrap().parse().unwrap();
            assert!(length < 8192);
            let mut request = vec![0; length];
            socket.read_exact(&mut request).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&request).unwrap(),
                json!({
                    "schema": SESSION_WORKSPACE_RESOLVE_SCHEMA,
                    "jobId": "fixture-job", "leaseOwner": "fixture-owner",
                    "agentRunId": "agent_run_test",
                    "authorizationDigest": format!("sha256:{}", "a".repeat(64)),
                })
            );
            let body = serde_json::to_vec(&body).unwrap();
            write!(socket, "HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            socket.write_all(&body).unwrap();
        }
    });
    let mut host = super::tests::test_docker_execution_host_runner();
    host.api_url = format!("http://{address}");
    host.api_token = "fixture-internal-token".into();
    host.api_client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    (host, handle)
}

fn frozen() -> SessionWorkspace {
    SessionWorkspace {
        generation: 3,
        snapshot_sha256: String::new(),
        snapshot_size_bytes: 0,
        expanded_size_bytes: 0,
        file_count: 0,
    }
}

fn lease() -> SessionWorkspaceLease {
    SessionWorkspaceLease {
        job_id: "fixture-job".into(),
        lease_owner: "fixture-owner".into(),
    }
}

fn resolved(disposition: &str, workspace: &SessionWorkspace) -> Value {
    json!({"schema": SESSION_WORKSPACE_RESOLVED_SCHEMA,
        "disposition": disposition, "sessionWorkspace": workspace})
}

#[test]
fn workspace_api_resolve_binds_request_and_all_frozen_fields() {
    let frozen = frozen();
    let mut nonempty = frozen.clone();
    nonempty.snapshot_sha256 = format!("sha256:{}", "b".repeat(64));
    nonempty.snapshot_size_bytes = 32;
    nonempty.expanded_size_bytes = 8;
    nonempty.file_count = 1;
    let (host, server) = fixture(vec![
        (200, resolved("empty", &frozen)),
        (200, resolved("download", &nonempty)),
        (200, resolved("download", &nonempty)),
    ]);
    assert_eq!(
        host.resolve_session_workspace(&lease(), &frozen).unwrap(),
        SessionWorkspaceResolution::Empty
    );
    assert_eq!(
        host.resolve_session_workspace(&lease(), &nonempty).unwrap(),
        SessionWorkspaceResolution::Download
    );
    let mut mismatch = nonempty.clone();
    mismatch.file_count = 2;
    assert!(matches!(
        host.resolve_session_workspace(&lease(), &mismatch),
        Err(SessionWorkspaceApiError::Rejected(_))
    ));
    server.join().unwrap();
}

#[test]
fn workspace_api_resolve_rejects_unsupported_response_and_classifies_failures() {
    let frozen = frozen();
    let mut unknown = resolved("empty", &frozen);
    unknown["unexpected"] = json!(true);
    let mut wrong_schema = resolved("empty", &frozen);
    wrong_schema["schema"] = json!("runtime.session_workspace.resolved.v0");
    let (host, server) = fixture(vec![
        (503, json!({})),
        (403, json!({})),
        (200, unknown),
        (200, wrong_schema),
    ]);
    assert!(matches!(
        host.resolve_session_workspace(&lease(), &frozen),
        Err(SessionWorkspaceApiError::Unavailable(_))
    ));
    for _ in 0..3 {
        assert!(matches!(
            host.resolve_session_workspace(&lease(), &frozen),
            Err(SessionWorkspaceApiError::Rejected(_))
        ));
    }
    server.join().unwrap();
}

#[test]
fn workspace_api_reconciles_uncertain_commit_without_reupload_or_execution() {
    let frozen = frozen();
    let mut advanced = frozen.clone();
    advanced.generation += 1;
    let mut too_far = advanced.clone();
    too_far.generation += 1;
    let (host, server) = fixture(vec![
        (200, resolved("advanced", &advanced)),
        (200, resolved("empty", &frozen)),
        (503, json!({})),
        (200, resolved("advanced", &too_far)),
    ]);
    assert_eq!(
        host.reconcile_session_workspace_commit(&lease(), &frozen)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Accepted
    );
    assert!(matches!(
        host.reconcile_session_workspace_commit(&lease(), &frozen)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Rejected(_)
    ));
    assert_eq!(
        host.reconcile_session_workspace_commit(&lease(), &frozen)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Pending
    );
    assert!(matches!(
        host.reconcile_session_workspace_commit(&lease(), &frozen)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Rejected(_)
    ));
    server.join().unwrap();
}
