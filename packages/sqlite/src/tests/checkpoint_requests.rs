use super::*;

#[test]
fn direct_store_rejects_invalid_checkpoint_requests_without_writes() {
    let db_path = temp_db_path("checkpoint_request_validation");
    let store = SqliteRuntimeStore::new(&db_path).unwrap();
    let checkpoint = CheckpointRecord {
        checkpoint_id: "checkpoint".into(),
        kind: CheckpointKindV1::Wait,
        session_id: "session".into(),
        turn_id: "turn".into(),
        status: "waiting".into(),
        done_reason: None,
        updated_at_ms: 1,
        payload_json: "{}".into(),
    };
    let event = RuntimeEvent {
        event_id: "event".into(),
        session_id: "session".into(),
        task_id: Some("turn".into()),
        event_type: "wait".into(),
        at_ms: 1,
        visibility: EventVisibility::Internal,
        payload_json: "{}".into(),
    };
    for wrong_session in [false, true] {
        let mut invalid = event.clone();
        if wrong_session {
            invalid.session_id = "other".into();
        } else {
            invalid.task_id = None;
        }
        let save = SaveWaitCheckpointRequest {
            checkpoint: checkpoint.clone(),
            event: invalid.clone(),
        };
        assert!(store
            .save_wait_checkpoint(save)
            .unwrap_err()
            .starts_with("save_wait_checkpoint scope mismatch:"));
        let consume = ConsumeWaitCheckpointRequest {
            checkpoint: checkpoint.clone(),
            events: vec![invalid],
        };
        assert_eq!(
            store.consume_wait_checkpoint(consume).unwrap_err(),
            "consume_wait_checkpoint event scope mismatch"
        );
    }
    assert_eq!(
        store
            .consume_wait_checkpoint(ConsumeWaitCheckpointRequest {
                checkpoint: checkpoint.clone(),
                events: vec![]
            })
            .unwrap_err(),
        "consume_wait_checkpoint requires events"
    );
    assert_eq!(
        store
            .consume_wait_checkpoint(ConsumeWaitCheckpointRequest {
                checkpoint,
                events: vec![event.clone(), event]
            })
            .unwrap_err(),
        "consume_wait_checkpoint duplicate event id"
    );
    let db = Connection::open(&db_path).unwrap();
    for table in ["checkpoints", "runtime_events"] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    drop(db);
    fs::remove_file(db_path).unwrap();
}
