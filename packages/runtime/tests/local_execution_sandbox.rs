use std::fs;
#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;
#[cfg(not(target_os = "windows"))]
use std::thread::sleep;
#[cfg(not(target_os = "windows"))]
use std::time::Duration;
#[cfg(not(target_os = "windows"))]
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::execution::ExecutionHostRunner;
use centaeris_core::execution::{ExecutionCommandRequest, ExecutionPolicy};
#[cfg(not(target_os = "windows"))]
use centaeris_core::execution::{
    ExecutionFileSystemErrorKind, ExecutionFileSystemOperation, ExecutionFileSystemOutput,
    ExecutionFileSystemRequest,
};
use centaeris_runtime::local_execution_host::LocalExecutionHostRunner;

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn public_network_keeps_tcp_and_udp_available() {
    use std::net::{TcpListener, UdpSocket};
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    udp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let workspace = std::env::temp_dir().join(format!("centaeris-ip-{}", std::process::id()));
    fs::create_dir_all(&workspace).unwrap();
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .unwrap();
    let output = runner.run_host_command(None, ExecutionCommandRequest {
        program: "python3".into(),
        args: vec!["-c".into(), "import socket,sys; socket.create_connection(('127.0.0.1',int(sys.argv[1])),2).close(); socket.socket(socket.AF_INET,socket.SOCK_DGRAM).sendto(b'ok',('127.0.0.1',int(sys.argv[2])))".into(), tcp.local_addr().unwrap().port().to_string(), udp.local_addr().unwrap().port().to_string()],
        cwd: workspace.clone(), env: Default::default(), timeout_ms: 5000,
        policy: ExecutionPolicy::workspace_write_public_internet(&workspace),
    }, None).unwrap();
    fs::remove_dir_all(workspace).unwrap();
    assert_eq!(
        output.process.exit_code,
        Some(0),
        "{}",
        output.process.stderr
    );
    let mut bytes = [0; 2];
    assert_eq!(udp.recv(&mut bytes).unwrap(), 2);
    assert_eq!(&bytes, b"ok");
}

#[test]
#[cfg(target_os = "macos")]
#[ignore = "requires external DNS; exercised explicitly by macOS CI"]
fn macos_public_network_resolves_dns() {
    let workspace = std::env::temp_dir().join(format!("centaeris-dns-{}", std::process::id()));
    fs::create_dir_all(&workspace).unwrap();
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .unwrap();
    let output = runner.run_host_command(None, ExecutionCommandRequest {
        program: "python3".into(),
        args: vec!["-c".into(), "import socket; assert socket.getaddrinfo('example.com',443,type=socket.SOCK_STREAM)".into()],
        cwd: workspace.clone(), env: Default::default(), timeout_ms: 15000,
        policy: ExecutionPolicy::workspace_write_public_internet(&workspace),
    }, None).unwrap();
    fs::remove_dir_all(workspace).unwrap();
    assert_eq!(
        output.process.exit_code,
        Some(0),
        "{}",
        output.process.stderr
    );
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn local_commands_cannot_connect_to_a_private_host_socket() {
    use std::os::unix::net::{UnixListener, UnixStream};
    let root = PathBuf::from("/tmp").join(format!(
        "cn-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let workspace = root.join("work");
    fs::create_dir_all(&workspace).unwrap();
    let socket = root.join("private.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .unwrap();
    let output = runner.run_host_command(None, ExecutionCommandRequest {
        program: "python3".into(), args: vec!["-c".into(),
            "import socket,sys\ntry:\n s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1])\nexcept PermissionError: sys.exit(0)\nsys.exit(77)".into(), socket.to_string_lossy().into_owned()],
        cwd: workspace.clone(), env: Default::default(), timeout_ms: 5000,
        policy: ExecutionPolicy::workspace_write_public_internet(&workspace),
    }, None).unwrap();
    assert_eq!(
        output.process.exit_code,
        Some(0),
        "{}",
        output.process.stderr
    );
    assert!(listener.accept().is_err());
    let parent_connection = UnixStream::connect(&socket).unwrap();
    assert!(
        listener.accept().is_ok(),
        "the Runtime parent retains its own access"
    );
    drop(parent_connection);
    drop(listener);
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stdin_hooks_can_write_output_before_reading_a_large_event() {
    let workspace =
        std::env::temp_dir().join(format!("centaeris-hook-duplex-{}", std::process::id()));
    fs::create_dir_all(&workspace).unwrap();
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .unwrap();
    let output = runner.run_command_with_stdin(ExecutionCommandRequest {
        program: "python3".into(), args: vec!["-c".into(),
            "import sys; sys.stdout.write('x'*200000); sys.stdout.flush(); data=sys.stdin.read(); sys.stderr.write(str(len(data)))".into()],
        cwd: workspace.clone(), env: Default::default(), timeout_ms: 3000,
        policy: ExecutionPolicy::workspace_write_no_network(&workspace),
    }, &vec![b'a'; 200000]).unwrap();
    fs::remove_dir_all(workspace).unwrap();
    assert_eq!(output.process.exit_code, Some(0));
    assert_eq!(output.process.stderr, "200000");
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stdin_hook_capture_is_bounded_while_preserving_total_byte_counts() {
    let workspace =
        std::env::temp_dir().join(format!("centaeris-hook-capture-{}", std::process::id()));
    fs::create_dir_all(&workspace).unwrap();
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .unwrap();
    let output = runner.run_command_with_stdin(ExecutionCommandRequest {
        program: "python3".into(), args: vec!["-c".into(),
            "import sys; sys.stdin.read(); sys.stdout.write('x'*200000); sys.stderr.write('y'*200000)".into()],
        cwd: workspace.clone(), env: Default::default(), timeout_ms: 5000,
        policy: ExecutionPolicy::workspace_write_no_network(&workspace),
    }, b"input").unwrap();
    fs::remove_dir_all(workspace).unwrap();
    assert_eq!(output.process.exit_code, Some(0));
    assert!(output.process.stdout.len() <= 65536);
    assert!(output.process.stderr.len() <= 65536);
    assert_eq!(output.process.stdout_decode.raw_byte_length, 200000);
    assert_eq!(output.process.stderr_decode.raw_byte_length, 200000);
}

#[test]
#[cfg(target_os = "linux")]
fn owned_process_drop_stops_detached_descendants_and_denies_private_files() {
    let root = std::env::temp_dir().join(format!(
        "centaeris-owned-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(root.join("private"), "SECRET").unwrap();
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .unwrap();
    let child = runner.spawn_owned_process(
        "python3".into(), vec!["-c".into(),
        "import os,time,pathlib\ntry:\n pathlib.Path('../private').read_text(); pathlib.Path('leaked').touch()\nexcept PermissionError: pass\nif os.fork()==0:\n os.setsid(); pathlib.Path('ready').touch(); time.sleep(1); pathlib.Path('escaped').touch()\nelse: time.sleep(20)".into()],
        workspace.clone(), Default::default(), ExecutionPolicy::workspace_write_public_internet(&workspace)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !workspace.join("ready").exists() && Instant::now() < deadline {
        sleep(Duration::from_millis(10));
    }
    let ready = workspace.join("ready").exists();
    drop(child);
    assert!(ready, "owned process must run before testing cleanup");
    sleep(Duration::from_millis(1100));
    assert!(!workspace.join("leaked").exists());
    assert!(!workspace.join("escaped").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stdin_commands_share_the_filesystem_boundary_and_preserve_process_facts() {
    let root = std::env::temp_dir().join(format!(
        "centaeris-hook-input-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(root.join("private"), "SECRET").unwrap();
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .unwrap();
    let output = runner.run_command_with_stdin(ExecutionCommandRequest {
        program: "python3".to_string(),
        args: vec!["-c".to_string(), "import json,sys,pathlib\nevent=json.load(sys.stdin)\nprint(event['message'])\ntry: pathlib.Path('../private').read_text()\nexcept PermissionError: print('DENIED',file=sys.stderr); sys.exit(7)\nsys.exit(77)".to_string()],
        cwd: workspace.clone(), env: std::collections::HashMap::new(), timeout_ms: 5000,
        policy: ExecutionPolicy::workspace_write_no_network(&workspace),
    }, br#"{"message":"hook input"}"#).unwrap();
    assert_eq!(output.process.exit_code, Some(7));
    assert_eq!(output.process.stdout, "hook input\n");
    assert_eq!(output.process.stderr, "DENIED\n");
    assert_eq!(fs::read_to_string(root.join("private")).unwrap(), "SECRET");
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(not(target_os = "windows"))]
fn production_runtime_helper_enforces_platform_sandbox_and_lifecycle() {
    let root = std::env::temp_dir().join(format!(
        "centaeris-runtime-platform-sandbox-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    let workspace = root.join("workspace");
    let protected = root.join("protected-inputs");
    fs::create_dir_all(&workspace).expect("create workspace directory");
    fs::create_dir_all(&protected).expect("create protected directory");
    fs::write(protected.join("secret.txt"), "SECRET").expect("write protected fixture");
    let outside = root.join("outside.txt");
    let allowed = workspace.join("allowed.txt");
    let background = workspace.join("background.txt");
    let background_escape = root.join("background-escape.txt");
    let mut command = "(sleep 0.25; printf ESCAPED > ../background-escape.txt 2>/dev/null || true) & (sleep 0.5; printf BACKGROUND > background.txt) & printf ALLOWED > allowed.txt; printf OUTSIDE > ../outside.txt 2>/dev/null || true; whoami; cat ../protected-inputs/secret.txt; if printf TAMPERED > ../protected-inputs/secret.txt 2>/dev/null; then exit 76; fi; if printf TAMPERED > ../protected-inputs/blocked.txt 2>/dev/null; then exit 71; fi".to_string();
    command.push_str("; if exec 3<>/dev/tcp/1.1.1.1/53; then exit 72; fi");
    let capture_root = root.join("agent-tool-results").join("banana");
    let spill_path = capture_root.join("tool-result.log");
    let mut policy = ExecutionPolicy::workspace_write_no_network(workspace.clone());
    policy.filesystem.read_only_roots.push(protected.clone());
    policy.filesystem.tmp_root = Some(capture_root.clone());
    policy.filesystem.read_only_roots.push(capture_root.clone());
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .expect("create local runner with production Runtime helper");
    let output = runner
        .run_host_command(
            None,
            ExecutionCommandRequest {
                program: "bash".to_string(),
                args: vec!["-c".to_string(), command],
                cwd: workspace.clone(),
                env: std::collections::HashMap::new(),
                timeout_ms: 10_000,
                policy: policy.clone(),
            },
            None,
        )
        .expect("run platform sandbox");

    assert_eq!(output.process.exit_code, Some(0), "{:#?}", output.process);
    assert!(output.process.attempt.policy.enforced);
    assert_eq!(fs::read_to_string(&allowed).unwrap(), "ALLOWED");
    assert!(output.process.stdout.contains("SECRET"));
    assert_eq!(
        fs::read_to_string(protected.join("secret.txt")).unwrap(),
        "SECRET"
    );
    assert!(!protected.join("blocked.txt").exists());
    assert!(!outside.exists());
    runner
        .run_file_system_operation(ExecutionFileSystemRequest {
            operation_id: None,
            cwd: workspace.clone(),
            policy: policy.clone(),
            model_path: "allowed.txt".to_string(),
            operation: ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
        })
        .expect("production filesystem helper reads an allowed file");
    let mut spill_write_policy = policy.clone();
    spill_write_policy
        .filesystem
        .writable_roots
        .push(capture_root.clone());
    runner
        .run_file_system_operation(ExecutionFileSystemRequest {
            operation_id: None,
            cwd: workspace.clone(),
            policy: spill_write_policy,
            model_path: spill_path.to_string_lossy().to_string(),
            operation: ExecutionFileSystemOperation::WriteFile {
                content: b"IMMUTABLE".to_vec(),
                expected_file_hash: None,
                create_only: true,
            },
        })
        .expect("spill writer receives a temporary exact write grant");
    let published_spill = runner
        .run_file_system_operation(ExecutionFileSystemRequest {
            operation_id: None,
            cwd: workspace.clone(),
            policy: policy.clone(),
            model_path: spill_path.to_string_lossy().to_string(),
            operation: ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
        })
        .expect("published spill remains readable");
    let ExecutionFileSystemOutput::ReadFile(published_spill) = published_spill else {
        panic!("published spill read returned the wrong filesystem output")
    };
    let immutable_error = runner
        .run_file_system_operation(ExecutionFileSystemRequest {
            operation_id: None,
            cwd: workspace.clone(),
            policy: policy.clone(),
            model_path: spill_path.to_string_lossy().to_string(),
            operation: ExecutionFileSystemOperation::WriteFile {
                content: b"TAMPERED".to_vec(),
                expected_file_hash: Some(published_spill.file_hash),
                create_only: false,
            },
        })
        .expect_err("published spill must reject filesystem mutation");
    assert_eq!(
        immutable_error.kind,
        ExecutionFileSystemErrorKind::PermissionDenied
    );
    let spill_bash = runner
        .run_host_command(
            None,
            ExecutionCommandRequest {
                program: "bash".to_string(),
                args: vec![
                    "-c".to_string(),
                    "spill_root=\"$1\"; cat \"$spill_root/tool-result.log\"; if printf TAMPERED >> \"$spill_root/tool-result.log\" 2>/dev/null; then exit 75; fi".to_string(),
                    "centaeris-spill-check".to_string(),
                    capture_root.to_string_lossy().to_string(),
                ],
                cwd: workspace.clone(),
                env: std::collections::HashMap::new(),
                timeout_ms: 10_000,
                policy: policy.clone(),
            },
            None,
        )
        .expect("read published spill from sandboxed Bash");
    assert_eq!(spill_bash.process.exit_code, Some(0));
    assert_eq!(spill_bash.process.stdout, "IMMUTABLE");
    let deadline = Instant::now() + Duration::from_secs(3);
    while fs::read_to_string(&background).ok().as_deref() != Some("BACKGROUND")
        && Instant::now() < deadline
    {
        sleep(Duration::from_millis(25));
    }
    assert_eq!(
        fs::read_to_string(&background).expect("background sandbox output"),
        "BACKGROUND"
    );
    assert!(!background_escape.exists());

    let timed_marker = workspace.join("timed-marker.txt");
    let timed_output = runner
        .run_host_command(
            None,
            ExecutionCommandRequest {
                program: "bash".to_string(),
                args: vec![
                    "-c".to_string(),
                    "(sleep 1; printf ESCAPED > timed-marker.txt) & wait".to_string(),
                ],
                cwd: workspace.clone(),
                env: std::collections::HashMap::new(),
                timeout_ms: 250,
                policy: policy.clone(),
            },
            None,
        )
        .expect("time out platform sandbox");
    assert!(timed_output.process.timed_out);
    sleep(Duration::from_millis(1_100));
    assert!(!timed_marker.exists());

    let cancelled_marker = workspace.join("cancelled-marker.txt");
    let cancellation_probe = || Ok(Some("user_interrupt".to_string()));
    let cancellation = runner
        .run_host_command(
            None,
            ExecutionCommandRequest {
                program: "bash".to_string(),
                args: vec![
                    "-c".to_string(),
                    "(sleep 1; printf ESCAPED > cancelled-marker.txt) & wait".to_string(),
                ],
                cwd: workspace.clone(),
                env: std::collections::HashMap::new(),
                timeout_ms: 10_000,
                policy,
            },
            Some(&cancellation_probe),
        )
        .expect_err("cancel platform sandbox");
    assert!(cancellation.is_cancellation_indeterminate());
    sleep(Duration::from_millis(1_100));
    assert!(!cancelled_marker.exists());

    let cleanup_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match fs::remove_dir_all(&root) {
            Ok(()) => break,
            Err(_) if Instant::now() < cleanup_deadline => sleep(Duration::from_millis(25)),
            Err(error) => panic!("remove platform sandbox fixture: {error}"),
        }
    }
}

#[test]
#[cfg(target_os = "windows")]
fn native_windows_execution_fails_without_running_bash() {
    let workspace = std::env::temp_dir().join(format!(
        "centaeris-runtime-windows-host-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir(&workspace).expect("create Windows host workspace");
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .expect("create Windows host runner");
    let status = runner
        .status(&ExecutionPolicy::workspace_write_public_internet(
            workspace.clone(),
        ))
        .expect_err("native Windows execution is unavailable");
    assert!(status.internal_debug_message().contains("WSL2"));

    let marker = workspace.join("host-process-ran.txt");
    let output = runner
        .run_host_command(
            None,
            ExecutionCommandRequest {
                program: "bash".to_string(),
                args: vec![
                    "-c".to_string(),
                    "printf RAN > host-process-ran.txt".to_string(),
                ],
                cwd: workspace.clone(),
                env: std::collections::HashMap::new(),
                timeout_ms: 10_000,
                policy: ExecutionPolicy::workspace_write_public_internet(workspace.clone()),
            },
            None,
        )
        .expect_err("must not execute Git Bash");
    assert!(output.internal_debug_message().contains("WSL2"));
    assert!(!marker.exists());
    let error = runner
        .run_file_system_operation(centaeris_core::execution::ExecutionFileSystemRequest {
            operation_id: None,
            cwd: workspace.clone(),
            policy: ExecutionPolicy::workspace_write_public_internet(workspace.clone()),
            model_path: "host-process-ran.txt".into(),
            operation: centaeris_core::execution::ExecutionFileSystemOperation::WriteFile {
                content: b"RAN".to_vec(),
                expected_file_hash: None,
                create_only: true,
            },
        })
        .expect_err("native Windows filesystem fallback must be unavailable");
    assert_eq!(
        error.kind,
        centaeris_core::execution::ExecutionFileSystemErrorKind::HostUnavailable
    );
    assert!(!marker.exists());

    fs::remove_dir_all(workspace).expect("remove Windows host workspace");
}

#[test]
#[cfg(windows)]
fn native_windows_runtime_requires_wsl() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_centaeris-runtime"))
        .arg("--runtime-server-endpoint")
        .output()
        .expect("start native Runtime");
    assert!(
        !output.status.success(),
        "native Windows execution must be unavailable"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("WSL2"));
}

// Capability probing launches the production binary, not the Rust test harness.
#[test]
#[cfg(unix)]
fn desktop_execution_host_reports_its_platform_capability() {
    let runner = centaeris_runtime::local_execution_host::LocalExecutionHostRunner::new_with_runtime_executable(
        None, std::path::PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
        .expect("local execution host");
    let status = runner
        .status(
            &centaeris_core::execution::ExecutionPolicy::workspace_write_public_internet(
                std::env::current_dir().expect("current directory"),
            ),
        )
        .expect("local status");
    #[cfg(not(target_os = "windows"))]
    assert_eq!(
        status.kind,
        centaeris_core::execution::ExecutionHostKind::SandboxedProcess
    );
    #[cfg(target_os = "linux")]
    assert!(status.policy_enforced);
    #[cfg(target_os = "macos")]
    assert!(status.policy_enforced);
}
