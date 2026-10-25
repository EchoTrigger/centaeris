//! One owned rotation loop. Stopping never cancels a dispatched provider callback.
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub(crate) struct RotationTask {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl RotationTask {
    pub(super) fn spawn<S, F>(
        runtime: &tokio::runtime::Runtime,
        state: S,
        tick: F,
        interval: Duration,
    ) -> Self
    where
        S: Send + 'static,
        F: Fn(&mut S) -> Result<(), String> + Send + Clone + 'static,
    {
        let (stop, receiver) = watch::channel(false);
        Self {
            stop,
            task: runtime.spawn(run(state, tick, interval, receiver)),
        }
    }

    pub(crate) async fn shutdown(&mut self, timeout: Duration) -> Result<(), &'static str> {
        self.stop.send_replace(true);
        match tokio::time::timeout(timeout, &mut self.task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err("e2b_rotation_shutdown_failed"),
            Err(_) => Err("e2b_rotation_shutdown_pending"),
        }
    }
}

impl Drop for RotationTask {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        // Dropping a JoinHandle detaches it. Do not abort an in-flight callback;
        // its existing durable operation remains responsible for its outcome.
    }
}

async fn run<S, F>(mut state: S, tick: F, interval: Duration, mut stop: watch::Receiver<bool>)
where
    S: Send + 'static,
    F: Fn(&mut S) -> Result<(), String> + Send + Clone + 'static,
{
    let mut interval = tokio::time::interval(interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() {
            return;
        }
        tokio::select! {
            biased;
            _ = stop.changed() => return,
            _ = interval.tick() => {}
        }
        if *stop.borrow() {
            return;
        }
        let tick = tick.clone();
        match tokio::task::spawn_blocking(move || {
            let result = tick(&mut state);
            (state, result)
        })
        .await
        {
            Ok((next, result)) => {
                state = next;
                if result.is_err() {
                    eprintln!("e2b_rotation_observation_unavailable");
                }
            }
            Err(_) => {
                eprintln!("e2b_rotation_worker_unavailable");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};

    #[test]
    fn shutdown_waits_for_current_callback_and_never_starts_the_next_tick() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let mut task = RotationTask::spawn(
            &runtime,
            (),
            {
                let calls = calls.clone();
                let completed = completed.clone();
                move |_| {
                    let ordinal = calls.fetch_add(1, Ordering::SeqCst);
                    if ordinal == 0 {
                        entered.send(()).unwrap();
                        release_rx
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(3))
                            .unwrap();
                    }
                    completed.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
            Duration::from_millis(5),
        );
        runtime.block_on(async {
            entered_rx.recv().await.unwrap();
            let mut stopping = Box::pin(task.shutdown(Duration::from_millis(300)));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut stopping)
                    .await
                    .is_err()
            );
            assert_eq!(
                completed.load(Ordering::SeqCst),
                0,
                "stop cannot manufacture completion"
            );
            release.send(()).unwrap();
            assert_eq!(stopping.await, Ok(()));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(completed.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn shutdown_timeout_retains_current_callback_without_replay_or_later_ticks() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let (release, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let mut task = RotationTask::spawn(
            &runtime,
            (),
            {
                let calls = calls.clone();
                move |_| {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        entered.send(()).unwrap();
                        release_rx
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(3))
                            .unwrap();
                    }
                    Ok(())
                }
            },
            Duration::from_millis(5),
        );
        runtime.block_on(async {
            entered_rx.recv().await.unwrap();
            assert_eq!(
                task.shutdown(Duration::from_millis(20)).await,
                Err("e2b_rotation_shutdown_pending")
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            release.send(()).unwrap();
            assert_eq!(task.shutdown(Duration::from_millis(300)).await, Ok(()));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn stopping_an_idle_loop_does_not_wait_for_its_scan_interval() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut task = RotationTask::spawn(
            &runtime,
            (),
            move |_| {
                entered.send(()).unwrap();
                Ok(())
            },
            Duration::from_secs(60),
        );
        runtime.block_on(async {
            entered_rx.recv().await.unwrap();
            assert_eq!(task.shutdown(Duration::from_millis(100)).await, Ok(()));
        });
    }
}
