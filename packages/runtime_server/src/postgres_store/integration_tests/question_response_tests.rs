use super::*;
use centaeris_core::model::{GenerateResult, ToolCallEnvelope};
use centaeris_core::runtime::contracts::RuntimeAgentRunIdentityV1;
use centaeris_core::runtime::question::{QuestionCheckpoint, QuestionResponse, RESPONSE_SCHEMA};

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_question_response_receipt_replays_after_consumption_and_preserves_yield_wake() {
    let _guard = TEST_LOCK.lock().unwrap();
    for running in [false, true] {
        let url = test_url();
        reset_store(&url);
        let store = PostgresRuntimeStore::new(&url).unwrap();
        let mut db = Client::connect(&url, NoTls).unwrap();
        super::question_wait_notifications::install_presentation_spy(&mut db);
        let identity = RuntimeAgentRunIdentityV1 {
            agent_run_id: "question-run".into(),
            execution_id: "execution".into(),
            authorization_digest: format!("sha256:{}", "a".repeat(64)),
        };
        let wait = QuestionCheckpoint::new(
            &identity,
            "question-turn",
            &GenerateResult {
                content: "Choose".into(),
                tool_calls: vec![ToolCallEnvelope {
                    id: "question-call".into(),
                    name: "request_user_input".into(),
                    args_json: serde_json::json!({"question":"Which?","options":["One","Two"]})
                        .to_string(),
                }],
                continuation_reasoning_content: None,
                reasoning_content: None,
                input_tokens: None,
                total_tokens: None,
                prompt_cache_hit_tokens: None,
                prompt_cache_miss_tokens: None,
            },
        )
        .unwrap();
        store
            .save_checkpoint(CheckpointRecord {
                checkpoint_id: "question-checkpoint".into(),
                kind: centaeris_core::runtime::contracts::CheckpointKindV1::Wait,
                session_id: "question-session".into(),
                turn_id: wait.turn_id.clone(),
                status: "paused_question".into(),
                done_reason: Some("question".into()),
                updated_at_ms: 1,
                payload_json: serde_json::to_string(&wait).unwrap(),
            })
            .unwrap();
        let job_id = "agent_run.lifecycle:question-run";
        let mut lifecycle = job(
            job_id,
            &format!("{job_id}:{}", identity.authorization_digest),
        );
        lifecycle.job_kind = "agent_run.lifecycle".into();
        lifecycle.session_id = Some("question-session".into());
        lifecycle.payload_ref = Some("record:agent_run:question-run".into());
        lifecycle.run_at_ms = 10_000;
        store
            .schedule_runtime_job(ScheduleRuntimeJobRequest { job: lifecycle })
            .unwrap();
        if running {
            db.execute("UPDATE runtime.runtime_jobs SET status='running',lease_owner='owner',lease_expires_at_ms=100 WHERE job_id=$1",&[&job_id]).unwrap();
        }
        let response = QuestionResponse {
            schema: RESPONSE_SCHEMA.into(),
            agent_run_id: identity.agent_run_id,
            authorization_digest: identity.authorization_digest,
            continuation_id: wait.continuation_id,
            question_id: wait.question_id,
            operation_id: "response-one".into(),
            action: "answer".into(),
            answer: Some("2\u{0000}extra".into()),
        };
        db.batch_execute("UPDATE runtime.presentation_wait_control SET reject=true")
            .unwrap();
        assert!(store
            .respond_to_question("question-session", job_id, &response, 2)
            .is_err());
        assert_eq!(
            db.query_one(
                "SELECT count(*) FROM runtime.runtime_events WHERE event_type=$1",
                &[&RESPONSE_SCHEMA]
            )
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        db.batch_execute("UPDATE runtime.presentation_wait_control SET reject=false")
            .unwrap();
        let mut foreign = response.clone();
        foreign.question_id = "foreign".into();
        assert!(store
            .respond_to_question("question-session", job_id, &foreign, 2)
            .is_err());
        assert_eq!(
            store
                .respond_to_question("question-session", job_id, &response, 3)
                .unwrap(),
            "accepted"
        );
        assert!(store
            .load_checkpoint_by_turn("question-session", "question-turn")
            .unwrap()
            .is_some());
        if running {
            store
                .yield_runtime_job(YieldRuntimeJobRequest {
                    job_id: job_id.into(),
                    lease_owner: "owner".into(),
                    yielded_at_ms: 4,
                    run_at_ms: 20_000,
                    transition_reason: "question_wait".into(),
                })
                .unwrap();
            assert_eq!(store.get_runtime_job(job_id).unwrap().unwrap().run_at_ms, 4);
        } else {
            assert_eq!(store.get_runtime_job(job_id).unwrap().unwrap().run_at_ms, 3);
        }
        db.execute(
            "DELETE FROM runtime.checkpoints WHERE checkpoint_id='question-checkpoint'",
            &[],
        )
        .unwrap();
        db.execute(
            "UPDATE runtime.runtime_jobs SET status='succeeded' WHERE job_id=$1",
            &[&job_id],
        )
        .unwrap();
        assert_eq!(
            store
                .respond_to_question("question-session", job_id, &response, 99)
                .unwrap(),
            "existing"
        );
        assert_eq!(db.query_one("SELECT count(*) FROM runtime.presentation_wait_calls WHERE key='session:question-session:state'",&[]).unwrap().get::<_,i64>(0),1);
        let mut conflict = response.clone();
        conflict.answer = Some("1".into());
        assert!(store
            .respond_to_question("question-session", job_id, &conflict, 100)
            .is_err());
        let mut conflict = response.clone();
        conflict.operation_id = "response-two".into();
        assert!(store
            .respond_to_question("question-session", job_id, &conflict, 100)
            .is_err());
        assert_eq!(
            db.query_one(
                "SELECT count(*) FROM runtime.runtime_events WHERE event_type=$1",
                &[&RESPONSE_SCHEMA]
            )
            .unwrap()
            .get::<_, i64>(0),
            1
        );
    }
}
