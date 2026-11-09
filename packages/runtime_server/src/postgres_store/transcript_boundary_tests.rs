// The observer is test-only and thread-local; the real catch-up path does all reads and commits.
#[test]
#[ignore = "explicit PostgreSQL catch-up contract against a fresh disposable database"]
fn postgres_transcript_head_recovery_read_interleaves_with_next_commit() {
    use crate::postgres_store::{PostgresRuntimeStore, PostgresSessionLog};
    use centaeris_core::session::{AgentRunSessionState, SessionLogPort};
    use std::sync::mpsc;
    use std::time::Duration;

    let url = std::env::var("CENTAERIS_PROJECTION_BOUNDARY_DATABASE")
        .expect("dedicated database required");
    let mut db = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let name: String = db
        .query_one("SELECT current_database()", &[])
        .unwrap()
        .get(0);
    assert!(
        name.starts_with("test_managed_assistant_projection_") || name.starts_with("test_outbox_")
    );
    db.batch_execute(r#"
        CREATE TABLE IF NOT EXISTS app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL,
            status text NOT NULL DEFAULT 'active',"purgedAt" timestamptz);
        CREATE TABLE IF NOT EXISTS app_core_sessionevent(
            "eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,
            agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,
            session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,
            payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL,
            UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));
        INSERT INTO app_core_session(id,workspace_id) VALUES('session-head-race','workspace-fixture');
    "#).unwrap();
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let session = "session-head-race";
    let current = store
        .load_or_initialize_current_transcript_generation(session)
        .unwrap();
    let generation = current.projection_generation.as_str();
    let run = "run-head-race";
    let mut state = AgentRunSessionState::new(session, run).unwrap();
    let records = state.start(run, "synthetic", vec![], 1).unwrap();
    // Leave the second event pending for the two explicit catch-up calls.
    let log = PostgresSessionLog::new(
        store.ordinary_connections.clone(),
        "workspace-fixture".into(),
        session.into(),
        "synthetic".into(),
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(log.append_session_records(run, &records))
        .unwrap();
    assert_eq!(
        store
            .catch_up_transcript_projection(session, generation, 1)
            .unwrap()
            .projected_high_water,
        1
    );

    let (pinned_tx, pinned_rx) = mpsc::sync_channel(0);
    let (committed_tx, committed_rx) = mpsc::sync_channel(0);
    let reader_store = store.clone();
    let reader_generation = generation.to_owned();
    let reader = std::thread::spawn(move || {
        super::CATCH_UP_AFTER_INITIAL_READ.with(|observer| {
            *observer.borrow_mut() = Some(Box::new(move || {
                pinned_tx.send(()).unwrap();
                committed_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            }));
        });
        reader_store.catch_up_transcript_projection(session, &reader_generation, 2)
    });
    pinned_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let competing = store.catch_up_transcript_projection(session, generation, 2);
    committed_tx.send(()).unwrap();
    assert!(competing.unwrap().caught_up);
    let progress = reader
        .join()
        .unwrap()
        .expect("actual catch-up must retain a coherent initial head/recovery pair");
    assert!(progress.caught_up);
    assert_eq!(progress.projected_high_water, 2);
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.transcript_projection_current_recoveries WHERE session_id=$1", &[&session]).unwrap().get::<_,i64>(0), 1);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.transcript_projection_commits WHERE session_id=$1",
            &[&session]
        )
        .unwrap()
        .get::<_, i64>(0),
        2
    );
    assert_eq!(
        store
            .catch_up_transcript_projection(session, generation, 2)
            .unwrap()
            .events_projected,
        0
    );

    // A true missing checkpoint and a corrupt checkpoint must still fail loudly.
    db.execute("INSERT INTO runtime.transcript_projection_heads(session_id,projection_version,projection_generation,source_high_water) VALUES('session-missing-recovery','transcript.projection.v1',$1,1)", &[&generation]).unwrap();
    assert_eq!(
        store
            .catch_up_transcript_projection("session-missing-recovery", generation, 2)
            .unwrap_err(),
        "transcript projection recovery checkpoint is missing"
    );
    let valid: String = db.query_one("SELECT checkpoint_json FROM runtime.transcript_projection_current_recoveries WHERE session_id=$1", &[&session]).unwrap().get(0);
    db.execute("UPDATE runtime.transcript_projection_current_recoveries SET checkpoint_json='{}' WHERE session_id=$1", &[&session]).unwrap();
    assert!(store
        .catch_up_transcript_projection(session, generation, 3)
        .unwrap_err()
        .starts_with("decode transcript projection checkpoint failed:"));
    db.execute("UPDATE runtime.transcript_projection_current_recoveries SET checkpoint_json=$2 WHERE session_id=$1", &[&session,&valid]).unwrap();
    db.execute("UPDATE runtime.transcript_projection_heads SET invalidation_reason='synthetic invalidation' WHERE session_id=$1", &[&session]).unwrap();
    assert_eq!(
        store
            .catch_up_transcript_projection(session, generation, 2)
            .unwrap_err(),
        "transcript projection is invalidated: synthetic invalidation"
    );
    println!(
        "{}",
        serde_json::json!({"contract":"actual-catch-up-interleaving","initialHead":"1","competingHead":"2","projectedHead":progress.projected_high_water,"duplicateCommits":0,"missingAndCorruptRejected":true,"invalidationRejected":true})
    );
}

fn projection_boundary_store() -> (
    postgres::Client,
    crate::postgres_store::PostgresRuntimeStore,
) {
    let url = std::env::var("CENTAERIS_PROJECTION_BOUNDARY_DATABASE")
        .expect("dedicated database required");
    let mut db = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let name: String = db
        .query_one("SELECT current_database()", &[])
        .unwrap()
        .get(0);
    assert!(
        name.starts_with("test_managed_assistant_projection_") || name.starts_with("test_outbox_")
    );
    db.batch_execute(r#"
        CREATE TABLE IF NOT EXISTS app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL,
            status text NOT NULL DEFAULT 'active',"purgedAt" timestamptz);
        CREATE TABLE IF NOT EXISTS app_core_sessionevent(
            "eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,
            agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,
            session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,
            payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL,
            UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));
    "#).unwrap();
    let store = crate::postgres_store::PostgresRuntimeStore::new(&url).unwrap();
    (db, store)
}

fn boundary_envelope(session: &str, sequence: i32) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": "session.event.v1", "eventVersion": 1,
        "sequence": sequence, "type": "model_request_started",
        "eventId": format!("{session}-event-{sequence}"), "sessionId": session,
        "turnId": "turn-fixture", "agentRunId": format!("{session}-run"),
        "createdAtMs": sequence,
        "payload": {"requestId": format!("request-{sequence}"),
            "observations": {"manifestDigest": format!("sha256:{}", "a".repeat(64))}}
    })
}

fn insert_boundary_envelopes(db: &mut postgres::Client, session: &str, count: i32) {
    db.execute(
        "INSERT INTO app_core_session(id,workspace_id) VALUES($1,'workspace-fixture')",
        &[&session],
    )
    .unwrap();
    let run = format!("{session}-run");
    let mut tx = db.transaction().unwrap();
    for sequence in 1..=count {
        let wire = boundary_envelope(session, sequence);
        let event = wire["eventId"].as_str().unwrap();
        tx.execute(
            "INSERT INTO app_core_sessionevent(\"eventId\",workspace_id,session_id,agent_run_id,sequence,agent_run_sequence,projects_to_agent_run_stream,payload,\"createdAtMs\",\"insertedAt\") VALUES($1,'workspace-fixture',$2,$3,$4,$4,false,$5::text::jsonb,$6,now())",
            &[&event, &session, &run, &sequence, &serde_json::to_string(&wire).unwrap(), &i64::from(sequence)],
        ).unwrap();
    }
    tx.commit().unwrap();
}

#[test]
#[ignore = "explicit PostgreSQL batch contract against a fresh disposable database"]
fn postgres_transcript_batch_is_atomic_bounded_and_resumable() {
    use centaeris_core::session::transcript::TranscriptProjectionStorePort;
    let (mut db, store) = projection_boundary_store();
    let session = "session-batch-atomic";
    insert_boundary_envelopes(&mut db, session, 130);
    let current = store
        .load_or_initialize_current_transcript_generation(session)
        .unwrap();
    let generation = &current.projection_generation;
    let first = store
        .catch_up_transcript_projection(session, generation, 130)
        .unwrap();
    assert_eq!(first.events_projected, 128);
    assert_eq!(first.projected_high_water, 128);
    assert!(!first.caught_up);
    // All observable event commits in this batch became durable together.
    let transactions: i64 = db.query_one(
        "SELECT count(DISTINCT xmin::text) FROM runtime.transcript_projection_commits WHERE session_id=$1",
        &[&session],
    ).unwrap().get(0);
    assert_eq!(transactions, 1);
    let recovery = store
        .load_latest_transcript_recovery(session, generation, 130)
        .unwrap()
        .unwrap();
    assert_eq!(recovery.checkpoint.source_high_water, "128");

    // A late malformed source rolls back the whole next batch, including its
    // earlier event's patch and replacement current recovery slot.
    db.execute(
        "UPDATE app_core_sessionevent SET payload='{}'::jsonb WHERE session_id=$1 AND sequence=130",
        &[&session],
    )
    .unwrap();
    assert!(store
        .catch_up_transcript_projection(session, generation, 130)
        .is_err());
    assert_eq!(
        store
            .load_transcript_projection_head(session, generation)
            .unwrap()
            .unwrap()
            .source_high_water,
        "128"
    );
    assert_eq!(
        store
            .load_latest_transcript_recovery(session, generation, 130)
            .unwrap()
            .unwrap()
            .checkpoint
            .source_high_water,
        "128"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.transcript_projection_commits WHERE session_id=$1",
            &[&session]
        )
        .unwrap()
        .get::<_, i64>(0),
        128
    );
    let repaired = serde_json::to_string(&boundary_envelope(session, 130)).unwrap();
    db.execute("UPDATE app_core_sessionevent SET payload=$2::text::jsonb WHERE session_id=$1 AND sequence=130", &[&session, &repaired]).unwrap();
    let resumed = store
        .catch_up_transcript_projection(session, generation, 130)
        .unwrap();
    assert_eq!(resumed.events_projected, 2);
    assert_eq!(resumed.projected_high_water, 130);
    assert!(resumed.caught_up);
    assert_eq!(
        store
            .catch_up_transcript_projection(session, generation, 130)
            .unwrap()
            .events_projected,
        0
    );

    // The existing byte bound applies before decoding, including its one-event
    // progress exception. It is independent of the event-count bound above.
    let session = "session-batch-bytes";
    insert_boundary_envelopes(&mut db, session, 2);
    let padding = "输入".repeat(32);
    db.execute("UPDATE app_core_sessionevent SET payload=jsonb_set(payload,'{payload,padding}',to_jsonb($2::text)) WHERE session_id=$1", &[&session, &padding]).unwrap();
    let query = |db: &mut postgres::Client, after: i64, through: i64, count: i64, bytes: i64| {
        db.query(
            TRANSCRIPT_SOURCE_SLICE_SQL,
            &[&session, &after, &through, &count, &bytes],
        )
        .unwrap()
    };
    let full = query(&mut db, 0, 2, 128, i64::MAX);
    assert_eq!(full.len(), 2);
    for row in &full {
        let payload: String = row.get(2);
        assert_eq!(row.get::<_, i64>(3), payload.len() as i64);
        assert!(payload.len() > payload.chars().count());
    }
    let exact_bytes = full.iter().map(|row| row.get::<_, i64>(3)).sum::<i64>();
    assert_eq!(query(&mut db, 0, 2, 128, exact_bytes - 1).len(), 1);
    assert_eq!(query(&mut db, 0, 2, 128, exact_bytes).len(), 2);
    assert_eq!(query(&mut db, 0, 2, 1, i64::MAX).len(), 1);
    assert_eq!(query(&mut db, 0, 1, 128, i64::MAX)[0].get::<_, i64>(0), 1);
    assert_eq!(query(&mut db, 1, 2, 128, i64::MAX)[0].get::<_, i64>(0), 2);
    let padding = "x".repeat(super::TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES);
    db.execute("UPDATE app_core_sessionevent SET payload=jsonb_set(payload,'{payload,padding}',to_jsonb($2::text)) WHERE session_id=$1", &[&session, &padding]).unwrap();
    let rows = db
        .query(
            TRANSCRIPT_SOURCE_SLICE_SQL,
            &[
                &session,
                &0_i64,
                &2_i64,
                &128_i64,
                &(super::TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES as i64),
            ],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, i64>(0), 1);
    assert!(rows[0].get::<_, i64>(3) > super::TRANSCRIPT_PROJECTION_SLICE_MAX_BYTES as i64);
}

#[test]
#[ignore = "explicit PostgreSQL mixed batch contract against a fresh disposable database"]
fn postgres_transcript_mixed_batch_rolls_back_flush_and_preserves_block_contracts() {
    use crate::postgres_store::PostgresSessionLog;
    use centaeris_core::model::ToolCallEnvelope;
    use centaeris_core::session::transcript::{
        TranscriptBlockV1, TranscriptCommittedBlockV1, TranscriptProjectionCommitV1,
        TranscriptProjectionStorePort,
    };
    use centaeris_core::session::{AgentRunSessionState, SessionLogPort};
    use centaeris_core::tool::layer::ToolExecutionResult;

    let (mut db, store) = projection_boundary_store();
    let session = "session-mixed-batch";
    let run = "run-mixed-batch";
    let turn = "turn-mixed-batch";
    db.execute(
        "INSERT INTO app_core_session(id,workspace_id) VALUES($1,'workspace-fixture')",
        &[&session],
    )
    .unwrap();
    let mut state = AgentRunSessionState::new(session, run).unwrap();
    let mut records = state.start(turn, "hello", vec![], 1).unwrap();
    let call = ToolCallEnvelope {
        id: "call-mixed".into(),
        name: "bash".into(),
        args_json: serde_json::json!({"command":"printf hello"}).to_string(),
    };
    records.push(
        state
            .record_tool_call(
                turn,
                &call,
                "synthetic",
                &format!("sha256:{}", "a".repeat(64)),
                "synthetic.txt",
                2,
            )
            .unwrap()
            .unwrap(),
    );
    records.extend(
        state
            .record_tool_result(
                turn,
                &call,
                &ToolExecutionResult {
                    tool_call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    status: "ok".into(),
                    content: "hello".into(),
                    details: serde_json::json!({}),
                    facts: vec![],
                    error: None,
                    started_at_ms: 2,
                    completed_at_ms: 3,
                    latency_ms: 1,
                    parallel_group: None,
                    transition_reason: None,
                },
                3,
            )
            .unwrap(),
    );
    records.push(
        state
            .assistant(turn, "done", vec![], "done", 4)
            .unwrap()
            .unwrap(),
    );
    records.push(state.complete(turn, "completed", 5).unwrap());
    assert_eq!(records.len(), 6);
    let log = PostgresSessionLog::new(
        store.ordinary_connections.clone(),
        "workspace-fixture".into(),
        session.into(),
        "hello".into(),
    );
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(log.append_session_records(run, &records))
        .unwrap();
    let generation = store
        .load_or_initialize_current_transcript_generation(session)
        .unwrap()
        .projection_generation;
    store
        .catch_up_transcript_projection(session, &generation, 2)
        .unwrap();
    let state_counts = |db: &mut postgres::Client| {
        let row = db.query_one("SELECT h.source_high_water,r.source_high_water,(SELECT count(*) FROM runtime.transcript_block_identities WHERE session_id=$1),(SELECT count(*) FROM runtime.transcript_block_versions WHERE session_id=$1),(SELECT count(*) FROM runtime.transcript_resume_cursors WHERE session_id=$1),(SELECT count(*) FROM runtime.transcript_projection_commits WHERE session_id=$1) FROM runtime.transcript_projection_heads h JOIN runtime.transcript_projection_current_recoveries r USING(session_id,projection_version,projection_generation) WHERE h.session_id=$1", &[&session]).unwrap();
        (0..6)
            .map(|index| row.get::<_, i64>(index))
            .collect::<Vec<_>>()
    };
    let before_failed_flush = state_counts(&mut db);
    // Fail the last INSERT after block/cursor/recovery/head writes have run.
    // PostgreSQL must roll them all back, then a normal retry must succeed.
    db.batch_execute(r#"
        CREATE FUNCTION public.reject_mixed_batch_commit() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN IF NEW.session_id='session-mixed-batch' AND NEW.source_high_water=6 THEN
            RAISE EXCEPTION 'synthetic final batch write failure'; END IF; RETURN NEW; END $$;
        CREATE TRIGGER reject_mixed_batch_commit BEFORE INSERT ON runtime.transcript_projection_commits
            FOR EACH ROW EXECUTE FUNCTION public.reject_mixed_batch_commit();
    "#).unwrap();
    assert!(store
        .catch_up_transcript_projection(session, &generation, 6)
        .unwrap_err()
        .contains("record transcript projection commits failed"));
    assert_eq!(state_counts(&mut db), before_failed_flush);
    db.batch_execute("DROP TRIGGER reject_mixed_batch_commit ON runtime.transcript_projection_commits; DROP FUNCTION public.reject_mixed_batch_commit()").unwrap();
    assert!(
        store
            .catch_up_transcript_projection(session, &generation, 6)
            .unwrap()
            .caught_up
    );
    let after_retry = state_counts(&mut db);
    assert_eq!(
        (&after_retry[..2], &after_retry[4..]),
        (&[6, 6][..], &[6, 6][..])
    );
    assert!(after_retry[2] > before_failed_flush[2]);
    assert!(after_retry[3] > after_retry[2]);

    let tool_json: String = db.query_one("SELECT block_json FROM runtime.transcript_block_versions WHERE session_id=$1 AND applied_source_sequence=4", &[&session]).unwrap().get(0);
    let tool: TranscriptBlockV1 = serde_json::from_str(&tool_json).unwrap();
    let mut commit = TranscriptProjectionCommitV1 {
        commit_id: "mixed-next".into(),
        session_id: session.into(),
        projection_version: "transcript.projection.v1".into(),
        projection_generation: generation.clone(),
        expected_source_high_water: "6".into(),
        source_high_water: "7".into(),
        upserts: vec![TranscriptCommittedBlockV1 {
            applied_source_sequence: "7".into(),
            block: tool.clone(),
        }],
        resume_cursors: vec![],
        checkpoint: None,
        frontier: None,
        invalidation_reason: None,
    };
    commit.upserts[0].block.block_revision = "3".into();
    commit.upserts[0].block.order_key.source_sequence = "2".into();
    assert!(store
        .commit_transcript_projection(commit.clone())
        .unwrap_err()
        .contains("orderKey changed across revisions"));
    commit.upserts[0].block = tool.clone();
    assert!(store
        .commit_transcript_projection(commit.clone())
        .unwrap_err()
        .contains("version is not increasing"));
    commit.upserts[0].block.block_revision = "3".into();
    commit.upserts[0].applied_source_sequence = "4".into();
    assert!(store
        .commit_transcript_projection(commit.clone())
        .unwrap_err()
        .contains("version is not increasing"));
    commit.upserts[0].applied_source_sequence = "7".into();
    store.commit_transcript_projection(commit.clone()).unwrap();
    // A commitId belongs to its entire original scope, not just its patch bytes.
    commit.session_id = "session-other".into();
    commit.expected_source_high_water = "0".into();
    commit.source_high_water = "1".into();
    commit.upserts.clear();
    assert!(store
        .commit_transcript_projection(commit)
        .unwrap_err()
        .contains("commitId conflict"));
    assert_eq!(
        store
            .load_transcript_projection_head("session-other", &generation)
            .unwrap(),
        None
    );
}

#[test]
#[ignore = "explicit PostgreSQL generation contract against a fresh disposable database"]
fn postgres_transcript_current_encoding_rebuild_survives_initialization() {
    use centaeris_core::session::transcript::{
        TranscriptProjectionGenerationStorePortV1, TranscriptProjectionStorePort,
    };
    let (mut db, store) = projection_boundary_store();
    let session = "session-current-rebuild";
    let generation = super::workspace_transcript_generation("generation-rebuild-97");
    db.execute("INSERT INTO runtime.transcript_projection_heads(session_id,projection_version,projection_generation,source_high_water) VALUES($1,'transcript.projection.v1',$2,97)", &[&session, &generation]).unwrap();
    db.execute("INSERT INTO runtime.transcript_projection_current_generations(session_id,projection_version,projection_generation,source_high_water) VALUES($1,'transcript.projection.v1',$2,97)", &[&session, &generation]).unwrap();
    for _ in 0..2 {
        let current = store
            .load_or_initialize_current_transcript_generation(session)
            .unwrap();
        assert_eq!(current.projection_generation, generation);
        assert_eq!(current.source_high_water, "97");
        assert_eq!(
            store
                .load_transcript_projection_head(session, &generation)
                .unwrap()
                .unwrap()
                .source_high_water,
            "97"
        );
    }
    assert_eq!(
        store
            .load_current_transcript_projection_generation(session)
            .unwrap()
            .unwrap()
            .projection_generation,
        generation
    );
}
