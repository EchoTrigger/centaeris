use std::fs;
#[cfg(not(target_os = "windows"))]
use std::path::PathBuf;
#[cfg(not(target_os = "windows"))]
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::execution::ExecutionHostRunner;
use centaeris_core::execution::{ExecutionCommandRequest, ExecutionPolicy};
#[cfg(not(target_os = "windows"))]
use centaeris_core::execution::{
    ExecutionFileSystemOperation, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
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
        policy: ExecutionPolicy::workspace_write_public_internet(&workspace),
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
        policy: ExecutionPolicy::workspace_write_public_internet(&workspace),
    }, b"input").unwrap();
    fs::remove_dir_all(workspace).unwrap();
    assert_eq!(output.process.exit_code, Some(0));
    assert!(output.process.stdout.len() <= 65536);
    assert!(output.process.stderr.len() <= 65536);
    assert_eq!(output.process.stdout_decode.raw_byte_length, 200000);
    assert_eq!(output.process.stderr_decode.raw_byte_length, 200000);
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stdin_commands_preserve_process_facts() {
    let workspace =
        std::env::temp_dir().join(format!("centaeris-hook-input-{}", std::process::id()));
    fs::create_dir_all(&workspace).unwrap();
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .unwrap();
    let output = runner.run_command_with_stdin(ExecutionCommandRequest {
        program: "python3".to_string(),
        args: vec!["-c".to_string(), "import json,sys\nevent=json.load(sys.stdin)\nprint(event['message'])\nsys.stderr.write('stderr-line')".to_string()],
        cwd: workspace.clone(), env: std::collections::HashMap::new(), timeout_ms: 5000,
        policy: ExecutionPolicy::workspace_write_public_internet(&workspace),
    }, br#"{"message":"hook input"}"#).unwrap();
    assert_eq!(output.process.exit_code, Some(0));
    assert_eq!(output.process.stdout, "hook input\n");
    assert_eq!(output.process.stderr, "stderr-line");
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn unix_local_host_executes_commands_and_filesystem_operations() {
    let workspace = std::env::temp_dir().join(format!(
        "centaeris-native-unix-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos()
    ));
    fs::create_dir_all(&workspace).expect("create workspace directory");
    let policy = ExecutionPolicy::workspace_write_public_internet(workspace.clone());
    let runner = LocalExecutionHostRunner::new_with_runtime_executable(
        None,
        PathBuf::from(env!("CARGO_BIN_EXE_centaeris-runtime")),
    )
    .expect("create local runner");

    let status = runner.status(&policy).expect("native unix status is ready");
    assert_eq!(
        status.kind,
        centaeris_core::execution::ExecutionHostKind::LocalProcess
    );
    assert!(
        !status.policy_enforced,
        "a native host process is not an OS sandbox"
    );

    let output = runner
        .run_host_command(
            None,
            ExecutionCommandRequest {
                program: "bash".to_string(),
                args: vec!["-c".to_string(), "printf ALLOWED > allowed.txt".to_string()],
                cwd: workspace.clone(),
                env: std::collections::HashMap::new(),
                timeout_ms: 10_000,
                policy: policy.clone(),
            },
            None,
        )
        .expect("run native command");
    assert_eq!(output.process.exit_code, Some(0), "{:#?}", output.process);
    assert_eq!(
        fs::read_to_string(workspace.join("allowed.txt")).unwrap(),
        "ALLOWED"
    );

    let read = runner
        .run_file_system_operation(ExecutionFileSystemRequest {
            operation_id: None,
            cwd: workspace.clone(),
            policy,
            model_path: "allowed.txt".to_string(),
            operation: ExecutionFileSystemOperation::ReadFile { max_bytes: 1024 },
        })
        .expect("read allowed file");
    let ExecutionFileSystemOutput::ReadFile(read) = read else {
        panic!("read returned the wrong filesystem output")
    };
    assert_eq!(read.bytes, b"ALLOWED");

    fs::remove_dir_all(workspace).expect("remove workspace directory");
}

#[test]
#[cfg(target_os = "windows")]
fn native_windows_executes_commands_through_git_bash() {
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
    let policy = ExecutionPolicy::workspace_write_public_internet(workspace.clone());
    let status = runner
        .status(&policy)
        .expect("native Windows status is ready");
    assert_eq!(
        status.kind,
        centaeris_core::execution::ExecutionHostKind::LocalProcess
    );
    assert!(
        !status.policy_enforced,
        "a Git Bash host process is not an OS sandbox"
    );

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
                policy: policy.clone(),
            },
            None,
        )
        .expect("execute through Git Bash");
    assert_eq!(output.process.exit_code, Some(0));
    assert_eq!(output.process.stdout, "");
    assert!(marker.exists(), "Git Bash must create the workspace file");

    let _written = runner
        .run_file_system_operation(centaeris_core::execution::ExecutionFileSystemRequest {
            operation_id: None,
            cwd: workspace.clone(),
            policy: policy.clone(),
            model_path: "native-write.txt".into(),
            operation: centaeris_core::execution::ExecutionFileSystemOperation::WriteFile {
                content: b"RAN".to_vec(),
                observed_file_hash: None,
                create_only: true,
            },
        })
        .expect("native Windows filesystem write");
    assert!(workspace.join("native-write.txt").exists());

    fs::remove_dir_all(workspace).expect("remove Windows host workspace");
}

#[test]
#[cfg(windows)]
fn native_windows_runtime_no_longer_requires_wsl() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_centaeris-runtime"))
        .arg("--centaeris-unknown-startup-option")
        .output()
        .expect("start native Runtime");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("WSL2"),
        "native Windows Runtime must not require WSL2: {stderr}"
    );
    assert!(
        stderr.contains("unknown Runtime Host startup option"),
        "expected unknown-option startup diagnostics, got: {stderr}"
    );
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
    assert_eq!(
        status.kind,
        centaeris_core::execution::ExecutionHostKind::LocalProcess
    );
    assert!(
        !status.policy_enforced,
        "a native host process is not an OS sandbox"
    );
}
