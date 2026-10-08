use super::*;
use centaeris_core::runtime::subagent::*;
use centaeris_core::session::manager::SessionManager;
use centaeris_core::session::reliability::*;
use centaeris_runtime_sqlite::SqliteRuntimeStore;

fn fixture() -> (SqliteRuntimeStore, WaitHandoff, SessionStateSnapshot) {
    let path = std::env::temp_dir().join(format!(
        "centaeris-wait-replay-{}-{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let store = SqliteRuntimeStore::new(path).unwrap();
    let snapshot = SessionStateSnapshot::new("parent".into(), 1);
    SessionManager::new(store.clone())
        .save_session(&snapshot)
        .unwrap();
    let wait = CheckpointRecord {
        checkpoint_id: "wait:parent".into(),
        kind: CheckpointKindV1::Wait,
        session_id: "parent".into(),
        turn_id: "parent-turn".into(),
        status: "waiting".into(),
        done_reason: Some("runtime_job".into()),
        updated_at_ms: 1,
        payload_json: "{}".into(),
    };
    let handoff = WaitHandoff {
        schema: SCHEMA.into(),
        handoff_key: "immutable-key".into(),
        source_session_sequence: 19,
        model_request_id: "request-parent".into(),
        wait_checkpoint: wait,
        snapshot_json: serde_json::to_string(&serde_json::to_value(&snapshot).unwrap()).unwrap(),
    };
    (store, handoff, snapshot)
}

#[test]
fn wait_replay_preserves_unchanged_state_and_rejects_checkpoint_or_history_changes() {
    let (store, handoff, snapshot) = fixture();
    handoff
        .validate_replay(&store, &handoff.wait_checkpoint, &snapshot)
        .unwrap();
    let mut changed_wait = handoff.wait_checkpoint.clone();
    changed_wait.turn_id = "foreign-turn".into();
    assert!(handoff
        .validate_replay(&store, &changed_wait, &snapshot)
        .is_err());
    let mut changed = snapshot;
    changed
        .metadata
        .insert("pending-state".into(), "changed".into());
    assert!(handoff
        .validate_replay(&store, &handoff.wait_checkpoint, &changed)
        .is_err());
}

fn complete_child(store: &SqliteRuntimeStore) {
    let mut ids = vec![];
    for name in ["a", "b"] {
        let job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: "parent".into(),
            parent_turn_id: "parent-turn".into(),
            tool_call_id: format!("spawn-{name}"),
            subagent_id: name.into(),
            work_packet_ref: format!("external_context:packet-{name}"),
            checkpoint_id: None,
            run_at_ms: 1,
            created_at_ms: 1,
            max_retries: 0,
        });
        ids.push(job.job_id.clone());
        store
            .schedule_runtime_job(ScheduleRuntimeJobRequest { job })
            .unwrap();
    }
    let claimed = store
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: 2,
            worker_id: "worker".into(),
            job_id: Some(ids[0].clone()),
            job_kind: None,
            session_id: Some("parent".into()),
            limit: 1,
            lease_ms: 60000,
        })
        .unwrap()
        .remove(0);
    let owner = claimed.lease_owner.unwrap();
    store
        .start_runtime_job(StartRuntimeJobRequest {
            job_id: ids[0].clone(),
            lease_owner: owner.clone(),
            started_at_ms: 3,
        })
        .unwrap();
    let event = complete_subagent_run_job(
        store,
        CompleteSubagentRunJobRequest {
            job_id: ids[0].clone(),
            lease_owner: owner,
            output_refs: vec!["external_context:result-a".into()],
            completed_at_ms: 4,
        },
    )
    .unwrap();
    centaeris_core::runtime::persist_subagent_result_projection_from_scheduler_events(
        store,
        "parent",
        &[event],
    )
    .unwrap();
    assert_eq!(
        store.get_runtime_job(&ids[1]).unwrap().unwrap().status,
        RuntimeJobStatus::Queued
    );
}

#[test]
fn wait_replay_accepts_committed_child_result_without_resealing() {
    let (store, handoff, _) = fixture();
    let sealed_id = handoff.checkpoint_id().unwrap();
    complete_child(&store);
    let current = SessionManager::new(store.clone())
        .load_session("parent")
        .unwrap()
        .unwrap();
    handoff
        .validate_replay(&store, &handoff.wait_checkpoint, &current)
        .expect("a completed child must not fail the parent while its sibling is queued");
    assert_eq!(handoff.checkpoint_id().unwrap(), sealed_id);
    assert_eq!(handoff.source_session_sequence, 19);
}
