//! One bounded filesystem request in an explicitly authorized benchmark host.
//! Host authorization and container selection stay outside the task container.

use centaeris_core::execution::{
    run_policy_scoped_execution_file_system_operation, ExecutionFileSystemError,
    ExecutionFileSystemOutput, ExecutionFileSystemRequest,
};

fn execute(
    request: ExecutionFileSystemRequest,
) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
    run_policy_scoped_execution_file_system_operation(request)
}

pub(super) fn filesystem_once() -> Result<(), String> {
    let request: ExecutionFileSystemRequest = super::read_json(super::MAX_JSON_BYTES)?;
    let result = super::SandboxFileSystemResult::from(execute(request));
    serde_json::to_writer(std::io::stdout(), &result)
        .map_err(|_| "write benchmark filesystem response failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::{
        ExecutionFileSystemOperation, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
        ExecutionPolicy,
    };
    use std::fs;

    #[test]
    fn filesystem_uses_original_task_directory_and_policy_without_relocating_files() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-benchmark-files-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cwd = root.join("original-task");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(cwd.join("input.txt"), b"original bytes").unwrap();
        let policy = ExecutionPolicy::workspace_write_no_network(&root);
        let mut request = ExecutionFileSystemRequest {
            operation_id: None,
            cwd: cwd.clone(),
            policy,
            model_path: "input.txt".into(),
            operation: ExecutionFileSystemOperation::ReadFile { max_bytes: 100 },
        };
        let ExecutionFileSystemOutput::ReadFile(read) = execute(request.clone()).unwrap() else {
            panic!("file read expected")
        };
        assert_eq!(read.bytes, b"original bytes");
        request.model_path = "../artifact.txt".into();
        request.operation = ExecutionFileSystemOperation::WriteFile {
            content: b"task output".to_vec(),
            observed_file_hash: None,
            create_only: true,
        };
        execute(request).unwrap();
        assert_eq!(fs::read(root.join("artifact.txt")).unwrap(), b"task output");
        fs::remove_dir_all(root).unwrap();
    }
}
