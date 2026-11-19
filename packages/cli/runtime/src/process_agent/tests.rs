use super::*;
use std::time::{Duration, Instant};

fn record(manager: &process_sessions::Manager) -> Record {
    Record {
        schema_version: 1,
        owner: Owner {
            session_id: "session-test".into(),
            agent_run_id: "run-test".into(),
            tool_call_id: "call-test".into(),
        },
        operation_id: "op-test".into(),
        request_digest: "digest-test".into(),
        service_instance_id: manager.service_instance_id().into(),
        process_session_id: None,
        completion: None,
        output_available: false,
        output_pages: vec![],
        delivery: Delivery::Pending,
    }
}
#[test]
fn busy_sessions_queue_and_cancelled_or_failed_owners_never_auto_resume() {
    assert_eq!(
        decision(true, Some("succeeded"), Some("running")),
        Decision::Wait
    );
    assert_eq!(
        decision(false, Some("succeeded"), Some("succeeded")),
        Decision::Deliver
    );
    for state in [None, Some("cancelled"), Some("failed"), Some("running")] {
        assert_eq!(
            decision(false, state, Some("succeeded")),
            Decision::Suppress
        );
    }
    assert_eq!(
        decision(false, Some("succeeded"), Some("cancelled")),
        Decision::Suppress
    );
}
#[test]
fn terminal_output_survives_outbox_reload_and_is_session_scoped() {
    let manager = process_sessions::Manager::default();
    let mut record = record(&manager);
    let root = std::env::temp_dir().join(format!(
        "centaeris-outbox-{}",
        manager.service_instance_id()
    ));
    save(&root, &record).unwrap();
    manager
        .start_for_agent_at(
            process_sessions::StartRequest {
                session_id: record.owner.session_id.clone(),
                service_instance_id: record.service_instance_id.clone(),
                operation_id: record.operation_id.clone(),
                program: "bash".into(),
                args: vec![
                    "-c".into(),
                    "sleep 0.1; printf complete; printf error >&2".into(),
                ],
                timeout_ms: 3000,
            },
            std::env::current_dir().unwrap(),
        )
        .unwrap();
    // Lost start reply: recover the process identity from admission's receipt.
    assert!(!capture(&mut record, &manager).unwrap());
    assert!(record.process_session_id.is_some());
    assert!(manager.has_pending_notifications());
    let deadline = Instant::now() + Duration::from_secs(8);
    while !capture(&mut record, &manager).unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(record.completion.as_ref().unwrap()["exitCode"], 0);
    assert_eq!(record.completion.as_ref().unwrap()["outputComplete"], true);
    save(&root, &record).unwrap();
    let mut loaded = records(&root).unwrap();
    assert!(loaded[0].output_pages.is_empty());
    load_output(&root, &mut loaded[0]).unwrap();
    let id = record.process_session_id.clone().unwrap();
    assert!(retained_page(&loaded, "other-session", &id, "0").is_none());
    let page = retained_page(&loaded, "session-test", &id, "0").unwrap();
    assert!(!page["chunks"].as_array().unwrap().is_empty());
    assert!(notification(&record).contains("automatic notification, not a user request"));
    assert!(!notification(&record).contains("Y29tcGxldGU="));
    let before = record.completion.clone();
    assert!(capture(&mut record, &manager).unwrap());
    assert_eq!(record.completion, before);
    manager.acknowledge_notification(process_sessions::Target {
        session_id: "session-test".into(),
        process_session_id: id.clone(),
    });
    assert!(!manager.has_pending_notifications());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn restart_reports_unknown_outcome_without_reexecuting_command() {
    let old = process_sessions::Manager::default();
    let mut r = record(&old);
    let restarted = process_sessions::Manager::default();
    assert!(capture(&mut r, &restarted).unwrap());
    assert_eq!(r.completion.unwrap()["status"], "interrupted");
    assert!(restarted.list("session-test").is_empty());
}
#[test]
fn tool_contracts_have_exact_arguments_and_stable_identity() {
    let registry = centaeris_core::tool::DynamicToolRegistry::from_contracts(contracts()).unwrap();
    assert_eq!(registry.len(), 6);
    assert!(
        serde_json::from_value::<Start>(json!({"program":"bash","args":[],"timeoutMs":0})).is_err()
    );
    assert!(serde_json::from_value::<Stop>(
        json!({"process_session_id":"id","session_id":"other"})
    )
    .is_err());
}

#[test]
fn completed_outbox_history_does_not_enter_worker_pending_set() {
    let manager = process_sessions::Manager::default();
    let root = std::env::temp_dir().join(format!("outbox-index-{}", manager.service_instance_id()));
    let mut r = record(&manager);
    r.delivery = Delivery::Delivered;
    // Verify explicit forward import of the old JSON layout.
    fs::create_dir_all(&root).unwrap();
    fs::write(
        record_path(&root, &r.operation_id),
        serde_json::to_vec(&r).unwrap(),
    )
    .unwrap();
    assert!(records_for(&root, None).unwrap().is_empty());
    assert!(load_record(&root, &r.operation_id).unwrap().is_some());
    let mut conn = open_records(&root).unwrap();
    let tx = conn.transaction().unwrap();
    for i in 0..300 {
        r.operation_id = format!("done-{i}");
        store_record(&tx, &r).unwrap();
    }
    tx.commit().unwrap();
    conn.execute(
        "UPDATE records SET body='not json' WHERE namespace='completions' AND state='finished'",
        [],
    )
    .unwrap();
    r.operation_id = "new-pending".into();
    r.delivery = Delivery::Pending;
    save(&root, &r).unwrap();
    let pending = records_for(&root, None).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].operation_id, "new-pending");
    drop(conn);
    fs::remove_dir_all(root).unwrap();
}
