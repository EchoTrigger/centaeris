use super::*;
use hosted_execution::protocol::{
    SandboxSnapshotCaptureResponse, SandboxWorkspaceSnapshotFile, SandboxWorkspaceSnapshotManifest,
    SnapshotCaptureDisposition, SANDBOX_SNAPSHOT_CAPTURE_SCHEMA, SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::Duration;

const OVERHEAD: u64 = 64 * 1024;

fn profile(limit: u64) -> EnvironmentProfile {
    let mut value: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
    )))
    .unwrap();
    value["workspaceRoot"] = json!("/mnt/data");
    value["user"] = json!("agent");
    value["inputBudgetBytes"] = json!(limit);
    serde_json::from_value(value).unwrap()
}
fn empty() -> SessionWorkspace {
    SessionWorkspace {
        generation: 0,
        snapshot_sha256: String::new(),
        snapshot_size_bytes: 0,
        expanded_size_bytes: 0,
        file_count: 0,
    }
}
fn lease(run: &str) -> SessionWorkspaceLease {
    SessionWorkspaceLease {
        job_id: format!("agent_run.lifecycle:{run}"),
        lease_owner: format!("owner-{run}"),
    }
}
fn digest(run: &str) -> String {
    format!(
        "sha256:{}",
        if run == "run-a" { "a" } else { "b" }.repeat(64)
    )
}
fn service(url: &str, run: &str, port: Arc<Helper>, limit: u64) -> E2bSessionWorkspace {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    E2bSessionWorkspace::new(
        profile(limit),
        SessionWorkspaceApi {
            client: &client,
            api_url: url,
            api_token: "fixture-only",
            agent_run_id: run,
            authorization_digest: &digest(run),
        },
        port,
    )
    .unwrap()
}
fn frame(data: &[u8]) -> (Vec<u8>, SandboxWorkspaceSnapshotManifest) {
    let manifest = SandboxWorkspaceSnapshotManifest {
        schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
        files: vec![SandboxWorkspaceSnapshotFile {
            path: "result.bin".into(),
            size_bytes: data.len() as u64,
            sha256: format!("sha256:{:x}", Sha256::digest(data)),
            executable: false,
        }],
    };
    let metadata = serde_json::to_vec(&manifest).unwrap();
    let mut wire = (metadata.len() as u32).to_be_bytes().to_vec();
    wire.extend(metadata);
    wire.extend(data);
    (wire, manifest)
}
fn frozen(wire: &[u8], size: usize) -> SessionWorkspace {
    SessionWorkspace {
        generation: 1,
        snapshot_sha256: format!("sha256:{:x}", Sha256::digest(wire)),
        snapshot_size_bytes: wire.len() as u64,
        expanded_size_bytes: size as u64,
        file_count: 1,
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    Busy,
    BeginUnknown,
    QuiesceUnknown,
    CaptureUnknown,
    RestoreUnknown,
    WrongCaptureId,
    Unchanged,
    CorruptFrame,
}
struct Helper {
    wire: Vec<u8>,
    manifest: SandboxWorkspaceSnapshotManifest,
    fault: Fault,
    held: AtomicBool,
    events: Mutex<Vec<&'static str>>,
    restored: Mutex<Vec<Vec<u8>>>,
}
impl Helper {
    fn new(data: &[u8], fault: Fault) -> Arc<Self> {
        let (wire, manifest) = frame(data);
        Arc::new(Self {
            wire,
            manifest,
            fault,
            held: AtomicBool::new(false),
            events: Mutex::new(Vec::new()),
            restored: Mutex::new(Vec::new()),
        })
    }
}
struct Held<'a>(&'a Helper);
impl Drop for Held<'_> {
    fn drop(&mut self) {
        self.0.events.lock().unwrap().push("release");
        self.0.held.store(false, Ordering::SeqCst);
    }
}
impl WorkspaceHelperPort for Helper {
    fn try_exclusive(&self) -> Result<Option<Box<dyn WorkspaceHelperSession + '_>>, E2bHostError> {
        self.events.lock().unwrap().push("exclusive");
        if self.fault == Fault::Busy {
            return Ok(None);
        }
        if self.fault == Fault::BeginUnknown {
            return Err(E2bHostError::Unknown("fixture_helper_unknown"));
        }
        assert!(!self.held.swap(true, Ordering::SeqCst));
        Ok(Some(Box::new(Held(self))))
    }
}
impl WorkspaceHelperSession for Held<'_> {
    fn require_quiescent(&self) -> Result<(), E2bHostError> {
        assert!(self.0.held.load(Ordering::SeqCst));
        self.0.events.lock().unwrap().push("requireQuiescent");
        Ok(())
    }
    fn quiesce(&self) -> Result<(), E2bHostError> {
        assert!(self.0.held.load(Ordering::SeqCst));
        self.0.events.lock().unwrap().push("quiesce");
        if self.0.fault == Fault::QuiesceUnknown {
            Err(E2bHostError::Unknown("fixture_quiescence_unknown"))
        } else {
            Ok(())
        }
    }
    fn restore_snapshot(
        &self,
        input: &mut File,
        descriptor: &WorkspaceSnapshotDescriptor,
    ) -> Result<(), E2bHostError> {
        assert!(self.0.held.load(Ordering::SeqCst));
        self.0.events.lock().unwrap().push("restore");
        if self.0.fault == Fault::RestoreUnknown {
            return Err(E2bHostError::Unknown("fixture_restore_unknown"));
        }
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).unwrap();
        assert_eq!(descriptor.size_bytes, bytes.len() as u64);
        assert_eq!(
            descriptor.sha256,
            format!("sha256:{:x}", Sha256::digest(&bytes))
        );
        self.0.restored.lock().unwrap().push(bytes);
        Ok(())
    }
    fn capture_snapshot(
        &self,
        request: &SandboxSnapshotCaptureRequest,
        output_limit: u64,
    ) -> Result<File, E2bHostError> {
        assert!(self.0.held.load(Ordering::SeqCst));
        self.0.events.lock().unwrap().push("capture");
        assert!(
            request.baseline.is_none(),
            "terminal capture has no unverified manifest baseline"
        );
        assert!(output_limit <= 128 * 1024 * 1024);
        if self.0.fault == Fault::CaptureUnknown {
            return Err(E2bHostError::Unknown("fixture_capture_unknown"));
        }
        let response = SandboxSnapshotCaptureResponse {
            schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
            capture_id: if self.0.fault == Fault::WrongCaptureId {
                "wrong-capture".into()
            } else {
                request.capture_id.clone()
            },
            disposition: if self.0.fault == Fault::Unchanged {
                SnapshotCaptureDisposition::Unchanged
            } else {
                SnapshotCaptureDisposition::Snapshot
            },
            baseline_snapshot_sha256: None,
            manifest_sha256: format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&self.0.manifest).unwrap())
            ),
        };
        let metadata = serde_json::to_vec(&response).unwrap();
        let mut bytes = (metadata.len() as u32).to_be_bytes().to_vec();
        bytes.extend(metadata);
        let mut wire = self.0.wire.clone();
        if self.0.fault == Fault::CorruptFrame {
            *wire.last_mut().unwrap() ^= 1;
        }
        bytes.extend(wire);
        let mut output = tempfile::tempfile().unwrap();
        output.write_all(&bytes).unwrap();
        output.rewind().unwrap();
        Ok(output)
    }
}

struct HttpRequest {
    path: String,
    body: Vec<u8>,
}
struct Reply {
    status: u16,
    body: Vec<u8>,
    hash: Option<String>,
    length: Option<u64>,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            body: serde_json::to_vec(&value).unwrap(),
            hash: None,
            length: None,
        }
    }
}
struct Server {
    url: String,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<Vec<HttpRequest>>>,
}
impl Server {
    fn new(mut handler: impl FnMut(&HttpRequest) -> Option<Reply> + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (stop, done) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            loop {
                if done.try_recv().is_ok() {
                    break;
                }
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let request = read_request(&mut socket);
                        if let Some(reply) = handler(&request) {
                            let mut encoded = Vec::new();
                            write!(encoded,"HTTP/1.1 {} fixture\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n",reply.status,reply.length.unwrap_or(reply.body.len() as u64)).unwrap();
                            if let Some(hash) = reply.hash {
                                write!(encoded, "X-Content-Sha256: {hash}\r\n").unwrap();
                            }
                            encoded.extend_from_slice(b"\r\n");
                            encoded.extend_from_slice(&reply.body);
                            socket.write_all(&encoded).unwrap();
                        }
                        requests.push(request);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::park_timeout(Duration::from_millis(5))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
            requests
        });
        Self {
            url,
            stop: Some(stop),
            thread: Some(worker),
        }
    }
    fn finish(mut self) -> Vec<HttpRequest> {
        self.stop.take().unwrap().send(()).ok();
        self.thread.take().unwrap().join().unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        if let Some(worker) = self.thread.take() {
            if !thread::panicking() {
                worker.join().unwrap();
            }
        }
    }
}
fn read_request(socket: &mut TcpStream) -> HttpRequest {
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
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
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    assert!(length < 1024 * 1024);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).unwrap();
    HttpRequest {
        path: headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .into(),
        body,
    }
}
fn query(request: &HttpRequest, schema: &str, run: &str) {
    assert_eq!(
        serde_json::from_slice::<Value>(&request.body).unwrap(),
        json!({"schema":schema,"jobId":lease(run).job_id,
        "leaseOwner":lease(run).lease_owner,"agentRunId":run,"authorizationDigest":digest(run)})
    );
}
fn resolved(value: &SessionWorkspace, disposition: &str) -> Reply {
    Reply::json(
        json!({"schema":"runtime.session_workspace.resolved.v1","disposition":disposition,"sessionWorkspace":value}),
    )
}

#[test]
fn one_session_new_run_restores_previous_committed_binary_bytes_over_actual_http() {
    let data = b"\0\xffworkspace-result";
    let helper_a = Helper::new(data, Fault::None);
    let expected = frozen(&helper_a.wire, data.len());
    let wire = helper_a.wire.clone();
    let current = expected.clone();
    let server = Server::new(move |request| match request.path.as_str() {
        "/internal/agent-runs/session-workspace/resolve" => {
            let run = serde_json::from_slice::<Value>(&request.body).unwrap()["agentRunId"]
                .as_str()
                .unwrap()
                .to_string();
            query(request, "runtime.session_workspace.resolve.v1", &run);
            Some(if run == "run-a" {
                resolved(&empty(), "empty")
            } else {
                resolved(&current, "download")
            })
        }
        "/internal/agent-runs/session-workspace/commit" => {
            let length = u32::from_be_bytes(request.body[..4].try_into().unwrap()) as usize;
            let metadata: Value = serde_json::from_slice(&request.body[4..4 + length]).unwrap();
            assert_eq!(metadata["agentRunId"], "run-a");
            assert_eq!(metadata["authorizationDigest"], digest("run-a"));
            assert_eq!(&request.body[4 + length..], wire);
            Some(Reply::json(
                json!({"schema":"runtime.session_workspace.commit.result.v1","disposition":"committed","sessionWorkspace":current}),
            ))
        }
        "/internal/agent-runs/session-workspace/download" => {
            query(request, "runtime.session_workspace.download.v1", "run-b");
            Some(Reply {
                status: 200,
                body: wire.clone(),
                hash: Some(current.snapshot_sha256.clone()),
                length: None,
            })
        }
        _ => panic!("unexpected workspace request"),
    });
    assert_eq!(
        service(&server.url, "run-a", helper_a.clone(), 64 * 1024 * 1024)
            .collect_and_commit(&lease("run-a"), &empty(), 0)
            .unwrap(),
        SessionWorkspaceCommitOutcome::Accepted
    );
    assert_eq!(
        *helper_a.events.lock().unwrap(),
        vec!["exclusive", "quiesce", "capture", "release"]
    );
    let helper_b = Helper::new(b"unused", Fault::None);
    assert_eq!(
        service(&server.url, "run-b", helper_b.clone(), 64 * 1024 * 1024)
            .restore(&lease("run-b"), &expected, 0)
            .unwrap(),
        SessionWorkspaceResolution::Download
    );
    assert_eq!(
        *helper_b.restored.lock().unwrap(),
        vec![helper_a.wire.clone()]
    );
    assert_eq!(
        *helper_b.events.lock().unwrap(),
        vec!["exclusive", "requireQuiescent", "restore", "release"]
    );
    assert_eq!(server.finish().len(), 4);
}

#[test]
fn restore_rejects_header_frame_and_frozen_descriptor_mismatch_before_applying_bytes() {
    for mode in [
        "headerHash",
        "length",
        "truncated",
        "contentHash",
        "descriptor",
    ] {
        let helper = Helper::new(b"original", Fault::None);
        let mut expected = frozen(&helper.wire, 8);
        let mut wire = helper.wire.clone();
        if mode == "contentHash" {
            *wire.last_mut().unwrap() ^= 1;
            expected.snapshot_sha256 = format!("sha256:{:x}", Sha256::digest(&wire));
        }
        if mode == "descriptor" {
            expected.file_count += 1;
        }
        let returned = expected.clone();
        let server = Server::new(move |request| {
            if request.path.ends_with("/resolve") {
                Some(resolved(&returned, "download"))
            } else {
                assert!(request.path.ends_with("/download"));
                let mut body = wire.clone();
                if mode == "truncated" {
                    body.pop();
                }
                Some(Reply {
                    status: 200,
                    body,
                    hash: Some(if mode == "headerHash" {
                        format!("sha256:{}", "f".repeat(64))
                    } else {
                        returned.snapshot_sha256.clone()
                    }),
                    length: Some(returned.snapshot_size_bytes + u64::from(mode == "length")),
                })
            }
        });
        assert!(
            matches!(
                service(&server.url, "run-b", helper.clone(), 64 * 1024 * 1024).restore(
                    &lease("run-b"),
                    &expected,
                    0
                ),
                Err(SessionWorkspaceApiError::Rejected(_))
            ),
            "{mode}"
        );
        assert!(helper.restored.lock().unwrap().is_empty());
        assert!(!helper.events.lock().unwrap().contains(&"restore"));
        assert_eq!(server.finish().len(), 2);
    }
}

#[test]
fn fixed_profile_and_transfer_budget_reject_without_claiming_a_filesystem_quota() {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    for (field, value) in [
        ("lifetime", json!("retained")),
        ("user", json!("root")),
        ("workspaceRoot", json!("/elsewhere")),
    ] {
        let mut value_profile = serde_json::to_value(profile(64 * 1024 * 1024)).unwrap();
        value_profile[field] = value;
        assert!(E2bSessionWorkspace::new(
            serde_json::from_value(value_profile).unwrap(),
            SessionWorkspaceApi {
                client: &client,
                api_url: "http://127.0.0.1:1",
                api_token: "fixture-only",
                agent_run_id: "run-a",
                authorization_digest: &digest("run-a")
            },
            Helper::new(b"", Fault::None)
        )
        .is_err());
    }
    for input in [2, u64::MAX] {
        let helper = Helper::new(b"", Fault::None);
        let server = Server::new(|_| Some(resolved(&empty(), "empty")));
        assert!(matches!(
            service(&server.url, "run-a", helper.clone(), OVERHEAD + 1).restore(
                &lease("run-a"),
                &empty(),
                input
            ),
            Err(SessionWorkspaceApiError::Rejected(_))
        ));
        assert!(helper.restored.lock().unwrap().is_empty());
        server.finish();
    }
    let helper = Helper::new(b"", Fault::None);
    let server = Server::new(|_| Some(resolved(&empty(), "empty")));
    assert_eq!(
        service(&server.url, "run-a", helper.clone(), OVERHEAD + 1)
            .restore(&lease("run-a"), &empty(), 1)
            .unwrap(),
        SessionWorkspaceResolution::Empty
    );
    assert!(!helper.events.lock().unwrap().contains(&"restore"));
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn busy_or_unknown_helpers_never_authorize_capture_or_false_restore_success() {
    for fault in [
        Fault::Busy,
        Fault::BeginUnknown,
        Fault::QuiesceUnknown,
        Fault::CaptureUnknown,
    ] {
        let helper = Helper::new(b"result", fault);
        let server = Server::new(|request| {
            assert!(request.path.ends_with("/resolve"));
            Some(resolved(&empty(), "empty"))
        });
        assert_eq!(
            service(&server.url, "run-a", helper.clone(), 64 * 1024 * 1024)
                .collect_and_commit(&lease("run-a"), &empty(), 0)
                .unwrap(),
            SessionWorkspaceCommitOutcome::Pending
        );
        let events = helper.events.lock().unwrap();
        assert!(!events.contains(&"restore"));
        if fault != Fault::CaptureUnknown {
            assert!(!events.contains(&"capture"));
        }
        drop(events);
        assert_eq!(server.finish().len(), 1);
    }
    let helper = Helper::new(b"original", Fault::RestoreUnknown);
    let expected = frozen(&helper.wire, 8);
    let returned = expected.clone();
    let wire = helper.wire.clone();
    let server = Server::new(move |request| {
        if request.path.ends_with("/resolve") {
            Some(resolved(&returned, "download"))
        } else {
            Some(Reply {
                status: 200,
                body: wire.clone(),
                hash: Some(returned.snapshot_sha256.clone()),
                length: None,
            })
        }
    });
    assert!(matches!(
        service(&server.url, "run-b", helper, 64 * 1024 * 1024).restore(
            &lease("run-b"),
            &expected,
            0
        ),
        Err(SessionWorkspaceApiError::Unavailable(_))
    ));
    assert_eq!(server.finish().len(), 2);
}

#[test]
fn invalid_capture_and_over_budget_content_cannot_be_committed() {
    for fault in [
        Fault::WrongCaptureId,
        Fault::Unchanged,
        Fault::CorruptFrame,
        Fault::None,
    ] {
        let helper = Helper::new(b"result", fault);
        let limit = if fault == Fault::None {
            OVERHEAD
        } else {
            64 * 1024 * 1024
        };
        let server = Server::new(|request| {
            assert!(request.path.ends_with("/resolve"));
            Some(resolved(&empty(), "empty"))
        });
        assert!(matches!(
            service(&server.url, "run-a", helper, limit)
                .collect_and_commit(&lease("run-a"), &empty(), 0)
                .unwrap(),
            SessionWorkspaceCommitOutcome::Rejected(_)
        ));
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn lost_commit_reply_reconciles_once_and_keeps_unresolved_work_pending() {
    for unavailable in [true, false] {
        let helper = Helper::new(b"result", Fault::None);
        let expected = frozen(&helper.wire, 6);
        let mut resolves = 0;
        let server = Server::new(move |request| {
            if request.path.ends_with("/commit") {
                None
            } else {
                assert!(request.path.ends_with("/resolve"));
                resolves += 1;
                Some(if resolves == 1 {
                    resolved(&empty(), "empty")
                } else if unavailable {
                    Reply {
                        status: 503,
                        body: vec![],
                        hash: None,
                        length: None,
                    }
                } else {
                    resolved(&expected, "advanced")
                })
            }
        });
        assert_eq!(
            service(&server.url, "run-a", helper.clone(), 64 * 1024 * 1024)
                .collect_and_commit(&lease("run-a"), &empty(), 0)
                .unwrap(),
            if unavailable {
                SessionWorkspaceCommitOutcome::Pending
            } else {
                SessionWorkspaceCommitOutcome::Accepted
            }
        );
        assert_eq!(
            helper
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| **event == "capture")
                .count(),
            1
        );
        assert_eq!(server.finish().len(), 3);
    }
}
