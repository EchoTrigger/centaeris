use super::*;
use crate::runtime_rpc_transport::RuntimeServerClientHub;
use crate::subagent_scheduler::queue_tests::{job, object, packet, Profile};
use centaeris_core::session::reliability::*;
use centaeris_core::session::store::RuntimeStoreTransactionPort;
use std::sync::Arc;

#[test]
fn terminal_delivery_pages_past_a_full_failed_page_after_process_restart() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    if let Ok(good_id) = std::env::var("CENTAERIS_TERMINAL_PAGE_TEST_JOB") {
        runtime.block_on(async {
            let writer = Arc::new(RuntimeServerClientHub::default()).broadcaster();
            assert!(reconcile_terminal_jobs(&writer).await.is_err());
            reconcile_terminal_jobs(&writer).await.unwrap();
            assert_terminal_page_delivery(&good_id).await;
            agent_runtime::close_storage_owners().await.unwrap();
        });
        return;
    }
    let profile = Profile::new();
    let good_id = runtime.block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().into(),
        })
        .unwrap();
        let mut rows = Vec::new();
        for index in 0..65 {
            let name = format!("page-child-{index:03}");
            rows.push((queued_child(&profile, &parent.id, &name).await, name));
        }
        rows.sort_by(|left, right| left.0.job_id.cmp(&right.0.job_id));
        let mut completed = Vec::new();
        for (row, name) in rows {
            completed.push(complete_queued_child(row, &name).await);
        }
        // Complete the last scan key last so it remains in the bounded parent projection.
        let good = completed.pop().unwrap();
        let conn = rusqlite::Connection::open(profile.root.join("runtime.sqlite3")).unwrap();
        for bad in completed {
            conn.execute(
                "UPDATE external_context_objects SET content_sha256='corrupt' WHERE object_id=?1",
                [&bad.output_refs[0]],
            )
            .unwrap();
        }
        let writer = Arc::new(RuntimeServerClientHub::default()).broadcaster();
        assert!(reconcile_terminal_jobs(&writer).await.is_err());
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        assert_eq!(
            jobs.pending_terminal_deliveries(100, Some(SUBAGENT_RUN_JOB_KIND.into()))
                .await
                .unwrap()
                .len(),
            65,
            "one bounded pass must not reach the sixty-fifth delivery"
        );
        assert_eq!(
            message_log::project_agent_run(&good.job_id)
                .unwrap()
                .unwrap()
                .status,
            "running"
        );
        good.job_id
    });
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["subagent_delivery::tests::terminal_delivery_pages_past_a_full_failed_page_after_process_restart", "--exact", "--nocapture"])
        .env("CENTAERIS_TERMINAL_PAGE_TEST_JOB", &good_id)
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    runtime.block_on(async {
        assert_terminal_page_delivery(&good_id).await;
        let writer = Arc::new(RuntimeServerClientHub::default()).broadcaster();
        let mut failed_passes = 0;
        for _ in 0..3 {
            failed_passes += usize::from(reconcile_terminal_jobs(&writer).await.is_err());
        }
        assert!(
            failed_passes > 0,
            "wrapped scans must retry failed deliveries"
        );
        assert_terminal_page_delivery(&good_id).await;
        agent_runtime::close_storage_owners().await.unwrap();
    });
}

async fn assert_terminal_page_delivery(good_id: &str) {
    let jobs = agent_runtime::native_job_store_handle().unwrap();
    let pending = jobs
        .pending_terminal_deliveries(100, Some(SUBAGENT_RUN_JOB_KIND.into()))
        .await
        .unwrap();
    assert_eq!(
        pending.len(),
        64,
        "failed deliveries must remain unacknowledged"
    );
    assert!(pending.iter().all(|record| record.job_id != good_id));
    for record in &pending {
        assert_eq!(
            message_log::project_agent_run(&record.job_id)
                .unwrap()
                .unwrap()
                .status,
            "running",
            "failed delivery must not invent a child terminal"
        );
    }
    assert_eq!(
        message_log::project_agent_run(good_id)
            .unwrap()
            .unwrap()
            .status,
        "succeeded"
    );
    let row = jobs.get_runtime_job(good_id).await.unwrap().unwrap();
    assert_eq!(row.status, RuntimeJobStatus::Succeeded);
    let snapshots = agent_runtime::agent_runtime_store_actor().unwrap();
    let raw =
        centaeris_core::session::store::AgentRuntimeSnapshotStorePort::load_agent_runtime_snapshot(
            &snapshots,
            row.session_id.as_deref().unwrap(),
        )
        .unwrap()
        .unwrap();
    let state: centaeris_core::session::state::SessionStateSnapshot =
        serde_json::from_str(&raw).unwrap();
    let projection = state
        .metadata
        .values()
        .filter_map(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .find(|value| value["schema"] == "subagent_result_projection_v1")
        .unwrap();
    assert!(projection["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["subagentRunRef"] == format!("runtime_job:{good_id}")));
}

#[test]
fn terminal_delivery_emits_parent_runtime_hint_and_child_terminal_once_after_ack() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest { title: None,
            cwd: profile.root.join("workspace").to_string_lossy().into() }).unwrap();
        let row = completed_child(&profile, &parent.id, "stream-terminal").await;
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (client, mut notifications) = hub.connect().unwrap().unwrap();
        client.register_client(crate::runtime_server::RuntimeClientKind::Desktop, "terminal-observer").unwrap();
        reconcile_terminal_jobs(&hub.broadcaster()).await.unwrap();
        let mut frames = Vec::new();
        while let Ok(frame) = notifications.try_recv() {
            frames.push(serde_json::from_str::<serde_json::Value>(&frame).unwrap());
        }
        let child = "session-stream-terminal";
        let child_terminal = message_log::terminal_agent_run_stream_projection(&row.job_id).unwrap();
        assert!(frames.iter().any(|frame| frame["method"] == "session/update"
            && frame["params"]["sessionId"] == child
            && frame["params"]["agentRunId"] == row.job_id
            && frame["params"]["payload"] == child_terminal), "child terminal notification must remain observable: {frames:?}");
        let parent_hints = frames.iter().filter(|frame| frame["method"] == "session/update"
            && frame["params"]["sessionId"] == parent.id
            && frame["params"]["payload"]["type"] == "runtime_event").collect::<Vec<_>>();
        assert_eq!(parent_hints.len(), 1, "completed subagent must emit its parent runtime_event hint: {frames:?}");
        let hint = &parent_hints[0]["params"];
        assert_eq!(hint["agentRunId"], "parent-run");
        assert_eq!(hint["payload"]["event"]["type"], "SubagentResult");
        assert_eq!(hint["payload"]["event"]["sessionId"], parent.id);
        assert_eq!(hint["payload"]["event"]["turnId"], "parent-turn");
        assert_eq!(hint["payload"]["event"]["status"], "done");
        assert_eq!(hint["payload"]["event"]["payload"]["childSessionId"], child);
        assert_eq!(hint["payload"]["event"]["payload"]["taskNotification"]["outputRef"]["runtimeJobId"], row.job_id);
        assert_eq!(hint["payload"]["event"]["payload"]["taskNotification"]["outputRef"]["resultRef"], row.output_refs[0]);
        assert!(jobs.pending_terminal_deliveries(64, None).await.unwrap().is_empty());
        let snapshots = agent_runtime::agent_runtime_store_actor().unwrap();
        let before = centaeris_core::session::store::AgentRuntimeSnapshotStorePort::load_agent_runtime_snapshot(&snapshots, &parent.id).unwrap().unwrap();
        let state: centaeris_core::session::state::SessionStateSnapshot = serde_json::from_str(&before).unwrap();
        let projection = state.metadata.values().filter_map(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .find(|value| value["schema"] == "subagent_result_projection_v1").unwrap();
        assert_eq!(projection["items"].as_array().unwrap().len(), 1);
        reconcile_terminal_jobs(&hub.broadcaster()).await.unwrap();
        assert!(matches!(notifications.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)),
            "acknowledged outbox must not emit duplicate parent or child notifications on rescan");
        assert_eq!(centaeris_core::session::store::AgentRuntimeSnapshotStorePort::load_agent_runtime_snapshot(&snapshots, &parent.id).unwrap().unwrap(), before);
        agent_runtime::close_storage_owners().await.unwrap();
    });
}

pub(super) async fn completed_child(
    profile: &Profile,
    parent: &str,
    name: &str,
) -> RuntimeJobRecord {
    complete_queued_child(queued_child(profile, parent, name).await, name).await
}

async fn complete_queued_child(mut row: RuntimeJobRecord, name: &str) -> RuntimeJobRecord {
    let jobs = agent_runtime::native_job_store_handle().unwrap();
    let now = centaeris_core::runtime::contracts::current_timestamp_ms();
    row = jobs
        .claim_due_runtime_jobs(ClaimDueRuntimeJobsRequest {
            now_ms: now,
            worker_id: "delivery-worker".into(),
            job_id: Some(row.job_id.clone()),
            job_kind: None,
            session_id: None,
            limit: 1,
            lease_ms: 120000,
        })
        .await
        .unwrap()
        .pop()
        .unwrap();
    let result_id = format!("result:{name}");
    jobs.upsert_external_context_link_and_complete_job(
        centaeris_core::session::store::UpsertExternalContextLinkAndCompleteJobRequest {
            object: Some(object(&result_id, format!("Result for {name}"))),
            link: None,
            complete_job: CompleteRuntimeJobRequest {
                job_id: row.job_id.clone(),
                lease_owner: row.lease_owner.clone().unwrap(),
                output_refs: vec![result_id],
                completed_at_ms: now,
            },
        },
    )
    .await
    .unwrap();
    jobs.get_runtime_job(&row.job_id).await.unwrap().unwrap()
}

#[test]
fn rejected_packet_terminal_delivery_keeps_durable_facts_without_guessing_parent_hint() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest { title: None,
            cwd: profile.root.join("workspace").to_string_lossy().into() }).unwrap();
        let row = completed_child(&profile, &parent.id, "rejected-packet-hint").await;
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        let conn = rusqlite::Connection::open(profile.root.join("runtime.sqlite3")).unwrap();
        assert_eq!(conn.execute("DELETE FROM external_context_objects WHERE object_id=?1", [row.payload_ref.as_ref().unwrap()]).unwrap(), 1);
        assert!(matches!(load_subagent_work_packet_async(&jobs, &row).await, Err(JobStoreError::Rejected(_))));
        let hub = Arc::new(RuntimeServerClientHub::default());
        let (client, mut notifications) = hub.connect().unwrap().unwrap();
        client.register_client(crate::runtime_server::RuntimeClientKind::Desktop, "rejected-packet-observer").unwrap();
        reconcile_terminal_jobs(&hub.broadcaster()).await.unwrap();
        let mut frames = Vec::new();
        while let Ok(frame) = notifications.try_recv() { frames.push(serde_json::from_str::<serde_json::Value>(&frame).unwrap()); }
        assert!(frames.iter().any(|frame| frame["method"] == "session/update"
            && frame["params"]["sessionId"] == "session-rejected-packet-hint"
            && frame["params"]["agentRunId"] == row.job_id
            && frame["params"]["payload"]["event"]["type"] == "AgentRunCompleted"));
        assert!(!frames.iter().any(|frame| frame["params"]["sessionId"] == parent.id),
            "a rejected packet cannot authorize a guessed parent AgentRun hint");
        assert_eq!(message_log::project_agent_run(&row.job_id).unwrap().unwrap().status, "succeeded");
        let snapshots = agent_runtime::agent_runtime_store_actor().unwrap();
        let raw = centaeris_core::session::store::AgentRuntimeSnapshotStorePort::load_agent_runtime_snapshot(&snapshots, &parent.id).unwrap().unwrap();
        let state: centaeris_core::session::state::SessionStateSnapshot = serde_json::from_str(&raw).unwrap();
        let projection = state.metadata.values().filter_map(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .find(|value| value["schema"] == "subagent_result_projection_v1").unwrap();
        assert_eq!(projection["items"].as_array().unwrap().len(), 1);
        assert!(jobs.pending_terminal_deliveries(64, None).await.unwrap().is_empty());
        agent_runtime::close_storage_owners().await.unwrap();
    });
}

pub(super) async fn queued_child(profile: &Profile, parent: &str, name: &str) -> RuntimeJobRecord {
    let cwd = profile.root.join("workspace").to_string_lossy().to_string();
    let row = job(parent, name, 1);
    // Core's canonical child identifier and native manifest agree.
    let child = format!("session-{name}");
    let packet = packet(parent, &child, &row, &cwd);
    let snapshots = agent_runtime::agent_runtime_store_actor().unwrap();
    RuntimeStoreTransactionPort::upsert_external_context_and_schedule_job(
        &snapshots,
        centaeris_core::session::store::UpsertExternalContextAndScheduleJobRequest {
            object: object(&format!("packet:{name}"), packet.to_string()),
            job: row.clone(),
        },
    )
    .unwrap();
    sessions::ensure_agent_session(&child, parent, &row.job_id, name, &cwd, 1).unwrap();
    message_log::append_agent_turn_queued(&child, "child-turn", &row.job_id, name, 1).unwrap();
    row
}

#[test]
fn recovered_service_stop_and_user_reason_keep_distinct_child_terminals() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().to_string(),
        })
        .unwrap();
        let service = queued_child(&profile, &parent.id, "service-stop").await;
        let user = queued_child(&profile, &parent.id, "user-stop").await;
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        let cancel = |row: &RuntimeJobRecord| CancelRuntimeJobRequest {
            job_id: row.job_id.clone(),
            reason: "runtime_server_shutdown".into(),
            cancelled_at_ms: 20,
            expected_status: Some(RuntimeJobStatus::Queued),
            expected_lease_owner: None,
        };
        jobs.cancel_for_service_shutdown(cancel(&service))
            .await
            .unwrap();
        jobs.cancel_runtime_job(cancel(&user)).await.unwrap();
        // Recovery has no volatile shutdown_jobs membership.
        reconcile_terminal_jobs(&Arc::new(RuntimeServerClientHub::default()).broadcaster())
            .await
            .unwrap();
        assert_eq!(
            message_log::project_agent_run(&service.job_id)
                .unwrap()
                .unwrap()
                .status,
            "stopped"
        );
        assert_eq!(
            message_log::project_agent_run(&user.job_id)
                .unwrap()
                .unwrap()
                .status,
            "cancelled"
        );
        for (child, reason) in [
            ("session-service-stop", "shutdown"),
            ("session-user-stop", "cancelled"),
        ] {
            let path = crate::user_data_layout::find_session_log_file_path(child)
                .unwrap()
                .unwrap();
            let records = message_log::read_session_document(&path).unwrap().records;
            assert_eq!(records.last().unwrap().payload["reasonType"], reason);
        }
        assert!(jobs
            .pending_terminal_deliveries(64, Some(SUBAGENT_RUN_JOB_KIND.into()))
            .await
            .unwrap()
            .is_empty());
        jobs.close().await.unwrap();
    });
}

#[test]
fn session_deletion_waits_for_host_sequences_then_removes_job_delivery_facts() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().to_string(),
        })
        .unwrap();
        let row = completed_child(&profile, &parent.id, "deletion-child").await;
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        let writer = Arc::new(RuntimeServerClientHub::default()).broadcaster();
        let work = writer.begin_background_work(&parent.id).unwrap();
        let delete_writer = writer.clone();
        let root = parent.id.clone();
        let deletion = tokio::spawn(sessions::delete_async(
            delete_writer,
            sessions::SessionDeleteRequest { session_id: root },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match writer.begin_background_work(&parent.id) {
                    Err(error) => {
                        assert!(error.contains("being deleted"));
                        break;
                    }
                    Ok(probe) => drop(probe),
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!deletion.is_finished());
        assert!(jobs.get_runtime_job(&row.job_id).await.unwrap().is_some());
        assert_eq!(
            sessions::list(sessions::SessionListRequest {})
                .unwrap()
                .len(),
            2
        );
        drop(work);
        let deleted = tokio::time::timeout(std::time::Duration::from_secs(10), deletion)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(deleted.deleted_session_ids.len(), 2);
        assert!(jobs.get_runtime_job(&row.job_id).await.unwrap().is_none());
        assert!(jobs
            .pending_terminal_deliveries(64, None)
            .await
            .unwrap()
            .is_empty());
        reconcile_terminal_jobs(&writer).await.unwrap();
        assert!(sessions::list(sessions::SessionListRequest {})
            .unwrap()
            .is_empty());
        let check = rusqlite::Connection::open(profile.root.join("runtime.sqlite3")).unwrap();
        for table in [
            "runtime_jobs",
            "runtime_job_outbox",
            "session_runtime_snapshots",
        ] {
            let count: i64 = check
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        jobs.close().await.unwrap();
    });
}

#[test]
fn restart_repairs_every_terminal_delivery_boundary_without_worker_reexecution() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if std::env::var("CENTAERIS_JOB_DELIVERY_TEST_EXIT").is_ok() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            reconcile_terminal_jobs(&Arc::new(RuntimeServerClientHub::default()).broadcaster())
                .await
                .unwrap();
        });
        panic!("child did not reach delivery boundary");
    }
    for boundary in [
        "BeforeParent",
        "AfterParent",
        "BeforeChildTerminal",
        "AfterChild",
        "BeforeAcknowledgement",
    ] {
        let profile = Profile::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (parent, row) = rt.block_on(async {
            let parent = sessions::create(sessions::SessionCreateRequest {
                title: None,
                cwd: profile.root.join("workspace").to_string_lossy().to_string(),
            })
            .unwrap();
            let row = completed_child(&profile, &parent.id, "completed").await;
            (parent, row)
        });
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["subagent_delivery::tests::restart_repairs_every_terminal_delivery_boundary_without_worker_reexecution", "--exact", "--nocapture"])
            .env("CENTAERIS_JOB_DELIVERY_TEST_EXIT", boundary).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(81),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("delivery boundary reached:"));
        rt.block_on(async {
            let jobs = agent_runtime::native_job_store_handle().unwrap();
            reconcile_terminal_jobs(&Arc::new(RuntimeServerClientHub::default()).broadcaster()).await.unwrap();
            reconcile_terminal_jobs(&Arc::new(RuntimeServerClientHub::default()).broadcaster()).await.unwrap();
            assert_eq!(jobs.get_runtime_job(&row.job_id).await.unwrap().unwrap().status, RuntimeJobStatus::Succeeded);
            assert!(jobs.pending_terminal_deliveries(64, Some(SUBAGENT_RUN_JOB_KIND.into())).await.unwrap().is_empty());
            let run = message_log::project_agent_run(&row.job_id).unwrap().unwrap();
            assert_eq!(run.status, "succeeded");
            let snapshots = agent_runtime::agent_runtime_store_actor().unwrap();
            let raw = centaeris_core::session::store::AgentRuntimeSnapshotStorePort::load_agent_runtime_snapshot(&snapshots, &parent.id).unwrap().unwrap();
            assert!(raw.contains(&row.job_id));
            jobs.close().await.unwrap();
        });
    }
}

#[test]
fn one_delivery_failure_does_not_lose_a_successful_sibling() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().to_string(),
        })
        .unwrap();
        let failed = completed_child(&profile, &parent.id, "failed-delivery").await;
        let succeeded = completed_child(&profile, &parent.id, "successful-delivery").await;
        // Corrupt only one result object; both Workers have already committed.
        rusqlite::Connection::open(profile.root.join("runtime.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE external_context_objects SET content_sha256='corrupt' WHERE object_id=?1",
                [&failed.output_refs[0]],
            )
            .unwrap();
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        assert!(reconcile_terminal_jobs(
            &Arc::new(RuntimeServerClientHub::default()).broadcaster()
        )
        .await
        .is_err());
        assert_eq!(
            message_log::project_agent_run(&succeeded.job_id)
                .unwrap()
                .unwrap()
                .status,
            "succeeded"
        );
        let pending = jobs
            .pending_terminal_deliveries(64, Some(SUBAGENT_RUN_JOB_KIND.into()))
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].job_id, failed.job_id);
        assert_eq!(
            jobs.get_runtime_job(&succeeded.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuntimeJobStatus::Succeeded
        );
        jobs.close().await.unwrap();
    });
}
