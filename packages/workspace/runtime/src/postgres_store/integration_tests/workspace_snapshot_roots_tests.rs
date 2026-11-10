use super::retained_instance_tests as fixture;
use super::*;
use centaeris_core::execution::ExecutionWorkspaceGeneration;
use centaeris_core::runtime::contracts::{
    CheckpointKindV1, RecoveryWorkspaceSnapshotV1, RuntimeRecoveryCheckpointV1,
};
use centaeris_core::session::AgentRunSessionState;
use sha2::{Digest, Sha256};

fn manifest(store: &PostgresRuntimeStore) -> serde_json::Value {
    let (status, body) = crate::runtime_http_tests::stored_request(
        store.clone(),
        "POST",
        "/internal/workspace-snapshots/roots",
        br#"{"schema":"runtime.workspace_snapshot.roots.get.v1","sessionId":"sess_1"}"#,
    );
    assert_eq!(
        status,
        200,
        "read-only root manifest must be available: {}",
        String::from_utf8_lossy(&body)
    );
    serde_json::from_slice(&body).unwrap()
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_workspace_snapshot_roots_reject_foreign_malformed_or_rewritten_objects() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for invalid in [
        "foreignRun",
        "invalidDirectory",
        "uppercaseDirectory",
        "wrongDigest",
        "rewrittenBody",
    ] {
        let (_url, store, mut db) = fixture::setup();
        let run = fixture::run(&store, &mut db, "object-root-owner", 1);
        db.batch_execute(r#"DROP TABLE public.app_core_sessionevent;
        CREATE TABLE public.app_core_sessionevent("eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));"#).unwrap();
        let owner = if invalid == "foreignRun" {
            "foreign-run"
        } else {
            &run.original.agent_run_id
        };
        let directory = match invalid {
            "invalidDirectory" => "not-a-directory-hash".into(),
            "uppercaseDirectory" => "A".repeat(64),
            _ => "c".repeat(64),
        };
        let filename = if invalid == "wrongDigest" {
            "b".repeat(64)
        } else {
            "a".repeat(64)
        };
        let now = crate::now_ms().unwrap();
        let payload=RuntimeRecoveryCheckpointV1 {schema:"runtime.recovery_checkpoint.v1".into(),checkpoint_id:"owner-checkpoint".into(),session_id:"sess_1".into(),agent_run_id:run.original.agent_run_id.clone(),execution_id:"owner-execution".into(),authorization_digest:run.original.digest().unwrap(),session_sequence:4,model_request_id:"owner-model-request".into(),workspace_snapshot:RecoveryWorkspaceSnapshotV1 {object_ref:Some(format!("workspaces/ws_1/sessions/sess_1/agent-runs/{owner}/execution-checkpoints/{directory}/{filename}.snapshot")),snapshot_sha256:format!("sha256:{}","a".repeat(64)),snapshot_size_bytes:16,expanded_size_bytes:16,file_count:1},workspace_generation:ExecutionWorkspaceGeneration::Unknown {reason:"fixture".into()},created_at_ms:now};
        let checkpoint = CheckpointRecord {
            checkpoint_id: payload.checkpoint_id.clone(),
            kind: CheckpointKindV1::Recovery,
            session_id: "sess_1".into(),
            turn_id: "owner-turn".into(),
            status: "committed".into(),
            done_reason: None,
            updated_at_ms: now,
            payload_json: serde_json::to_string(&payload).unwrap(),
        };
        let mut state = AgentRunSessionState::new("sess_1", &run.original.agent_run_id).unwrap();
        let mut records = state
            .start("owner-turn", "root contract", vec![], now)
            .unwrap();
        records.push(
            state
                .start_execution(
                    "owner-turn",
                    "owner-execution",
                    &run.original.digest().unwrap(),
                    None,
                    now,
                )
                .unwrap(),
        );
        records.push(state.checkpoint_ref(&checkpoint).unwrap());
        store
            .session_log("ws_1".into(), "sess_1".into(), "root contract".into())
            .append_recovery_checkpoint_with_runtime_job_lease_blocking(
                &run.original.agent_run_id,
                &records,
                &checkpoint,
                &run.fence,
            )
            .unwrap();
        if invalid == "rewrittenBody" {
            db.batch_execute("UPDATE runtime.checkpoints SET payload_json=(payload_json::jsonb||'{\"modelRequestId\":\"rewritten-request\"}'::jsonb)::text").unwrap();
        }
        let (status, body) = crate::runtime_http_tests::stored_request(
            store,
            "POST",
            "/internal/workspace-snapshots/roots",
            br#"{"schema":"runtime.workspace_snapshot.roots.get.v1","sessionId":"sess_1"}"#,
        );
        assert_eq!(
            status,
            503,
            "reject {invalid}: {}",
            String::from_utf8_lossy(&body)
        );
    }
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_workspace_snapshot_roots_fail_closed_without_partial_manifest_at_live_bound() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    db.batch_execute("ALTER TABLE public.app_core_sessionevent ADD COLUMN sequence integer; INSERT INTO public.app_core_agentrun(id,session_id,status) SELECT 'live-root-'||n,'sess_1','queued' FROM generate_series(1,257) n").unwrap();
    let (status, body) = crate::runtime_http_tests::stored_request(
        store,
        "POST",
        "/internal/workspace-snapshots/roots",
        br#"{"schema":"runtime.workspace_snapshot.roots.get.v1","sessionId":"sess_1"}"#,
    );
    assert_eq!(status, 503);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"error":"workspace_snapshot_roots_unavailable"})
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM public.app_core_agentrun", &[])
            .unwrap()
            .get::<_, i64>(0),
        257,
        "root inspection cannot mutate Run responsibility"
    );
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_workspace_snapshot_roots_protect_unknown_responsibility_without_rooting_settled_run_history(
) {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "unknown-root-run", 1);
    db.batch_execute("ALTER TABLE public.app_core_sessionevent ADD COLUMN sequence integer, ADD COLUMN agent_run_sequence integer")
        .unwrap();
    let physical = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    fixture::prepare(&store, &run, "unknown-root-allocation", &physical).unwrap();
    db.batch_execute("UPDATE runtime.runtime_jobs SET status='succeeded',lease_owner=NULL,lease_expires_at_ms=NULL; UPDATE public.app_core_agentrun SET status='completed'; UPDATE public.app_core_sessionenvironment SET controller_released=true; INSERT INTO public.app_core_agentrun(id,session_id,status) SELECT 'settled-history-'||n,'sess_1','completed' FROM generate_series(1,300) n").unwrap();
    assert_eq!(
        manifest(&store)["protectedAgentRunIds"],
        serde_json::json!([]),
        "settled history does not consume the live-root bound"
    );
    db.batch_execute("UPDATE runtime.resident_sandboxes SET state='releasing'")
        .unwrap();
    let unknown = manifest(&store);
    assert_eq!(
        unknown["protectedAgentRunIds"],
        serde_json::json!([run.original.agent_run_id])
    );
    assert_eq!(unknown["roots"],serde_json::json!([]),"unknown physical liability protects its signed baseline without inventing checkpoint references");
}

#[test]
#[ignore = "requires owned disposable PostgreSQL"]
fn postgres_workspace_snapshot_roots_keep_only_latest_used_checkpoint_until_responsibility_settles()
{
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_url, store, mut db) = fixture::setup();
    let run = fixture::run(&store, &mut db, "root-manifest-run", 1);
    db.batch_execute(r#"DROP TABLE public.app_core_sessionevent;
    CREATE TABLE public.app_core_sessionevent("eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));"#).unwrap();
    let empty = manifest(&store);
    assert_eq!(empty["sessionSequence"], 0);
    assert_eq!(empty["roots"], serde_json::json!([]));
    assert_eq!(
        empty["protectedAgentRunIds"],
        serde_json::json!([run.original.agent_run_id])
    );
    let now = crate::now_ms().unwrap();
    let mut state = AgentRunSessionState::new("sess_1", &run.original.agent_run_id).unwrap();
    let mut records = state.start("root-turn", "capture", vec![], now).unwrap();
    records.push(
        state
            .start_execution(
                "root-turn",
                "root-execution",
                &run.original.digest().unwrap(),
                None,
                now,
            )
            .unwrap(),
    );
    let log = store.session_log("ws_1".into(), "sess_1".into(), "capture".into());
    log.append_session_records_with_runtime_job_lease_blocking(
        &run.original.agent_run_id,
        &records,
        &run.fence,
    )
    .unwrap();
    let mut refs = vec![];
    for index in 0..2 {
        let id = format!("root-checkpoint-{index}");
        let object = format!(
            "workspaces/ws_1/sessions/sess_1/agent-runs/{}/execution-checkpoints/{:x}/{}.snapshot",
            run.original.agent_run_id,
            // Later checkpoints retain this first immutable upload while the
            // workspace generation stays unchanged across model requests.
            Sha256::digest(b"root-checkpoint-0"),
            "a".repeat(64)
        );
        let payload = RuntimeRecoveryCheckpointV1 {
            schema: "runtime.recovery_checkpoint.v1".into(),
            checkpoint_id: id.clone(),
            session_id: "sess_1".into(),
            agent_run_id: run.original.agent_run_id.clone(),
            execution_id: "root-execution".into(),
            authorization_digest: run.original.digest().unwrap(),
            session_sequence: 4 + index,
            model_request_id: "root-model-request".into(),
            workspace_snapshot: RecoveryWorkspaceSnapshotV1 {
                object_ref: Some(object.clone()),
                snapshot_sha256: format!("sha256:{}", "a".repeat(64)),
                snapshot_size_bytes: 16,
                expanded_size_bytes: 16,
                file_count: 1,
            },
            workspace_generation: ExecutionWorkspaceGeneration::Unknown {
                reason: "test capture".into(),
            },
            created_at_ms: now,
        };
        let checkpoint = CheckpointRecord {
            checkpoint_id: id.clone(),
            kind: CheckpointKindV1::Recovery,
            session_id: "sess_1".into(),
            turn_id: "root-turn".into(),
            status: "committed".into(),
            done_reason: None,
            updated_at_ms: now,
            payload_json: serde_json::to_string(&payload).unwrap(),
        };
        let event = state.checkpoint_ref(&checkpoint).unwrap();
        log.append_recovery_checkpoint_with_runtime_job_lease_blocking(
            &run.original.agent_run_id,
            &[event],
            &checkpoint,
            &run.fence,
        )
        .unwrap();
        refs.push((id, object));
    }
    let end = state
        .end_execution(
            "root-turn",
            "root-execution",
            "lost",
            "execution_environment_lost",
            true,
            Some(&refs[1].0),
            vec![],
            now,
        )
        .unwrap();
    let started = state
        .start_execution(
            "root-turn",
            "root-replacement",
            &run.original.digest().unwrap(),
            Some(&refs[1].0),
            now,
        )
        .unwrap();
    log.append_session_records_with_runtime_job_lease_blocking(
        &run.original.agent_run_id,
        &[end, started],
        &run.fence,
    )
    .unwrap();
    let live = manifest(&store);
    assert_eq!(live["sessionSequence"], 7);
    assert_eq!(live["roots"].as_array().unwrap().len(), 1);
    assert_eq!(live["roots"][0]["checkpointId"], refs[1].0);
    assert_eq!(live["roots"][0]["objectRef"], refs[1].1);
    assert_eq!(
        refs[0].1, refs[1].1,
        "the latest checkpoint may reuse an earlier staged immutable object"
    );
    assert_ne!(live["authorityFingerprint"], empty["authorityFingerprint"]);
    db.batch_execute("UPDATE runtime.runtime_jobs SET status='succeeded',lease_owner=NULL,lease_expires_at_ms=NULL; UPDATE public.app_core_agentrun SET status='completed'; UPDATE public.app_core_sessionenvironment SET controller_released=true").unwrap();
    let settled = manifest(&store);
    assert_eq!(settled["roots"], serde_json::json!([]));
    assert_eq!(settled["protectedAgentRunIds"], serde_json::json!([]));
    assert_ne!(
        settled["authorityFingerprint"],
        live["authorityFingerprint"]
    );
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.checkpoints", &[])
            .unwrap()
            .get::<_, i64>(0),
        2,
        "GC roots cannot erase historical checkpoint records"
    );
}
