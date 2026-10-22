use super::*;
use serde_json::json;
use std::process::Command;
use std::time::Duration;

fn fixture(mode: &str) -> Command {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let python = std::env::var_os("E2B_ADAPTER_TEST_PYTHON")
        .map(std::path::PathBuf::from)
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
import json, sys, time
mode = sys.argv[1]
if mode == 'stallInput':
    time.sleep(30)
for line in sys.stdin:
    request = json.loads(line)
    if request['operation'] == 'block':
        from pathlib import Path
        Path(sys.argv[2]).write_text('entered')
        release = Path(sys.argv[3])
        deadline = time.monotonic() + 20
        while not release.exists():
            if time.monotonic() >= deadline:
                sys.exit(2)
            time.sleep(0.005)
    if mode == 'exit':
        sys.exit(1)
    if mode == 'oversize':
        sys.stdout.write('x' * (1024 * 1024 + 1) + '\n')
        sys.stdout.flush()
        continue
    response = {'schema': 'runtime.e2b.adapter.response.v1', 'requestId': request['requestId'],
                'outcome': 'succeeded', 'result': {'pid': 123, 'process': __import__('os').getpid()}}
    if mode == 'wrongId':
        response['requestId'] = 'another-request'
    print(json.dumps(response), flush=True)
"#);
    command.arg(mode);
    command
}

#[test]
fn persistent_adapter_reuses_one_sdk_process_and_correlates_responses() {
    let adapter = AdapterProcess::spawn(fixture("echo")).unwrap();
    let first = adapter
        .call("first", "runCommand", json!({}), Duration::from_secs(10))
        .unwrap();
    let second = adapter
        .call("second", "inspect", json!({}), Duration::from_secs(10))
        .unwrap();
    assert_eq!(
        first.result.unwrap()["process"],
        second.result.unwrap()["process"]
    );
}

#[test]
fn lost_adapter_reply_is_unknown_and_never_replayed() {
    let adapter = AdapterProcess::spawn(fixture("exit")).unwrap();
    assert_eq!(
        adapter.call("effect", "runCommand", json!({}), Duration::from_secs(10)),
        Err(AdapterFailure::Unknown("adapter_response_unavailable"))
    );
    assert_eq!(
        adapter.call("effect", "runCommand", json!({}), Duration::from_secs(1)),
        Err(AdapterFailure::NotDispatched("adapter_closed"))
    );
}

#[test]
fn wrong_response_identity_and_oversize_output_cannot_settle_an_attempt() {
    for mode in ["wrongId", "oversize"] {
        let adapter = AdapterProcess::spawn(fixture(mode)).unwrap();
        assert!(matches!(
            adapter.call("attempt", "create", json!({}), Duration::from_secs(10)),
            Err(AdapterFailure::Unknown(_))
        ));
        assert_eq!(
            adapter.call("next", "inspect", json!({}), Duration::from_secs(1)),
            Err(AdapterFailure::NotDispatched("adapter_closed"))
        );
    }
}

#[test]
fn blocked_ordinary_sdk_cannot_consume_control_process_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let entered = directory.path().join("entered");
    let release = directory.path().join("release");
    let mut ordinary_command = fixture("barrier");
    ordinary_command.arg(&entered).arg(&release);
    let ordinary = std::sync::Arc::new(AdapterProcess::spawn(ordinary_command).unwrap());
    let control = AdapterProcess::spawn(fixture("echo")).unwrap();
    let worker = ordinary.clone();
    let (finished, completion) = std::sync::mpsc::channel();
    let blocked = std::thread::spawn(move || {
        let result = worker.call("slow", "block", json!({}), Duration::from_secs(15));
        finished.send(()).unwrap();
        result
    });
    let admission_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !entered.exists() {
        assert!(
            std::time::Instant::now() < admission_deadline,
            "ordinary fixture did not enter the operation"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        ordinary.call("extra", "inspect", json!({}), Duration::from_secs(1)),
        Err(AdapterFailure::NotDispatched("adapter_busy"))
    );
    assert!(control
        .call("control", "inspect", json!({}), Duration::from_secs(2))
        .is_ok());
    assert!(
        matches!(
            completion.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "ordinary operation must still be blocked when control completes"
    );
    std::fs::write(&release, "released").unwrap();
    assert!(blocked.join().unwrap().is_ok());
}

#[test]
fn stalled_sdk_stdin_cannot_bypass_the_entire_call_deadline() {
    let adapter = AdapterProcess::spawn(fixture("stallInput")).unwrap();
    let started = std::time::Instant::now();
    let result = adapter.call(
        "large",
        "writeFile",
        json!({"data": "x".repeat(MAX_FRAME_BYTES - 1024)}),
        Duration::from_millis(300),
    );
    assert_eq!(
        result,
        Err(AdapterFailure::Unknown("adapter_deadline_exceeded"))
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "unread stdin must not hang the caller"
    );
    assert_eq!(
        adapter.call("next", "inspect", json!({}), Duration::from_secs(1)),
        Err(AdapterFailure::NotDispatched("adapter_closed"))
    );
}

#[test]
fn invalid_local_envelopes_do_not_dispatch() {
    let adapter = AdapterProcess::spawn(fixture("echo")).unwrap();
    assert!(matches!(
        adapter.call("", "inspect", json!({}), Duration::from_secs(1)),
        Err(AdapterFailure::NotDispatched(_))
    ));
    assert!(matches!(
        adapter.call("valid", "inspect", json!({}), Duration::ZERO),
        Err(AdapterFailure::NotDispatched(_))
    ));
    assert!(adapter
        .call("valid", "inspect", json!({}), Duration::from_secs(10))
        .is_ok());
}
