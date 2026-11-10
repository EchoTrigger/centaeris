use centaeris_core::execution::{
    ExecutionFileSystemOperation, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
    ExecutionPolicy,
};
use hosted_execution::protocol::{read_filesystem_result, write_filesystem_request};
use std::{
    fs,
    process::{Command, Stdio},
};

fn execute(request: ExecutionFileSystemRequest) -> ExecutionFileSystemOutput {
    let mut child = Command::new(env!("CARGO_BIN_EXE_execution_agent"))
        .arg("filesystem-once")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    write_filesystem_request(&mut child.stdin.take().unwrap(), request).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    read_filesystem_result(&mut output.stdout.as_slice())
        .unwrap()
        .into_result()
        .unwrap()
}

#[test]
fn filesystem_binary_entrypoint_writes_and_reads_the_selected_workspace() {
    let root = std::env::temp_dir().join(format!(
        "centaeris-file-helper-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let bytes: Vec<u8> = (0..65537).map(|n| (n % 251) as u8).collect();
    let mut request = ExecutionFileSystemRequest {
        operation_id: None,
        cwd: root.clone(),
        policy: ExecutionPolicy::workspace_write_no_network(&root),
        model_path: "payload.bin".into(),
        operation: ExecutionFileSystemOperation::WriteFile {
            content: bytes.clone(),
            observed_file_hash: None,
            create_only: true,
        },
    };
    assert!(matches!(
        execute(request.clone()),
        ExecutionFileSystemOutput::WriteFile(_)
    ));
    request.operation = ExecutionFileSystemOperation::ReadFile {
        max_bytes: bytes.len(),
    };
    let ExecutionFileSystemOutput::ReadFile(read) = execute(request) else {
        panic!("read expected")
    };
    fs::remove_dir_all(root).unwrap();
    assert_eq!(read.bytes, bytes);
}
