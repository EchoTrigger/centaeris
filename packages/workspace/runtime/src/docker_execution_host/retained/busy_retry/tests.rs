use super::*;
use std::cell::{Cell, RefCell};

struct Pin<'a>(&'a Cell<bool>);
impl Drop for Pin<'_> {
    fn drop(&mut self) {
        assert!(self.0.replace(false));
    }
}

#[test]
fn explicit_busy_retries_the_identical_task_operation_and_cursor_after_releasing_each_pin() {
    let original = (
        "task-original",
        "operation-original",
        "fence-original",
        417_u64,
    );
    let calls = Cell::new(0);
    let pinned = Cell::new(false);
    let elapsed = Cell::new(Duration::ZERO);
    let seen = RefCell::new(Vec::new());
    let delays = RefCell::new(Vec::new());
    let result = call_with_clock(
        &original,
        |request| {
            assert!(!pinned.replace(true));
            let _pin = Pin(&pinned);
            seen.borrow_mut().push(*request);
            calls.set(calls.get() + 1);
            if calls.get() < 4 {
                Err("environment_host_busy".into())
            } else {
                Ok("original-response")
            }
        },
        |duration| {
            assert!(
                !pinned.get(),
                "never sleep while database authority is pinned"
            );
            delays.borrow_mut().push(duration);
            elapsed.set(elapsed.get() + duration);
        },
        || elapsed.get(),
    );
    assert_eq!(result.unwrap(), "original-response");
    assert_eq!(calls.get(), 4);
    assert_eq!(*delays.borrow(), [10, 20, 40].map(Duration::from_millis));
    assert!(seen.borrow().iter().all(|request| request == &original));
}

#[test]
fn continuous_busy_preserves_the_exact_rejection_with_a_half_second_budget() {
    let elapsed = Cell::new(Duration::ZERO);
    let calls = Cell::new(0);
    let result = call_with_clock::<_, ()>(
        &"same-query",
        |_| {
            calls.set(calls.get() + 1);
            Err("environment_host_busy".into())
        },
        |duration| elapsed.set(elapsed.get() + duration),
        || elapsed.get(),
    );
    assert_eq!(result.unwrap_err(), "environment_host_busy");
    assert_eq!(elapsed.get(), Duration::from_millis(500));
    assert!(calls.get() > 1 && calls.get() <= 8);
}

#[test]
fn each_retry_revalidates_authority_and_lease_revocation_prevents_a_second_dispatch() {
    let current = Cell::new(true);
    let host_calls = Cell::new(0);
    let elapsed = Cell::new(Duration::ZERO);
    let result = call_with_clock::<_, ()>(
        &"unchanged-input-receipt",
        |_| {
            if !current.get() {
                return Err("agent_run_lifecycle_lease_lost".into());
            }
            host_calls.set(host_calls.get() + 1);
            Err("environment_host_busy".into())
        },
        |duration| {
            current.set(false);
            elapsed.set(elapsed.get() + duration);
        },
        || elapsed.get(),
    );
    assert_eq!(result.unwrap_err(), "agent_run_lifecycle_lease_lost");
    assert_eq!(host_calls.get(), 1);
}

#[test]
fn unknown_transport_nested_errors_and_near_matches_are_not_retried() {
    for reason in [
        "environment_host_transport_unavailable",
        "environment_host_response_invalid",
        "environment_task_response_unknown",
        "host_busy",
        "environment_host_busy: detail",
        "task_error_environment_host_busy",
        "task_capacity",
    ] {
        let calls = Cell::new(0);
        let result = call_with_clock::<_, ()>(
            &"unchanged-start",
            |_| {
                calls.set(calls.get() + 1);
                Err(reason.into())
            },
            |_| panic!("only the exact trusted host rejection may wait"),
            || Duration::ZERO,
        );
        assert_eq!(result.unwrap_err(), reason);
        assert_eq!(calls.get(), 1);
    }
}

#[test]
fn unknown_outcome_after_busy_is_returned_without_reissuing_the_request() {
    let calls = Cell::new(0);
    let elapsed = Cell::new(Duration::ZERO);
    let result = call_with_clock::<_, ()>(
        &"same-query-cursor",
        |_| {
            calls.set(calls.get() + 1);
            Err(if calls.get() == 1 {
                "environment_host_busy".into()
            } else {
                "environment_host_transport_unavailable".into()
            })
        },
        |duration| elapsed.set(elapsed.get() + duration),
        || elapsed.get(),
    );
    assert_eq!(
        result.unwrap_err(),
        "environment_host_transport_unavailable"
    );
    assert_eq!(calls.get(), 2);
}

#[test]
fn rpc_time_and_scheduler_delay_count_toward_the_retry_budget() {
    for slow_attempt in [false, true] {
        let elapsed = Cell::new(Duration::ZERO);
        let calls = Cell::new(0);
        let result = call_with_clock::<_, ()>(
            &"query-original",
            |_| {
                calls.set(calls.get() + 1);
                if slow_attempt {
                    elapsed.set(Duration::from_millis(600));
                }
                Err("environment_host_busy".into())
            },
            |_| {
                assert!(!slow_attempt, "an expired budget must not sleep");
                elapsed.set(Duration::from_millis(600));
            },
            || elapsed.get(),
        );
        assert_eq!(result.unwrap_err(), "environment_host_busy");
        assert_eq!(calls.get(), 1);
    }
}
