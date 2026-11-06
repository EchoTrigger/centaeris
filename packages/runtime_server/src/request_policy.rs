use std::time::Duration;

#[derive(Clone, Copy)]
pub(crate) struct RequestPolicy {
    ordinary_deadline: Duration,
    control_deadline: Duration,
}

pub(crate) struct RequestLane {
    pub(crate) deadline: Option<Duration>,
    pub(crate) control: bool,
    pub(crate) cleanup: bool,
}

impl RequestPolicy {
    #[cfg(test)]
    pub(crate) fn for_http_tests(deadline: Duration) -> Self {
        Self {
            ordinary_deadline: deadline,
            control_deadline: deadline,
        }
    }

    pub(crate) fn from_env() -> Result<Self, String> {
        Ok(Self {
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
            && matches!(
                path,
                "/agent-runs/cancel"
                    | "/agent-runs/control"
                    | "/internal/workspace-snapshots/roots"
                    | "/internal/jobs/control/ready"
                    | "/internal/jobs/control/claim"
                    | "/internal/jobs/control/start"
                    | "/internal/jobs/yield"
                    | "/internal/jobs/complete"
                    | "/internal/jobs/heartbeat"
                    | "/internal/environments/retire"
                    | "/internal/environments/maintenance/task"
                    | "/internal/environments/observe"
            ))
            || (method == "GET" && path.starts_with("/internal/jobs/"));
        let listener = method == "POST" && path == "/internal/jobs/wait";
        let cleanup = method == "POST" && path == "/agent-runs/teardown";
        // Long execution and cleanup retain their lease/cancellation lifetime.
        let execution =
            method == "POST" && matches!(path, "/agent-runs/step" | "/internal/benchmark/exec");
        RequestLane {
            deadline: if execution || cleanup {
                None
            } else if control {
                Some(self.control_deadline)
            } else if listener {
                Some(Duration::from_secs(25))
            } else {
                Some(self.ordinary_deadline)
            },
            control,
            cleanup,
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
    fn request_deadlines_follow_operation_lifetimes() {
        let policy = RequestPolicy::for_http_tests(Duration::from_secs(2));
        for path in [
            "/agent-runs/step",
            "/agent-runs/teardown",
            "/internal/benchmark/exec",
        ] {
            assert!(policy.lane("POST", path).deadline.is_none());
        }
        assert_eq!(
            policy.lane("POST", "/skills/catalog").deadline,
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            policy.lane("POST", "/internal/jobs/wait").deadline,
            Some(Duration::from_secs(25))
        );
        assert!(policy.lane("POST", "/agent-runs/teardown").cleanup);
        for path in [
            "/agent-runs/cancel",
            "/internal/jobs/heartbeat",
            "/internal/environments/retire",
        ] {
            let lane = policy.lane("POST", path);
            assert!(lane.control);
            assert_eq!(lane.deadline, Some(Duration::from_secs(2)));
        }
    }
}
