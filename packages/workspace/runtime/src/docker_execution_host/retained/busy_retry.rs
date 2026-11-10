//! Retry only a host's explicit rejection before this RPC was dispatched.
use std::time::{Duration, Instant};

pub(super) fn call<Request, Response>(
    request: &Request,
    attempt: impl FnMut(&Request) -> Result<Response, String>,
    delay: impl FnMut(Duration),
) -> Result<Response, String> {
    let started = Instant::now();
    call_with_clock(request, attempt, delay, || started.elapsed())
}

fn call_with_clock<Request, Response>(
    request: &Request,
    mut attempt: impl FnMut(&Request) -> Result<Response, String>,
    mut delay: impl FnMut(Duration),
    mut elapsed: impl FnMut() -> Duration,
) -> Result<Response, String> {
    let budget = Duration::from_millis(500);
    let mut backoff = 0usize;
    loop {
        let error = match attempt(request) {
            Ok(response) => return Ok(response),
            Err(error) if error == "environment_host_busy" => error,
            Err(error) => return Err(error),
        };
        let remaining = budget.saturating_sub(elapsed());
        if remaining.is_zero() {
            return Err(error);
        }
        let wait = Duration::from_millis([10, 20, 40, 80, 160][backoff]).min(remaining);
        backoff = (backoff + 1).min(4);
        // The attempt owns all authority pins. It has returned before sleeping;
        // the next attempt must reacquire and validate them independently.
        delay(wait);
        if elapsed() >= budget {
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests;
