//! Wait only after a definitive rejection of an original, unaccepted Start.
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
            Err(error)
                if error == "environment_host_busy"
                    || centaeris_core::execution::environment_task_capacity_rejection(&error) => {}
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
