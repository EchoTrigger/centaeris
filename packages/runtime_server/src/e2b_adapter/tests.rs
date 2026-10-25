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
if mode == 'diagnostics':
    sys.stderr.write('private diagnostic '*1024+'\n')
    sys.stderr.flush()
if mode == 'stallInput':
    time.sleep(30)
for sequence, line in enumerate(sys.stdin, 1):
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
                'outcome': 'succeeded', 'result': {'pid': 123, 'process': __import__('os').getpid(), 'sequence': sequence}}
    if mode in ('unknown', 'notDispatched') and request['operation'] == 'create':
        response['outcome'] = mode
        response['result'] = None
        response['error'] = {'code': 'providerFailure', 'message': 'private-provider-diagnostic'}
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
        Err(AdapterFailure::NotDispatched(
            "adapter_deadline_before_dispatch"
        ))
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

#[test]
fn observed_sdk_lane_preserves_ipc_and_discards_nonmeasurement_diagnostics() {
    use crate::observations::power::{capture_power_observations, PowerObservation};
    let (response, records) = capture_power_observations(|| {
        let adapter = AdapterProcess::spawn_observed(
            fixture("diagnostics"),
            PowerObservation::new("power-transport", "pause"),
        )
        .unwrap();
        adapter
            .call("power-request", "pause", json!({}), Duration::from_secs(10))
            .unwrap()
    });
    assert_eq!(response.outcome, AdapterOutcome::Succeeded);
    assert_eq!(response.request_id, "power-request");
    assert!(records.is_empty());
}

#[test]
fn observed_sdk_lane_keeps_lost_ack_unknown_and_never_retries() {
    use crate::observations::power::PowerObservation;
    let adapter = AdapterProcess::spawn_observed(
        fixture("exit"),
        PowerObservation::new("power-lost", "resume"),
    )
    .unwrap();
    assert!(matches!(
        adapter.call("power-lost", "connect", json!({}), Duration::from_secs(10)),
        Err(AdapterFailure::Unknown(_))
    ));
    assert_eq!(
        adapter.call("power-lost", "connect", json!({}), Duration::from_secs(10)),
        Err(AdapterFailure::NotDispatched("adapter_closed"))
    );
}

#[test]
fn adapter_transport_timings_record_each_closed_operation_once_without_payloads() {
    use crate::observations::{capture_timings, Context};
    let operations = [
        ("capabilities", "e2bAdapterCallCapabilities"),
        ("create", "e2bAdapterCallCreate"),
        ("attach", "e2bAdapterCallAttach"),
        ("inspect", "e2bAdapterCallInspect"),
        ("kill", "e2bAdapterCallKill"),
        ("pause", "e2bAdapterCallPause"),
        ("connect", "e2bAdapterCallConnect"),
        ("readFile", "e2bAdapterCallReadFile"),
        ("writeFile", "e2bAdapterCallWriteFile"),
        ("uploadFile", "e2bAdapterCallUploadFile"),
        ("downloadFile", "e2bAdapterCallDownloadFile"),
        ("runCommand", "e2bAdapterCallRunCommand"),
        ("waitCommand", "e2bAdapterCallWaitCommand"),
        ("recoverCommand", "e2bAdapterCallRecoverCommand"),
        ("cancelCommand", "e2bAdapterCallCancelCommand"),
        ("invokeControlHelper", "e2bAdapterCallInvokeControlHelper"),
    ];
    let (_, records) = capture_timings(true, || {
        let _context = Context::enter(br#"{"jobId":"agent_run.lifecycle:timed"}"#);
        let adapter = AdapterProcess::spawn(fixture("echo")).unwrap();
        for (index, (operation, _)) in operations.iter().enumerate() {
            let response = adapter
                .call(
                    "private-request-id",
                    operation,
                    json!({"secret": "private-request-body"}),
                    Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(response.outcome, AdapterOutcome::Succeeded);
            assert_eq!(response.result.unwrap()["sequence"], index + 1);
        }
    });
    assert_eq!(records.len(), operations.len());
    for (record, (_, phase)) in records.iter().zip(operations) {
        assert_eq!(record["schema"], "workspace.perf.v1");
        assert_eq!(record["source"], "runtime");
        assert_eq!(record["phase"], phase);
        assert_eq!(record["jobId"], "agent_run.lifecycle:timed");
        assert_eq!(record["outcome"], "ok");
        assert!(record["elapsedMs"].as_f64().unwrap() >= 0.0);
        assert!(record["atUnixMs"].as_u64().is_some());
        assert_eq!(record.as_object().unwrap().len(), 7);
    }
}

#[test]
fn adapter_transport_timings_preserve_unknown_rejection_and_lost_ack_without_replay() {
    use crate::observations::capture_timings;
    for outcome in ["unknown", "notDispatched", "exit"] {
        let (_, records) = capture_timings(true, || {
            let adapter = AdapterProcess::spawn(fixture(outcome)).unwrap();
            let response = adapter.call("attempt", "create", json!({}), Duration::from_secs(10));
            if outcome == "exit" {
                assert_eq!(
                    response,
                    Err(AdapterFailure::Unknown("adapter_response_unavailable"))
                );
                assert_eq!(
                    adapter.call("probe", "inspect", json!({}), Duration::from_secs(10)),
                    Err(AdapterFailure::NotDispatched("adapter_closed"))
                );
            } else {
                let response = response.unwrap();
                assert_eq!(
                    response.outcome,
                    if outcome == "unknown" {
                        AdapterOutcome::Unknown
                    } else {
                        AdapterOutcome::NotDispatched
                    }
                );
                assert_eq!(
                    response.error.unwrap().message,
                    "private-provider-diagnostic"
                );
                let probe = adapter
                    .call("probe", "inspect", json!({}), Duration::from_secs(10))
                    .unwrap();
                assert_eq!(
                    probe.result.unwrap()["sequence"],
                    2,
                    "observations must never replay create"
                );
            }
        });
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["phase"], "e2bAdapterCallCreate");
        assert_eq!(records[0]["outcome"], "error");
        assert_eq!(records[1]["phase"], "e2bAdapterCallInspect");
        assert_eq!(
            records[1]["outcome"],
            if outcome == "exit" { "error" } else { "ok" }
        );
        assert!(!serde_json::to_string(&records)
            .unwrap()
            .contains("private-provider-diagnostic"));
    }
}

#[test]
fn adapter_transport_timings_keep_local_rejection_opt_out_and_open_operation_semantics() {
    use crate::observations::capture_timings;
    for enabled in [false, true] {
        let (_, records) = capture_timings(enabled, || {
            let adapter = AdapterProcess::spawn(fixture("echo")).unwrap();
            assert_eq!(
                adapter.call("invalid", "create", json!([]), Duration::from_secs(10)),
                Err(AdapterFailure::NotDispatched("adapter_request_invalid"))
            );
            assert_eq!(
                adapter.call(
                    "invalid",
                    "unsupported\noperation",
                    json!({}),
                    Duration::from_secs(10)
                ),
                Err(AdapterFailure::NotDispatched("adapter_request_invalid"))
            );
            let response = adapter
                .call("valid", "inspect", json!({}), Duration::from_secs(10))
                .unwrap();
            assert_eq!(
                response.result.unwrap()["sequence"],
                1,
                "invalid envelopes must not dispatch"
            );
            // This low-level transport already accepts bounded operation names;
            // the SDK owns its supported-operation policy. Timing never widens
            // its closed observation labels to match arbitrary names.
            let response = adapter
                .call(
                    "opaque",
                    "private-opaque-operation",
                    json!({}),
                    Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(response.result.unwrap()["sequence"], 2);
        });
        if enabled {
            assert_eq!(records.len(), 2);
            assert_eq!(records[0]["phase"], "e2bAdapterCallCreate");
            assert_eq!(records[0]["outcome"], "error");
            assert_eq!(records[1]["phase"], "e2bAdapterCallInspect");
            assert_eq!(records[1]["outcome"], "ok");
        } else {
            assert!(records.is_empty());
        }
    }
}

#[test]
fn adapter_transport_timings_carry_owned_job_into_worker_without_overriding_or_leaking_context() {
    use crate::observations::{capture_timings, Context, Span};
    let (adapter, _) = capture_timings(true, || {
        let _context = Context::enter(br#"{"jobId":"agent_run.lifecycle:owner"}"#);
        AdapterProcess::spawn(fixture("echo")).unwrap()
    });
    let records = std::thread::spawn(move || {
        let (_, records) = capture_timings(true, || {
            let first = adapter
                .call("first", "inspect", json!({}), Duration::from_secs(10))
                .unwrap();
            assert_eq!(first.result.unwrap()["sequence"], 1);
            let mut after = Span::new("testAfterAdapterCall");
            after.finished(true);
            drop(after);
            {
                let _context = Context::enter(br#"{"jobId":"agent_run.lifecycle:current"}"#);
                let second = adapter
                    .call("second", "inspect", json!({}), Duration::from_secs(10))
                    .unwrap();
                assert_eq!(second.result.unwrap()["sequence"], 2);
                let mut existing = Span::new("testInsideExistingContext");
                existing.finished(true);
            }
            let mut restored = Span::new("testAfterExistingContext");
            restored.finished(true);
        });
        records
    })
    .join()
    .unwrap();
    assert_eq!(records.len(), 5);
    assert_eq!(records[0]["phase"], "e2bAdapterCallInspect");
    assert_eq!(records[0]["jobId"], "agent_run.lifecycle:owner");
    assert!(records[1].get("jobId").is_none());
    assert_eq!(records[2]["phase"], "e2bAdapterCallInspect");
    assert_eq!(records[2]["jobId"], "agent_run.lifecycle:current");
    assert_eq!(records[3]["jobId"], "agent_run.lifecycle:current");
    assert!(records[4].get("jobId").is_none());
}

fn blocking_adapter() -> (
    std::sync::Arc<AdapterProcess>,
    tempfile::TempDir,
    std::thread::JoinHandle<Result<AdapterResponse, AdapterFailure>>,
) {
    let directory = tempfile::tempdir().unwrap();
    let entered = directory.path().join("entered");
    let mut command = fixture("barrier");
    command.arg(&entered).arg(directory.path().join("release"));
    let adapter = std::sync::Arc::new(AdapterProcess::spawn(command).unwrap());
    let worker = adapter.clone();
    let running = std::thread::spawn(move || {
        worker.call("held", "block", json!({}), Duration::from_secs(15))
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !entered.exists() {
        assert!(
            Instant::now() < deadline,
            "SDK fixture never received first frame"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    (adapter, directory, running)
}

#[test]
fn short_sdk_calls_queue_without_rejection_or_frame_mixing() {
    let (adapter, directory, running) = blocking_adapter();
    let waiting = adapter.clone();
    let (began, started) = mpsc::channel();
    let (finished, completion) = mpsc::channel();
    let queued = std::thread::spawn(move || {
        began.send(()).unwrap();
        let result = waiting.call("queued", "inspect", json!({}), Duration::from_secs(5));
        finished.send(result).unwrap();
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    let premature = completion.recv_timeout(Duration::from_millis(80));
    std::fs::write(directory.path().join("release"), "released").unwrap();
    let first = running.join().unwrap().unwrap();
    let was_pending = matches!(&premature, Err(mpsc::RecvTimeoutError::Timeout));
    let second = match premature {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            completion.recv_timeout(Duration::from_secs(5)).unwrap()
        }
        Err(error) => panic!("queued worker disconnected: {error}"),
    };
    queued.join().unwrap();
    assert!(
        was_pending,
        "a short concurrent request was rejected while the SDK lane was still healthy: {second:?}"
    );
    let second = second.unwrap();
    assert_eq!(first.request_id, "held");
    assert_eq!(first.result.as_ref().unwrap()["sequence"], 1);
    assert_eq!(second.request_id, "queued");
    assert_eq!(second.result.as_ref().unwrap()["sequence"], 2);
    assert_eq!(
        first.result.unwrap()["process"],
        second.result.unwrap()["process"]
    );
}

fn wait_for_adapter_queue(adapter: &AdapterProcess, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let queued = adapter.streams.state.lock().unwrap().waiters.len();
        if queued == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected} queued calls, observed {queued}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn queued_sdk_deadline_never_dispatches_or_closes_a_healthy_lane_and_is_observed() {
    let (adapter, directory, running) = blocking_adapter();
    let started = Instant::now();
    let (result, records) = crate::observations::capture_timings(true, || {
        adapter.call("deadline", "inspect", json!({}), Duration::from_millis(80))
    });
    let elapsed = started.elapsed();
    std::fs::write(directory.path().join("release"), "released").unwrap();
    assert!(running.join().unwrap().is_ok());
    assert_eq!(
        result,
        Err(AdapterFailure::NotDispatched(
            "adapter_deadline_before_dispatch"
        ))
    );
    assert!(elapsed >= Duration::from_millis(80));
    assert!(
        elapsed < Duration::from_secs(2),
        "queue wait ignored its call deadline"
    );
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["phase"], "e2bAdapterQueueWait");
    assert_eq!(records[0]["outcome"], "error");
    assert_eq!(records[1]["phase"], "e2bAdapterCallInspect");
    assert!(!serde_json::to_string(&records)
        .unwrap()
        .contains("deadline"));
    let probe = adapter
        .call("probe", "inspect", json!({}), Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        probe.result.unwrap()["sequence"],
        2,
        "expired queued call reached the SDK"
    );
}

#[test]
fn closing_sdk_lane_wakes_unsent_callers_without_waiting_for_the_holder() {
    let (adapter, directory, running) = blocking_adapter();
    let waiting = adapter.clone();
    let (done, completion) = mpsc::channel();
    let queued = std::thread::spawn(move || {
        done.send(waiting.call("queued", "inspect", json!({}), Duration::from_secs(10)))
            .unwrap();
    });
    wait_for_adapter_queue(&adapter, 1);
    let closing = Instant::now();
    adapter.close();
    let result = completion.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(result, Err(AdapterFailure::NotDispatched("adapter_closed")));
    assert!(closing.elapsed() < Duration::from_secs(2));
    assert!(matches!(
        running.join().unwrap(),
        Err(AdapterFailure::Unknown(_))
    ));
    queued.join().unwrap();
    assert!(
        !directory.path().join("release").exists(),
        "close required the holder's normal release"
    );
}

#[test]
fn sixty_four_sdk_waiters_are_fifo_bounded_and_never_mix_frames() {
    let (adapter, directory, running) = blocking_adapter();
    let mut queued = Vec::new();
    for index in 0..64 {
        let waiting = adapter.clone();
        queued.push(
            std::thread::Builder::new()
                .stack_size(256 * 1024)
                .spawn(move || {
                    let id = format!("queued-{index}");
                    waiting.call(&id, "inspect", json!({}), Duration::from_secs(15))
                })
                .unwrap(),
        );
        wait_for_adapter_queue(&adapter, index + 1);
    }
    let excess = adapter.call("excess", "inspect", json!({}), Duration::from_secs(1));
    std::fs::write(directory.path().join("release"), "released").unwrap();
    assert!(running.join().unwrap().is_ok());
    assert_eq!(
        excess,
        Err(AdapterFailure::NotDispatched("adapter_queue_capacity"))
    );
    for (index, queued) in queued.into_iter().enumerate() {
        let response = queued.join().unwrap().unwrap();
        assert_eq!(response.request_id, format!("queued-{index}"));
        assert_eq!(response.result.unwrap()["sequence"], index + 2);
    }
    assert_eq!(
        adapter
            .call("probe", "inspect", json!({}), Duration::from_secs(5))
            .unwrap()
            .result
            .unwrap()["sequence"],
        66,
        "overflow request was dispatched"
    );
}

#[test]
fn lost_sdk_reply_closes_the_lane_and_wakes_queued_calls_without_replay() {
    let directory = tempfile::tempdir().unwrap();
    let entered = directory.path().join("entered");
    let release = directory.path().join("release");
    let mut command = fixture("exit");
    command.arg(&entered).arg(&release);
    let adapter = std::sync::Arc::new(AdapterProcess::spawn(command).unwrap());
    let worker = adapter.clone();
    let running = std::thread::spawn(move || {
        worker.call("effect", "block", json!({}), Duration::from_secs(10))
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !entered.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let waiting = adapter.clone();
    let queued = std::thread::spawn(move || {
        waiting.call("effect", "inspect", json!({}), Duration::from_secs(10))
    });
    wait_for_adapter_queue(&adapter, 1);
    std::fs::write(&release, "released").unwrap();
    assert_eq!(
        running.join().unwrap(),
        Err(AdapterFailure::Unknown("adapter_response_unavailable"))
    );
    assert_eq!(
        queued.join().unwrap(),
        Err(AdapterFailure::NotDispatched("adapter_closed"))
    );
    assert_eq!(std::fs::read_to_string(&entered).unwrap(), "entered");
    assert_eq!(
        adapter.call("effect", "block", json!({}), Duration::from_secs(1)),
        Err(AdapterFailure::NotDispatched("adapter_closed"))
    );
}

#[test]
fn expired_fifo_head_does_not_pin_the_next_unsent_sdk_call() {
    let (adapter, directory, running) = blocking_adapter();
    let front = adapter.clone();
    let expired = std::thread::spawn(move || {
        front.call("expired", "inspect", json!({}), Duration::from_millis(200))
    });
    wait_for_adapter_queue(&adapter, 1);
    let next = adapter.clone();
    let queued =
        std::thread::spawn(move || next.call("next", "inspect", json!({}), Duration::from_secs(5)));
    wait_for_adapter_queue(&adapter, 2);
    let expiration = expired.join().unwrap();
    wait_for_adapter_queue(&adapter, 1);
    std::fs::write(directory.path().join("release"), "released").unwrap();
    assert!(running.join().unwrap().is_ok());
    assert_eq!(
        expiration,
        Err(AdapterFailure::NotDispatched(
            "adapter_deadline_before_dispatch"
        ))
    );
    let next = queued.join().unwrap().unwrap();
    assert_eq!(next.request_id, "next");
    assert_eq!(next.result.unwrap()["sequence"], 2);
}

#[test]
fn reused_adapter_registers_each_power_request_before_dispatch_without_restarting() {
    use crate::observations::power::{capture_power_observations, PowerObservation};
    let (process_ids, records) = capture_power_observations(|| {
        let adapter = AdapterProcess::spawn_observed(
            fixture("echo"),
            PowerObservation::new("first", "pause"),
        )
        .unwrap();
        let mut process_ids = Vec::new();
        for (request, operation, owner, action) in [
            ("pause-request", "pause", "first", "pause"),
            ("resume-request", "connect", "second", "resume"),
        ] {
            let response = adapter
                .call_before_observed(
                    request,
                    operation,
                    json!({}),
                    Instant::now() + Duration::from_secs(10),
                    PowerObservation::new(owner, action),
                )
                .unwrap();
            process_ids.push(response.result.unwrap()["process"].clone());
        }
        // Model a delayed stderr reader after both real IPC calls returned.
        for (request, operation, stage) in [
            ("resume-request", "connect", "sdkConnect"),
            ("pause-request", "pause", "sdkPause"),
        ] {
            let line = json!({"schema":"runtime.e2b.power_timing.v1", "stage":stage,
                "requestId":request,"operation":operation,"value":1.0,"outcome":"ok"})
            .to_string();
            adapter.power_bindings.forward(line.as_bytes());
        }
        process_ids
    });
    assert_eq!(process_ids[0], process_ids[1]);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["operationId"], "second");
    assert_eq!(records[1]["operationId"], "first");
}
