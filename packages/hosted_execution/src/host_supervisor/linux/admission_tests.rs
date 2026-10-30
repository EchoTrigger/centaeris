use super::*;
use std::sync::atomic::AtomicU64;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    host: Arc<Host>,
    admission: Arc<ClientAdmission>,
    workers: Vec<thread::JoinHandle<()>>,
    clients: Vec<UnixStream>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "host-admission-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let script = root.join("attest");
        fs::write(&script, format!("#!/bin/sh\ntouch '{0}/entered-'$$\nwhile [ ! -f '{0}/release' ]; do sleep 0.01; done\nprintf '[]'\n",root.display())).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let config = HostSupervisorConfig {
            schema: HOST_SUPERVISOR_SCHEMA.into(),
            socket_path: root.join("socket").to_string_lossy().into(),
            control_root: root.join("control").to_string_lossy().into(),
            output_root: root.join("output").to_string_lossy().into(),
            oci_runtime: HostOciRuntime::Runsc,
            runtime_path: "/unused/runsc".into(),
            runtime_root: "/unused/runtime".into(),
            docker_path: script.to_string_lossy().into(),
            docker_socket: "/unused/docker".into(),
            controller_uid: unsafe { libc::geteuid() },
            helper_path: None,
        };
        fs::create_dir(&config.control_root).unwrap();
        fs::create_dir(&config.output_root).unwrap();
        let host = Arc::new(Host {
            retirement: retirement::RetirementStore::new(&config),
            config,
            managers: Mutex::new(BTreeMap::new()),
            _lease: File::open(&root).unwrap(),
            helper_binary: None,
        });
        Self {
            root,
            host,
            admission: Arc::new(ClientAdmission::new()),
            workers: Vec::new(),
            clients: Vec::new(),
        }
    }
    fn target(&self) -> HostLaunchTarget {
        HostLaunchTarget {
            environment_instance_id: format!("instance_{}", "a".repeat(64)),
            container_id: "b".repeat(64),
            environment_id: "env".into(),
            profile_digest: format!("sha256:{}", "c".repeat(64)),
            image_digest: format!("sha256:{}", "d".repeat(64)),
        }
    }
    fn connect_raw(&mut self, bytes: &[u8]) -> UnixStream {
        let (server, mut client) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let worker = start_client(
            self.host.clone(),
            server,
            Instant::now(),
            self.admission.clone(),
        )
        .unwrap();
        if let Some(worker) = worker {
            self.workers.push(worker);
        }
        let _ = client.write_all(bytes);
        client
    }
    fn connect(&mut self, request: HostSupervisorRequest) -> UnixStream {
        let envelope = HostSupervisorEnvelope {
            schema: HOST_SUPERVISOR_SCHEMA.into(),
            request,
        };
        let mut bytes = serde_json::to_vec(&envelope).unwrap();
        bytes.push(b'\n');
        self.connect_raw(&bytes)
    }
    fn saturate(&mut self) {
        for _ in 0..8 {
            let client = self.connect(HostSupervisorRequest::Register {
                target: self.target(),
            });
            self.clients.push(client);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let entered = fs::read_dir(&self.root)
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("entered-")
                })
                .count();
            if entered == 8 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "eight requests did not enter their real attestation command"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn reply(&self, client: UnixStream) -> serde_json::Value {
        let mut bytes = Vec::new();
        let result = BufReader::new(client).read_until(b'\n', &mut bytes);
        assert!(
            result.is_ok(),
            "expected a framed rejection, got {result:?}"
        );
        assert!(
            !bytes.is_empty(),
            "ninth accepted connection was closed without a busy response"
        );
        serde_json::from_slice(&bytes).unwrap()
    }
    fn release(&mut self) {
        fs::write(self.root.join("release"), b"released").unwrap();
        for worker in self.workers.drain(..) {
            worker.join().unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.release();
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn ninth_valid_request_gets_explicit_unaccepted_busy_while_eight_dispatches_are_blocked() {
    let mut fixture = Fixture::new();
    fixture.saturate();
    let client = fixture.connect(HostSupervisorRequest::Inspect {
        environment_instance_id: fixture.target().environment_instance_id,
    });
    assert_eq!(
        fixture.reply(client),
        serde_json::json!({"status":"error","schema":HOST_SUPERVISOR_SCHEMA,"error":"host_busy"})
    );
    assert!(
        fixture.host.managers.lock().unwrap().is_empty(),
        "busy request reached registration side effects"
    );
}

#[test]
fn saturated_helper_is_rejected_without_waiting_for_or_spooling_binary_input() {
    use centaeris_core::execution::{
        EnvironmentTaskControllerPurpose, EnvironmentTaskFence, EnvironmentTaskOwner,
        EnvironmentTaskScope,
    };
    let mut fixture = Fixture::new();
    fixture.saturate();
    let request = HostHelperRequest {
        fence: EnvironmentTaskFence {
            environment_instance_id: fixture.target().environment_instance_id,
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "run-a".into(),
            },
            controller_epoch: 1,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        },
        operation_id: "1:materialize".into(),
        scope: EnvironmentTaskScope::Run {
            run_id: "run-a".into(),
        },
        kind: TrustedHelperKind::MaterializeInput,
        input_digest: format!("sha256:{:x}", Sha256::digest(b"private synthetic upload")),
        input_length: 24,
    };
    let client = fixture.connect(HostSupervisorRequest::Helper { helper: request });
    assert_eq!(fixture.reply(client)["error"], "host_busy");
    assert_eq!(
        fs::read_dir(&fixture.host.config.control_root)
            .unwrap()
            .count(),
        0,
        "busy helper created input spool or manager state"
    );
}

#[test]
fn released_execution_capacity_accepts_another_valid_request() {
    let mut fixture = Fixture::new();
    fixture.saturate();
    fixture.release();
    let client = fixture.connect(HostSupervisorRequest::Inspect {
        environment_instance_id: fixture.target().environment_instance_id,
    });
    assert_eq!(
        fixture.reply(client)["error"],
        "host_path_unavailable",
        "released slot did not dispatch the next valid request"
    );
}

#[test]
fn malformed_and_unsupported_headers_are_never_classified_as_busy() {
    let mut fixture = Fixture::new();
    fixture.saturate();
    for (bytes,expected) in [(b"not-json\n".as_slice(),"host_request_invalid"),(b"{\"schema\":\"unknown\",\"request\":{\"operation\":\"inspect\",\"environmentInstanceId\":\"instance_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}}\n".as_slice(),"host_schema_unsupported")] {
        let client=fixture.connect_raw(bytes);
        assert_eq!(fixture.reply(client)["error"],expected);
    }
    assert_eq!(
        fixture.admission.executing.load(Ordering::Acquire),
        8,
        "invalid header changed execution capacity"
    );
}

#[test]
fn untrusted_peer_cannot_obtain_an_unaccepted_busy_proof() {
    let mut fixture = Fixture::new();
    Arc::get_mut(&mut fixture.host)
        .unwrap()
        .config
        .controller_uid ^= 1;
    let mut client = fixture.connect(HostSupervisorRequest::Inspect {
        environment_instance_id: fixture.target().environment_instance_id,
    });
    let mut bytes = Vec::new();
    let _ = client.read_to_end(&mut bytes);
    assert!(
        bytes.is_empty(),
        "unauthenticated peer received a protocol acceptance or busy response"
    );
    assert_eq!(fixture.admission.executing.load(Ordering::Acquire), 0);
}

#[test]
fn header_slots_are_bounded_without_claiming_unknown_sockets_are_busy() {
    let mut fixture = Fixture::new();
    for _ in 0..8 {
        let client = fixture.connect_raw(b"");
        fixture.clients.push(client);
    }
    let (server, mut peer) = UnixStream::pair().unwrap();
    assert!(start_client(
        fixture.host.clone(),
        server,
        Instant::now(),
        fixture.admission.clone()
    )
    .unwrap()
    .is_none());
    let mut bytes = Vec::new();
    peer.read_to_end(&mut bytes).unwrap();
    assert!(bytes.is_empty());
    assert_eq!(fixture.admission.headers.load(Ordering::Acquire), 8);
    assert_eq!(fixture.admission.executing.load(Ordering::Acquire), 0);
    for client in &fixture.clients {
        client.shutdown(std::net::Shutdown::Write).unwrap();
    }
}

#[test]
fn slow_drip_header_cannot_extend_its_absolute_read_deadline() {
    let (server, mut peer) = UnixStream::pair().unwrap();
    let writer = thread::spawn(move || {
        for _ in 0..30 {
            if peer.write_all(b" ").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
    });
    let started = Instant::now();
    let mut reader = BufReader::new(server);
    assert_eq!(
        read_envelope(&mut reader, started + Duration::from_millis(100)).unwrap_err(),
        "host_request_invalid"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "header timeout restarted on each byte"
    );
    drop(reader);
    writer.join().unwrap();
}

#[test]
fn helper_upload_ready_binds_the_exact_request_before_binary_input_is_sent() {
    use centaeris_core::execution::{
        EnvironmentTaskControllerPurpose, EnvironmentTaskFence, EnvironmentTaskOwner,
        EnvironmentTaskScope,
    };
    let mut fixture = Fixture::new();
    let request = HostHelperRequest {
        fence: EnvironmentTaskFence {
            environment_instance_id: fixture.target().environment_instance_id,
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "run-a".into(),
            },
            controller_epoch: 1,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        },
        operation_id: "1:materialize".into(),
        scope: EnvironmentTaskScope::Run {
            run_id: "run-a".into(),
        },
        kind: TrustedHelperKind::MaterializeInput,
        input_digest: format!("sha256:{:x}", Sha256::digest(b"synthetic upload")),
        input_length: 16,
    };
    let expected = serde_json::json!({"status":"success","schema":HOST_SUPERVISOR_SCHEMA,"result":{"kind":"helperInputReady","operationId":request.operation_id,"inputLength":request.input_length,"inputDigest":request.input_digest}});
    let client = fixture.connect(HostSupervisorRequest::Helper { helper: request });
    assert_eq!(fixture.reply(client), expected);
    assert_eq!(
        fs::read_dir(&fixture.host.config.control_root)
            .unwrap()
            .count(),
        0
    );
}
