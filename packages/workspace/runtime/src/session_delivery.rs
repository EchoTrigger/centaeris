//! Hosted persistence recovery retries a commit, never a tool execution.
use std::time::Duration;

const RETRYABLE: &str = "session_storage_retryable:";
const PENDING: &str = "session_delivery_pending:";

pub(crate) fn pending(error: &str) -> bool {
    error.starts_with(PENDING)
}

pub(crate) fn admission<T>(
    result: Result<T, String>,
    cleanup: impl FnOnce() -> Result<(), String>,
) -> Result<T, String> {
    match result {
        Err(error) if !pending(&error) => {
            if let Err(cleanup_error) = cleanup() {
                return Err(format!(
                    "{error}; unadmitted_execution_cleanup_failed:{cleanup_error}"
                ));
            }
            Err(error)
        }
        result => result,
    }
}

pub(crate) fn retry<T>(
    mut commit: impl FnMut() -> Result<T, String>,
    mut delay: impl FnMut(Duration),
) -> Result<T, String> {
    for attempt in 0..3 {
        match commit() {
            Ok(receipt) => return Ok(receipt),
            Err(error)
                if error.starts_with(RETRYABLE)
                    || error.starts_with("connect Postgres runtime store failed:")
                    || error == "Postgres connection pool checkout timed out" =>
            {
                if attempt == 2 {
                    return Err(format!("{PENDING}{}", error.trim_start_matches(RETRYABLE)));
                }
                delay(Duration::from_millis(50 * (attempt + 1)));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded commit loop always returns")
}

pub(crate) fn postgres_error(context: &str, error: postgres::Error) -> String {
    let transient = error.is_closed()
        || error.as_db_error().is_none()
        || error.as_db_error().is_some_and(|e| {
            matches!(
                e.code().code(),
                "40001" | "40P01" | "55P03" | "57P01" | "53300"
            )
        });
    if transient {
        format!("{RETRYABLE}{context}")
    } else {
        // SQL rejection is hosted storage failure, never evidence that Core or
        // the tool failed. Yield for repair without hot-looping a permanent error.
        format!("{PENDING}{context}: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transient_commit_retries_the_same_receipt_without_terminating_the_run() {
        let mut commits = 0;
        let mut waits = Vec::new();
        let receipt = retry(
            || {
                commits += 1;
                if commits < 3 {
                    Err(format!("{RETRYABLE}serialization"))
                } else {
                    Ok("committed original receipt")
                }
            },
            |time| waits.push(time),
        );
        assert_eq!(receipt.unwrap(), "committed original receipt");
        assert_eq!(commits, 3);
        assert_eq!(waits.len(), 2);
    }

    #[test]
    fn persistent_outage_is_a_recoverable_delivery_wait_not_a_tool_failure() {
        let mut calls = 0;
        let error = retry::<()>(
            || {
                calls += 1;
                Err(format!("{RETRYABLE}connection"))
            },
            |_| {},
        )
        .unwrap_err();
        assert!(pending(&error));
        assert_eq!(calls, 3);
    }

    #[test]
    fn pool_unavailability_does_not_escape_as_a_core_failure() {
        for failure in [
            "connect Postgres runtime store failed: connection refused",
            "Postgres connection pool checkout timed out",
        ] {
            let error = retry::<()>(|| Err(failure.into()), |_| {}).unwrap_err();
            assert!(pending(&error));
        }
    }

    #[test]
    fn lease_and_validation_failures_are_never_retried_or_hidden() {
        for failure in [
            "runtime_job_lease_fence_rejected",
            "session_payload_storage_index_mismatch",
        ] {
            let mut calls = 0;
            let error = retry::<()>(
                || {
                    calls += 1;
                    Err(failure.into())
                },
                |_| panic!("no wait"),
            )
            .unwrap_err();
            assert_eq!(error, failure);
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn failed_admission_cleans_only_its_provisional_execution_and_preserves_the_primary_error() {
        let mut cleanup = 0;
        let error = admission::<()>(Err("invalid history".into()), || {
            cleanup += 1;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(cleanup, 1);
        assert_eq!(error, "invalid history");
        let error = admission::<()>(Err("invalid history".into()), || {
            Err("cleanup unavailable".into())
        })
        .unwrap_err();
        assert!(error.starts_with("invalid history;"));
        assert!(error.contains("cleanup unavailable"));
    }

    #[test]
    fn pending_admission_keeps_its_execution_for_recovery() {
        assert!(
            admission::<()>(Err(format!("{PENDING}connection")), || panic!(
                "retain provisional execution"
            ))
            .is_err()
        );
        assert_eq!(admission(Ok(7), || panic!("admitted execution")), Ok(7));
    }
}
