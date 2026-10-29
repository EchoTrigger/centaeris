use super::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type ControlledTimings = (
    RunTimings,
    Arc<Mutex<Vec<serde_json::Value>>>,
    Arc<Mutex<Duration>>,
);

fn controlled() -> ControlledTimings {
    let base = Instant::now();
    let elapsed = Arc::new(Mutex::new(Duration::ZERO));
    let clock = elapsed.clone();
    let (run, records) = RunTimings::for_test(
        "agent_run.lifecycle:run-1",
        "run-1",
        Arc::new(move || base + *clock.lock().unwrap()),
    );
    (run, records, elapsed)
}

#[test]
fn model_markers_keep_full_service_time_and_skip_empty_text() {
    let (run, records, elapsed) = controlled();
    run.request_started("turn-1", "request-1");
    let mut model = run.model_http("turn-1", 2, true);
    *elapsed.lock().unwrap() = Duration::from_millis(25);
    model.first_event();
    model.text_delta("");
    *elapsed.lock().unwrap() = Duration::from_millis(60);
    model.text_delta("private model text");
    *elapsed.lock().unwrap() = Duration::from_millis(90);
    model.text_delta("more private text");
    *elapsed.lock().unwrap() = Duration::from_millis(200);
    model.complete(["call-1"].into_iter());
    drop(model);
    let records = records.lock().unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["phase"], "modelFirstEvent");
    assert_eq!(records[0]["elapsedMs"], 25.0);
    assert_eq!(records[1]["phase"], "modelFirstText");
    assert_eq!(records[1]["elapsedMs"], 60.0);
    assert_eq!(records[2]["phase"], "modelHttp");
    assert_eq!(records[2]["elapsedMs"], 200.0);
    for record in records.iter() {
        assert_eq!(record["requestId"], "request-1");
        assert_eq!(record["jobId"], "agent_run.lifecycle:run-1");
        assert_eq!(record["attempt"], 2);
        assert_eq!(record["streaming"], true);
        assert!(!record.to_string().contains("private"));
    }
}

#[test]
fn tool_service_pairs_parallel_calls_by_identity_and_requires_receipt_commit() {
    let (run, records, elapsed) = controlled();
    run.request_started("turn-1", "request-1");
    let mut model = run.model_http("turn-1", 1, true);
    model.complete(["call-a", "call-b"].into_iter());
    drop(model);
    records.lock().unwrap().clear();
    *elapsed.lock().unwrap() = Duration::from_millis(100);
    run.tool_started("call-a");
    *elapsed.lock().unwrap() = Duration::from_millis(120);
    run.tool_started("call-b");
    // A successful executor result alone must not end the interval.
    assert!(records.lock().unwrap().is_empty());
    *elapsed.lock().unwrap() = Duration::from_millis(200);
    run.tool_committed("call-b", true);
    *elapsed.lock().unwrap() = Duration::from_millis(250);
    run.tool_committed("call-a", false);
    let records = records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["callId"], "call-b");
    assert_eq!(records[0]["elapsedMs"], 80.0);
    assert_eq!(records[0]["outcome"], "ok");
    assert_eq!(records[1]["callId"], "call-a");
    assert_eq!(records[1]["elapsedMs"], 150.0);
    assert_eq!(records[1]["outcome"], "error");
    assert_eq!(records[1]["requestId"], "request-1");
}

#[test]
fn failed_commit_and_missing_pairs_never_report_success_or_zero_duration() {
    let (run, records, elapsed) = controlled();
    run.tool_started("call-a");
    *elapsed.lock().unwrap() = Duration::from_millis(40);
    run.tool_commit_failed("call-a");
    run.tool_committed("never-observed", true);
    let records = records.lock().unwrap();
    assert_eq!(records[0]["outcome"], "commitError");
    assert_eq!(records[0]["elapsedMs"], 40.0);
    assert_eq!(records[1]["outcome"], "unpaired");
    assert!(records[1].get("elapsedMs").is_none());
}

#[test]
fn startup_profile_preserves_all_stages_counts_and_explicit_identity() {
    let (run, records, _) = controlled();
    run.startup_profile(StartupProfile {
        server_count: 2,
        credential_count: 3,
        mcp_connection_count: 4,
        runtime_prelude_ms: 1,
        sandbox_ensure_ms: 2,
        mcp_credential_resolve_ms: 3,
        mcp_discovery_ms: 4,
        workspace_restore_ms: 5,
        session_start_commit_ms: 6,
        startup_total_ms: 27,
    });
    let records = records.lock().unwrap();
    let record = &records[0];
    assert_eq!(record["phase"], "startupProfile");
    assert_eq!(record["startupTotalMs"], 27);
    assert_eq!(record["runtimePreludeMs"], 1);
    assert_eq!(record["sessionStartCommitMs"], 6);
    assert_eq!(record["serverCount"], 2);
    assert_eq!(record["credentialCount"], 3);
    assert_eq!(record["mcpConnectionCount"], 4);
    assert_eq!(record["runId"], "run-1");
}

#[test]
fn explicit_identity_is_bounded_and_rejects_payload_like_values() {
    assert!(safe_id("model_request:abc-123"));
    for value in ["", "line\nbreak", "token=secret", "command with spaces"] {
        assert!(!safe_id(value));
    }
    assert!(!safe_id(&"a".repeat(161)));
}

#[test]
fn tool_only_model_has_event_and_complete_service_without_text_ttft() {
    let (run, records, elapsed) = controlled();
    run.request_started("turn-1", "request-1");
    let mut model = run.model_http("turn-1", 1, true);
    *elapsed.lock().unwrap() = Duration::from_millis(30);
    model.first_event();
    *elapsed.lock().unwrap() = Duration::from_millis(100);
    model.complete(["call-tool"].into_iter());
    drop(model);
    let records = records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["phase"], "modelFirstEvent");
    assert_eq!(records[1]["phase"], "modelHttp");
    assert_eq!(records[1]["elapsedMs"], 100.0);
}

#[test]
fn later_requests_do_not_relabel_earlier_tool_calls() {
    let (run, records, elapsed) = controlled();
    run.request_started("turn-1", "request-old");
    let mut old = run.model_http("turn-1", 1, true);
    old.complete(["old-call"].into_iter());
    drop(old);
    run.request_started("turn-1", "request-new");
    let mut new = run.model_http("turn-1", 1, true);
    new.complete(["new-call"].into_iter());
    drop(new);
    records.lock().unwrap().clear();
    run.tool_started("new-call");
    run.tool_started("old-call");
    // Re-entering the same pending call must preserve its original request and start.
    *elapsed.lock().unwrap() = Duration::from_millis(15);
    run.tool_started("old-call");
    *elapsed.lock().unwrap() = Duration::from_millis(35);
    run.tool_committed("old-call", true);
    run.tool_committed("new-call", false);
    let records = records.lock().unwrap();
    assert_eq!(records[0]["requestId"], "request-old");
    assert_eq!(records[0]["elapsedMs"], 35.0);
    assert_eq!(records[1]["requestId"], "request-new");
    let allowed = [
        "phase",
        "outcome",
        "elapsedMs",
        "runId",
        "jobId",
        "callId",
        "requestId",
    ];
    assert!(records.iter().all(|event| event
        .as_object()
        .unwrap()
        .keys()
        .all(|field| allowed.contains(&field.as_str()))));
}

#[test]
fn disabled_or_unsafe_identity_and_unregistered_requests_emit_nothing() {
    let (run, records, _) = controlled();
    let mut unknown = run.model_http("unknown", 1, true);
    unknown.first_event();
    unknown.text_delta("private text");
    unknown.complete(std::iter::empty());
    drop(unknown);
    assert!(records.lock().unwrap().is_empty());
    let (unsafe_run, records) = RunTimings::for_test(
        "agent_run.lifecycle:run-1",
        "unsafe identity",
        Arc::new(Instant::now),
    );
    unsafe_run.request_started("turn", "request");
    unsafe_run.tool_started("call");
    unsafe_run.tool_committed("call", true);
    assert!(records.lock().unwrap().is_empty());
}

#[test]
fn unpaired_drop_reports_remaining_call_without_reclassifying_a_committed_call() {
    let (run, records, elapsed) = controlled();
    {
        let _guard = run.unpaired_guard();
        run.tool_started("committed");
        run.tool_started("pending");
        *elapsed.lock().unwrap() = Duration::from_millis(30);
        run.tool_committed("committed", true);
        *elapsed.lock().unwrap() = Duration::from_millis(90);
    }
    let records = records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["callId"], "committed");
    assert_eq!(records[0]["outcome"], "ok");
    assert_eq!(records[1]["callId"], "pending");
    assert_eq!(records[1]["outcome"], "unpaired");
    assert_eq!(records[1]["elapsedMs"], 90.0);
}
