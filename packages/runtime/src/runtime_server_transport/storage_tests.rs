use super::*;
use crate::subagent_scheduler::queue_tests::{job, object, packet, Profile};
use centaeris_core::session::store::job::{JobStoreError, JobWriteOutcome};
use centaeris_core::session::store::{AsyncJobStorePort, RuntimeStoreTransactionPort};

#[test]
fn startup_recovery_failure_closes_initialized_storage_owners_without_replay() {
    let _guard = crate::message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let rt = tokio::runtime::Runtime::new().unwrap();
    if std::env::var_os("CENTAERIS_TEST_JOB_STARTUP_FAILURE").is_some() {
        rt.block_on(async {
            let error = run_server().await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("runtime_server_job_recovery_failed"),
                "{error}"
            );
            let native = agent_runtime::native_job_store_handle().unwrap();
            assert!(matches!(
                native.get_runtime_job("unused").await,
                Err(JobStoreError::Storage {
                    outcome: JobWriteOutcome::NotExecuted,
                    ..
                })
            ));
            assert!(
                !agent_runtime::agent_runtime_store_actor()
                    .unwrap()
                    .resource_metrics()
                    .accepting
            );
        });
        return;
    }
    let profile = Profile::new();
    let id = rt.block_on(async {
        crate::user_data_layout::ensure_user_data_layout().unwrap();
        let parent = crate::sessions::create(crate::sessions::SessionCreateRequest {
            title: None,
            cwd: profile.root.join("workspace").to_string_lossy().to_string(),
        })
        .unwrap();
        let row = job(&parent.id, "startup", 1);
        let value = packet(
            &parent.id,
            "session-startup",
            &row,
            &profile.root.join("workspace").to_string_lossy(),
        );
        let snapshots = agent_runtime::agent_runtime_store_actor().unwrap();
        RuntimeStoreTransactionPort::upsert_external_context_and_schedule_job(
            &snapshots,
            centaeris_core::session::store::UpsertExternalContextAndScheduleJobRequest {
                object: object("packet:startup", value.to_string()),
                job: row.clone(),
            },
        )
        .unwrap();
        row.job_id
    });
    let independent = rusqlite::Connection::open(profile.root.join("runtime.sqlite3")).unwrap();
    independent
        .execute(
            "UPDATE runtime_jobs SET output_refs_json='{' WHERE job_id=?1",
            [&id],
        )
        .unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["runtime_server_transport::storage_tests::startup_recovery_failure_closes_initialized_storage_owners_without_replay", "--exact", "--nocapture"])
        .env("CENTAERIS_TEST_JOB_STARTUP_FAILURE", "1").output().unwrap();
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    let persisted: String = independent
        .query_row(
            "SELECT output_refs_json FROM runtime_jobs WHERE job_id=?1",
            [&id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(persisted, "{");
    rt.block_on(agent_runtime::close_storage_owners()).unwrap();
}

#[test]
fn background_work_blocks_idle_and_service_drain_until_its_actual_owner_finishes() {
    let _guard = crate::message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _profile = Profile::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let clients = Arc::new(RuntimeServerClientHub::default());
        let writer = clients.broadcaster();
        let work = writer.begin_background_work("session").unwrap();
        assert!(!runtime_server_is_idle(&clients).await.unwrap());
        assert!(!clients
            .begin_idle_shutdown(clients.activity_generation())
            .unwrap());
        clients.request_service_shutdown().unwrap();
        assert!(writer
            .begin_background_work("session")
            .unwrap_err()
            .contains("draining"));
        let draining = tokio::spawn(async move { clients.drain_background_work().await });
        // Polling a zero budget never claims the owner was cancelled or drained.
        let mut waiter = draining;
        assert!(tokio::time::timeout(Duration::ZERO, &mut waiter)
            .await
            .is_err());
        drop(work);
        waiter.await.unwrap().unwrap();
        agent_runtime::close_storage_owners().await.unwrap();
    });
}
