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
