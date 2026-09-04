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

use centaeris_core::execution::sandbox::{SandboxPolicy, SandboxTransformRequest, SandboxType};
#[cfg(target_os = "windows")]
use centaeris_core::execution::ExecutionHostKind;
use centaeris_core::execution::ExecutionHostRunner;
#[cfg(not(target_os = "windows"))]
use centaeris_core::execution::{
    ExecutionFileSystemErrorKind, ExecutionFileSystemOperation, ExecutionFileSystemOutput,
    ExecutionFileSystemRequest,
};
use centaeris_runtime::local_execution_host::LocalExecutionHostRunner;

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
    let protected = workspace.join(".centaeris");
    fs::create_dir_all(&protected).expect("create protected directory");
    fs::write(protected.join("secret.txt"), "SECRET").expect("write protected fixture");
    let outside = root.join("outside.txt");
    let allowed = workspace.join("allowed.txt");
    let background = workspace.join("background.txt");
    let background_escape = root.join("background-escape.txt");
    // Linux may accept these writes inside its private /tmp; Host-side absence is the boundary.
    let mut command = "(sleep 0.25; printf ESCAPED > ../background-escape.txt 2>/dev/null || true) & (sleep 0.5; printf BACKGROUND > background.txt) & printf ALLOWED > allowed.txt; printf OUTSIDE > ../outside.txt 2>/dev/null || true; whoami; cat .centaeris/secret.txt; if printf TAMPERED > .centaeris/secret.txt 2>/dev/null; then exit 76; fi; if printf TAMPERED > .centaeris/blocked.txt 2>/dev/null; then exit 71; fi".to_string();
    command.push_str("; if exec 3<>/dev/tcp/1.1.1.1/53; then exit 72; fi");
    command.push_str("; if test -S /var/run/docker.sock; then exit 74; fi");
    let capture_root = root.join("agent-tool-results").join("banana");
    let spill_path = capture_root.join("tool-result.log");
    let mut policy = SandboxPolicy::workspace_write_no_network(workspace.clone());
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
            SandboxTransformRequest {
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
    #[cfg(target_os = "linux")]
    assert_eq!(
        output.process.attempt.sandbox_type,
        SandboxType::LinuxBubblewrap
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        output.process.attempt.sandbox_type,
        SandboxType::MacOsSeatbelt
    );
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
            SandboxTransformRequest {
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
            SandboxTransformRequest {
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
            SandboxTransformRequest {
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
fn windows_host_process_reports_unsandboxed_and_executes_bash() {
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
        .status(&SandboxPolicy::workspace_write_public_internet(
            workspace.clone(),
        ))
        .expect("Windows host status");
    assert_eq!(status.kind, ExecutionHostKind::LocalProcess);
    assert_eq!(status.sandbox_type, SandboxType::HostProcess);

    let marker = workspace.join("host-process-ran.txt");
    let output = runner
        .run_host_command(
            None,
            SandboxTransformRequest {
                program: "bash".to_string(),
                args: vec![
                    "-c".to_string(),
                    "printf RAN > host-process-ran.txt".to_string(),
                ],
                cwd: workspace.clone(),
                env: std::collections::HashMap::new(),
                timeout_ms: 10_000,
                policy: SandboxPolicy::workspace_write_public_internet(workspace.clone()),
            },
            None,
        )
        .expect("Windows HostProcess executes Git Bash without an OS sandbox");
    assert_eq!(output.process.exit_code, Some(0));
    assert_eq!(
        output.process.attempt.sandbox_type,
        SandboxType::HostProcess
    );
    assert!(!output.process.attempt.policy.enforced);
    assert_eq!(
        fs::read_to_string(marker).expect("read Windows host marker"),
        "RAN"
    );
    fs::remove_dir_all(workspace).expect("remove Windows host workspace");
}
