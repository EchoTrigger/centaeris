use super::*;
use crate::observations::WorkspaceProtection;
use crate::session_workspace::snapshot::capture_snapshot_artifact_observed;
use crate::session_workspace::SESSION_WORKSPACE_RESOLVED_SCHEMA;
use hosted_execution::protocol::{
    SandboxWorkspaceSnapshotFile, SandboxWorkspaceSnapshotManifest,
    SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Instant;

fn snapshot() -> (ImmutableWorkspaceSnapshot, Vec<u8>) {
    let bytes = b"\0\xffsaved-result";
    let manifest = SandboxWorkspaceSnapshotManifest {
        schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
        files: vec![SandboxWorkspaceSnapshotFile {
            path: "result.bin".into(),
            size_bytes: bytes.len() as u64,
            sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
            executable: false,
        }],
    };
    let metadata = serde_json::to_vec(&manifest).unwrap();
    let mut frame = (metadata.len() as u32).to_be_bytes().to_vec();
    frame.extend(metadata);
    frame.extend(bytes);
    let artifact = capture_snapshot_artifact_observed(
        &mut frame.as_slice(),
        frame.len() as u64,
        WorkspaceProtection::new(),
    )
    .unwrap();
    (artifact, frame)
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
fn candidate(artifact: &ImmutableWorkspaceSnapshot) -> SessionWorkspace {
    let d = &artifact.descriptor;
    SessionWorkspace {
        generation: 4,
        snapshot_sha256: d.sha256.clone(),
        snapshot_size_bytes: d.size_bytes,
        expanded_size_bytes: d.expanded_size_bytes,
        file_count: d.file_count,
    }
}
fn lease() -> SessionWorkspaceLease {
    SessionWorkspaceLease {
        job_id: "job-fixture".into(),
        lease_owner: "owner-fixture".into(),
    }
}
fn request(socket: &mut TcpStream) -> (String, Vec<u8>) {
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
        .lines()
        .any(|line| line.eq_ignore_ascii_case("x-internal-token: fixture-only")));
    let length: usize = headers
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().unwrap())
        })
        .unwrap();
    assert!(length < 16384);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).unwrap();
    (headers.lines().next().unwrap().to_string(), body)
}
fn accept(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((socket, _)) => return socket,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "missing expected workspace HTTP request"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("{error}"),
        }
    }
}
fn respond(socket: &mut TcpStream, status: u16, body: Value) {
    let body = serde_json::to_vec(&body).unwrap();
    write!(socket,"HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
    socket.write_all(&body).unwrap();
}
fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
}
fn api<'a>(client: &'a reqwest::blocking::Client, url: &'a str) -> SessionWorkspaceApi<'a> {
    SessionWorkspaceApi {
        client,
        api_url: url,
        api_token: "fixture-only",
        agent_run_id: "run-fixture",
        authorization_digest: "sha256:fixture",
    }
}

#[test]
fn workspace_commit_preserves_binary_upload_and_lease_binding() {
    let (artifact, wire) = snapshot();
    let expected = candidate(&artifact);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let returned = expected.clone();
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        let (path, body) = request(&mut socket);
        assert_eq!(
            path,
            "POST /internal/agent-runs/session-workspace/commit HTTP/1.1"
        );
        let length = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
        assert_eq!(&body[4 + length..], wire);
        assert_eq!(
            serde_json::from_slice::<Value>(&body[4..4 + length]).unwrap(),
            json!({"schema":SESSION_WORKSPACE_COMMIT_SCHEMA,
            "jobId":"job-fixture","leaseOwner":"owner-fixture","agentRunId":"run-fixture","authorizationDigest":"sha256:fixture",
            "snapshotSha256":returned.snapshot_sha256,"snapshotSizeBytes":returned.snapshot_size_bytes,"expandedSizeBytes":returned.expanded_size_bytes,"fileCount":returned.file_count})
        );
        respond(
            &mut socket,
            200,
            json!({"schema":SESSION_WORKSPACE_COMMIT_RESULT_SCHEMA,"disposition":"committed","sessionWorkspace":returned}),
        );
    });
    assert_eq!(
        api(&client(), &url)
            .commit_snapshot(&lease(), &frozen(), artifact)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Accepted
    );
    server.join().unwrap();
}

#[test]
fn workspace_commit_lost_reply_only_resolves_and_keeps_unresolved_protection_pending() {
    for lose_reply in [false, true] {
        let (artifact, _) = snapshot();
        let expected = candidate(&artifact);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut first = accept(&listener);
            assert!(request(&mut first).0.contains("/session-workspace/commit "));
            if !lose_reply {
                respond(&mut first, 503, json!({}));
            }
            drop(first);
            let mut second = accept(&listener);
            let (path, body) = request(&mut second);
            assert!(path.contains("/session-workspace/resolve "));
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap()["jobId"],
                "job-fixture"
            );
            if lose_reply {
                respond(&mut second, 503, json!({}));
            } else {
                respond(
                    &mut second,
                    200,
                    json!({"schema":SESSION_WORKSPACE_RESOLVED_SCHEMA,"disposition":"advanced","sessionWorkspace":expected}),
                );
            }
        });
        let result = api(&client(), &url)
            .commit_snapshot(&lease(), &frozen(), artifact)
            .unwrap();
        assert_eq!(
            result,
            if lose_reply {
                SessionWorkspaceCommitOutcome::Pending
            } else {
                SessionWorkspaceCommitOutcome::Accepted
            }
        );
        server.join().unwrap();
    }
}

#[test]
fn workspace_commit_rejects_mismatched_response_and_skips_unchanged_upload() {
    let (artifact, _) = snapshot();
    let mut wrong = candidate(&artifact);
    wrong.file_count += 1;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let mut socket = accept(&listener);
        request(&mut socket);
        respond(
            &mut socket,
            200,
            json!({"schema":SESSION_WORKSPACE_COMMIT_RESULT_SCHEMA,"disposition":"idempotent","sessionWorkspace":wrong}),
        );
    });
    assert!(matches!(
        api(&client(), &url)
            .commit_snapshot(&lease(), &frozen(), artifact)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Rejected(_)
    ));
    server.join().unwrap();
    let (artifact, _) = snapshot();
    let d = &artifact.descriptor;
    let unchanged = SessionWorkspace {
        generation: 3,
        snapshot_sha256: d.sha256.clone(),
        snapshot_size_bytes: d.size_bytes,
        expanded_size_bytes: d.expanded_size_bytes,
        file_count: d.file_count,
    };
    assert_eq!(
        api(&client(), "http://127.0.0.1:9")
            .commit_snapshot(&lease(), &unchanged, artifact)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Unchanged
    );
}
