use super::*;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_direct_store_validates_checkpoint_requests_before_transactions() {
    let _guard = TEST_LOCK.lock().unwrap();
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let checkpoint = CheckpointRecord {
        checkpoint_id: "checkpoint".into(),
        kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
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
    // Invalid requests must be rejected even when SQL execution would fail.
    let mut db = Client::connect(&url, NoTls).unwrap();
    db.batch_execute("CREATE FUNCTION runtime.reject_checkpoint_test_write() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''unexpected checkpoint write''; END'; CREATE TRIGGER reject_checkpoint_test_write BEFORE INSERT ON runtime.checkpoints FOR EACH ROW EXECUTE FUNCTION runtime.reject_checkpoint_test_write();").unwrap();
    for wrong_session in [false, true] {
        let mut invalid = event.clone();
        if wrong_session {
            invalid.session_id = "other".into();
        } else {
            invalid.task_id = None;
        }
        assert!(store
            .save_wait_checkpoint(SaveWaitCheckpointRequest {
                checkpoint: checkpoint.clone(),
                event: invalid.clone()
            })
            .unwrap_err()
            .starts_with("save_wait_checkpoint scope mismatch:"));
        assert_eq!(
            store
                .consume_wait_checkpoint(ConsumeWaitCheckpointRequest {
                    checkpoint: checkpoint.clone(),
                    events: vec![invalid]
                })
                .unwrap_err(),
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
    for table in ["checkpoints", "runtime_events"] {
        assert_eq!(
            db.query_one(&format!("SELECT count(*) FROM runtime.{table}"), &[])
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
}
