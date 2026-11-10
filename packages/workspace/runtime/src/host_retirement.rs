//! Host-local durable retirement jobs do not use the model lifecycle Worker.
use crate::postgres_store::PostgresRuntimeStore;
use std::{sync::Arc, time::Duration};

pub(crate) async fn run(store: Arc<PostgresRuntimeStore>, host: String) {
    let mut cursor = Some(String::new());
    loop {
        let store = store.clone();
        let host = host.clone();
        let previous = cursor.clone();
        match tokio::task::spawn_blocking(move || {
            let mut cursor = previous;
            let backfill = if let Some(after) = cursor.as_deref() {
                store
                    .backfill_host_retirements(&host, after)
                    .map(|next| cursor = next)
            } else {
                Ok(())
            };
            // A malformed historical fact must not starve newly queued cleanup.
            let drain =
                store.process_host_retirements(&host, crate::environment_supervisor::retire);
            (cursor, backfill.and(drain.map(|_| ())))
        })
        .await
        {
            Ok((next, result)) => {
                cursor = next;
                if result.is_err() {
                    eprintln!("environment_host_retirement_pending");
                }
            }
            Err(_) => eprintln!("environment_host_retirement_worker_unavailable"),
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}
