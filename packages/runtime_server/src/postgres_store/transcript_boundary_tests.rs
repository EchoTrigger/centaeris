// The observer is test-only and thread-local; the real catch-up path does all reads and commits.
#[test]
#[ignore = "explicit PostgreSQL catch-up contract against a fresh disposable database"]
fn postgres_transcript_head_recovery_read_interleaves_with_next_commit() {
    use crate::postgres_store::PostgresRuntimeStore;
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
    let generation = "generation-1";
    let run = "run-head-race";
    let mut state = AgentRunSessionState::new(session, run).unwrap();
    let records = state.start(run, "synthetic", vec![], 1).unwrap();
    let log = store.session_log(
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
    let reader = std::thread::spawn(move || {
        super::CATCH_UP_AFTER_INITIAL_READ.with(|observer| {
            *observer.borrow_mut() = Some(Box::new(move || {
                pinned_tx.send(()).unwrap();
                committed_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            }));
        });
        reader_store.catch_up_transcript_projection(session, generation, 2)
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
    db.execute("INSERT INTO runtime.transcript_projection_heads(session_id,projection_version,projection_generation,source_high_water) VALUES('session-missing-recovery','transcript.projection.v1','generation-1',1)", &[]).unwrap();
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
