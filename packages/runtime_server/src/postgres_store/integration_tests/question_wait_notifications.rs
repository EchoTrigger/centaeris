use super::*;
use centaeris_core::runtime::contracts::{
    RuntimeAgentRunIdentityV1, RuntimeAwaitJobCheckpointV1, RuntimeJobWaitV1,
};

fn install_presentation_spy(db: &mut Client) {
    // This owned fixture records transactional calls; Django tests cover the real outbox/NOTIFY.
    db.batch_execute(
        "CREATE TABLE runtime.presentation_wait_calls(key text NOT NULL);
         CREATE TABLE runtime.presentation_wait_control(reject boolean NOT NULL);
         INSERT INTO runtime.presentation_wait_control VALUES (false);
         CREATE OR REPLACE FUNCTION public.workspace_presentation_bump_v1(change_key text)
         RETURNS void LANGUAGE plpgsql AS $$
         BEGIN
             INSERT INTO runtime.presentation_wait_calls(key) VALUES (change_key);
             IF (SELECT reject FROM runtime.presentation_wait_control) THEN
                 RAISE EXCEPTION 'presentation spy rejected call';
             END IF;
         END;
         $$;",
    )
    .expect("install transactional presentation spy");
}

fn presentation_calls(db: &mut Client) -> Vec<String> {
    db.query("SELECT key FROM runtime.presentation_wait_calls", &[])
        .expect("read presentation calls")
        .into_iter()
        .map(|row| row.get(0))
        .collect()
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_question_wait_presentation_changes_are_atomic_and_idempotent() {
    let _guard = TEST_LOCK.lock().expect("Postgres test lock");
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).expect("open Postgres store");
    let mut db = Client::connect(&url, NoTls).expect("connect presentation audit");
    install_presentation_spy(&mut db);
    let checkpoint = CheckpointRecord {
        checkpoint_id: "checkpoint:wait-pg".into(),
        kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
        session_id: "session_pg".into(),
        turn_id: "turn_wait_pg".into(),
        status: "waiting".into(),
        done_reason: Some("question".into()),
        updated_at_ms: 5,
        payload_json: "{\"schema\":\"runtime.await_question.v1\"}".into(),
    };
    let save = SaveWaitCheckpointRequest {
        checkpoint: checkpoint.clone(),
        event: RuntimeEvent {
            event_id: "runtime_wait_pg_waiting".into(),
            session_id: "session_pg".into(),
            task_id: Some("turn_wait_pg".into()),
            event_type: "runtime_wait_changed.v1".into(),
            at_ms: 5,
            visibility: EventVisibility::Internal,
            payload_json: "{\"status\":\"waiting\"}".into(),
        },
    };
    db.batch_execute("UPDATE runtime.presentation_wait_control SET reject=true")
        .expect("reject save notification");
    assert!(store.save_wait_checkpoint(save.clone()).is_err());
    assert!(store
        .load_checkpoint_by_turn("session_pg", "turn_wait_pg")
        .expect("read rolled back checkpoint")
        .is_none());
    assert!(store.list_events("session_pg", 10, 0).unwrap().is_empty());
    assert!(presentation_calls(&mut db).is_empty());

    db.batch_execute("UPDATE runtime.presentation_wait_control SET reject=false")
        .expect("allow save notification");
    store.save_wait_checkpoint(save.clone()).expect("save wait");
    store
        .save_wait_checkpoint(save.clone())
        .expect("replay save");
    assert_eq!(presentation_calls(&mut db), ["session:session_pg:state"]);
    assert_eq!(store.list_events("session_pg", 10, 0).unwrap().len(), 1);

    let consume = ConsumeWaitCheckpointRequest {
        checkpoint: checkpoint.clone(),
        events: vec![RuntimeEvent {
            event_id: "runtime_wait_pg_resumed".into(),
            at_ms: 6,
            payload_json: "{\"status\":\"resumed\"}".into(),
            ..save.event.clone()
        }],
    };
    db.batch_execute("UPDATE runtime.presentation_wait_control SET reject=true")
        .expect("reject consume notification");
    assert!(store.consume_wait_checkpoint(consume.clone()).is_err());
    assert_eq!(
        store
            .load_checkpoint_by_turn("session_pg", "turn_wait_pg")
            .expect("read preserved checkpoint"),
        Some(checkpoint)
    );
    assert_eq!(store.list_events("session_pg", 10, 0).unwrap().len(), 1);
    assert_eq!(presentation_calls(&mut db), ["session:session_pg:state"]);

    db.batch_execute("UPDATE runtime.presentation_wait_control SET reject=false")
        .expect("allow consume notification");
    store
        .consume_wait_checkpoint(consume.clone())
        .expect("consume wait");
    store
        .consume_wait_checkpoint(consume)
        .expect("replay consumption");
    assert!(store
        .load_checkpoint_by_turn("session_pg", "turn_wait_pg")
        .expect("read consumed checkpoint")
        .is_none());
    assert_eq!(store.list_events("session_pg", 10, 0).unwrap().len(), 2);
    assert_eq!(
        presentation_calls(&mut db),
        ["session:session_pg:state", "session:session_pg:state"]
    );

    db.batch_execute("DROP FUNCTION public.workspace_presentation_bump_v1(text)")
        .expect("remove required presentation contract");
    let error = store.save_wait_checkpoint(save.clone()).unwrap_err();
    assert!(error.starts_with("notify Postgres question wait presentation failed:"));
    assert!(store
        .load_checkpoint_by_turn("session_pg", "turn_wait_pg")
        .expect("missing contract rolls back checkpoint")
        .is_none());
    assert_eq!(store.list_events("session_pg", 10, 0).unwrap().len(), 2);

    // Ordinary runtime-job waits do not require the question presentation contract.
    let mut other_wait = save;
    other_wait.checkpoint.done_reason = Some("runtime_job".into());
    let digest = format!("sha256:{}", "a".repeat(64));
    let job_wait = RuntimeAwaitJobCheckpointV1::new(
        &RuntimeAgentRunIdentityV1 {
            agent_run_id: "run_pg".into(),
            execution_id: "execution_pg".into(),
            authorization_digest: digest.clone(),
        },
        &other_wait.checkpoint.turn_id,
        vec![RuntimeJobWaitV1 {
            tool_call_id: "call_pg".into(),
            source_tool_name: "read_result".into(),
            tool_definition_digest: digest,
            job_id: "source:pg".into(),
            job_kind: "subagent.run".into(),
        }],
    )
    .expect("valid ordinary job wait");
    other_wait.checkpoint.payload_json = serde_json::to_string(&job_wait).unwrap();
    store
        .save_wait_checkpoint(other_wait.clone())
        .expect("save ordinary wait without presentation function");
    store
        .consume_wait_checkpoint(ConsumeWaitCheckpointRequest {
            checkpoint: other_wait.checkpoint,
            events: vec![other_wait.event],
        })
        .expect("consume ordinary wait without presentation function");
}
