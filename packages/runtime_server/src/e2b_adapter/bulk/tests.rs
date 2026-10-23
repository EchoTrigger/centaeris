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
fn bulk_local_staging_reserves_one_nonwaiting_lane_without_killing_child() {
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
    assert!(matches!(
        client.upload_reader(&request(1), &mut Cursor::new([1])),
        Err(BulkTransferError::NotDispatched(_))
    ));
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
