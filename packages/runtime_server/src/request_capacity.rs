use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

#[derive(Clone)]
pub(crate) struct RequestCapacity {
    ordinary: Arc<Semaphore>,
    listeners: Arc<Semaphore>,
    control: Arc<Semaphore>,
    ordinary_deadline: Duration,
    control_deadline: Duration,
}

pub(crate) struct RequestLane {
    pub(crate) slots: Arc<Semaphore>,
    pub(crate) deadline: Option<Duration>,
    pub(crate) control: bool,
}

impl RequestCapacity {
    pub(crate) fn from_env() -> Result<Self, String> {
        Ok(Self {
            ordinary: Arc::new(Semaphore::new(capacity("RUNTIME_HTTP_REQUEST_LIMIT", 24)?)),
            listeners: Arc::new(Semaphore::new(capacity("RUNTIME_HTTP_LISTENER_LIMIT", 8)?)),
            control: Arc::new(Semaphore::new(capacity("RUNTIME_HTTP_CONTROL_LIMIT", 4)?)),
            ordinary_deadline: Duration::from_secs(capacity(
                "RUNTIME_HTTP_REQUEST_TIMEOUT_SECONDS",
                30,
            )? as u64),
            control_deadline: Duration::from_secs(capacity(
                "RUNTIME_HTTP_CONTROL_TIMEOUT_SECONDS",
                5,
            )? as u64),
        })
    }

    pub(crate) fn lane(&self, method: &str, path: &str) -> RequestLane {
        let control = (method == "POST"
            && matches!(path, "/agent-runs/cancel" | "/internal/jobs/heartbeat"))
            || (method == "GET" && path.starts_with("/internal/jobs/"));
        let listener = method == "POST" && path == "/internal/jobs/wait";
        // A step can contain long tools and provider streams. Its lifetime is
        // governed by the execution lease/cancellation protocol, not a short RPC timer.
        let execution = method == "POST" && path == "/agent-runs/step";
        RequestLane {
            slots: if control {
                self.control.clone()
            } else if listener {
                self.listeners.clone()
            } else {
                self.ordinary.clone()
            },
            deadline: if execution {
                None
            } else if control {
                Some(self.control_deadline)
            } else if listener {
                Some(Duration::from_secs(25))
            } else {
                Some(self.ordinary_deadline)
            },
            control,
        }
    }
}

pub(crate) fn capacity(name: &str, default: u64) -> Result<usize, String> {
    let value = crate::positive_u64_env(name, default)?;
    if value > 1_000_000 {
        return Err(format!("{name} must be at most 1000000"));
    }
    Ok(value as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_execution_and_listener_lanes_leave_control_available() {
        let limits = RequestCapacity {
            ordinary: Arc::new(Semaphore::new(1)),
            listeners: Arc::new(Semaphore::new(1)),
            control: Arc::new(Semaphore::new(1)),
            ordinary_deadline: Duration::from_secs(30),
            control_deadline: Duration::from_secs(5),
        };
        let execution = limits.lane("POST", "/agent-runs/step");
        assert!(execution.deadline.is_none());
        let _execution = execution.slots.try_acquire_owned().unwrap();
        let listener = limits.lane("POST", "/internal/jobs/wait");
        assert_eq!(listener.deadline, Some(Duration::from_secs(25)));
        let _listener = listener.slots.try_acquire_owned().unwrap();
        assert!(limits
            .lane("POST", "/skills/catalog")
            .slots
            .try_acquire_owned()
            .is_err());
        for (method, path) in [
            ("POST", "/agent-runs/cancel"),
            ("POST", "/internal/jobs/heartbeat"),
            ("GET", "/internal/jobs/job_1"),
        ] {
            let control = limits.lane(method, path);
            assert!(control.control);
            assert_eq!(control.deadline, Some(Duration::from_secs(5)));
            assert!(control.slots.try_acquire_owned().is_ok());
        }
        assert!(limits.lane("POST", "/agent-runs/step").deadline.is_none());
    }
}
