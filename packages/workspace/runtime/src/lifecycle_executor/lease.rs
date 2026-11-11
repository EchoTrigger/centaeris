//! A renewal acknowledgement grants only the window measured from request start.
use super::*;
use crate::postgres_store::lease_renewal::WorkerLeaseRenewalError;
use std::sync::Condvar;

#[derive(Debug)]
struct Window {
    deadline: Instant,
    healthy: bool,
    lost: bool,
    stopped: bool,
}

impl Window {
    fn acknowledge(&mut self, started: Instant, finished: Instant, duration: Duration) {
        if self.lost || finished >= self.deadline {
            self.lost = true;
            return;
        }
        self.deadline = started + duration;
        self.healthy = finished < self.deadline;
        self.lost = !self.healthy;
    }
}

pub(super) struct Lease {
    state: Arc<(Mutex<Window>, Condvar)>,
    renewal: Option<tokio::task::JoinHandle<()>>,
    stop: watch::Sender<bool>,
    shutdown: Shutdown,
}

impl Lease {
    pub(super) fn acquire(
        store: Arc<PostgresRuntimeStore>,
        job: &RuntimeJobRecord,
        duration: Duration,
        shutdown: Shutdown,
    ) -> Result<Self, Failure> {
        let owner = owner(job)?.to_string();
        let started = Instant::now();
        store
            .renew_worker_job_lease(RenewRuntimeJobLeaseRequest {
                job_id: job.job_id.clone(),
                lease_owner: owner.clone(),
                heartbeat_at_ms: now_ms()?,
                lease_ms: duration.as_millis() as u64,
            })
            .map_err(|_| Failure::rejected(409, "agent_run_lifecycle_lease_lost"))?;
        let window = duration - duration.div_f64(10.0).min(Duration::from_secs(1));
        if started.elapsed() >= window {
            return Err(Failure::rejected(409, "agent_run_lifecycle_lease_lost"));
        }
        let state = Arc::new((
            Mutex::new(Window {
                deadline: started + window,
                healthy: true,
                lost: false,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let shared = state.clone();
        let stopping = shutdown.clone();
        let id = job.job_id.clone();
        let (stop, mut stop_receiver) = watch::channel(false);
        let renewal = tokio::runtime::Handle::current().spawn(async move {
            let mut interval = duration / 3;
            loop {
                tokio::select! {
                    _ = stop_receiver.changed() => break,
                    stopped = stopping.wait(interval) => if stopped {break;},
                }
                let (lock, changed) = &*shared;
                {
                    let mut guard = lock.lock().unwrap();
                    if guard.stopped || stopping.is_stopped() {
                        break;
                    }
                    if Instant::now() >= guard.deadline {
                        guard.lost = true;
                        changed.notify_all();
                        break;
                    }
                }
                let started = Instant::now();
                let renewing_store = store.clone();
                let renewing_id = id.clone();
                let renewing_owner = owner.clone();
                let result = tokio::task::spawn_blocking(move || {
                    renewing_store.renew_worker_job_lease(RenewRuntimeJobLeaseRequest {
                        job_id: renewing_id,
                        lease_owner: renewing_owner,
                        heartbeat_at_ms: now_ms().unwrap_or_default(),
                        lease_ms: duration.as_millis() as u64,
                    })
                })
                .await;
                let mut guard = lock.lock().unwrap();
                match result {
                    Ok(Ok(())) => {
                        guard.acknowledge(started, Instant::now(), window);
                        interval = duration / 3;
                    }
                    Ok(Err(
                        WorkerLeaseRenewalError::FenceRejected
                        | WorkerLeaseRenewalError::InvalidRequest,
                    )) => guard.lost = true,
                    _ => {
                        guard.healthy = false;
                        interval = Duration::from_millis(100);
                    }
                }
                changed.notify_all();
                if guard.lost {
                    break;
                }
            }
        });
        Ok(Self {
            state,
            renewal: Some(renewal),
            stop,
            shutdown,
        })
    }

    pub(super) fn require(&self) -> Result<(), Failure> {
        let (lock, changed) = &*self.state;
        let mut state = lock.lock().unwrap();
        loop {
            if state.lost
                || (state.stopped && !state.healthy)
                || self.shutdown.is_stopped()
                || Instant::now() >= state.deadline
            {
                return Err(Failure::rejected(409, "agent_run_lifecycle_lease_lost"));
            }
            if state.healthy {
                return Ok(());
            }
            let remaining = state.deadline.saturating_duration_since(Instant::now());
            state = changed.wait_timeout(state, remaining).unwrap().0;
        }
    }

    // Stop and join the only renewer before any lease-ending mutation.
    pub(super) fn handoff(mut self) -> Result<(), Failure> {
        self.stop();
        self.require()
    }

    fn stop(&mut self) {
        self.state.0.lock().unwrap().stopped = true;
        self.stop.send_replace(true);
        self.state.1.notify_all();
        if let Some(task) = self.renewal.take() {
            let _ = tokio::runtime::Handle::current().block_on(task);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renewal_acknowledgement_cannot_revive_an_expired_or_fenced_owner() {
        let start = Instant::now();
        let duration = Duration::from_secs(10);
        for lost in [false, true] {
            let mut state = Window {
                deadline: start + duration,
                healthy: !lost,
                lost,
                stopped: false,
            };
            state.acknowledge(
                start + Duration::from_secs(9),
                start + Duration::from_secs(11),
                duration,
            );
            assert!(state.lost);
            assert_eq!(state.deadline, start + duration);
        }
    }

    #[test]
    fn acknowledgement_latency_is_not_added_to_the_confirmed_lease_window() {
        let start = Instant::now();
        let mut state = Window {
            deadline: start + Duration::from_secs(10),
            healthy: false,
            lost: false,
            stopped: false,
        };
        state.acknowledge(
            start + Duration::from_secs(2),
            start + Duration::from_secs(4),
            Duration::from_secs(10),
        );
        assert!(state.healthy);
        assert_eq!(state.deadline, start + Duration::from_secs(12));
    }
}
