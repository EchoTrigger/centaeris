use super::*;
#[test]
fn five_field_cron_has_explicit_timezone() {
    let after = chrono::DateTime::parse_from_rfc3339("2026-09-27T00:00:00Z")
        .unwrap()
        .timestamp_millis();
    let expected = chrono::DateTime::parse_from_rfc3339("2026-09-27T01:00:00Z")
        .unwrap()
        .timestamp_millis();
    assert_eq!(
        next_time("0 9 * * *", "Asia/Taipei", after).unwrap(),
        expected
    );
    assert!(next_time("0 0 9 * * *", "UTC", after).is_err());
    assert!(next_time("* * * * *", "unknown", after).is_err());
}

fn fixture() -> Store {
    Store {
        service_enabled: true,
        plans: vec![Plan {
            id: "schedule-test".into(),
            revision: 1,
            enabled: true,
            deleted: false,
            create_digest: "digest".into(),
            next_at: Some(60_000),
            spec: Spec {
                name: "test".into(),
                cwd: "unused".into(),
                prompt: "inspect".into(),
                cron: Some("* * * * *".into()),
                at: None,
                expires_at: None,
                timezone: "UTC".into(),
                model: ModelSelection {
                    provider_id: "p".into(),
                    model: "m".into(),
                    thinking_mode: Some("high".into()),
                },
            },
        }],
        ..Store::default()
    }
}
#[test]
fn due_occurrence_is_unique_and_advances_atomically_across_reload() {
    let mut store = fixture();
    plan_due(&mut store, 60_001).unwrap();
    assert_eq!(store.runs.len(), 1);
    assert_eq!(store.runs[0].status, "pending");
    let bytes = serde_json::to_vec(&store).unwrap();
    let mut restored: Store = serde_json::from_slice(&bytes).unwrap();
    plan_due(&mut restored, 60_002).unwrap();
    assert_eq!(restored.runs.len(), 1);
    assert_eq!(restored.plans[0].next_at, Some(120_000));
}
#[test]
fn late_wake_coalesces_backlog_and_active_occurrence_skips_overlap() {
    let mut store = fixture();
    plan_due(&mut store, 600_000).unwrap();
    assert_eq!(store.runs.len(), 1);
    assert_eq!(store.runs[0].status, "pending");
    assert_eq!(store.runs[0].scheduled_at, 600_000);
    assert_eq!(store.plans[0].next_at, Some(660_000));
    let mut store = fixture();
    plan_due(&mut store, 60_000).unwrap();
    plan_due(&mut store, 120_000).unwrap();
    assert_eq!(store.runs[1].status, "skippedOverlap");
}
#[test]
fn paused_deleted_and_disabled_service_never_plan_work() {
    for mode in 0..3 {
        let mut store = fixture();
        match mode {
            0 => store.service_enabled = false,
            1 => store.plans[0].enabled = false,
            _ => store.plans[0].deleted = true,
        }
        plan_due(&mut store, 60_000).unwrap();
        assert!(store.runs.is_empty());
    }
}
#[test]
fn one_shot_disables_after_claim_without_deleting_history() {
    let mut store = fixture();
    store.plans[0].spec.cron = None;
    store.plans[0].spec.at = Some(60_000);
    plan_due(&mut store, 60_000).unwrap();
    assert!(!store.plans[0].enabled);
    assert_eq!(store.runs.len(), 1);
}
#[test]
fn claimed_run_retains_spec_when_plan_changes() {
    let mut store = fixture();
    plan_due(&mut store, 60_000).unwrap();
    store.plans[0].spec.prompt = "changed".into();
    store.plans[0].revision += 1;
    assert_eq!(store.runs[0].spec.prompt, "inspect");
    assert_eq!(store.runs[0].revision, 1);
}
#[test]
fn strict_requests_reject_misspelled_and_irrelevant_fields() {
    for v in [
        json!({"action":"list","unexpected":true}),
        json!({"action":"pause","schedule_id":"s"}),
        json!({"action":"service","enabled":true,"spec":{}}),
    ] {
        assert!(serde_json::from_value::<Request>(v).is_err());
    }
}
#[test]
fn numeric_weekdays_use_posix_and_dst_moves_daily_wall_time() {
    let after = DateTime::parse_from_rfc3339("2026-09-27T00:00:00Z")
        .unwrap()
        .timestamp_millis();
    assert_eq!(
        next_time("0 1 * * 0", "UTC", after).unwrap(),
        after + 3_600_000
    );
    let after = DateTime::parse_from_rfc3339("2026-03-07T14:00:00Z")
        .unwrap()
        .timestamp_millis();
    let expected = DateTime::parse_from_rfc3339("2026-03-08T13:00:00Z")
        .unwrap()
        .timestamp_millis();
    assert_eq!(
        next_time("0 9 * * *", "America/New_York", after).unwrap(),
        expected
    );
}

#[test]
fn occurrence_knows_deterministic_run_identity_before_admission() {
    let store = fixture();
    let run = occurrence(&store.plans[0], "occurrence-test".into(), 60_000, "pending");
    assert_eq!(
        run.agent_run_id.as_deref(),
        Some(
            operation_receipts::deterministic_identity(
                "agent-run-",
                "session/prompt",
                "occurrence-test-prompt"
            )
            .as_str()
        )
    );
}

#[test]
fn accumulated_history_does_not_disable_due_work() {
    let mut store = fixture();
    for i in 0..4096 {
        store.runs.push(occurrence(
            &store.plans[0],
            format!("old-{i}"),
            0,
            "succeeded",
        ));
    }
    plan_due(&mut store, 60_000).unwrap();
    assert!(store.plans[0].enabled);
    assert_eq!(store.runs.last().unwrap().status, "pending");
    assert_eq!(store.runs.len(), 4097);
}
#[test]
fn late_one_shot_is_admitted_once_without_implicit_expiry() {
    let mut store = fixture();
    store.plans[0].spec.cron = None;
    store.plans[0].spec.at = Some(60_000);
    plan_due(&mut store, 86_400_000).unwrap();
    assert_eq!(store.runs[0].status, "pending");
    plan_due(&mut store, 86_400_001).unwrap();
    assert_eq!(store.runs.len(), 1);
}

#[test]
fn explicit_one_shot_expiry_is_respected() {
    let mut store = fixture();
    store.plans[0].spec.cron = None;
    store.plans[0].spec.at = Some(60_000);
    store.plans[0].spec.expires_at = Some(120_000);
    plan_due(&mut store, 120_001).unwrap();
    assert_eq!(store.runs[0].status, "expired");
}

#[test]
fn absent_expiry_preserves_existing_create_request_digest() {
    let spec = fixture().plans.remove(0).spec;
    let value = serde_json::to_value(&spec).unwrap();
    assert!(value.get("expiresAt").is_none());
    let restored: Spec = serde_json::from_value(value).unwrap();
    assert_eq!(
        operation_receipts::request_digest(&spec).unwrap(),
        operation_receipts::request_digest(&restored).unwrap()
    );
}

#[test]
fn scheduler_migrates_atomically_and_hot_reads_ignore_finished_history() {
    let root = std::env::temp_dir()
        .join(crate::process_sessions::Manager::default().service_instance_id());
    fs::create_dir_all(&root).unwrap();
    let path = root.join("schedules.json");
    let mut legacy = fixture();
    let mut historical = occurrence(&legacy.plans[0], "finished".into(), 0, "succeeded");
    legacy.runs.push(historical.clone());
    let original = serde_json::to_vec(&legacy).unwrap();
    fs::write(&path, &original).unwrap();
    let mut work = storage::load_work(&path, 60_000).unwrap();
    assert_eq!(work.plans.len(), 1);
    assert!(work.runs.is_empty());
    assert_eq!(
        fs::read(path.with_extension("json.pre-sqlite.backup")).unwrap(),
        original
    );
    plan_due(&mut work, 60_000).unwrap();
    storage::save(&path, &work).unwrap();
    let mut conn = crate::local_work_store::open(&path.with_extension("sqlite3")).unwrap();
    let tx = conn.transaction().unwrap();
    for i in 0..4100 {
        historical.id = format!("history-{i:05}");
        crate::local_work_store::put(
            &tx,
            "runs",
            &historical.id,
            "schedule-test",
            "finished",
            Some(0),
            &historical,
        )
        .unwrap();
    }
    tx.commit().unwrap();
    // Poison unrelated completed bodies: hot-path reads must not deserialize them.
    conn.execute(
        "UPDATE records SET body='not json' WHERE namespace='runs' AND state='finished'",
        [],
    )
    .unwrap();
    let work = storage::load_work(&path, 60_001).unwrap();
    assert!(work.plans.is_empty());
    assert_eq!(work.runs.len(), 1);
    assert!(storage::keep_alive(&path).unwrap());
    // Migration is not replayed over newer durable state.
    assert_eq!(storage::load_work(&path, 60_001).unwrap().runs.len(), 1);
    drop(conn);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scheduler_plan_pages_are_bounded_and_cursor_scoped() {
    let root = std::env::temp_dir()
        .join(crate::process_sessions::Manager::default().service_instance_id());
    let path = root.join("schedules.json");
    let mut store = fixture();
    let prototype = store.plans[0].clone();
    store.plans = (0..270)
        .map(|i| {
            let mut p = prototype.clone();
            p.id = format!("plan-{i:03}");
            p
        })
        .collect();
    storage::save(&path, &store).unwrap();
    let first = storage::page(&path, None, None, None).unwrap();
    assert_eq!(first["schedules"].as_array().unwrap().len(), 50);
    assert_eq!(first["total"], 270);
    let cursor = first["nextCursor"].as_str().unwrap().to_string();
    let next = storage::page(&path, None, None, Some(cursor.clone())).unwrap();
    assert_ne!(first["schedules"][0]["id"], next["schedules"][0]["id"]);
    assert!(storage::page(&path, Some("other"), None, Some(cursor)).is_err());
    fs::remove_dir_all(root).unwrap();
}

struct ReconciliationProfile {
    root: PathBuf,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl ReconciliationProfile {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .join(crate::process_sessions::Manager::default().service_instance_id());
        fs::create_dir_all(root.join("workspace")).unwrap();
        let previous = [
            ("CENTAERIS_DESKTOP_DATA_DIR", root.clone()),
            ("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions")),
        ]
        .into_iter()
        .map(|(name, value)| {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            (name, previous)
        })
        .collect();
        Self { root, previous }
    }

    fn admitted(&self, store: &Store, receipt_status: &str, queued: bool) -> Run {
        let session = crate::session_files::SessionFiles::new(self.root.join("sessions"))
            .create(
                Some("scheduled"),
                self.root.join("workspace").to_str().unwrap(),
                1_800_000_000_000,
            )
            .unwrap();
        let id = operation_receipts::deterministic_identity(
            "occurrence-",
            &store.plans[0].id,
            "admitted",
        );
        let mut run = occurrence(&store.plans[0], id, 30_000, receipt_status);
        let run_id = run.agent_run_id.as_deref().unwrap();
        message_log::append_agent_turn_queued(
            &session.id,
            "turn",
            run_id,
            "inspect",
            1_800_000_000_001,
        )
        .unwrap();
        if !queued {
            message_log::append_agent_turn_running(&session.id, run_id).unwrap();
        }
        if receipt_status == "running" {
            run.session_id = Some(session.id);
        }
        run
    }

    fn terminal(&self, run: &Run, reason: &str) -> message_log::ProjectedAgentRun {
        let run_id = run.agent_run_id.as_deref().unwrap();
        let session_id = message_log::project_agent_run(run_id)
            .unwrap()
            .unwrap()
            .session_id;
        if reason == "succeeded" {
            message_log::append_assistant_message(
                &session_id,
                "turn",
                Some(run_id),
                "complete",
                "done",
                1_800_000_000_002,
            )
            .unwrap();
        }
        let mut state = message_log::agent_run_session_state(&session_id, run_id).unwrap();
        let terminal = match reason {
            "succeeded" => state.complete("turn", "finalized", 1_800_000_000_003),
            "failed" => state.fail(
                "turn",
                "runtime_error",
                "synthetic failure",
                1_800_000_000_003,
            ),
            _ => state.interrupt(
                "turn",
                reason,
                "synthetic terminal",
                false,
                1_800_000_000_003,
            ),
        }
        .unwrap();
        message_log::append_agent_run_records(&session_id, vec![terminal]).unwrap();
        message_log::project_agent_run(run_id).unwrap().unwrap()
    }
}

impl Drop for ReconciliationProfile {
    fn drop(&mut self) {
        for (name, previous) in &self.previous {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn stopped_projection_releases_persisted_scheduler_receipt() {
    let _env = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for reason in ["stopped", "shutdown", "provider_interrupted"] {
        for receipt_status in ["pending", "running"] {
            for legacy_json in [false, true] {
                assert_terminal_receipt_reconciles(reason, "stopped", receipt_status, legacy_json);
            }
        }
    }
}

#[test]
fn normal_terminal_projections_release_persisted_scheduler_receipts() {
    let _env = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for status in ["succeeded", "failed", "cancelled"] {
        for receipt_status in ["pending", "running"] {
            for legacy_json in [false, true] {
                assert_terminal_receipt_reconciles(status, status, receipt_status, legacy_json);
            }
        }
    }
}

fn manual_run(schedule_id: &str, operation_id: &str) -> Result<Value, String> {
    manage(Request::Run {
        schedule_id: schedule_id.into(),
        operation_id: operation_id.into(),
    })
}

fn history(schedule_id: &str) -> Value {
    manage(Request::History {
        schedule_id: schedule_id.into(),
        limit: None,
        cursor: None,
    })
    .unwrap()
}

fn assert_terminal_receipt_reconciles(
    reason: &str,
    expected: &str,
    receipt_status: &str,
    legacy_json: bool,
) {
    let profile = ReconciliationProfile::new();
    let mut store = fixture();
    store.service_enabled = false;
    // A pending receipt without a Session ID models an admission acknowledgement
    // that was not saved; reconciliation must reuse the admitted identities.
    let run = profile.admitted(&store, receipt_status, false);
    let terminal = profile.terminal(&run, reason);
    assert_eq!(terminal.status, expected);
    assert_eq!(terminal.completed_at_ms, Some(1_800_000_000_003));
    store.runs.push(run.clone());
    let original = serde_json::to_vec(&store).unwrap();
    if legacy_json {
        fs::create_dir_all(root().parent().unwrap()).unwrap();
        fs::write(root(), &original).unwrap();
    } else {
        storage::save(&root(), &store).unwrap();
    }

    let writer = Arc::new(crate::runtime_rpc_transport::RuntimeServerClientHub::default())
        .process_scheduler();
    tick_with_clock(&writer, || 60_000).unwrap();
    let result = history(&run.schedule_id);
    assert_eq!(result["runs"].as_array().unwrap().len(), 1);
    assert_eq!(
        result["runs"][0]["status"], expected,
        "{reason}/{receipt_status}/legacy={legacy_json}"
    );
    assert_eq!(result["runs"][0]["sessionId"], terminal.session_id);
    assert_eq!(result["runs"][0]["agentRunId"], terminal.agent_run_id);
    assert!(storage::load_work(&root(), 60_000).unwrap().runs.is_empty());
    assert!(!storage::keep_alive(&root()).unwrap());
    tick_with_clock(&writer, || 60_000).unwrap();
    assert_eq!(history(&run.schedule_id), result);
    if legacy_json {
        assert_eq!(
            fs::read(root().with_extension("json.pre-sqlite.backup")).unwrap(),
            original
        );
    }
    assert_eq!(
        manual_run(&run.schedule_id, "admitted").unwrap(),
        result["runs"][0]
    );
    let manual = manual_run(&run.schedule_id, "next-manual").unwrap();
    assert_eq!(manual["status"], "pending");
    assert_ne!(manual["id"], run.id);
    assert_eq!(manual_run(&run.schedule_id, "next-manual").unwrap(), manual);
    assert_eq!(
        manual_run(&run.schedule_id, "overlap").unwrap_err(),
        "previous occurrence is still active"
    );
    assert_eq!(storage::load_work(&root(), 60_000).unwrap().runs.len(), 1);
    assert_eq!(
        message_log::project_agent_runs_for_session(&terminal.session_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn stopped_projection_unblocks_next_periodic_occurrence() {
    let _env = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for reason in ["stopped", "shutdown", "provider_interrupted"] {
        let profile = ReconciliationProfile::new();
        let mut store = fixture();
        let run = profile.admitted(&store, "running", false);
        let terminal = profile.terminal(&run, reason);
        store.runs.push(run.clone());
        storage::save(&root(), &store).unwrap();
        let writer = Arc::new(crate::runtime_rpc_transport::RuntimeServerClientHub::default())
            .process_scheduler();
        tick_with_clock(&writer, || 60_000).unwrap();

        let result = history(&run.schedule_id);
        assert_eq!(result["runs"].as_array().unwrap().len(), 2);
        let completed = result["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run.id)
            .unwrap();
        assert_eq!(completed["status"], "stopped");
        assert_eq!(completed["sessionId"], terminal.session_id);
        let work = storage::load_work(&root(), 0).unwrap();
        assert_eq!(work.runs.len(), 1);
        let next = &work.runs[0];
        assert_eq!(next.status, "pending");
        assert_eq!(
            next.id,
            operation_receipts::deterministic_identity(
                "occurrence-",
                &run.schedule_id,
                &format!("{}:{}", run.revision, next.scheduled_at)
            )
        );
        let mut due = storage::load_request(
            &root(),
            &Request::Pause {
                schedule_id: run.schedule_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(due.plans[0].next_at, Some(next.scheduled_at + 60_000));
        let scheduled_at = next.scheduled_at;
        due.runs = work.runs;
        // Replanning the same due time does not enqueue another occurrence.
        plan_due(&mut due, scheduled_at).unwrap();
        assert_eq!(due.runs.len(), 1);
        assert_eq!(
            manual_run(&run.schedule_id, "admitted").unwrap(),
            *completed
        );
        assert_eq!(
            manual_run(&run.schedule_id, "overlap").unwrap_err(),
            "previous occurrence is still active"
        );
    }
}

#[test]
fn queued_and_running_projections_remain_active_and_reject_unknown_interruption() {
    let _env = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for queued in [true, false] {
        for receipt_status in ["pending", "running"] {
            let profile = ReconciliationProfile::new();
            let mut store = fixture();
            let run = profile.admitted(&store, receipt_status, queued);
            let run_id = run.agent_run_id.as_deref().unwrap();
            let projected = message_log::project_agent_run(run_id).unwrap().unwrap();
            assert_eq!(projected.completed_at_ms, None);
            let mut state =
                message_log::agent_run_session_state(&projected.session_id, run_id).unwrap();
            assert!(state
                .interrupt("turn", "unknown", "unsupported", false, 1_800_000_000_003)
                .is_err());
            store.runs.push(run.clone());
            storage::save(&root(), &store).unwrap();
            let writer = Arc::new(crate::runtime_rpc_transport::RuntimeServerClientHub::default())
                .process_scheduler();
            tick_with_clock(&writer, || 60_000).unwrap();

            let result = history(&run.schedule_id);
            assert_eq!(result["runs"].as_array().unwrap().len(), 2);
            assert!(result["runs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["status"] == "skippedOverlap"));
            let work = storage::load_work(&root(), 0).unwrap();
            assert_eq!(work.runs.len(), 1);
            assert_eq!(work.runs[0].id, run.id);
            assert_eq!(work.runs[0].status, "running");
            assert_eq!(
                work.runs[0].session_id.as_deref(),
                Some(projected.session_id.as_str())
            );
            assert!(storage::keep_alive(&root()).unwrap());
            assert_eq!(
                manual_run(&run.schedule_id, "admitted").unwrap()["id"],
                run.id
            );
            assert_eq!(
                manual_run(&run.schedule_id, "overlap").unwrap_err(),
                "previous occurrence is still active"
            );
            assert_eq!(
                message_log::project_agent_run(run_id)
                    .unwrap()
                    .unwrap()
                    .completed_at_ms,
                None
            );
        }
    }
}
