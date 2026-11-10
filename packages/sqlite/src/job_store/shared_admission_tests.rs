use super::*;
use crate::sqlite_job_connection::{Outcome, WorkError};
use centaeris_core::runtime::contracts::{EventVisibility, RuntimeEvent};
use centaeris_core::session::store::RuntimeStoreActor;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_admission_epoch_replacement_preserves_capacity_and_never_replays_unknown_write() {
    for synchronous_assembly in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.db");
        let store = crate::SqliteRuntimeStore::new(&path).unwrap();
        let admission = Arc::new(Semaphore::new(1));
        let actor =
            RuntimeStoreActor::start_with_admission(store.clone(), admission.clone()).unwrap();
        let jobs = if synchronous_assembly {
            store
                .job_store_with_admission(129, admission.clone())
                .unwrap()
        } else {
            SqliteJobStore::open_with_admission(path.clone(), 129, admission.clone())
                .await
                .unwrap()
        };
        let driver = jobs.connection(JobStoreOperation::Create).await.unwrap();
        // The SQL write really completes, but transaction confirmation is absent.
        // The owner must retire this epoch without replaying this operation.
        let receipt = driver
            .submit(Options::default(), |conn, _| {
                crate::sqlite_runtime::append_event_conn_idempotent(
                    conn,
                    &RuntimeEvent {
                        event_id: "unconfirmed-event".into(),
                        session_id: "parent".into(),
                        task_id: None,
                        event_type: "test".into(),
                        at_ms: 1,
                        visibility: EventVisibility::Internal,
                        payload_json: "{}".into(),
                    },
                )
            })
            .await
            .unwrap()
            .await;
        assert_eq!(receipt.outcome, Outcome::Unknown);
        assert!(matches!(
            receipt.result,
            Err(WorkError::ReplyUnavailable(_))
        ));
        assert!(!receipt.healthy);
        assert_eq!(admission.available_permits(), 1);

        let held = admission.clone().acquire_owned().await.unwrap();
        let mut read = tokio::spawn({
            let jobs = jobs.clone();
            async move { jobs.get_runtime_job("absent").await }
        });
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut read)
            .await
            .is_err());
        assert!(
            jobs.owner.lock().await.driver.is_healthy(),
            "the new epoch has opened"
        );
        assert_eq!(
            jobs.resource_metrics().await.started,
            0,
            "replacement SQL is waiting for the original shared capacity"
        );
        assert_eq!(admission.available_permits(), 0);
        drop(held);
        assert!(tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .is_none());
        assert_eq!(admission.available_permits(), 1);

        jobs.close().await.unwrap();
        assert!(!admission.is_closed());
        assert_eq!(actor.list_events("parent", 10, 0).await.unwrap().len(), 1);
        actor.close().await.unwrap();
        assert_eq!(admission.available_permits(), 1);
    }
}
