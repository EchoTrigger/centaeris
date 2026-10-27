use super::*;
use std::{
    net::Shutdown,
    os::{fd::AsFd, unix::net::UnixStream},
    sync::{mpsc, Arc},
    thread,
};

#[test]
fn expired_probe_never_dispatches_or_acquires_a_lock() {
    let budget = ProbeBudget::new(Instant::now(), None);
    let mut dispatched = false;
    if budget.check().is_ok() {
        dispatched = true;
    }
    assert!(!dispatched, "expired probe dispatched work");
    assert!(budget.lock(&Mutex::new(())).is_err());
    assert!(ProbeAdmission::new().try_acquire(&budget).is_err());
}

#[test]
fn fully_closed_peer_cancels_probe() {
    let (server, peer) = UnixStream::pair().unwrap();
    let budget = ProbeBudget::new(
        Instant::now() + Duration::from_secs(1),
        Some(server.as_fd()),
    );
    drop(peer);
    assert_eq!(budget.check().unwrap_err(), "host_probe_cancelled");
}

#[test]
fn request_write_half_close_keeps_response_reader_eligible() {
    let (server, peer) = UnixStream::pair().unwrap();
    let budget = ProbeBudget::new(
        Instant::now() + Duration::from_secs(1),
        Some(server.as_fd()),
    );
    peer.shutdown(Shutdown::Write).unwrap();
    budget
        .check()
        .expect("half-closed peer still awaits response");
}

#[test]
fn copied_budget_shares_one_deadline_across_steps() {
    let budget = ProbeBudget::new(Instant::now() + Duration::from_millis(25), None);
    budget.check().unwrap();
    let next_step = budget;
    thread::sleep(Duration::from_millis(40));
    assert_eq!(next_step.remaining().unwrap_err(), "host_probe_timeout");
}

#[test]
fn contended_mutex_wait_ends_before_holder_releases() {
    let mutex = Arc::new(Mutex::new(()));
    let (ready, started) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let held = mutex.clone();
    let holder = thread::spawn(move || {
        let _guard = held.lock().unwrap();
        ready.send(()).unwrap();
        released.recv().unwrap();
    });
    started.recv().unwrap();
    let (result, completed) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let budget = ProbeBudget::new(Instant::now() + Duration::from_millis(25), None);
        let outcome = budget.lock(&mutex).map(|_guard| ());
        result.send(outcome).unwrap();
    });
    let outcome = completed.recv_timeout(Duration::from_millis(250));
    release.send(()).unwrap();
    holder.join().unwrap();
    waiter.join().unwrap();
    assert_eq!(
        outcome
            .expect("probe stayed queued after its deadline")
            .unwrap_err(),
        "host_probe_timeout"
    );
}

#[test]
fn probe_admission_is_bounded_and_reusable_after_release() {
    let admission = ProbeAdmission::new();
    let budget = ProbeBudget::new(Instant::now() + Duration::from_secs(1), None);
    let first = admission.try_acquire(&budget).unwrap();
    let second = admission.try_acquire(&budget).unwrap();
    assert!(matches!(admission.try_acquire(&budget), Err(reason) if reason == "host_probe_busy"));
    drop(first);
    let replacement = admission.try_acquire(&budget).unwrap();
    drop(second);
    drop(replacement);
    assert!(admission.try_acquire(&budget).is_ok());
}
