use super::*;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_hosted_persistence_fences_late_execution_and_deleted_session_projection() {
    use centaeris_core::runtime::subagent::SUBAGENT_RUN_JOB_KIND;
    use centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND;

    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = super::retained_instance_tests::setup();
    db.batch_execute(
        "ALTER TABLE public.app_core_session ADD COLUMN status text NOT NULL DEFAULT 'active', ADD COLUMN \"purgedAt\" timestamptz",
    )
    .unwrap();
    let mut child_job = job("hosted-persistence-child", "hosted-persistence-child");
    child_job.job_kind = SUBAGENT_RUN_JOB_KIND.into();
    child_job.session_id = Some("sess_1".into());
    child_job.status = RuntimeJobStatus::Running;
    child_job.lease_owner = Some("child-owner".into());
    child_job.lease_expires_at_ms = Some(4_000_000_000_000);
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: child_job.clone(),
        })
        .unwrap();
    let mut parent_job = child_job.clone();
    parent_job.job_id = "hosted-persistence-parent".into();
    parent_job.idempotency_key = parent_job.job_id.clone();
    parent_job.job_kind = AGENT_RUN_LIFECYCLE_JOB_KIND.into();
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: parent_job.clone(),
        })
        .unwrap();
    let child_fence = RuntimeJobLeaseFence {
        job_id: child_job.job_id.clone(),
        job_kind: child_job.job_kind.clone(),
        lease_owner: "child-owner".into(),
    };
    let child = store
        .hosted_execution_view("sess_1", "child-session", child_fence.clone())
        .unwrap();
    let parent = store
        .hosted_execution_view(
            "sess_1",
            "sess_1",
            RuntimeJobLeaseFence {
                job_id: parent_job.job_id,
                job_kind: parent_job.job_kind,
                lease_owner: "child-owner".into(),
            },
        )
        .unwrap();
    parent
        .save_agent_runtime_snapshot("sess_1", "{}", 1)
        .unwrap();
    db.execute("DELETE FROM public.app_core_presentationchangeoutbox", &[])
        .unwrap();
    child
        .save_agent_runtime_snapshot("child-session", "{}", 1)
        .unwrap();
    assert_eq!(db.query_one("SELECT count(*) FROM public.app_core_presentationchangeoutbox WHERE key='session:sess_1:subagents'", &[]).unwrap().get::<_, i64>(0), 1);
    db.execute("DELETE FROM public.app_core_presentationchangeoutbox", &[])
        .unwrap();
    assert!(!child
        .compare_and_save_agent_runtime_snapshot("child-session", Some("other"), "changed", 2)
        .unwrap());
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM public.app_core_presentationchangeoutbox",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert!(child
        .compare_and_save_agent_runtime_snapshot("child-session", Some("{}"), "changed", 2)
        .unwrap());
    assert_eq!(db.query_one("SELECT count(*) FROM public.app_core_presentationchangeoutbox WHERE key='session:sess_1:subagents'", &[]).unwrap().get::<_, i64>(0), 1);
    assert!(child
        .save_agent_runtime_snapshot("sess_1", "wrong target", 2)
        .is_err());
    assert!(parent
        .save_agent_runtime_snapshot("child-session", "wrong target", 2)
        .is_err());

    db.execute(
        "UPDATE runtime.runtime_jobs SET lease_owner='replacement-owner' WHERE job_id=$1",
        &[&child_job.job_id],
    )
    .unwrap();
    assert_eq!(
        child
            .save_agent_runtime_snapshot("child-session", "stale owner", 2)
            .unwrap_err(),
        RUNTIME_JOB_LEASE_FENCE_REJECTED,
    );
    let cancelled_child = store
        .hosted_execution_view(
            "sess_1",
            "child-session",
            RuntimeJobLeaseFence {
                lease_owner: "replacement-owner".into(),
                ..child_fence
            },
        )
        .unwrap();
    store
        .cancel_runtime_job(CancelRuntimeJobRequest {
            expected_lease_owner: Some("replacement-owner".into()),
            job_id: child_job.job_id,
            reason: "delete-session".into(),
            cancelled_at_ms: 3,
            expected_status: Some(RuntimeJobStatus::Running),
        })
        .unwrap();
    let late_event = RuntimeEvent {
        event_id: "late-child-event".into(),
        session_id: "child-session".into(),
        task_id: None,
        event_type: "subagent_tool_receipt_committed".into(),
        at_ms: 4,
        visibility: EventVisibility::Internal,
        payload_json: "{}".into(),
    };
    assert!(cancelled_child.append_event_idempotent(late_event).is_err());
    assert!(cancelled_child
        .upsert_external_context_object(ExternalContextObject {
            schema_version:
                centaeris_core::session::external_context::EXTERNAL_CONTEXT_SCHEMA_VERSION.into(),
            object_id: "late-child-object".into(),
            object_kind: "tool_evidence".into(),
            source_provider_id: "centaeris.core".into(),
            source_tool_name: "read".into(),
            title: "late evidence".into(),
            content: "late evidence".into(),
            metadata: serde_json::json!({}),
            updated_at_ms: 4,
        })
        .is_err());
    store.delete_session_data("child-session").unwrap();
    store.delete_session_data("sess_1").unwrap();
    assert_eq!(
        cancelled_child
            .save_agent_runtime_snapshot("child-session", "after purge", 5)
            .unwrap_err(),
        RUNTIME_JOB_LEASE_FENCE_REJECTED,
    );
    assert_eq!(
        parent
            .save_agent_runtime_snapshot("sess_1", "after purge", 5)
            .unwrap_err(),
        RUNTIME_JOB_LEASE_FENCE_REJECTED,
    );

    let projection = store.subagent_terminal_projection_view("sess_1").unwrap();
    assert!(projection
        .compare_and_save_agent_runtime_snapshot("sess_1", None, "{}", 6)
        .unwrap());
    store.delete_session_data("sess_1").unwrap();
    db.execute(
        "UPDATE public.app_core_session SET status='deleted',\"purgedAt\"=clock_timestamp() WHERE id='sess_1'",
        &[],
    ).unwrap();
    assert!(projection
        .compare_and_save_agent_runtime_snapshot("sess_1", None, "late result", 7)
        .is_err());
    assert!(store
        .load_agent_runtime_snapshot("sess_1")
        .unwrap()
        .is_none());
    assert!(store
        .load_agent_runtime_snapshot("child-session")
        .unwrap()
        .is_none());
    assert!(store
        .list_events("child-session", 10, 0)
        .unwrap()
        .is_empty());
    assert!(store
        .load_external_context_object("late-child-object")
        .unwrap()
        .is_none());
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_snapshot_cas_rejects_stale_writers_and_preserves_concurrent_results() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert!(store
        .compare_and_save_agent_runtime_snapshot("parent", None, "{}", 1)
        .unwrap());
    assert!(!store
        .compare_and_save_agent_runtime_snapshot("parent", None, "bad", 2)
        .unwrap());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writers: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|key| {
            let store = PostgresRuntimeStore::new(&url).unwrap();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let expected = store
                    .load_agent_runtime_snapshot("parent")
                    .unwrap()
                    .unwrap();
                barrier.wait();
                let candidate = serde_json::json!({key:true}).to_string();
                let won = store
                    .compare_and_save_agent_runtime_snapshot(
                        "parent",
                        Some(&expected),
                        &candidate,
                        3,
                    )
                    .unwrap();
                if !won {
                    let latest = store
                        .load_agent_runtime_snapshot("parent")
                        .unwrap()
                        .unwrap();
                    let mut merged: serde_json::Value = serde_json::from_str(&latest).unwrap();
                    merged[key] = serde_json::json!(true);
                    assert!(store
                        .compare_and_save_agent_runtime_snapshot(
                            "parent",
                            Some(&latest),
                            &merged.to_string(),
                            4
                        )
                        .unwrap());
                }
                won
            })
        })
        .collect();
    assert_eq!(
        writers
            .into_iter()
            .map(|writer| usize::from(writer.join().unwrap()))
            .sum::<usize>(),
        1
    );
    let actual: serde_json::Value = serde_json::from_str(
        &store
            .load_agent_runtime_snapshot("parent")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(actual, serde_json::json!({"a":true,"b":true}));
    assert!(!store
        .compare_and_save_agent_runtime_snapshot("missing", Some("{}"), "bad", 5)
        .unwrap());
    assert!(store
        .load_agent_runtime_snapshot("missing")
        .unwrap()
        .is_none());
}
