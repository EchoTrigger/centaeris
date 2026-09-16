#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn stdio_relay_preserves_frames_and_disconnects_on_client_eof() {
    let root = std::env::temp_dir().join(format!(
        "cr-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let executable = env!("CARGO_BIN_EXE_centaeris-runtime");
    let endpoint = Command::new(executable)
        .arg("--runtime-server-endpoint")
        .env("CENTAERIS_DESKTOP_DATA_DIR", &root)
        .output()
        .unwrap();
    assert!(endpoint.status.success());
    let endpoint: serde_json::Value = serde_json::from_slice(&endpoint.stdout).unwrap();
    let listener = UnixListener::bind(endpoint["endpoint"].as_str().unwrap()).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = Command::new(executable)
        .arg("--runtime-server-connect")
        .env("CENTAERIS_DESKTOP_DATA_DIR", &root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (send, recv) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let connected = recv.recv_timeout(Duration::from_secs(5));
    if connected.is_err() {
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        std::fs::remove_dir_all(root).unwrap();
        panic!(
            "relay did not connect: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(connected.unwrap(), r#"{"connected":true}"#);
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"request\n")
        .unwrap();
    let mut input = [0; 8];
    socket.read_exact(&mut input).unwrap();
    assert_eq!(&input, b"request\n");
    socket.write_all("回答\n".as_bytes()).unwrap();
    assert_eq!(recv.recv_timeout(Duration::from_secs(5)).unwrap(), "回答");
    drop(child.stdin.take());
    assert_eq!(socket.read(&mut input).unwrap(), 0);
    assert!(child.wait().unwrap().success());
    drop(socket);
    drop(listener);
    std::fs::remove_dir_all(root).unwrap();
}
