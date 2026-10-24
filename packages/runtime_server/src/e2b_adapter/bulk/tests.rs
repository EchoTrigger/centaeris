use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::{Path, PathBuf};

fn fixture(mode: &str, log: &Path) -> Command {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("E2B_ADAPTER_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            root.join(if cfg!(windows) {
                ".venv/Scripts/python.exe"
            } else {
                ".venv/bin/python"
            })
        });
    let mut command = Command::new(python);
    command.args(["-u", "-c"]);
    command.arg(r#"
import hashlib, json, os, sys, time
from pathlib import Path
root=Path(os.environ['E2B_ADAPTER_SPOOL_ROOT'])
log=Path(sys.argv[2]);mode=sys.argv[1]
seen=[]
for line in sys.stdin:
 request=json.loads(line);payload=request['payload'];operation=request['operation']
 seen.append(request)
 log.write_text(json.dumps({'root':str(root),'requests':seen}))
 if operation == 'block':
  Path(sys.argv[3]).write_text('entered')
  release = Path(sys.argv[4]); deadline = time.monotonic() + 20
  while not release.exists():
   if time.monotonic() >= deadline: sys.exit(2)
   time.sleep(0.005)
 if mode=='exit': sys.exit(1)
 if operation not in ('uploadFile','downloadFile'):
  result={'connected':True}
 elif operation=='uploadFile':
  path=root/(payload['transferId']+'.upload')
  size=0;digest=hashlib.sha256()
  with path.open('rb') as stream:
   while chunk:=stream.read(65536):size+=len(chunk);digest.update(chunk)
  result={'sandboxId':payload['sandboxId'],'transferId':payload['transferId'],'byteLength':size,'sha256':digest.hexdigest()}
 else:
  path=root/(payload['transferId']+'.download')
  size=0 if mode=='empty' else min(payload['maxBytes'],2*1024*1024+17)
  digest=hashlib.sha256()
  with path.open('xb') as stream:
   remaining=size
   while remaining:
    chunk=b'Z'*min(65536,remaining);stream.write(chunk);digest.update(chunk);remaining-=len(chunk)
  if mode=='timeout':
   log.write_text(json.dumps({'root':str(root),'requests':seen,'partial':str(path)}))
   time.sleep(30)
  result={'sandboxId':payload['sandboxId'],'transferId':payload['transferId'],'byteLength':size,'sha256':digest.hexdigest()}
 if operation in ('uploadFile','downloadFile'):
  if mode=='wrongSandbox':result['sandboxId']='another-sandbox'
  if mode=='wrongTransfer':result['transferId']='123e4567-e89b-12d3-a456-426614174999'
  if mode=='wrongLength':result['byteLength']+=1
  if mode=='wrongDigest':result['sha256']='0'*64
 response={'schema':'runtime.e2b.adapter.response.v1','requestId':request['requestId'],'outcome':'succeeded','result':result}
 if mode=='providerUnknown' and operation=='create':
  response.update(outcome='unknown',result=None,error={'code':'providerFailure','message':'fixture-provider-unknown'})
 print(json.dumps(response),flush=True)
"#);
    command.arg(mode).arg(log);
    command
}

fn request(max_bytes: u64) -> BulkFileRequest<'static> {
    BulkFileRequest {
        sandbox_id: "sandbox-a",
        path: "/tmp/input-frame",
        user: Some("user"),
        max_bytes,
        timeout: Duration::from_secs(20),
    }
}

fn metadata(log: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(log).unwrap()).unwrap()
}

fn prime(client: &SpoolingAdapterProcess) {
    client
        .call(
            "fixture-connect",
            "connect",
            json!({"sandboxId":"sandbox-a","timeoutSeconds":60,"onResume":"restore"}),
            Duration::from_secs(20),
        )
        .unwrap();
}

struct GeneratedSource {
    remaining: u64,
    read_bytes: u64,
    max_read: usize,
}
impl Read for GeneratedSource {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.max_read = self.max_read.max(buffer.len());
        let count = usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap();
        buffer[..count].fill(b'Z');
        self.remaining -= count as u64;
        self.read_bytes += count as u64;
        Ok(count)
    }
}

#[test]
fn bulk_upload_streams_core_64mib_input_once_and_keeps_bytes_out_of_json() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let client = SpoolingAdapterProcess::spawn(fixture("success", &log)).unwrap();
    prime(&client);
    let length = 64 * 1024 * 1024;
    let mut source = GeneratedSource {
        remaining: length,
        read_bytes: 0,
        max_read: 0,
    };
    let receipt = client.upload_reader(&request(length), &mut source).unwrap();
    assert_eq!(receipt.byte_length, length);
    assert_eq!(source.read_bytes, length);
    assert_eq!(source.remaining, 0);
    assert!(source.max_read <= TRANSFER_CHUNK_BYTES);
    let recorded = metadata(&log);
    let sent = &recorded["requests"][1];
    assert_eq!(sent["operation"], "uploadFile");
    assert_eq!(sent["payload"].as_object().unwrap().len(), 6);
    assert_eq!(sent["payload"]["byteLength"], length);
    assert_eq!(sent["payload"]["sha256"], receipt.sha256);
    let id = sent["payload"]["transferId"].as_str().unwrap();
    assert_eq!(id.len(), 36);
    assert_eq!(id, &receipt.transfer_id);
    let wire = serde_json::to_string(sent).unwrap();
    assert!(wire.len() < 2048);
    assert!(!wire.contains(recorded["root"].as_str().unwrap()));
    assert_eq!(
        std::fs::read_dir(recorded["root"].as_str().unwrap())
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn bulk_download_streams_into_unpublished_sink_and_verifies_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let client = SpoolingAdapterProcess::spawn(fixture("success", &log)).unwrap();
    let mut sink = vec![];
    let receipt = client
        .download_into(&request(3 * 1024 * 1024), &mut sink)
        .unwrap();
    assert_eq!(sink.len(), 2 * 1024 * 1024 + 17);
    assert!(sink.iter().all(|byte| *byte == b'Z'));
    assert_eq!(receipt.sha256, format!("{:x}", Sha256::digest(&sink)));
    let recorded = metadata(&log);
    let sent = &recorded["requests"][0];
    assert_eq!(sent["operation"], "downloadFile");
    assert_eq!(sent["payload"].as_object().unwrap().len(), 5);
    assert_eq!(
        std::fs::read_dir(recorded["root"].as_str().unwrap())
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn bulk_empty_upload_and_download_have_exact_zero_length_integrity() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let client = SpoolingAdapterProcess::spawn(fixture("empty", &log)).unwrap();
    let upload = client
        .upload_reader(&request(1), &mut Cursor::new([]))
        .unwrap();
    let mut sink = vec![];
    let download = client.download_into(&request(1), &mut sink).unwrap();
    for receipt in [upload, download] {
        assert_eq!(receipt.byte_length, 0);
        assert_eq!(receipt.sha256, format!("{:x}", Sha256::digest([])));
    }
    assert!(sink.is_empty());
}

#[test]
fn bulk_upload_limit_rejection_has_no_sdk_dispatch_and_preserves_healthy_child() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let client = SpoolingAdapterProcess::spawn(fixture("success", &log)).unwrap();
    prime(&client);
    let mut source = Cursor::new(vec![1u8; TRANSFER_CHUNK_BYTES + 1]);
    assert!(matches!(
        client.upload_reader(&request(TRANSFER_CHUNK_BYTES as u64), &mut source),
        Err(BulkTransferError::NotDispatched(_))
    ));
    assert_eq!(metadata(&log)["requests"].as_array().unwrap().len(), 1);
    assert_eq!(
        std::fs::read_dir(metadata(&log)["root"].as_str().unwrap())
            .unwrap()
            .count(),
        0
    );
    assert!(client
        .call(
            "still-alive",
            "inspect",
            json!({"sandboxId":"sandbox-a"}),
            Duration::from_secs(10)
        )
        .is_ok());
}

#[test]
fn bulk_receipt_mismatch_never_returns_success_and_cleans_only_owned_transfer() {
    for mode in [
        "wrongSandbox",
        "wrongTransfer",
        "wrongLength",
        "wrongDigest",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("requests.json");
        let client = SpoolingAdapterProcess::spawn(fixture(mode, &log)).unwrap();
        prime(&client);
        let root = PathBuf::from(metadata(&log)["root"].as_str().unwrap());
        let unrelated = root.join("keep");
        std::fs::write(&unrelated, b"preserve").unwrap();
        assert!(
            matches!(
                client.download_into(&request(1024), &mut vec![]),
                Err(BulkTransferError::Unknown(_))
            ),
            "{mode}"
        );
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"preserve");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert!(client
            .call(
                "closed",
                "inspect",
                json!({"sandboxId":"sandbox-a"}),
                Duration::from_secs(1)
            )
            .is_err());
    }
}

#[test]
fn bulk_timeout_stops_child_before_removing_its_partial_download() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let client = SpoolingAdapterProcess::spawn(fixture("timeout", &log)).unwrap();
    prime(&client);
    let root = PathBuf::from(metadata(&log)["root"].as_str().unwrap());
    let mut bounded = request(1024);
    bounded.timeout = Duration::from_millis(300);
    assert!(matches!(
        client.download_into(&bounded, &mut vec![]),
        Err(BulkTransferError::Unknown(_))
    ));
    assert!(metadata(&log).get("partial").is_some());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    client.close().unwrap();
    drop(client);
    assert!(!root.exists());
}

#[test]
fn bulk_unconfirmed_child_stop_retains_its_owned_spool_for_later_cleanup() {
    let parent = tempfile::tempdir().unwrap();
    let spool = tempfile::tempdir_in(parent.path()).unwrap();
    let root = spool.path().to_path_buf();
    let marker = root.join("owned-partial");
    std::fs::write(&marker, b"still-owned").unwrap();
    let retained = finish_spool(spool, false).unwrap_err();
    assert_eq!(retained, root);
    assert_eq!(std::fs::read(&marker).unwrap(), b"still-owned");
    // The injected branch created no child. The test's parent owns its cleanup.
    assert_eq!(retained.parent(), Some(parent.path()));
}

#[test]
fn bulk_real_sdk_loopback_http_upload_and_download_preserve_binary_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("sdk-requests.json");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("E2B_ADAPTER_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            root.join(if cfg!(windows) {
                ".venv/Scripts/python.exe"
            } else {
                ".venv/bin/python"
            })
        });
    let mut command = Command::new(python);
    command.args(["-u", "-c"]);
    command.arg(r#"
import hashlib, json, os, sys
from pathlib import Path
root=Path(sys.argv[1]);sys.path.insert(0,str(root/'packages/e2b_adapter/tests'));sys.path.insert(0,str(root/'packages/e2b_adapter'))
from test_bulk_transport import bulk_data_fixture
from test_sdk_transport_acceptance import provider_fixture
from centaeris_e2b_adapter.bridge import Adapter, AdapterConfig
with provider_fixture() as control, bulk_data_fixture() as (data_url,received):
 adapter=Adapter(AdapterConfig(api_key='owned-bulk-fixture',api_url=control.api_url,sandbox_url=data_url,spool_root=os.environ['E2B_ADAPTER_SPOOL_ROOT']))
 for line in sys.stdin:
  response=adapter.handle(json.loads(line))
  Path(sys.argv[2]).write_text(json.dumps([{'byteLength':len(data),'sha256':hashlib.sha256(data).hexdigest()} for data in received]))
  print(json.dumps(response),flush=True)
"#);
    command.arg(&root).arg(&log);
    let client = SpoolingAdapterProcess::spawn(command).unwrap();
    client.call("fixture-create","create",json!({"attemptId":"bulk-attempt","idempotencyKey":"bulk-key","templateRef":{"templateId":"template-fixture","buildId":"123e4567-e89b-12d3-a456-426614174000"},"metadata":{},"timeoutSeconds":60,"allowInternetAccess":false,"onTimeout":"kill"}),Duration::from_secs(20)).unwrap();
    let binary: Vec<u8> = (0..=255u8).cycle().take(256 * 1537).collect();
    let expected = format!("{:x}", Sha256::digest(&binary));
    let request = BulkFileRequest {
        sandbox_id: "sandbox-fixture",
        path: "/tmp/staged-request",
        user: Some("user"),
        max_bytes: 1024 * 1024,
        timeout: Duration::from_secs(20),
    };
    let uploaded = client
        .upload_reader(&request, &mut Cursor::new(&binary))
        .unwrap();
    let mut downloaded = vec![];
    let receipt = client.download_into(&request, &mut downloaded).unwrap();
    assert_eq!(downloaded, binary);
    assert_eq!(uploaded.sha256, expected);
    assert_eq!(receipt.sha256, expected);
    let seen: Vec<Value> = serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["byteLength"], binary.len());
    assert_eq!(seen[0]["sha256"], expected);
}

#[test]
fn bulk_local_staging_reserves_one_deadline_bound_lane_without_killing_child() {
    struct BlockedSource {
        entered: Option<std::sync::mpsc::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
    }
    impl Read for BlockedSource {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let Some(entered) = self.entered.take() else {
                return Ok(0);
            };
            entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(10)).unwrap();
            buffer[0] = b'Z';
            Ok(1)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let client =
        std::sync::Arc::new(SpoolingAdapterProcess::spawn(fixture("success", &log)).unwrap());
    prime(&client);
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, proceed) = std::sync::mpsc::channel();
    let worker = client.clone();
    let running = std::thread::spawn(move || {
        worker.upload_reader(
            &request(1),
            &mut BlockedSource {
                entered: Some(entered),
                release: proceed,
            },
        )
    });
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(
        client.call(
            "busy",
            "inspect",
            json!({"sandboxId":"sandbox-a"}),
            Duration::from_secs(1)
        ),
        Err(AdapterFailure::NotDispatched(_))
    ));
    let mut queued_request = request(1);
    queued_request.timeout = Duration::from_millis(80);
    let mut queued_source = Cursor::new([1]);
    assert_eq!(
        client.upload_reader(&queued_request, &mut queued_source),
        Err(BulkTransferError::NotDispatched(
            "adapter_deadline_before_dispatch"
        ))
    );
    assert_eq!(
        queued_source.position(),
        0,
        "queue waiting consumed upload input"
    );
    assert_eq!(metadata(&log)["requests"].as_array().unwrap().len(), 1);
    release.send(()).unwrap();
    assert!(running.join().unwrap().is_ok());
    assert!(client
        .call(
            "healthy",
            "inspect",
            json!({"sandboxId":"sandbox-a"}),
            Duration::from_secs(10)
        )
        .is_ok());
}

#[test]
fn bulk_short_calls_queue_before_the_shared_spool_lane() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let entered = directory.path().join("entered");
    let release = directory.path().join("release");
    let mut command = fixture("barrier", &log);
    command.arg(&entered).arg(&release);
    let client = std::sync::Arc::new(SpoolingAdapterProcess::spawn(command).unwrap());
    let worker = client.clone();
    let running = std::thread::spawn(move || {
        worker.call("held", "block", json!({}), Duration::from_secs(15))
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !entered.exists() {
        assert!(
            Instant::now() < deadline,
            "spooling fixture never entered first call"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let waiting = client.clone();
    let (began, started) = std::sync::mpsc::channel();
    let (finished, completion) = std::sync::mpsc::channel();
    let queued = std::thread::spawn(move || {
        began.send(()).unwrap();
        finished
            .send(waiting.call("queued", "inspect", json!({}), Duration::from_secs(5)))
            .unwrap();
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let premature = completion.recv_timeout(Duration::from_millis(80));
    std::fs::write(&release, "released").unwrap();
    assert!(running.join().unwrap().is_ok());
    let was_pending = matches!(&premature, Err(std::sync::mpsc::RecvTimeoutError::Timeout));
    let second = match premature {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            completion.recv_timeout(Duration::from_secs(5)).unwrap()
        }
        Err(error) => panic!("queued worker disconnected: {error}"),
    };
    queued.join().unwrap();
    assert!(
        was_pending,
        "the outer spool lane rejected a normal concurrent short call: {second:?}"
    );
    assert_eq!(second.unwrap().request_id, "queued");
    let recorded = metadata(&log);
    assert_eq!(recorded["requests"].as_array().unwrap().len(), 2);
    assert_eq!(recorded["requests"][0]["requestId"], "held");
    assert_eq!(recorded["requests"][1]["requestId"], "queued");
}

struct HeldStagingSource {
    entered: Option<std::sync::mpsc::Sender<()>>,
    release: std::sync::mpsc::Receiver<()>,
}
impl Read for HeldStagingSource {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let Some(entered) = self.entered.take() else {
            return Ok(0);
        };
        entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(10)).unwrap();
        buffer[0] = b'Z';
        Ok(1)
    }
}

#[test]
fn bulk_close_wakes_waiters_before_a_local_source_releases_its_permit() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let client =
        std::sync::Arc::new(SpoolingAdapterProcess::spawn(fixture("success", &log)).unwrap());
    prime(&client);
    let (entered, ready) = std::sync::mpsc::channel();
    let (release, proceed) = std::sync::mpsc::channel();
    let worker = client.clone();
    let running = std::thread::spawn(move || {
        worker.upload_reader(
            &request(1),
            &mut HeldStagingSource {
                entered: Some(entered),
                release: proceed,
            },
        )
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    let waiting = client.clone();
    let (done, completion) = std::sync::mpsc::channel();
    let queued = std::thread::spawn(move || {
        done.send(waiting.call("queued", "inspect", json!({}), Duration::from_secs(10)))
            .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.lane.state.lock().unwrap().waiters.len() != 1 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let closing = Instant::now();
    client.close().unwrap();
    let unsent = completion.recv_timeout(Duration::from_secs(2)).unwrap();
    let elapsed = closing.elapsed();
    release.send(()).unwrap();
    let held = running.join().unwrap();
    queued.join().unwrap();
    assert_eq!(unsent, Err(AdapterFailure::NotDispatched("adapter_closed")));
    assert_eq!(
        held,
        Err(BulkTransferError::NotDispatched("adapter_closed"))
    );
    assert!(elapsed < Duration::from_secs(2));
    assert_eq!(metadata(&log)["requests"].as_array().unwrap().len(), 1);
    assert_eq!(std::fs::read_dir(&client.root).unwrap().count(), 0);
}

#[test]
fn bulk_queue_wait_and_local_staging_share_the_original_deadline() {
    struct SlowSource(bool);
    impl Read for SlowSource {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Ok(0);
            }
            self.0 = true;
            std::thread::sleep(Duration::from_millis(100));
            buffer[0] = b'Z';
            Ok(1)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let entered = directory.path().join("entered");
    let release = directory.path().join("release");
    let mut command = fixture("barrier", &log);
    command.arg(&entered).arg(&release);
    let client = std::sync::Arc::new(SpoolingAdapterProcess::spawn(command).unwrap());
    let worker = client.clone();
    let running = std::thread::spawn(move || {
        worker.call("held", "block", json!({}), Duration::from_secs(10))
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !entered.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let waiting = client.clone();
    let queued = std::thread::spawn(move || {
        let mut bounded = request(1);
        bounded.timeout = Duration::from_millis(160);
        waiting.upload_reader(&bounded, &mut SlowSource(false))
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.lane.state.lock().unwrap().waiters.len() != 1 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(100));
    std::fs::write(&release, "released").unwrap();
    assert!(running.join().unwrap().is_ok());
    assert_eq!(
        queued.join().unwrap(),
        Err(BulkTransferError::NotDispatched("bulk_deadline_exceeded"))
    );
    assert_eq!(
        metadata(&log)["requests"].as_array().unwrap().len(),
        1,
        "staging received a fresh timeout after queueing"
    );
    assert_eq!(std::fs::read_dir(&client.root).unwrap().count(), 0);
    assert!(client
        .call("probe", "inspect", json!({}), Duration::from_secs(5))
        .is_ok());
}

#[test]
fn bulk_lost_reply_closes_spool_queue_before_unsent_upload_reads_its_source() {
    struct CountedSource {
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        sent: bool,
    }
    impl Read for CountedSource {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.sent {
                return Ok(0);
            }
            self.sent = true;
            buffer[0] = b'Z';
            Ok(1)
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("requests.json");
    let entered = directory.path().join("entered");
    let release = directory.path().join("release");
    let mut command = fixture("exit", &log);
    command.arg(&entered).arg(&release);
    let client = std::sync::Arc::new(SpoolingAdapterProcess::spawn(command).unwrap());
    let worker = client.clone();
    let running = std::thread::spawn(move || {
        worker.call("effect", "block", json!({}), Duration::from_secs(10))
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !entered.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = reads.clone();
    let waiting = client.clone();
    let queued = std::thread::spawn(move || {
        waiting.upload_reader(
            &request(1),
            &mut CountedSource {
                reads: counter,
                sent: false,
            },
        )
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while client.lane.state.lock().unwrap().waiters.len() != 1 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    std::fs::write(&release, "released").unwrap();
    assert_eq!(
        running.join().unwrap(),
        Err(AdapterFailure::Unknown("adapter_response_unavailable"))
    );
    assert_eq!(
        queued.join().unwrap(),
        Err(BulkTransferError::NotDispatched("adapter_closed"))
    );
    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "an upload queued behind a lost reply consumed input after the physical lane closed"
    );
    assert_eq!(metadata(&log)["requests"].as_array().unwrap().len(), 1);
    assert_eq!(std::fs::read_dir(&client.root).unwrap().count(), 0);

    let healthy_log = directory.path().join("healthy-requests.json");
    let healthy = SpoolingAdapterProcess::spawn(fixture("providerUnknown", &healthy_log)).unwrap();
    let response = healthy
        .call("unknown", "create", json!({}), Duration::from_secs(5))
        .unwrap();
    assert_eq!(response.outcome, AdapterOutcome::Unknown);
    let receipt = healthy
        .upload_reader(&request(1), &mut Cursor::new([b'Z']))
        .unwrap();
    assert_eq!(
        receipt.byte_length, 1,
        "a valid provider Unknown response must keep transport healthy"
    );
    assert_eq!(
        metadata(&healthy_log)["requests"].as_array().unwrap().len(),
        2
    );
}
