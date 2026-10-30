use super::*;
use std::cell::{Cell, RefCell};

#[test]
fn exact_capacity_wait_reuses_the_original_start_and_succeeds_after_more_than_two_attempts() {
    for reason in [
        "task_capacity",
        "task_history_capacity",
        "task_thread_capacity",
        "task_output_capacity",
        "task_journal_capacity",
        "task_input_receipt_capacity",
    ] {
        let attempts = Cell::new(0);
        let pinned = Cell::new(false);
        let waits = RefCell::new(Vec::new());
        let original = (
            "original-operation",
            "original-fence",
            "sha256:original-input",
        );
        let seen = RefCell::new(Vec::new());
        let value = start(
            || {
                assert!(!pinned.replace(true));
                seen.borrow_mut().push(original);
                let next = attempts.get() + 1;
                attempts.set(next);
                pinned.set(false);
                if next < 4 {
                    Err(reason.into())
                } else {
                    Ok("accepted-original")
                }
            },
            || {
                assert!(
                    !pinned.get(),
                    "control probe must run outside authority pin"
                );
                Ok(None)
            },
            |duration| {
                assert!(!pinned.get(), "delay must run outside authority pin");
                waits.borrow_mut().push(duration);
            },
        )
        .unwrap();
        assert_eq!(value, "accepted-original");
        assert_eq!(attempts.get(), 4);
        assert!(!waits.borrow().is_empty());
        assert!(seen.borrow().iter().all(|request| request == &original));
    }
}

#[test]
fn accepted_start_is_returned_once_without_querying_control_or_restarting() {
    let count = Cell::new(0);
    assert_eq!(
        start(
            || {
                count.set(count.get() + 1);
                Ok(7)
            },
            || panic!("accepted Start is not a wait"),
            |_| panic!("accepted Start is not a wait")
        )
        .unwrap(),
        7
    );
    assert_eq!(count.get(), 1);
}

#[test]
fn trusted_host_busy_keeps_the_original_unaccepted_start_pending_and_checks_control() {
    for cancel in [false, true] {
        let attempts = Cell::new(0);
        let controls = Cell::new(0);
        let result = start(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() == 1 {
                    Err("environment_host_busy".into())
                } else {
                    Ok("original-start")
                }
            },
            || {
                controls.set(controls.get() + 1);
                Ok(cancel.then(|| "agent_run_cancel_requested".into()))
            },
            |_| {},
        );
        if cancel {
            assert_eq!(result.unwrap_err(), "agent_run_cancel_requested");
            assert_eq!(attempts.get(), 1);
        } else {
            assert_eq!(result.unwrap(), "original-start");
            assert_eq!(attempts.get(), 2);
        }
        assert!(controls.get() > 0);
    }
}

#[test]
fn unknown_or_noncapacity_outcome_is_never_retried() {
    for reason in [
        "environment_helper_start_unknown",
        "environment_host_transport_unavailable",
        "task_invalid_start",
        "task_capacity: context",
        "execution input rejected: task_capacity; the command was already started",
    ] {
        let count = Cell::new(0);
        let result = start::<()>(
            || {
                count.set(count.get() + 1);
                Err(reason.into())
            },
            || panic!("not a definitive rejection"),
            |_| panic!("not a definitive rejection"),
        );
        assert_eq!(result.unwrap_err(), reason);
        assert_eq!(count.get(), 1);
    }
}

#[test]
fn cancel_and_lease_loss_interrupt_wait_without_restarting_the_command() {
    for reason in [
        "agent_run_cancel_requested",
        "agent_run_lifecycle_lease_lost",
    ] {
        let count = Cell::new(0);
        let result = start::<()>(
            || {
                count.set(count.get() + 1);
                Err("task_capacity".into())
            },
            || Ok(Some(reason.into())),
            |_| panic!("cancelled wait must not sleep"),
        );
        assert_eq!(result.unwrap_err(), reason);
        assert_eq!(count.get(), 1);
    }
}

#[test]
fn unavailable_control_prevents_start_and_recovers_before_retry() {
    let count = Cell::new(0);
    let probes = Cell::new(0);
    let delays = Cell::new(0);
    let result = start(
        || {
            count.set(count.get() + 1);
            if count.get() == 1 {
                Err("task_capacity".into())
            } else {
                assert!(probes.get() >= 3);
                Ok(())
            }
        },
        || {
            let n = probes.get() + 1;
            probes.set(n);
            if n < 3 {
                Err("database unavailable".into())
            } else {
                Ok(None)
            }
        },
        |_| delays.set(delays.get() + 1),
    );
    result.unwrap();
    assert_eq!(count.get(), 2);
    assert!(delays.get() >= 2);
}

#[test]
fn cancellation_during_backoff_is_observed_before_the_next_start() {
    let cancelled = Cell::new(false);
    let starts = Cell::new(0);
    let result = start::<()>(
        || {
            starts.set(starts.get() + 1);
            Err("task_capacity".into())
        },
        || Ok(cancelled.get().then(|| "agent_run_cancel_requested".into())),
        |duration| {
            assert!(duration <= Duration::from_millis(100));
            cancelled.set(true);
        },
    );
    assert_eq!(result.unwrap_err(), "agent_run_cancel_requested");
    assert_eq!(starts.get(), 1);
}
