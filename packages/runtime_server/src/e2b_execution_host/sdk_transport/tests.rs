use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn process(log: &Path, frame: &[u8]) -> Arc<SpoolingAdapterProcess> {
    process_with_capture_outcome(log, frame, "default")
}
pub(crate) fn process_with_capture_outcome(
    log: &Path,
    frame: &[u8],
    mode: &str,
) -> Arc<SpoolingAdapterProcess> {
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
root=Path(os.environ['E2B_ADAPTER_SPOOL_ROOT']); log=Path(sys.argv[1]); output=bytes.fromhex(sys.argv[2]); pid=100; contexts={}; mode=sys.argv[3]
for sequence, line in enumerate(sys.stdin, 1):
 request=json.loads(line); p=request['payload']; op=request['operation']
 with log.open('a',encoding='utf-8') as stream:stream.write(json.dumps(request)+'\n')
 if op in ('downloadFile','listDirectory') and mode.startswith('nativeFile'):
  result={key:p[key] for key in ('sandboxId','transferId') if key in p}
  if mode=='nativeFileWrongTarget':result['sandboxId']='other-sandbox'
  print(json.dumps({'schema':'runtime.e2b.adapter.response.v1','requestId':request['requestId'],
   'outcome':'unknown' if mode=='nativeFileUnknown' else 'failed','result':result,
   'error':{'code':'unrecognized' if mode=='nativeFileUnknownCode' else 'fileNotFound','message':'bounded file failure'}}),flush=True)
  continue
 failure_at={'mkdirUnknown':1,'uploadUnknown':3,'executeUnknown':4,'waitUnknown':5,'downloadUnknown':6,'cleanupUnknown':7,'controlUnknown':1}.get(mode)
 if sequence==failure_at:
  print(json.dumps({'schema':'runtime.e2b.adapter.response.v1','requestId':request['requestId'],'outcome':'unknown','error':{'code':'fixtureUnknown','message':'private diagnostic /secret/path token=hidden'}}),flush=True)
  continue
 if op=='attach':
  result={'sandboxId':p['sandboxId'],'templateId':p['expectedTemplateId'],'metadata':p['expectedMetadata'],'state':'running'}
  if mode=='attachMismatch':result['sandboxId']='other-sandbox'
 elif op=='runCommand':
  pid+=1;contexts[pid]=p.get('envs',{});result={'sandboxId':p['sandboxId'],'pid':pid}
 elif op in ('waitCommand','invokeControlHelper'):
  data=b'{"activeProcessCount":0}' if op=='invokeControlHelper' and p['helper']=='agent-process-state' else b''
  if op=='waitCommand' and mode=='maintenanceCommand':data=output
  result={'sandboxId':p['sandboxId'],'pid':p.get('pid',999),'exitCode':0,'stdout':data.decode(errors='replace'),'stderr':'','stdoutBytesBase64':base64.b64encode(data).decode(),'stderrBytesBase64':'','stdoutByteLength':len(data),'stderrByteLength':0}
  if op=='invokeControlHelper':result['helper']=p['helper']
  if mode=='helperExitError' and sequence==5:result['exitCode']=7
  env=contexts.get(p.get('pid'),{})
  if env.get('WORKSPACE_PERF_OBSERVATIONS')=='1' and mode!='default':
   stderr=b'private diagnostic and credential must never be forwarded\n'
   for metric,value in [('scannedEntries',2),('sourceReadBytes',51)]:
    event={'schema':'workspace.perf.v1','source':'sandbox','phase':'workspaceProtection','operationId':env['WORKSPACE_PERF_OPERATION_ID'],'metric':metric,'value':value,'outcome':'error' if mode=='helperError' else 'ok'}
    stderr+=json.dumps(event).encode()+b'\n'
   event['message']='private injected field'; stderr+=json.dumps(event).encode()+b'\n'
   result.update({'stderrBytesBase64':base64.b64encode(stderr).decode(),'stderrByteLength':len(stderr),'exitCode':7 if mode=='helperError' else 0})
 elif op=='listDirectory':
  result={'sandboxId':p['sandboxId'],'entries':[{'path':p['path']+'/result.bin','kind':'file','sizeBytes':len(output)}]}
 elif op=='uploadFile':
  data=(root/(p['transferId']+'.upload')).read_bytes()
  assert hashlib.sha256(data).hexdigest()==p['sha256'] and len(data)==p['byteLength']
  result={'sandboxId':p['sandboxId'],'transferId':p['transferId'],'byteLength':len(data),'sha256':hashlib.sha256(data).hexdigest()}
 elif op=='downloadFile':
  if mode=='partialDownload':
   with (root/(p['transferId']+'.download')).open('xb') as stream:stream.write(b'partial')
   print(json.dumps({'schema':'runtime.e2b.adapter.response.v1','requestId':request['requestId'],'outcome':'unknown','errorCode':'unknown'}),flush=True)
   continue
  with (root/(p['transferId']+'.download')).open('xb') as stream:stream.write(output)
  result={'sandboxId':p['sandboxId'],'transferId':p['transferId'],'byteLength':len(output),'sha256':hashlib.sha256(output).hexdigest()}
 else:raise AssertionError('unexpected operation')
 print(json.dumps({'schema':'runtime.e2b.adapter.response.v1','requestId':request['requestId'],'outcome':'succeeded','result':result}),flush=True)
"#).arg(log).arg(frame.iter().map(|b|format!("{b:02x}")).collect::<String>()).arg(mode);
    Arc::new(SpoolingAdapterProcess::spawn(command).unwrap())
}
pub(crate) fn requests(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn transport(
    ordinary: Arc<SpoolingAdapterProcess>,
    control: Arc<SpoolingAdapterProcess>,
) -> E2bSdkTransport {
    E2bSdkTransport::new(ordinary, control, super::super::tests::profile())
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
    let transport = transport(ordinary, control);
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
    assert_eq!(helper["envs"]["CENTAERIS_WORKSPACE_USER"], "10001:10001");
    assert!(helper["envs"].get("WORKSPACE_PERF_OBSERVATIONS").is_none());
    assert!(helper["envs"].get("WORKSPACE_PERF_OPERATION_ID").is_none());
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
    let transport = transport(process(&log, b""), process(&control_log, b""));
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
    let transport = transport(
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
    let transport = transport(process(&log, b""), process(&control_log, b""));
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

#[test]
fn filesystem_helper_keeps_private_frames_but_runs_as_workload_identity() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let transport = transport(
        process(&log, b"result"),
        process(&temp.path().join("control.jsonl"), b""),
    );
    transport
        .helper(&binding(), TrustedHelperKind::FileSystem, b"input", 1024)
        .unwrap();
    let seen = requests(&log);
    let helper = &seen[3]["payload"];
    assert_eq!(
        helper["user"], "root",
        "the SDK broker must open the root-private frame descriptors"
    );
    let command = helper["command"].as_str().unwrap();
    assert!(command.contains("exec '/usr/bin/setpriv' '--reuid=10001' '--regid=10001' '--clear-groups' '--' '/opt/centaeris/bin/execution_agent' 'filesystem-once'"), "the dispatched filesystem helper must drop to the frozen workload UID before processing the frame");
    assert_eq!(seen[2]["payload"]["user"], "root");
    assert_eq!(seen[5]["payload"]["user"], "root");
}

#[test]
fn observed_capture_propagates_operation_context_to_the_dispatched_guest_helper() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let transport = transport(
        process(&log, b"capture-frame"),
        process(&temp.path().join("control.jsonl"), b""),
    );
    let (id, events) = crate::observations::capture_protection(|| {
        let observation = WorkspaceProtection::new().with_context(
            crate::observations::ProtectionTrigger::TerminalCommit,
            "run-a",
            Some("job-a"),
        );
        assert_eq!(
            transport
                .helper_observed(
                    &binding(),
                    TrustedHelperKind::SnapshotCapture,
                    b"request",
                    1024,
                    &observation
                )
                .unwrap(),
            b"capture-frame"
        );
        observation.operation_id().to_owned()
    });
    let seen = requests(&log);
    assert_eq!(seen.len(), 8, "observations add no SDK roundtrip");
    assert_eq!(
        seen[3]["payload"]["envs"]["WORKSPACE_PERF_OBSERVATIONS"],
        "1"
    );
    assert_eq!(
        seen[3]["payload"]["envs"]["WORKSPACE_PERF_OPERATION_ID"],
        id
    );
    assert!(seen[0]["payload"]["envs"]
        .get("WORKSPACE_PERF_OPERATION_ID")
        .is_none());
    let count = events
        .iter()
        .find(|e| e["metric"] == "sdkDownloadFrameBytes")
        .unwrap();
    assert_eq!(count["value"], 13);
    assert_eq!(count["outcome"], "ok");
    assert!(events
        .iter()
        .all(|e| e["operationId"] == id && e["runId"] == "run-a" && e["jobId"] == "job-a"));
}

#[test]
fn observed_capture_forwards_only_valid_numeric_guest_records_even_on_helper_failure() {
    for mode in ["guestSuccess", "helperError", "partialDownload"] {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("ordinary.jsonl");
        let transport = transport(
            process_with_capture_outcome(&log, b"capture-frame", mode),
            process(&temp.path().join("control.jsonl"), b""),
        );
        let (result, events) = crate::observations::capture_protection(|| {
            let observation = WorkspaceProtection::new().with_context(
                crate::observations::ProtectionTrigger::TerminalCommit,
                "run-capture",
                Some("job-capture"),
            );
            transport.helper_observed(
                &binding(),
                TrustedHelperKind::SnapshotCapture,
                b"request",
                1024,
                &observation,
            )
        });
        if mode == "guestSuccess" {
            assert_eq!(result.unwrap(), b"capture-frame");
        } else {
            assert!(matches!(
                result,
                Err(E2bHostError::Unknown(
                    "e2b_helper_process_failed" | "e2b_helper_output_staging_unconfirmed"
                ))
            ));
        }
        for (metric, value) in [("sourceReadBytes", 51), ("scannedEntries", 2)] {
            let counters: Vec<_> = events.iter().filter(|e| e["metric"] == metric).collect();
            assert_eq!(counters.len(), 1);
            assert_eq!(counters[0]["source"], "sandbox");
            assert_eq!(counters[0]["value"], value);
            assert_eq!(
                counters[0]["outcome"],
                if mode == "helperError" { "error" } else { "ok" }
            );
        }
        let download = events
            .iter()
            .find(|e| e["metric"] == "sdkDownloadFrameBytes")
            .unwrap();
        if mode == "guestSuccess" {
            assert_eq!(download["value"], 13);
        } else {
            assert_eq!(download["outcome"], "unavailable");
            assert!(download["value"].is_null());
        }
        assert!(events
            .iter()
            .all(|e| e["runId"] == "run-capture" && e["jobId"] == "job-capture"));
        assert!(!serde_json::to_string(&events).unwrap().contains("private"));
        let seen = requests(&log);
        assert_eq!(
            seen.len(),
            if mode == "guestSuccess" {
                8
            } else if mode == "helperError" {
                5
            } else {
                6
            }
        );
    }
}

fn helper_timings(events: Vec<Value>) -> Vec<Value> {
    events
        .into_iter()
        .filter(|event| event["phase"] == "e2bHelper")
        .collect()
}

#[test]
fn helper_timing_records_each_frame_kind_and_serial_stage_without_extra_requests() {
    use crate::observations::{capture_timings, Context};
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let kinds = [
        TrustedHelperKind::FileSystem,
        TrustedHelperKind::InputInventory,
        TrustedHelperKind::MaterializeInput,
        TrustedHelperKind::RevokeInput,
        TrustedHelperKind::ReadArtifact,
        TrustedHelperKind::SnapshotCollect,
        TrustedHelperKind::SnapshotCapture,
        TrustedHelperKind::SnapshotRestore,
        TrustedHelperKind::WorkspaceGeneration,
    ];
    let (_, events) = capture_timings(true, || {
        let _context = Context::enter(br#"{"jobId":"agent_run.lifecycle:helper-timing"}"#);
        let transport = transport(
            process(&log, b"private response bytes"),
            process(&control_log, b""),
        );
        for kind in kinds {
            let input = if kind.input_limit() == 0 {
                b"".as_slice()
            } else {
                b"private request /secret/path".as_slice()
            };
            assert_eq!(
                transport.helper(&binding(), kind, input, 1024).unwrap(),
                b"private response bytes"
            );
        }
    });
    let events = helper_timings(events);
    assert_eq!(events.len(), kinds.len() * 7);
    let mut identities = std::collections::HashSet::new();
    for (records, kind) in events.chunks_exact(7).zip(kinds) {
        identities.insert(records[0]["operationId"].as_str().unwrap().to_owned());
        for (record, metric) in records.iter().zip([
            "mkdirElapsedMs",
            "uploadElapsedMs",
            "executeElapsedMs",
            "waitElapsedMs",
            "downloadElapsedMs",
            "cleanupElapsedMs",
            "totalElapsedMs",
        ]) {
            assert_eq!(record["schema"], "workspace.perf.v1");
            assert_eq!(record["source"], "runtime");
            assert_eq!(record["helperKind"], serde_json::to_value(kind).unwrap());
            assert_eq!(record["metric"], metric);
            assert_eq!(record["outcome"], "ok");
            assert_eq!(record["operationId"], records[0]["operationId"]);
            assert_eq!(record["jobId"], "agent_run.lifecycle:helper-timing");
            assert!(record["value"]
                .as_f64()
                .is_some_and(|value| value.is_finite() && value >= 0.0));
            assert!(record["atUnixMs"].as_u64().is_some());
            assert_eq!(record.as_object().unwrap().len(), 10);
        }
    }
    assert_eq!(identities.len(), kinds.len());
    let seen = requests(&log);
    assert_eq!(seen.len(), kinds.len() * 8);
    for frame in seen.chunks_exact(8) {
        assert_eq!(
            frame
                .iter()
                .map(|r| r["operation"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
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
    }
    assert!(!control_log.exists());
    assert!(!serde_json::to_string(&events).unwrap().contains("private"));
}

#[test]
fn helper_timing_controls_are_counted_once_per_actual_kind_without_frame_staging() {
    use crate::observations::{capture_timings, Context};
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let (_, events) = capture_timings(true, || {
        let _context = Context::enter(br#"{"jobId":"agent_run.lifecycle:helper-control"}"#);
        let transport = transport(process(&log, b""), process(&control_log, b""));
        for kind in [
            TrustedHelperKind::PrepareWorkspace,
            TrustedHelperKind::AgentProcessState,
            TrustedHelperKind::QuiesceAgentProcesses,
        ] {
            transport.helper(&binding(), kind, b"", 1024).unwrap();
        }
        transport.inspect(&binding()).unwrap();
        transport.quiesce(&binding()).unwrap();
    });
    let events = helper_timings(events);
    assert_eq!(events.len(), 10);
    for (records, kind) in events.chunks_exact(2).zip([
        "prepareWorkspace",
        "agentProcessState",
        "quiesceAgentProcesses",
        "agentProcessState",
        "quiesceAgentProcesses",
    ]) {
        assert_eq!(records[0]["metric"], "controlElapsedMs");
        assert_eq!(records[1]["metric"], "totalElapsedMs");
        assert_eq!(records[0]["operationId"], records[1]["operationId"]);
        assert!(records.iter().all(|record| record["helperKind"] == kind
            && record["outcome"] == "ok"
            && record["jobId"] == "agent_run.lifecycle:helper-control"));
    }
    assert_eq!(requests(&control_log).len(), 5);
    assert!(!log.exists());
}

#[test]
fn helper_timing_failure_records_only_reached_stages_and_preserves_unknown_without_retry() {
    use crate::observations::capture_timings;
    let stages = [
        "mkdirElapsedMs",
        "uploadElapsedMs",
        "executeElapsedMs",
        "waitElapsedMs",
        "downloadElapsedMs",
        "cleanupElapsedMs",
    ];
    for (mode, reached, calls, error) in [
        ("mkdirUnknown", 1, 1, "e2b_adapter_result_unknown"),
        (
            "uploadUnknown",
            2,
            3,
            "e2b_helper_input_staging_unconfirmed",
        ),
        ("executeUnknown", 3, 4, "e2b_adapter_result_unknown"),
        ("waitUnknown", 4, 5, "e2b_adapter_result_unknown"),
        ("helperExitError", 4, 5, "e2b_helper_process_failed"),
        (
            "downloadUnknown",
            5,
            6,
            "e2b_helper_output_staging_unconfirmed",
        ),
        ("cleanupUnknown", 6, 7, "e2b_adapter_result_unknown"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("ordinary.jsonl");
        let control_log = temp.path().join("control.jsonl");
        let (result, events) = capture_timings(true, || {
            let transport = transport(
                process_with_capture_outcome(&log, b"result", mode),
                process(&control_log, b""),
            );
            transport.helper(
                &binding(),
                TrustedHelperKind::FileSystem,
                b"private input",
                1024,
            )
        });
        assert_eq!(result, Err(E2bHostError::Unknown(error)), "{mode}");
        let events = helper_timings(events);
        assert_eq!(events.len(), reached + 1, "{mode}");
        for (index, event) in events[..reached].iter().enumerate() {
            assert_eq!(event["metric"], stages[index], "{mode}");
            assert_eq!(
                event["outcome"],
                if index + 1 == reached { "error" } else { "ok" },
                "{mode}"
            );
        }
        assert_eq!(events[reached]["metric"], "totalElapsedMs");
        assert_eq!(events[reached]["outcome"], "error");
        assert_eq!(
            requests(&log).len(),
            calls,
            "failed or unknown helper must never retry or perform later cleanup"
        );
        assert!(!control_log.exists());
        assert!(!serde_json::to_string(&events).unwrap().contains("private"));
    }
}

#[test]
fn helper_timing_rejection_and_opt_out_preserve_existing_dispatch_and_results() {
    use crate::observations::capture_timings;
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let (_, events) = capture_timings(true, || {
        let transport = transport(process(&log, b""), process(&control_log, b""));
        for kind in [
            TrustedHelperKind::WorkspaceWatch,
            TrustedHelperKind::WorkspaceGenerationRpc,
        ] {
            assert_eq!(
                transport.helper(&binding(), kind, b"", 1024),
                Err(E2bHostError::NotDispatched("e2b_helper_budget_unsupported"))
            );
        }
        assert_eq!(
            transport.helper(&binding(), TrustedHelperKind::FileSystem, b"", 0),
            Err(E2bHostError::NotDispatched("e2b_helper_budget_unsupported"))
        );
        assert_eq!(
            transport.helper(
                &binding(),
                TrustedHelperKind::PrepareWorkspace,
                b"invalid control input",
                1024
            ),
            Err(E2bHostError::NotDispatched(
                "e2b_control_helper_input_invalid"
            ))
        );
    });
    let events = helper_timings(events);
    assert_eq!(events.len(), 4);
    assert!(events
        .iter()
        .all(|event| event["metric"] == "totalElapsedMs" && event["outcome"] == "error"));
    assert!(!log.exists() && !control_log.exists());
    let (_, events) = capture_timings(false, || {
        let transport = transport(process(&log, b"result"), process(&control_log, b""));
        assert_eq!(
            transport
                .helper(&binding(), TrustedHelperKind::FileSystem, b"input", 1024)
                .unwrap(),
            b"result"
        );
        assert!(transport.inspect(&binding()).unwrap().running);
        transport.quiesce(&binding()).unwrap();
    });
    assert!(events.is_empty());
    assert_eq!(requests(&log).len(), 8);
    assert_eq!(requests(&control_log).len(), 2);
}

#[test]
fn helper_timing_carries_safe_job_context_across_threads_and_restores_the_worker_context() {
    use crate::observations::{capture_timings, Context, Span};
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let (transport, _) = capture_timings(true, || {
        let _context = Context::enter(br#"{"jobId":"agent_run.lifecycle:helper-owner"}"#);
        transport(process(&log, b"result"), process(&control_log, b""))
    });
    let events = std::thread::spawn(move || {
        let (_, events) = capture_timings(true, || {
            transport
                .helper(&binding(), TrustedHelperKind::FileSystem, b"input", 1024)
                .unwrap();
            let mut after = Span::new("afterHelper");
            after.finished(true);
            drop(after);
            {
                let _context = Context::enter(br#"{"jobId":"agent_run.lifecycle:current"}"#);
                transport.inspect(&binding()).unwrap();
            }
            let mut restored = Span::new("afterControlHelper");
            restored.finished(true);
        });
        events
    })
    .join()
    .unwrap();
    let helper_events = events
        .iter()
        .filter(|e| e["phase"] == "e2bHelper")
        .collect::<Vec<_>>();
    assert_eq!(helper_events.len(), 9);
    assert!(helper_events[..7]
        .iter()
        .all(|event| event["jobId"] == "agent_run.lifecycle:helper-owner"));
    assert!(helper_events[7..]
        .iter()
        .all(|event| event["jobId"] == "agent_run.lifecycle:current"));
    assert!(events
        .iter()
        .filter(|e| e["phase"] == "afterHelper" || e["phase"] == "afterControlHelper")
        .all(|e| e.get("jobId").is_none()));
}

#[cfg(target_os = "linux")]
struct NativeHelperWrapper {
    _root: tempfile::TempDir,
    command: String,
    response: PathBuf,
    workspace_file: PathBuf,
}

#[cfg(target_os = "linux")]
impl NativeHelperWrapper {
    fn run(&self) -> std::process::Output {
        Command::new("/bin/bash")
            .args(["-c", &self.command])
            .env("CENTAERIS_TEST_WORKSPACE_FILE", &self.workspace_file)
            .output()
            .unwrap()
    }
}

#[cfg(target_os = "linux")]
fn native_restore_wrapper(output_limit: usize, helper_body: &str) -> NativeHelperWrapper {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("ordinary.jsonl");
    let transport = transport(
        process(&log, b""),
        process(&root.path().join("control.jsonl"), b""),
    );
    transport
        .helper(
            &binding(),
            TrustedHelperKind::SnapshotRestore,
            b"fixture request",
            output_limit,
        )
        .unwrap();
    let seen = requests(&log);
    let remote_request = seen[2]["payload"]["path"].as_str().unwrap();
    let remote_response = seen[5]["payload"]["path"].as_str().unwrap();
    let request = root.path().join("request");
    let response = root.path().join("response");
    let workspace_file = root.path().join("restored-workspace-file");
    let helper = root.path().join("fixture-helper");
    std::fs::write(&request, b"fixture request").unwrap();
    std::fs::write(
        &helper,
        format!(
            "#!/bin/bash\nset -e\n[[ \"$1\" == snapshot-restore ]] || exit 23\n{helper_body}\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();

    // Exercise the command sent by the real transport; only its fixed helper
    // and generated private frame paths are redirected into this owned fixture.
    let command = seen[3]["payload"]["command"]
        .as_str()
        .unwrap()
        .replace(&quote(HELPER_PATH), &quote(helper.to_str().unwrap()))
        .replace(&quote(remote_request), &quote(request.to_str().unwrap()))
        .replace(&quote(remote_response), &quote(response.to_str().unwrap()));
    NativeHelperWrapper {
        _root: root,
        command,
        response,
        workspace_file,
    }
}

#[cfg(target_os = "linux")]
#[test]
fn restore_wrapper_limits_stdout_without_limiting_restored_workspace_files() {
    let wrapper = native_restore_wrapper(
        1024 * 1024,
        "printf '%*s' 8388608 '' > \"$CENTAERIS_TEST_WORKSPACE_FILE\"\nprintf 'restored'",
    );
    let result = wrapper.run();
    assert!(
        result.status.success(),
        "a bounded restore receipt must not impose its limit on workspace writes: status={:?}, stderr={}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::metadata(&wrapper.workspace_file).unwrap().len(),
        8 * 1024 * 1024
    );
    assert_eq!(std::fs::read(&wrapper.response).unwrap(), b"restored");
}

#[cfg(target_os = "linux")]
#[test]
fn restore_wrapper_rejects_stdout_overflow_with_a_bounded_response_file() {
    let output_limit = 1031;
    let wrapper = native_restore_wrapper(output_limit, "printf '%*s' 8192 ''");
    let result = wrapper.run();
    assert!(
        !result.status.success(),
        "an oversized helper result must not be accepted"
    );
    assert!(
        std::fs::metadata(&wrapper.response).unwrap().len() <= output_limit as u64 + 1,
        "the response sink may retain at most one overflow-detection byte"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn restore_wrapper_accepts_stdout_at_the_exact_requested_budget() {
    let output_limit = 1031;
    let wrapper = native_restore_wrapper(output_limit, "printf '%*s' 1031 ''");
    let result = wrapper.run();
    assert!(
        result.status.success(),
        "a complete helper result at the exact byte limit must remain valid"
    );
    assert_eq!(
        std::fs::read(&wrapper.response).unwrap(),
        vec![b' '; output_limit]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn restore_wrapper_preserves_nonzero_helper_exit_after_small_output() {
    let wrapper = native_restore_wrapper(1024, "printf 'plausible-but-failed'\nexit 7");
    let result = wrapper.run();
    assert_eq!(
        result.status.code(),
        Some(7),
        "collecting bounded stdout must not turn helper failure into success"
    );
    assert_eq!(
        std::fs::read(&wrapper.response).unwrap(),
        b"plausible-but-failed"
    );
}

#[test]
fn managed_workspace_preparation_uses_control_without_staging_or_generic_shell() {
    let temp = tempfile::tempdir().unwrap();
    let ordinary_log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let transport = transport(process(&ordinary_log, b""), process(&control_log, b""));
    transport
        .helper(
            &binding(),
            TrustedHelperKind::PrepareManagedWorkspace,
            b"",
            1024,
        )
        .unwrap();
    assert!(
        control_log.exists(),
        "managed initialization must use the fixed control lane"
    );
    assert!(
        !ordinary_log.exists(),
        "managed initialization must not stage or run a generic shell"
    );
    let sent = requests(&control_log);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["operation"], "invokeControlHelper");
    assert_eq!(sent[0]["payload"]["helper"], "prepare-managed-workspace");
    assert_eq!(
        sent[0]["payload"]["expectedMetadata"],
        serde_json::to_value(binding().metadata).unwrap()
    );
    assert!(!ordinary_log.exists());
}

#[test]
fn task_image_command_uses_frozen_user_and_requested_working_directory() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let mut profile = serde_json::to_value(super::super::tests::profile()).unwrap();
    profile["user"] = "root".into();
    profile["workspaceRoot"] = "/app".into();
    let transport = E2bSdkTransport::new(
        process(&log, b""),
        process(&temp.path().join("control.jsonl"), b""),
        serde_json::from_value(profile).unwrap(),
    );
    let request = ExecutionCommandRequest {
        program: "/bin/bash".into(),
        args: vec!["-c".into(), "printf task-image".into()],
        cwd: "/app".into(),
        env: HashMap::from([("TASK_FLAG".into(), "preserved".into())]),
        timeout_ms: 1000,
        policy: ExecutionPolicy::workspace_write_no_network("/app"),
    };
    transport.start(&binding(), &request).unwrap();
    let seen = requests(&log);
    assert_eq!(seen[0]["payload"]["user"], "root");
    assert_eq!(seen[0]["payload"]["cwd"], "/app");
    assert_eq!(seen[0]["payload"]["envs"]["TASK_FLAG"], "preserved");
}

fn task_image_transport(
    ordinary: Arc<SpoolingAdapterProcess>,
    control: Arc<SpoolingAdapterProcess>,
) -> E2bSdkTransport {
    let mut profile = serde_json::to_value(super::super::tests::profile()).unwrap();
    profile["workspaceInitialization"] = json!("taskImage");
    profile["lifetime"] = json!("retained");
    profile["workspaceRoot"] = json!("/app");
    profile["user"] = json!("root");
    E2bSdkTransport::new(ordinary, control, serde_json::from_value(profile).unwrap())
}
#[test]
fn task_image_health_uses_read_only_native_attach_with_original_identity() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let transport = task_image_transport(process(&log, b""), process(&control_log, b""));
    let observed = transport.inspect(&binding()).unwrap();
    assert_eq!(observed.binding, binding());
    assert!(observed.running && observed.on_timeout_kill && !observed.auto_resume);
    let seen = requests(&log);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["operation"], "attach");
    assert_eq!(seen[0]["payload"]["workspaceRoot"], "/app");
    assert_eq!(seen[0]["payload"]["workspaceUser"], "root");
    assert_eq!(
        seen[0]["payload"]["expectedMetadata"],
        serde_json::to_value(binding().metadata).unwrap()
    );
    assert!(!control_log.exists());
}
#[test]
fn task_image_rejects_mismatched_attach_and_unimplemented_helpers_before_effects() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let transport = task_image_transport(
        process_with_capture_outcome(&log, b"", "attachMismatch"),
        process(&control_log, b""),
    );
    assert!(transport.inspect(&binding()).is_err());
    for kind in [
        TrustedHelperKind::FileSystem,
        TrustedHelperKind::SnapshotCapture,
        TrustedHelperKind::SnapshotRestore,
    ] {
        assert_eq!(
            transport.helper(&binding(), kind, &[], 1024),
            Err(E2bHostError::NotDispatched(
                "e2b_task_image_helper_unsupported"
            ))
        );
    }
    assert_eq!(
        transport.quiesce(&binding()),
        Err(E2bHostError::NotDispatched(
            "e2b_task_image_quiescence_unsupported"
        ))
    );
    assert_eq!(requests(&log).len(), 1);
    assert!(!control_log.exists());
}

struct TaskImageAuthority;
impl E2bDispatchAuthority for TaskImageAuthority {
    fn dispatch(
        &self,
        operation: &mut (dyn FnMut() -> Result<E2bDispatchResult, E2bHostError> + Send),
    ) -> Result<E2bDispatchResult, E2bHostError> {
        operation()
    }
}
#[test]
fn task_image_preparation_and_closed_run_cleanup_preserve_background_services() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("ordinary.jsonl");
    let control_log = temp.path().join("control.jsonl");
    let transport = task_image_transport(process(&log, b""), process(&control_log, b""));
    let profile = transport.profile.clone();
    let mut binding = binding();
    binding
        .metadata
        .insert("profileDigest".into(), profile.digest().unwrap());
    let runner = E2bExecutionHostRunner::new(
        profile,
        binding.clone(),
        Arc::new(transport),
        Arc::new(TaskImageAuthority),
    )
    .unwrap();
    runner.prepare_workspace().unwrap();
    runner
        .confirm_run_quiescence_under_cleanup_pins(
            &binding.metadata["instanceId"],
            &binding.sandbox_id,
        )
        .unwrap();
    let seen = requests(&log);
    assert_eq!(
        seen.iter()
            .map(|r| r["operation"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["attach", "runCommand", "waitCommand", "attach"]
    );
    assert_eq!(seen[1]["payload"]["user"], "root");
    assert_eq!(seen[1]["payload"]["cwd"], "/app");
    assert_eq!(
        seen[1]["payload"]["command"],
        "exec '/usr/bin/test' '-d' '.'"
    );
    assert!(!control_log.exists());
}

#[test]
fn plugin_projection_and_reads_use_native_sdk_binary_files_and_fixed_commands() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("native-plugin.jsonl");
    let bytes = [0, 255, 65];
    let process = process(&log, &bytes);
    let transport = E2bSdkTransport::new(process.clone(), process, super::super::tests::profile());
    let binding = super::super::tests::binding();
    let path = "/opt/centaeris/plugins/demo/result.bin";
    let projection = E2bPluginProjection {
        digest: "fixture".into(),
        files: BTreeMap::from([(
            path.into(),
            crate::e2b_plugin_projection::PluginFile {
                bytes: bytes.to_vec(),
                executable: true,
            },
        )]),
        directories: vec!["/opt/centaeris/plugins/demo".into()],
        command_directories: vec![],
    };
    transport.project_plugins(&binding, &projection).unwrap();
    assert_eq!(transport.read_plugin(&binding, path, 10).unwrap(), bytes);
    let requests = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let upload = requests
        .iter()
        .find(|r| r["operation"] == "uploadFile")
        .unwrap();
    assert_eq!(upload["payload"]["path"], path);
    assert_eq!(upload["payload"]["byteLength"], 3);
    assert!(requests
        .iter()
        .any(|r| r["operation"] == "downloadFile" && r["payload"]["path"] == path));
    assert!(requests
        .iter()
        .filter(|r| r["operation"] == "runCommand")
        .all(|r| r["payload"]["user"] == "root"));
}
#[cfg(unix)]
#[test]
fn plugin_command_path_preserves_template_or_request_environment_and_quotes_arguments() {
    let runner = super::super::tests::fixture_host();
    let request = ExecutionCommandRequest {
        program: "/usr/bin/printenv".into(),
        args: vec!["PATH".into()],
        cwd: "/mnt/data".into(),
        env: HashMap::new(),
        timeout_ms: 1000,
        policy: runner.execution_policy(),
    };
    for base in [
        "/opt/centaeris/venv/bin:/usr/bin:/bin",
        "/custom/bin:/usr/bin",
    ] {
        let script = command_script(&request, &["/opt/centaeris/plugins/demo/bin".into()]);
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("PATH", base)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            format!("/opt/centaeris/plugins/demo/bin:{base}")
        );
    }
    let request = ExecutionCommandRequest {
        program: "/usr/bin/printf".into(),
        args: vec!["%s".into(), "literal ' $HOME ; word".into()],
        ..request
    };
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(command_script(&request, &[]))
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"literal ' $HOME ; word");
}
