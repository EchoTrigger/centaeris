//! Wait for occupied execution slots, preserving the original unaccepted Start.
use std::time::Duration;

pub(super) fn start<T>(
    mut attempt: impl FnMut() -> Result<T, String>,
    mut control: impl FnMut() -> Result<Option<String>, String>,
    mut delay: impl FnMut(Duration),
) -> Result<T, String> {
    let mut backoff = 0usize;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            // A definitive rejection proves only that this RPC was not accepted.
            // Retained output/history budgets need consumption or Run closure;
            // waiting here cannot provide either while this Run owns the slot.
            Err(error) if matches!(error.as_str(), "environment_host_busy" | "task_capacity") => {}
            Err(error) => return Err(error),
        }
        let mut remaining = [100, 250, 500, 1000][backoff];
        backoff = (backoff + 1).min(3);
        loop {
            match control() {
                Ok(Some(reason)) => return Err(reason),
                Ok(None) if remaining == 0 => break,
                Ok(None) => {}
                Err(_) => {
                    // Authority is uncertain. Keep the original unaccepted
                    // operation pending; never dispatch under an unknown lease.
                    delay(Duration::from_millis(100));
                    continue;
                }
            }
            let interval = remaining.min(100);
            delay(Duration::from_millis(interval));
            remaining -= interval;
        }
    }
}

#[cfg(test)]
mod tests;
