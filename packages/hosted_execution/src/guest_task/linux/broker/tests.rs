use super::*;
use centaeris_core::execution::*;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "centaeris-guest-broker-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    for child in ["control", "output"] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join(child))
            .unwrap();
    }
    root
}
struct AdministrativeEndpoint(NativeManager);
impl Endpoint for AdministrativeEndpoint {
    fn handle(&self, envelope: EnvironmentTaskEnvelope) -> EnvironmentTaskResponse {
        EnvironmentTaskResponse::from_result((|| {
            envelope.validate()?;
            envelope.request.validate_authority()?;
            self.0.handle(envelope.request)
        })())
    }
}
fn broker(root: &Path) -> Broker {
    let root = root.to_path_buf();
    Broker::new(move |initial| {
        Ok(Arc::new(AdministrativeEndpoint(
            NativeManager::retirement_fixture(
                root.join("control"),
                root.join("output"),
                crate::host_supervisor::request_instance(&initial.request),
            )?,
        )))
    })
}
fn request(request: EnvironmentTaskRequest) -> EnvironmentTaskEnvelope {
    EnvironmentTaskEnvelope {
        schema: ENVIRONMENT_TASK_SCHEMA.into(),
        request,
    }
}
fn takeover() -> EnvironmentTaskEnvelope {
    request(EnvironmentTaskRequest::Takeover {
        environment_instance_id: "instance".into(),
        controller_owner: EnvironmentTaskOwner::MaintenanceOperation {
            operation_id: "prepare".into(),
        },
        controller_epoch: 1,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    })
}
fn taken(response: EnvironmentTaskResponse) -> EnvironmentTaskFence {
    match response {
        EnvironmentTaskResponse::Success {
            result: EnvironmentTaskResult::TakenOver { fence },
            ..
        } => fence,
        other => panic!("expected Core takeover, got {other:?}"),
    }
}
#[test]
fn client_lifetimes_keep_the_same_core_controller_and_scope_receipts() {
    let root = root();
    let server = broker(&root);
    let first = taken(server.handle(takeover()));
    assert_eq!(taken(server.handle(takeover())), first);
    assert!(
        matches!(server.handle(request(EnvironmentTaskRequest::List {fence:first.clone()})), EnvironmentTaskResponse::Success {result:EnvironmentTaskResult::Listed {tasks},..} if tasks.is_empty())
    );
    let close = request(EnvironmentTaskRequest::EndOperation {
        fence: first.clone(),
        owner_operation_id: "prepare".into(),
        operation_id: first.operation_id("end"),
    });
    let ended = server.handle(close.clone());
    assert!(matches!(
        ended,
        EnvironmentTaskResponse::Success {
            result: EnvironmentTaskResult::OperationEnded {
                cleanup_complete: true,
                ..
            },
            ..
        }
    ));
    assert_eq!(server.handle(close), ended);
    drop(server);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn broker_reopen_uses_the_existing_journal_and_does_not_replace_the_instance() {
    let root = root();
    let server = broker(&root);
    let first = taken(server.handle(takeover()));
    drop(server);
    let server = broker(&root);
    assert_eq!(taken(server.handle(takeover())), first);
    let mut wrong = takeover();
    if let EnvironmentTaskRequest::Takeover {
        environment_instance_id,
        ..
    } = &mut wrong.request
    {
        *environment_instance_id = "different-instance".into();
    }
    assert!(matches!(
        server.handle(wrong),
        EnvironmentTaskResponse::Error { .. }
    ));
    drop(server);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn missing_real_process_domain_rejects_start_without_journal_acceptance_or_host_fallback() {
    let root = root();
    let server = broker(&root);
    let fence = taken(server.handle(takeover()));
    let marker = root.join("must-not-execute");
    let start = request(EnvironmentTaskRequest::Start {
        fence: fence.clone(),
        operation_id: fence.operation_id("start"),
        scope: EnvironmentTaskScope::Operation {
            operation_id: "prepare".into(),
        },
        program: "/bin/touch".into(),
        args: vec![marker.to_string_lossy().into()],
        env: Default::default(),
        cwd: "/app".into(),
        timeout_ms: 1000,
    });
    assert!(
        matches!(server.handle(start.clone()),EnvironmentTaskResponse::Error {error,..} if error=="task_tree_supervision_unavailable")
    );
    assert!(matches!(
        server.handle(start),
        EnvironmentTaskResponse::Error { .. }
    ));
    assert!(!marker.exists());
    assert!(
        matches!(server.handle(request(EnvironmentTaskRequest::List {fence})), EnvironmentTaskResponse::Success {result:EnvironmentTaskResult::Listed {tasks},..} if tasks.is_empty())
    );
    drop(server);
    fs::remove_dir_all(root).unwrap();
}

struct Resident {
    socket: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<Result<(), String>>>,
    _lease: File,
}
impl Resident {
    fn start(root: &Path) -> Self {
        let lock = lease(root).unwrap();
        let socket = root.join("task.sock");
        let listener = bind(&socket).unwrap();
        let server = Arc::new(broker(root));
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let worker = thread::spawn(move || serve(listener, server, &signal));
        Self {
            socket,
            stop,
            worker: Some(worker),
            _lease: lock,
        }
    }
    fn rpc(&self, envelope: &EnvironmentTaskEnvelope) -> EnvironmentTaskResponse {
        request_once(connect(&self.socket).unwrap(), envelope).unwrap()
    }
}
impl Drop for Resident {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.socket);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap().unwrap();
        }
    }
}
#[test]
#[ignore = "requires Linux root for disposable root-owned guest controls"]
fn separate_unix_clients_and_a_broker_restart_keep_the_real_core_journal() {
    let root = root();
    let server = Resident::start(&root);
    let first = taken(server.rpc(&takeover()));
    // Read one byte then abandon the reply: the request was dispatched and its
    // controller mutation persisted, but this client never receives a receipt.
    let mut abandoned = connect(&server.socket).unwrap();
    let mut bytes = serde_json::to_vec(&takeover()).unwrap();
    bytes.push(b'\n');
    abandoned.write_all(&bytes).unwrap();
    abandoned.shutdown(Shutdown::Write).unwrap();
    let mut one = [0u8; 1];
    assert_eq!(abandoned.read(&mut one).unwrap(), 1);
    drop(abandoned);
    assert!(
        matches!(server.rpc(&request(EnvironmentTaskRequest::List {fence:first.clone()})),EnvironmentTaskResponse::Success {result:EnvironmentTaskResult::Listed {tasks},..} if tasks.is_empty())
    );
    drop(server);
    let server = Resident::start(&root);
    assert_eq!(taken(server.rpc(&takeover())), first);
    drop(server);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn lost_reply_is_unknown_and_the_wire_contains_exactly_one_core_request() {
    let (client, mut peer) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let expected = takeover();
    let request = expected.clone();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes).unwrap();
        let observed: EnvironmentTaskEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(observed, expected);
        // Closing without a response never becomes successful cleanup/exit.
    });
    assert_eq!(
        request_once(client, &request).unwrap_err(),
        "guest_broker_reply_unknown"
    );
    reader.join().unwrap();
}
#[test]
#[ignore = "requires Linux root for disposable root-owned guest controls"]
fn unknown_protocol_fields_are_rejected_without_replacing_a_resident_controller() {
    let root = root();
    let server = Resident::start(&root);
    let first = taken(server.rpc(&takeover()));
    let mut invalid = serde_json::to_value(takeover()).unwrap();
    invalid["pid"] = serde_json::json!(123);
    let mut bytes = serde_json::to_vec(&invalid).unwrap();
    bytes.push(b'\n');
    let mut client = connect(&server.socket).unwrap();
    client.write_all(&bytes).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let response: EnvironmentTaskResponse =
        serde_json::from_slice(&frame(&mut BufReader::new(client), MAX_RESPONSE).unwrap()).unwrap();
    assert!(
        matches!(response,EnvironmentTaskResponse::Error {error,..} if error=="guest_request_invalid")
    );
    assert_eq!(taken(server.rpc(&takeover())), first);
    drop(server);
    fs::remove_dir_all(root).unwrap();
}
#[test]
#[ignore = "requires Linux root for disposable root-owned guest controls"]
fn private_socket_and_broker_lease_reject_replacement_or_second_owner() {
    let root = root();
    let first = lease(&root).unwrap();
    assert!(lease(&root).is_err());
    let socket = root.join("task.sock");
    let listener = bind(&socket).unwrap();
    assert_eq!(fs::symlink_metadata(&socket).unwrap().mode() & 0o777, 0o600);
    assert!(bind(&socket).is_err());
    drop(listener);
    drop(first);
    fs::remove_file(&socket).unwrap();
    let outside = root.join("outside");
    fs::write(&outside, b"preserve").unwrap();
    std::os::unix::fs::symlink(&outside, &socket).unwrap();
    assert!(connect(&socket).is_err());
    assert!(bind(&socket).is_err());
    assert_eq!(fs::read(&outside).unwrap(), b"preserve");
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires Linux root for disposable root-owned guest controls"]
fn absent_resident_service_fails_closed_without_creating_a_socket_or_controller() {
    let root = root();
    let socket = root.join("task.sock");
    assert_eq!(
        client_request(&socket, &takeover()).unwrap_err(),
        "guest_broker_unavailable"
    );
    assert!(!socket.exists());
    assert!(!root.join("control").join("history.sqlite").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn incomplete_or_oversized_frames_cannot_dispatch_a_core_request() {
    for invalid in [b"{}".to_vec(), vec![b'x'; MAX_REQUEST + 1]] {
        assert_eq!(
            frame(&mut BufReader::new(invalid.as_slice()), MAX_REQUEST).unwrap_err(),
            "guest_frame_invalid"
        );
    }
}
