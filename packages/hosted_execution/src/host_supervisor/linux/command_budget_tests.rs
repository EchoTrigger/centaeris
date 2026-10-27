use super::*;
use std::sync::{atomic::AtomicU64, mpsc};
static NEXT: AtomicU64 = AtomicU64::new(0);

fn temporary() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "host-command-budget-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn expired_probe_cannot_start_a_command() {
    let root = temporary();
    let marker = root.join("dispatched");
    let budget = probe::ProbeBudget::new(Instant::now(), None);
    let result = command_bytes_with_budget(
        "/bin/sh",
        &[
            "-c",
            "touch \"$1\"",
            "budget-test",
            marker.to_str().unwrap(),
        ],
        Some(&budget),
    );
    assert_eq!(result.unwrap_err(), "host_probe_timeout");
    assert!(!marker.exists());
    fs::remove_dir(root).unwrap();
}

#[test]
fn peer_disconnect_cancels_running_probe_before_command_completion() {
    let root = temporary();
    let entered = root.join("entered");
    let release = root.join("release");
    let (server, peer) = UnixStream::pair().unwrap();
    let (send, receive) = mpsc::channel();
    let command_entered = entered.clone();
    let command_release = release.clone();
    let worker = thread::spawn(move || {
        let budget = probe::ProbeBudget::new(
            Instant::now() + Duration::from_secs(30),
            Some(server.as_fd()),
        );
        let result = command_bytes_with_budget(
            "/bin/sh",
            &[
                "-c",
                "printf ready > \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.01; done; printf '{}'",
                "budget-test",
                command_entered.to_str().unwrap(),
                command_release.to_str().unwrap(),
            ],
            Some(&budget),
        );
        send.send(result).unwrap();
    });
    let began = Instant::now();
    while !entered.exists() && began.elapsed() < Duration::from_secs(2) {
        thread::sleep(Duration::from_millis(5));
    }
    let started = entered.exists();
    drop(peer);
    let result = receive.recv_timeout(Duration::from_secs(1));
    fs::write(&release, b"release").unwrap();
    worker.join().unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(started);
    assert_eq!(
        result
            .expect("disconnected probe continued waiting")
            .unwrap_err(),
        "host_probe_cancelled"
    );
}

#[test]
fn output_pipe_inherited_after_leader_exit_remains_bounded() {
    let budget = probe::ProbeBudget::new(Instant::now() + Duration::from_millis(100), None);
    let began = Instant::now();
    let result =
        command_bytes_with_budget("/bin/sh", &["-c", "sleep 1 & printf '{}'"], Some(&budget));
    assert_eq!(result.unwrap_err(), "host_probe_timeout");
    assert!(began.elapsed() < Duration::from_millis(800));
}

#[test]
fn completed_command_retains_output_and_error_contracts() {
    assert_eq!(
        command_json("/bin/printf", &["{\"ready\":true}"]).unwrap(),
        serde_json::json!({"ready":true})
    );
    assert_eq!(
        command_bytes_unlocked("/bin/sh", &["-c", "printf rejected; exit 7"]).unwrap_err(),
        "host_command_failed"
    );
}
