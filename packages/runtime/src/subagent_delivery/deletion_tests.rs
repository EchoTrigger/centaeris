use super::tests::{completed_child, queued_child};
use super::*;
use crate::runtime_rpc_transport::RuntimeServerClientHub;
use crate::subagent_scheduler::queue_tests::Profile;
use centaeris_core::runtime::contracts::{EventVisibility, RuntimeEvent};
use centaeris_core::session::reliability::RuntimeJobStatus;
use std::sync::Arc;
use std::time::Duration;

async fn child_event(child: &str) {
    agent_runtime::agent_runtime_store_actor()
        .unwrap()
        .append_event(RuntimeEvent {
            event_id: format!("owned-event:{child}"),
            session_id: child.into(),
            task_id: None,
            event_type: "test.child_owned_event".into(),
            at_ms: 1,
            visibility: EventVisibility::Internal,
            payload_json: "{}".into(),
        })
        .await
        .unwrap();
}

fn assert_child_removed(profile: &Profile, child: &str, parent: &str) {
    let listed = sessions::list(sessions::SessionListRequest {}).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, parent);
    let conn = rusqlite::Connection::open(profile.root.join("runtime.sqlite3")).unwrap();
    for table in ["runtime_events", "checkpoints", "session_runtime_snapshots"] {
        let count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE session_id=?1"),
                [child],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "deleted child must have no rows in {table}");
    }
}

#[test]
fn direct_child_deletion_cancels_bound_queued_job_and_removes_delivery_work() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().into(),
        })
        .unwrap();
        let row = queued_child(&profile, &parent.id, "delete-queued-child").await;
        let child = "session-delete-queued-child";
        child_event(child).await;
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        let writer = Arc::new(RuntimeServerClientHub::default()).broadcaster();
        let deleted = sessions::delete_async(
            writer.clone(),
            sessions::SessionDeleteRequest {
                session_id: child.into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(deleted.deleted_session_ids, vec![child]);
        assert_eq!(
            jobs.get_runtime_job(&row.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuntimeJobStatus::Cancelled,
            "deleting the child must cancel its parent-owned Job"
        );
        assert_eq!(
            jobs.pending_terminal_deliveries(64, None)
                .await
                .unwrap()
                .len(),
            0,
            "child deletion must settle delivery before removing its authoritative binding"
        );
        reconcile_terminal_jobs(&writer).await.unwrap();
        assert_child_removed(&profile, child, &parent.id);
        agent_runtime::close_storage_owners().await.unwrap();
    });
}

#[test]
fn direct_child_deletion_settles_delivered_terminal_job_before_acknowledgement() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().into(),
        })
        .unwrap();
        let row = completed_child(&profile, &parent.id, "delete-terminal-child").await;
        let child = "session-delete-terminal-child";
        child_event(child).await;
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        let writer = Arc::new(RuntimeServerClientHub::default()).broadcaster();
        // Perform both authoritative projections, deliberately leaving durable acknowledgement pending.
        deliver_terminal_job(&writer, &row.job_id).await.unwrap();
        assert_eq!(
            message_log::project_agent_run(&row.job_id)
                .unwrap()
                .unwrap()
                .status,
            "succeeded"
        );
        assert_eq!(
            jobs.pending_terminal_deliveries(64, None)
                .await
                .unwrap()
                .len(),
            1
        );
        sessions::delete_async(
            writer.clone(),
            sessions::SessionDeleteRequest {
                session_id: child.into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            jobs.get_runtime_job(&row.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuntimeJobStatus::Succeeded
        );
        assert_eq!(
            jobs.pending_terminal_deliveries(64, None)
                .await
                .unwrap()
                .len(),
            0,
            "already delivered terminal must be acknowledged before its child binding is removed"
        );
        reconcile_terminal_jobs(&writer).await.unwrap();
        assert_child_removed(&profile, child, &parent.id);
        agent_runtime::close_storage_owners().await.unwrap();
    });
}

#[test]
fn direct_child_deletion_cancels_bound_job_before_waiting_for_child_host_sequence() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let profile = Profile::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().into(),
        })
        .unwrap();
        let row = queued_child(&profile, &parent.id, "delete-busy-child").await;
        let child = "session-delete-busy-child";
        child_event(child).await;
        let jobs = agent_runtime::native_job_store_handle().unwrap();
        let writer = Arc::new(RuntimeServerClientHub::default()).broadcaster();
        let work = writer.begin_background_work(child).unwrap();
        let deletion = tokio::spawn({
            let writer = writer.clone();
            async move {
                sessions::delete_async(
                    writer,
                    sessions::SessionDeleteRequest {
                        session_id: child.into(),
                    },
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if jobs
                    .get_runtime_job(&row.job_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status
                    == RuntimeJobStatus::Cancelled
                {
                    break;
                }
                assert!(
                    !deletion.is_finished(),
                    "deletion must not finish while real host work remains"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bound Job cancellation must precede waiting for child work");
        assert!(!deletion.is_finished());
        assert!(sessions::list(sessions::SessionListRequest {})
            .unwrap()
            .iter()
            .any(|session| session.id == child));
        // Release the actual sequence owner, with no sleep used to infer the phase.
        drop(work);
        tokio::time::timeout(Duration::from_secs(5), deletion)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        jobs.settle_job_operations().await.unwrap();
        reconcile_terminal_jobs(&writer).await.unwrap();
        assert_child_removed(&profile, child, &parent.id);
        agent_runtime::close_storage_owners().await.unwrap();
    });
}
