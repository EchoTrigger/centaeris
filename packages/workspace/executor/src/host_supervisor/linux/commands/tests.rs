use super::*;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Instant,
};
static SERIAL: AtomicU64 = AtomicU64::new(0);

#[test]
fn independent_commands_complete_while_another_command_is_blocked() {
    let root = std::env::temp_dir().join(format!(
        "centaeris-command-concurrency-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let entered = root.join("entered");
    let release = root.join("release");
    let first_entered = entered.clone();
    let first_release = release.clone();
    let first = thread::spawn(move || {
        super::super::command_json(
            "/bin/sh",
            &[
                "-c",
                "printf ready > \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.01; done; printf '{}'",
                "command-test",
                first_entered.to_str().unwrap(),
                first_release.to_str().unwrap(),
            ],
        )
    });
    let began = Instant::now();
    while !entered.exists() && began.elapsed() < Duration::from_secs(3) {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        entered.exists(),
        "first command did not enter its blocking operation"
    );
    let (send, receive) = mpsc::channel();
    let second = thread::spawn(move || {
        send.send(super::super::command_json("/bin/printf", &["{}"]).unwrap())
            .unwrap();
    });
    let concurrent = receive.recv_timeout(Duration::from_secs(1));
    fs::write(release, b"release").unwrap();
    let first_result = first.join().unwrap();
    second.join().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert_eq!(first_result.unwrap(), serde_json::json!({}));
    assert_eq!(
        concurrent.expect("independent command queued behind blocked command"),
        serde_json::json!({})
    );
}

#[test]
fn adopted_reaper_cannot_take_a_direct_child_exit_receipt() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 23"]);
    let mut child = OwnedChild::spawn(&mut command).unwrap();
    let pid = child.id();
    let began = Instant::now();
    while !fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .is_some_and(|(_, fields)| fields.split_whitespace().next() == Some("Z"))
    }) {
        assert!(
            began.elapsed() < Duration::from_secs(3),
            "child did not exit"
        );
        thread::sleep(Duration::from_millis(5));
    }
    reap_adopted_pid(pid);
    assert_eq!(
        child.wait().unwrap().code(),
        Some(23),
        "direct Child owns the actual wait receipt"
    );
    let registry = WAIT_REGISTRY.lock().unwrap();
    assert!(
        !registry.owned.contains(&pid),
        "reaped child retained its registration"
    );
    assert!(!registry.abandoned.contains_key(&pid));
}

#[test]
fn failed_exit_releases_its_wait_registration() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 17"]);
    let mut child = OwnedChild::spawn(&mut command).unwrap();
    let pid = child.id();
    assert_eq!(child.wait().unwrap().code(), Some(17));
    drop(child);
    let registry = WAIT_REGISTRY.lock().unwrap();
    assert!(
        !registry.owned.contains(&pid),
        "failed command leaked its registration"
    );
    assert!(!registry.abandoned.contains_key(&pid));
}

#[test]
fn early_error_transfers_the_live_child_to_continuous_cleanup() {
    fn fail_after_spawn() -> Result<u32, u32> {
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let child = OwnedChild::spawn(&mut command).unwrap();
        Err(child.id())
    }
    let pid = fail_after_spawn().unwrap_err();
    let began = Instant::now();
    while fs::metadata(format!("/proc/{pid}")).is_ok() {
        assert!(
            began.elapsed() < Duration::from_secs(3),
            "dropped child was not reaped without a task manager"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let registry = WAIT_REGISTRY.lock().unwrap();
    assert!(
        !registry.owned.contains(&pid),
        "cleanup reaped the child but leaked its registration"
    );
    assert!(!registry.abandoned.contains_key(&pid));
}

#[test]
fn cancellation_during_registry_contention_prevents_spawn() {
    ensure_cleanup().unwrap();
    let guard = WAIT_REGISTRY.lock().unwrap();
    let (send, receive) = mpsc::channel();
    let worker = thread::spawn(move || {
        let began = Instant::now();
        let mut command = Command::new("/bin/true");
        let result = OwnedChild::spawn_checked(&mut command, || {
            if began.elapsed() >= Duration::from_millis(30) {
                Err("probe_expired".into())
            } else {
                Ok(())
            }
        });
        send.send(result.map(|_| ())).unwrap();
    });
    let outcome = receive.recv_timeout(Duration::from_secs(1));
    drop(guard);
    worker.join().unwrap();
    assert_eq!(
        outcome.expect("cancelled spawn remained queued"),
        Err("probe_expired".into())
    );
}

#[test]
fn lifecycle_gates_serialize_one_container_and_release_other_containers() {
    let runtime = Path::new("/usr/bin/runsc");
    let root = Path::new("/run/runsc");
    let first = instance_gate(runtime, root, "first");
    let same = instance_gate(runtime, root, "first");
    let other = instance_gate(runtime, root, "second");
    let guard = first.lock().unwrap();
    assert!(matches!(
        same.try_lock(),
        Err(std::sync::TryLockError::WouldBlock)
    ));
    assert!(
        other.try_lock().is_ok(),
        "independent environment lifecycle was serialized"
    );
    drop(guard);
    assert!(same.try_lock().is_ok());
}

#[test]
fn adopted_handoff_preserves_the_leaders_receipt_until_registration() {
    let handoff = AdoptedHandoff::begin().unwrap();
    let child = Command::new("/bin/sh")
        .args(["-c", "exit 29"])
        .spawn()
        .unwrap();
    let pid = child.id();
    // std Child does not reap on drop; the runtime hands over only the leader PID.
    drop(child);
    thread::sleep(Duration::from_millis(30));
    reap_adopted_pid(pid);
    let mut leader = handoff.adopt(pid).unwrap();
    assert!(
        matches!(handoff.adopt(pid), Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    drop(handoff);
    reap_adopted_pid(pid);
    let began = Instant::now();
    let status = loop {
        if let Some(status) = leader.try_wait().unwrap() {
            break status;
        }
        assert!(began.elapsed() < Duration::from_secs(3));
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(status.code(), Some(29));
    assert!(!WAIT_REGISTRY.lock().unwrap().owned.contains(&pid));
}
