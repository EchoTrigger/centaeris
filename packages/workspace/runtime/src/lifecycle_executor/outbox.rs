//! Notifications shorten delivery latency; the durable outbox owns recovery.
use super::*;
use centaeris_core::session::reliability::RuntimeJobOutboxPort;
use centaeris_core::session::store::RuntimeJobWaiterCursor;

#[derive(Default)]
pub(crate) struct Dispatcher {
    cursors: HashMap<(String, u32), RuntimeJobWaiterCursor>,
}

impl Dispatcher {
    pub(crate) fn deliver(&mut self, store: &PostgresRuntimeStore) -> Result<usize, String> {
        let events = store.list_pending_runtime_job_outbox(100)?;
        let visible: HashSet<_> = events
            .iter()
            .map(|event| (event.job_id.clone(), event.generation))
            .collect();
        for event in &events {
            if event.event_type != "runtime_job.terminal" {
                return Err("terminal outbox event invalid".into());
            }
            let key = (event.job_id.clone(), event.generation);
            let source = store
                .get_runtime_job(&event.job_id)?
                .ok_or("terminal outbox source missing")?;
            if !source.status.is_terminal() {
                return Err("terminal outbox source not terminal".into());
            }
            let (_, _, next) =
                job_protocol::process_waiter_page(store, Some(source), self.cursors.get(&key))
                    .map_err(|(_, code)| code.to_string())?;
            if let Some(next) = next {
                self.cursors.insert(key, next);
                continue;
            }
            // An interrupted fan-out remains pending. Publication uses the exact
            // generation so a stale delivery cannot acknowledge a newer event.
            store.mark_runtime_job_outbox_published(
                &event.job_id,
                &event.event_type,
                event.generation,
                now_ms()?,
            )?;
            self.cursors.remove(&key);
        }
        self.cursors.retain(|key, _| visible.contains(key));
        Ok(events.len())
    }
}

pub(super) async fn run(store: Arc<PostgresRuntimeStore>, shutdown: Shutdown) {
    let mut dispatcher = Dispatcher::default();
    while !shutdown.is_stopped() {
        let batch_store = store.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = dispatcher.deliver(&batch_store);
            (dispatcher, result)
        })
        .await;
        let delivered = match result {
            Ok((returned, result)) => {
                dispatcher = returned;
                result
            }
            Err(_) => {
                dispatcher = Dispatcher::default();
                Err("terminal delivery task interrupted".into())
            }
        };
        match delivered {
            Ok(0) => {
                let store = store.clone();
                if !matches!(
                    tokio::task::spawn_blocking(
                        move || store.wait_for_terminal_outbox(Duration::from_secs(5))
                    )
                    .await,
                    Ok(Ok(_))
                ) {
                    shutdown.wait(Duration::from_secs(1)).await;
                }
            }
            Ok(_) => {}
            Err(_) => {
                eprintln!("Run terminal delivery unavailable; outbox remains pending");
                shutdown.wait(Duration::from_secs(1)).await;
            }
        }
    }
}

pub(super) async fn reconcile(store: Arc<PostgresRuntimeStore>, shutdown: Shutdown) {
    let mut after = None;
    while !shutdown.is_stopped() {
        let store = store.clone();
        let page_after = after.clone();
        let result = tokio::task::spawn_blocking(move || {
            job_protocol::process_waiter_page(&store, None, page_after.as_ref())
        })
        .await;
        match result {
            Ok(Ok((_, _, next))) => after = next,
            _ => {
                eprintln!("Run waiter reconciliation unavailable");
            }
        }
        shutdown.wait(Duration::from_secs(5)).await;
    }
}
