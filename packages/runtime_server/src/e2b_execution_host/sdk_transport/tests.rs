use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn process(log: &Path, frame: &[u8]) -> Arc<SpoolingAdapterProcess> {
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
    command.args(["-u","-c"]).arg(r#"
import base64,hashlib,json,os,sys
from pathlib import Path
root=Path(os.environ['E2B_ADAPTER_SPOOL_ROOT']); log=Path(sys.argv[1]); output=bytes.fromhex(sys.argv[2]); pid=100
for line in sys.stdin:
 request=json.loads(line); p=request['payload']; op=request['operation']
 with log.open('a',encoding='utf-8') as stream:stream.write(json.dumps(request)+'\n')
 if op=='runCommand':
  pid+=1;result={'sandboxId':p['sandboxId'],'pid':pid}
 elif op in ('waitCommand','invokeControlHelper'):
  data=b'{"activeProcessCount":0}' if op=='invokeControlHelper' and p['helper']=='agent-process-state' else b''
  result={'sandboxId':p['sandboxId'],'pid':p.get('pid',999),'exitCode':0,'stdout':data.decode(),'stderr':'','stdoutBytesBase64':base64.b64encode(data).decode(),'stderrBytesBase64':'','stdoutByteLength':len(data),'stderrByteLength':0}
  if op=='invokeControlHelper':result['helper']=p['helper']
 elif op=='uploadFile':
  data=(root/(p['transferId']+'.upload')).read_bytes()
  assert hashlib.sha256(data).hexdigest()==p['sha256'] and len(data)==p['byteLength']
  result={'sandboxId':p['sandboxId'],'transferId':p['transferId'],'byteLength':len(data),'sha256':hashlib.sha256(data).hexdigest()}
 elif op=='downloadFile':
  with (root/(p['transferId']+'.download')).open('xb') as stream:stream.write(output)
  result={'sandboxId':p['sandboxId'],'transferId':p['transferId'],'byteLength':len(output),'sha256':hashlib.sha256(output).hexdigest()}
 else:raise AssertionError('unexpected operation')
 print(json.dumps({'schema':'runtime.e2b.adapter.response.v1','requestId':request['requestId'],'outcome':'succeeded','result':result}),flush=True)
"#).arg(log).arg(frame.iter().map(|b|format!("{b:02x}")).collect::<String>());
    Arc::new(SpoolingAdapterProcess::spawn(command).unwrap())
}
fn requests(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn binding() -> E2bSandboxBinding {
    super::super::tests::binding()
}
#[test]
fn helper_frame_uses_private_bulk_staging_and_fixed_executable_then_exact_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let output = b"\0\xffbinary-frame";
    let ordinary = process(&log, output);
    let control = process(&control_log, output);
    let transport = E2bSdkTransport::new(ordinary, control);
    let input = vec![255; 2 * 1024 * 1024 + 17];
    assert_eq!(
        transport
            .helper(&binding(), TrustedHelperKind::FileSystem, &input, 1024)
            .unwrap(),
        output
    );
    let seen = requests(&log);
    let names: Vec<_> = seen
        .iter()
        .map(|r| r["operation"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "runCommand",
            "waitCommand",
            "uploadFile",
            "runCommand",
            "waitCommand",
            "downloadFile",
            "runCommand",
            "waitCommand"
        ]
    );
    let upload = &seen[2]["payload"];
    assert_eq!(upload["byteLength"], input.len());
    assert_eq!(upload["user"], "root");
    let request = upload["path"].as_str().unwrap();
    let directory = request.strip_suffix("/request").unwrap();
    assert!(directory.starts_with("/run/centaeris/e2b-"));
    assert!(Uuid::parse_str(directory.strip_prefix("/run/centaeris/e2b-").unwrap()).is_ok());
    let helper = &seen[3]["payload"];
    assert_eq!(helper["user"], "root");
    assert!(helper["command"]
        .as_str()
        .unwrap()
        .contains("'/opt/centaeris/bin/execution_agent' 'filesystem-once'"));
    assert!(helper["command"].as_str().unwrap().contains("ulimit -f"));
    assert_eq!(helper["envs"]["CENTAERIS_WORKSPACE_USER"], "10001:10001");
    assert_eq!(seen[5]["payload"]["path"], format!("{directory}/response"));
    assert_eq!(seen[5]["payload"]["maxBytes"], 1024);
    let cleanup = seen[6]["payload"]["command"].as_str().unwrap();
    assert!(cleanup.contains(&quote(request)));
    assert!(cleanup.contains(&quote(&format!("{directory}/response"))));
    assert!(cleanup.ends_with(&quote(directory)));
    assert!(!control_log.exists());
}
#[test]
fn status_and_quiescence_use_fresh_fixed_control_without_ordinary_connection() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let transport = E2bSdkTransport::new(process(&log, b""), process(&control_log, b""));
    let observed = transport.inspect(&binding()).unwrap();
    assert!(observed.running && observed.on_timeout_kill && !observed.auto_resume);
    assert_eq!(observed.binding, binding());
    transport.quiesce(&binding()).unwrap();
    let seen = requests(&control_log);
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|r| r["operation"] == "invokeControlHelper"));
    assert_eq!(seen[0]["payload"]["helper"], "agent-process-state");
    assert_eq!(seen[1]["payload"]["helper"], "quiesce-agent-processes");
    assert_eq!(
        seen[0]["payload"]["expectedMetadata"],
        serde_json::to_value(binding().metadata).unwrap()
    );
    assert!(!log.exists());
}
#[test]
fn ordinary_command_quotes_each_argv_and_retains_agent_identity() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let transport = E2bSdkTransport::new(
        process(&log, b""),
        process(&temp.path().join("control.jsonl"), b""),
    );
    let request = ExecutionCommandRequest {
        program: "/bin/bash".into(),
        args: vec!["-c".into(), "printf '%s' '$value; untouched'".into()],
        cwd: "/mnt/data".into(),
        env: HashMap::new(),
        timeout_ms: 1000,
        policy: ExecutionPolicy::workspace_write_no_network("/mnt/data"),
    };
    let pid = transport.start(&binding(), &request).unwrap();
    transport
        .wait(&binding(), pid, Duration::from_secs(6))
        .unwrap();
    let seen = requests(&log);
    assert_eq!(
        seen[0]["payload"]["command"],
        format!(
            "exec {} {} {}",
            quote("/bin/bash"),
            quote("-c"),
            quote(&request.args[1])
        )
    );
    assert_eq!(seen[0]["payload"]["user"], "agent");
    assert_eq!(seen[1]["payload"]["pid"], pid);
}
#[test]
fn invalid_raw_output_identity_or_length_is_never_treated_as_completed() {
    for mode in 0..3 {
        let mut receipt = json!({"sandboxId":"owned","pid":17,"exitCode":7,"stdout":"ignored display text","stderr":"","stdoutBytesBase64":"AP9B","stderrBytesBase64":"","stdoutByteLength":3,"stderrByteLength":0});
        match mode {
            0 => receipt["sandboxId"] = "foreign".into(),
            1 => receipt["pid"] = 18.into(),
            _ => receipt["stdoutByteLength"] = 4.into(),
        };
        let receipt: RawCommandReceipt = serde_json::from_value(receipt).unwrap();
        assert!(receipt.output("owned", Some(17)).is_err());
    }
}
#[test]
fn helper_budget_and_long_lived_helper_are_rejected_before_any_request() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let transport = E2bSdkTransport::new(process(&log, b""), process(&control_log, b""));
    assert!(matches!(
        transport.helper(&binding(), TrustedHelperKind::WorkspaceWatch, b"", 1024),
        Err(E2bHostError::NotDispatched(_))
    ));
    assert!(matches!(
        transport.helper(
            &binding(),
            TrustedHelperKind::FileSystem,
            b"",
            MAX_TRANSFER_BYTES as usize + 1
        ),
        Err(E2bHostError::NotDispatched(_))
    ));
    assert!(!log.exists());
    assert!(!control_log.exists());
}
