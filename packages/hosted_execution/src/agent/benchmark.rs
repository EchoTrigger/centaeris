//! Filesystem and process maintenance in an explicitly authorized benchmark host.
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

pub(super) fn keeper() -> Result<(), String> {
    loop {
        #[cfg(target_os = "linux")]
        // PID 1 owns adopted children, including double-forked commands.
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[cfg(target_os = "linux")]
fn quiesce() -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, Instant};
    let rejected = || "benchmark quiescence requires its keeper PID namespace".to_string();
    let own = std::fs::metadata("/proc/self/exe").map_err(|_| rejected())?;
    let init = std::fs::metadata("/proc/1/exe").map_err(|_| rejected())?;
    let command = std::fs::read("/proc/1/cmdline").map_err(|_| rejected())?;
    let status = std::fs::read_to_string("/proc/1/status").map_err(|_| rejected())?;
    let keeper_uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|uids| uids.split_whitespace().nth(1))
        .and_then(|uid| uid.parse::<u32>().ok());
    let args: Vec<_> = command
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .collect();
    if std::process::id() <= 1
        || keeper_uid != Some(unsafe { libc::geteuid() })
        || (own.dev(), own.ino()) != (init.dev(), init.ino())
        || args.len() != 2
        || args[1] != b"benchmark-keeper"
    {
        return Err(rejected());
    }
    // This namespace belongs to one disposable Run. Before verifier admission,
    // every process except PID 1 and this maintenance helper belongs to that Run.
    // Enumerating the namespace also catches setsid/double-fork descendants.
    for (signal, budget) in [(libc::SIGTERM, 300), (libc::SIGKILL, 1_000)] {
        let deadline = Instant::now() + Duration::from_millis(budget);
        loop {
            let processes = running_processes()?;
            if processes.is_empty() {
                return Ok(());
            }
            for pid in processes {
                if unsafe { libc::kill(pid, signal) } != 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    return Err("benchmark process signal unconfirmed".into());
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Err("benchmark process quiescence unconfirmed".into())
}

#[cfg(target_os = "linux")]
fn running_processes() -> Result<Vec<i32>, String> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir("/proc").map_err(|_| "benchmark process scan unavailable")? {
        let entry = entry.map_err(|_| "benchmark process scan unavailable")?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if pid <= 1 || pid == std::process::id() as i32 {
            continue;
        }
        let status = match std::fs::read_to_string(entry.path().join("status")) {
            Ok(status) => status,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("benchmark process status unavailable".into()),
        };
        if !status.lines().any(|line| {
            line.strip_prefix("State:")
                .is_some_and(|state| state.trim_start().starts_with(['Z', 'X']))
        }) {
            result.push(pid);
        }
    }
    Ok(result)
}

pub(super) fn quiesce_once() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    quiesce()?;
    #[cfg(not(target_os = "linux"))]
    {
        Err("benchmark process quiescence requires Linux".into())
    }
    #[cfg(target_os = "linux")]
    serde_json::to_writer(
        std::io::stdout(),
        &serde_json::json!({"schema":"runtime.benchmark.quiescence.v1","quiescent":true}),
    )
    .map_err(|_| "write benchmark quiescence receipt failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::{
        ExecutionFileSystemOperation, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
        ExecutionPolicy,
    };
    use std::fs;

    #[cfg(target_os = "linux")]
    #[test]
    fn process_quiescence_refuses_the_host_or_an_unrelated_pid_namespace() {
        assert_eq!(
            quiesce().unwrap_err(),
            "benchmark quiescence requires its keeper PID namespace"
        );
    }

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
