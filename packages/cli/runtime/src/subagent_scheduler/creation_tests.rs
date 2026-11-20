//! Restart recovery of the durable Job-to-native-Session creation handoff.
use super::queue_tests::{job, object, packet, Profile};
use super::*;
use centaeris_core::runtime::subagent::{
    run_due_subagent_jobs_async, NoopSubagentLifecycleObserver, SubagentWorkerRunOutcome,
};
use centaeris_core::session::store::{
    RuntimeStoreTransactionPort, UpsertExternalContextAndScheduleJobRequest,
};

const TEST: &str =
    "subagent_scheduler::creation_tests::committed_creation_recovers_missing_child_after_restart";

struct SuccessfulWorker;
impl AsyncSubagentWorkerRunner for SuccessfulWorker {
    fn run_async<'a>(&'a self, _: SubagentWorkerRunRequest) -> SubagentWorkerRunFuture<'a> {
        Box::pin(async {
            SubagentWorkerRunOutcome::Succeeded {
                summary: "Recovered creation completed.".into(),
                output_refs: vec![],
            }
        })
    }
}

#[test]
fn committed_creation_recovers_missing_child_after_restart() {
    if std::env::var("CENTAERIS_CREATION_TEST_MODE")
        .ok()
        .as_deref()
        == Some("scan")
    {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let parent = std::env::var("CENTAERIS_CREATION_TEST_PARENT").unwrap();
            for name in ["first", "later"] {
                assert_eq!(
                    next_queued_parent_session().await.unwrap(),
                    Some(parent.clone())
                );
                let row = job(&parent, name, 1);
                let child = format!("session-{name}");
                assert_eq!(
                    sessions::find_agent_runtime_binding(&child).unwrap(),
                    Some((parent.clone(), row.job_id.clone()))
                );
                let run = message_log::project_agent_run(&row.job_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(run.session_id, child);
                assert_eq!(run.turn_id, "child-turn");
                assert_eq!(
                    next_queued_parent_session().await.unwrap(),
                    Some(parent.clone())
                );
                assert_eq!(
                    message_log::project_session_agent_runs(&child)
                        .unwrap()
                        .len(),
                    1
                );
                let store = agent_runtime::agent_runtime_job_store().unwrap();
                let result = run_due_subagent_jobs_async(
                    store.as_ref(),
                    &SuccessfulWorker,
                    &NoopSubagentLifecycleObserver,
                    RunDueSubagentJobsRequest {
                        now_ms: 10,
                        worker_id: "creation-worker".into(),
                        session_id: Some(parent.clone()),
                        limit: 1,
                        lease_ms: 60_000,
                        started_at_ms: 10,
                        finished_at_ms: 11,
                    },
                )
                .await
                .unwrap();
                assert_eq!(result.succeeded, 1);
                assert_eq!(result.results[0].job_id, row.job_id);
            }
            assert_eq!(next_queued_parent_session().await.unwrap(), None);
            agent_runtime::close_storage_owners().await.unwrap();
        });
        return;
    }
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    let cwd = profile.root.join("workspace").to_string_lossy().to_string();
    let parent = sessions::create(sessions::SessionCreateRequest {
        title: None,
        cwd: cwd.clone(),
    })
    .unwrap();
    let store =
        centaeris_runtime_sqlite::SqliteRuntimeStore::new(profile.root.join("runtime.sqlite3"))
            .unwrap();
    for name in ["first", "later"] {
        let row = job(&parent.id, name, if name == "first" { 1 } else { 2 });
        let child = format!("session-{name}");
        let mut payload = packet(&parent.id, &child, &row, &cwd);
        payload["workPacket"]["run_context"]["agentRef"]["agentId"] = name.into();
        store
            .upsert_external_context_and_schedule_job(UpsertExternalContextAndScheduleJobRequest {
                object: object(&format!("packet:{name}"), payload.to_string()),
                job: row.clone(),
            })
            .unwrap();
        assert!(sessions::find_agent_runtime_binding(&child)
            .unwrap()
            .is_none());
        assert!(message_log::project_agent_run(&row.job_id)
            .unwrap()
            .is_none());
    }
    // A new process has neither reply state nor a cached Actor/native owner.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env("CENTAERIS_CREATION_TEST_MODE", "scan")
        .env("CENTAERIS_CREATION_TEST_PARENT", &parent.id)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "restart failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn lost_creation_reply_settles_before_host_materialization() {
    use centaeris_core::session::store::AsyncJobStorePort;
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    let cwd = profile.root.join("workspace").to_string_lossy().to_string();
    let parent = sessions::create(sessions::SessionCreateRequest {
        title: None,
        cwd: cwd.clone(),
    })
    .unwrap();
    let row = job(&parent.id, "valid", 1);
    let path = profile.root.join("runtime.sqlite3");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let jobs = centaeris_runtime_sqlite::job_store::SqliteJobStore::open(path.clone(), 2)
            .await
            .unwrap();
        let blocker = rusqlite::Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let request = UpsertExternalContextAndScheduleJobRequest {
            object: object(
                "packet:valid",
                packet(&parent.id, "session-valid", &row, &cwd).to_string(),
            ),
            job: row.clone(),
        };
        let waiting = tokio::spawn({
            let jobs = jobs.clone();
            async move { jobs.upsert_external_context_and_schedule_job(request).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while jobs.resource_metrics().await.in_flight != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());
        blocker.execute_batch("ROLLBACK").unwrap();
        jobs.settle_job_operations().await.unwrap();
        assert_eq!(
            jobs.get_runtime_job(&row.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuntimeJobStatus::Queued
        );
        assert!(sessions::find_agent_runtime_binding("session-valid")
            .unwrap()
            .is_none());
        jobs.close().await.unwrap();
        for _ in 0..2 {
            assert_eq!(
                next_queued_parent_session().await.unwrap(),
                Some(parent.id.clone())
            );
        }
        assert_eq!(
            message_log::project_session_agent_runs("session-valid")
                .unwrap()
                .len(),
            1
        );
        let authority = agent_runtime::agent_runtime_job_store().unwrap();
        assert_eq!(
            authority
                .list_runtime_jobs(ListRuntimeJobsRequest {
                    session_id: Some(parent.id.clone()),
                    limit: 10,
                    ..Default::default()
                })
                .await
                .unwrap()
                .len(),
            1
        );
        agent_runtime::close_storage_owners().await.unwrap();
    });
}

#[test]
fn orphan_creation_cancels_without_recreating_a_deleted_parent() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    let cwd = profile.root.join("workspace").to_string_lossy().to_string();
    let removed = sessions::create(sessions::SessionCreateRequest {
        title: None,
        cwd: cwd.clone(),
    })
    .unwrap();
    let retained = sessions::create(sessions::SessionCreateRequest {
        title: None,
        cwd: cwd.clone(),
    })
    .unwrap();
    let store =
        centaeris_runtime_sqlite::SqliteRuntimeStore::new(profile.root.join("runtime.sqlite3"))
            .unwrap();
    for (parent, name, at) in [(&removed.id, "orphan", 1), (&retained.id, "retained", 2)] {
        let row = job(parent, name, at);
        let child = format!("session-{name}");
        let mut payload = packet(parent, &child, &row, &cwd);
        payload["workPacket"]["run_context"]["agentRef"]["agentId"] = name.into();
        store
            .upsert_external_context_and_schedule_job(UpsertExternalContextAndScheduleJobRequest {
                object: object(&format!("packet:{name}"), payload.to_string()),
                job: row,
            })
            .unwrap();
    }
    // Simulate a parent removed while an accepted creation had no child manifest.
    crate::session_files::SessionFiles::new(crate::user_data_layout::sessions_dir_path())
        .delete(&removed.id)
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        assert_eq!(
            next_queued_parent_session().await.unwrap(),
            Some(retained.id.clone())
        );
        let jobs = agent_runtime::agent_runtime_job_store().unwrap();
        let orphan = jobs
            .get_runtime_job(&job(&removed.id, "orphan", 1).job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(orphan.status, RuntimeJobStatus::Cancelled);
        assert_eq!(orphan.last_error.as_deref(), Some("session_deleted"));
        assert!(sessions::find_agent_runtime_binding("session-orphan")
            .unwrap()
            .is_none());
        assert!(!sessions::list(sessions::SessionListRequest {})
            .unwrap()
            .iter()
            .any(|s| s.id == removed.id));
        agent_runtime::close_storage_owners().await.unwrap();
    });
}

#[test]
fn unavailable_workspaces_beyond_one_page_do_not_starve_later_creation() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    let cwd = profile.root.join("workspace").to_string_lossy().to_string();
    let store =
        centaeris_runtime_sqlite::SqliteRuntimeStore::new(profile.root.join("runtime.sqlite3"))
            .unwrap();
    let mut blocked = Vec::new();
    for index in 0..SUBAGENT_QUEUE_SCAN_LIMIT + 1 {
        let missing = profile
            .root
            .join(format!("temporarily-unavailable-{index}"));
        std::fs::create_dir(&missing).unwrap();
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: missing.to_string_lossy().to_string(),
        })
        .unwrap();
        let name = format!("blocked-{index}");
        let row = job(&parent.id, &name, 1);
        let mut payload = packet(&parent.id, &format!("session-{name}"), &row, &cwd);
        payload["workPacket"]["run_context"]["agentRef"]["agentId"] = name.clone().into();
        store
            .upsert_external_context_and_schedule_job(UpsertExternalContextAndScheduleJobRequest {
                object: object(&format!("packet:{name}"), payload.to_string()),
                job: row.clone(),
            })
            .unwrap();
        std::fs::remove_dir(&missing).unwrap();
        blocked.push(row);
    }
    let parent = sessions::create(sessions::SessionCreateRequest {
        title: None,
        cwd: cwd.clone(),
    })
    .unwrap();
    let row = job(&parent.id, "valid", 2);
    store
        .upsert_external_context_and_schedule_job(UpsertExternalContextAndScheduleJobRequest {
            object: object(
                "packet:valid",
                packet(&parent.id, "session-valid", &row, &cwd).to_string(),
            ),
            job: row.clone(),
        })
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        assert_eq!(
            next_queued_parent_session().await.unwrap(),
            Some(parent.id.clone())
        );
        assert_eq!(
            sessions::find_agent_runtime_binding("session-valid").unwrap(),
            Some((parent.id.clone(), row.job_id.clone()))
        );
        let jobs = agent_runtime::agent_runtime_job_store().unwrap();
        for row in blocked {
            assert_eq!(
                jobs.get_runtime_job(&row.job_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                RuntimeJobStatus::Queued
            );
        }
        // No ready work leaves a concrete diagnostic rather than silently hiding the blocked rows.
        stop_job(jobs.as_ref(), &row, "test_complete")
            .await
            .unwrap();
        assert!(next_queued_parent_session()
            .await
            .unwrap_err()
            .contains("existing directory"));
        agent_runtime::close_storage_owners().await.unwrap();
    });
}

#[test]
fn cancelled_parent_runs_do_not_revive_unmaterialized_delegation() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    let cwd = profile.root.join("workspace").to_string_lossy().to_string();
    let store =
        centaeris_runtime_sqlite::SqliteRuntimeStore::new(profile.root.join("runtime.sqlite3"))
            .unwrap();
    let mut rows = Vec::new();
    for (status, at) in [("cancelled", 1), ("stopped", 1), ("succeeded", 2)] {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: cwd.clone(),
        })
        .unwrap();
        let run_id = format!("parent-{status}");
        message_log::append_agent_run_started(&parent.id, "parent-turn", &run_id, "Delegate", 1)
            .unwrap();
        let row = job(&parent.id, status, at);
        let child = format!("session-{status}");
        let mut payload = packet(&parent.id, &child, &row, &cwd);
        payload["workPacket"]["run_context"]["agentRef"]["agentId"] = status.into();
        payload["workPacket"]["run_context"]["parentAgentRef"]["agentRunId"] =
            run_id.clone().into();
        store
            .upsert_external_context_and_schedule_job(UpsertExternalContextAndScheduleJobRequest {
                object: object(&format!("packet:{status}"), payload.to_string()),
                job: row.clone(),
            })
            .unwrap();
        if status == "succeeded" {
            message_log::append_assistant_message(
                &parent.id,
                "parent-turn",
                Some(&run_id),
                "Delegated work remains queued.",
                "done",
                2,
            )
            .unwrap();
        }
        message_log::append_agent_run_terminal(
            &parent.id,
            "parent-turn",
            &run_id,
            status,
            Some("persisted parent stop"),
            3,
        )
        .unwrap();
        assert!(sessions::find_agent_runtime_binding(&child)
            .unwrap()
            .is_none());
        rows.push((status, parent.id, child, row));
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        assert_eq!(
            next_queued_parent_session().await.unwrap(),
            Some(rows[2].1.clone())
        );
        let jobs = agent_runtime::agent_runtime_job_store().unwrap();
        for (status, parent, child, row) in &rows {
            let current = jobs.get_runtime_job(&row.job_id).await.unwrap().unwrap();
            if *status == "succeeded" {
                assert_eq!(current.status, RuntimeJobStatus::Queued);
                assert_eq!(
                    sessions::find_agent_runtime_binding(child).unwrap(),
                    Some((parent.clone(), row.job_id.clone()))
                );
            } else {
                assert_eq!(current.status, RuntimeJobStatus::Cancelled);
                assert_eq!(current.last_error.as_deref(), Some("persisted parent stop"));
                assert!(sessions::find_agent_runtime_binding(child)
                    .unwrap()
                    .is_none());
                assert!(message_log::project_agent_run(&row.job_id)
                    .unwrap()
                    .is_none());
            }
        }
        assert_eq!(
            agent_runtime::native_job_store_handle()
                .unwrap()
                .pending_terminal_deliveries(10, Some(SUBAGENT_RUN_JOB_KIND.into()))
                .await
                .unwrap()
                .len(),
            2
        );
        agent_runtime::close_storage_owners().await.unwrap();
    });
}
