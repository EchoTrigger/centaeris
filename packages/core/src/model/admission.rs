//! Per-attempt model admission. Hosts share one coordinator for each quota
//! domain; a domain is an explicit Host decision, never inferred from secrets,
//! provider names, or model names. Clones share capacity and cooldown state.
use super::{JsonHttpFuture, JsonHttpRequest, JsonHttpTransport};
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::sync::Notify;
use tokio::time::Instant;

/// Fixed concurrency and round-robin admission across runs, FIFO within a run.
/// Waiting and admitted attempts are cancellation-safe when their future drops.
#[derive(Clone)]
pub struct ModelAdmission(Arc<Shared>);

struct Shared {
    limit: usize,
    state: Mutex<State>,
    changed: Notify,
}
#[derive(Default)]
struct State {
    active: usize,
    next_ticket: u64,
    runs: VecDeque<RunQueue>,
    cooldown_until: Option<Instant>,
}
struct RunQueue {
    run_id: String,
    tickets: VecDeque<u64>,
}

impl ModelAdmission {
    pub fn new(limit: NonZeroUsize) -> Self {
        Self(Arc::new(Shared {
            limit: limit.get(),
            state: Mutex::new(State::default()),
            changed: Notify::new(),
        }))
    }

    /// Admit one actual provider attempt. The returned ticket owns capacity until
    /// it is dropped, including while a response body or stream is being read.
    /// Dropping the acquire future removes its queued request.
    pub async fn acquire_attempt(&self, run_id: &str) -> ModelAttemptTicket {
        let ticket = {
            let mut state = self.0.state.lock().expect("model admission lock poisoned");
            let ticket = state.next_ticket;
            state.next_ticket = ticket
                .checked_add(1)
                .expect("model admission ticket exhausted");
            if let Some(run) = state.runs.iter_mut().find(|run| run.run_id == run_id) {
                run.tickets.push_back(ticket);
            } else {
                state.runs.push_back(RunQueue {
                    run_id: run_id.to_owned(),
                    tickets: VecDeque::from([ticket]),
                });
            }
            ticket
        };
        let mut attempt = ModelAttemptTicket {
            admission: self.clone(),
            ticket,
            admitted: false,
        };
        loop {
            // Register before checking state: a release between the check and
            // await must not be lost, including with multiple executor threads.
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let cooldown = {
                let mut state = self.0.state.lock().expect("model admission lock poisoned");
                let cooldown = state.cooldown_until.filter(|until| *until > Instant::now());
                if cooldown.is_none()
                    && state.active < self.0.limit
                    && state.runs.front().and_then(|run| run.tickets.front()) == Some(&ticket)
                {
                    let mut run = state.runs.pop_front().expect("eligible run exists");
                    run.tickets.pop_front();
                    if !run.tickets.is_empty() {
                        state.runs.push_back(run);
                    }
                    state.active += 1;
                    attempt.admitted = true;
                    self.0.changed.notify_waiters();
                    return attempt;
                }
                cooldown
            };
            if let Some(until) = cooldown {
                tokio::select! {
                    _ = &mut changed => {},
                    _ = tokio::time::sleep_until(until) => {},
                }
            } else {
                changed.await;
            }
        }
    }

    fn observe_http_status(&self, status_code: u16, retry_after: Option<&str>) {
        if !super::transport::is_retryable_http_status(status_code) {
            return;
        }
        let delay = retry_after.and_then(|value| retry_after_delay(value, SystemTime::now()));
        let Some(until) = delay.and_then(|delay| Instant::now().checked_add(delay)) else {
            return;
        };
        let mut state = self.0.state.lock().expect("model admission lock poisoned");
        state.cooldown_until = Some(
            state
                .cooldown_until
                .map_or(until, |previous| previous.max(until)),
        );
        self.0.changed.notify_waiters();
    }
}

// One owner represents both queue membership and the eventual permit. Dropping
// an unpolled/dropped/failed HTTP future needs no asynchronous cleanup task.
/// A cancellation-safe, single-attempt capacity ticket. Hosts that own the
/// provider transport hold it through complete body or stream consumption.
pub struct ModelAttemptTicket {
    admission: ModelAdmission,
    ticket: u64,
    admitted: bool,
}
impl ModelAttemptTicket {
    /// Install a shared Retry-After cooldown before releasing the ticket.
    /// Invalid values and non-retryable statuses have no effect.
    pub fn observe_http_status(&self, status_code: u16, retry_after: Option<&str>) {
        self.admission.observe_http_status(status_code, retry_after);
    }
}
impl Drop for ModelAttemptTicket {
    fn drop(&mut self) {
        let mut state = self
            .admission
            .0
            .state
            .lock()
            .expect("model admission lock poisoned");
        if self.admitted {
            state.active -= 1;
        } else {
            for run in &mut state.runs {
                run.tickets.retain(|ticket| *ticket != self.ticket);
            }
            state.runs.retain(|run| !run.tickets.is_empty());
        }
        self.admission.0.changed.notify_waiters();
    }
}

fn retry_after_delay(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value.parse().ok().map(Duration::from_secs);
    }
    httpdate::parse_http_date(value)
        .ok()
        .map(|at| at.duration_since(now).unwrap_or_default())
}

/// Wrap the actual transport, inside the protocol adapter's retry loop. The
/// permit covers headers AND the complete JSON body / SSE stream. Returning a
/// response installs Retry-After before releasing capacity, even on a final
/// failed attempt. Backoff and subsequent parsing happen after release.
pub struct AdmittedJsonHttpTransport<T> {
    inner: T,
    admission: ModelAdmission,
    run_id: String,
}
impl<T> AdmittedJsonHttpTransport<T> {
    pub fn new(inner: T, admission: ModelAdmission, run_id: String) -> Self {
        Self {
            inner,
            admission,
            run_id,
        }
    }
}
impl<T: JsonHttpTransport> JsonHttpTransport for AdmittedJsonHttpTransport<T> {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            let attempt = self.admission.acquire_attempt(&self.run_id).await;
            let response = self.inner.execute_json(request).await;
            if let Ok(response) = &response {
                attempt.observe_http_status(
                    response.status_code,
                    response
                        .headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                        .map(|(_, value)| value.as_str()),
                );
            }
            response
        })
    }
    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        Box::pin(async move {
            let attempt = self.admission.acquire_attempt(&self.run_id).await;
            let response = self.inner.execute_sse(request, on_data).await;
            if let Ok(response) = &response {
                attempt.observe_http_status(
                    response.status_code,
                    response
                        .headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                        .map(|(_, value)| value.as_str()),
                );
            }
            response
        })
    }
}
#[cfg(test)]
mod tests;
