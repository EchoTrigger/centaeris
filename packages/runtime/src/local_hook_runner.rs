use centaeris_core::execution::ExecutionCommandRequest;
use centaeris_core::extension::hooks::{
    LifecycleHookCommandResultV1, LifecycleHookEventV1, LifecycleHookHandlerV1, LifecycleHookRunner,
};
use centaeris_runtime::local_execution_host::LocalExecutionHostRunner;
use std::collections::HashMap;
use std::path::PathBuf;

pub(crate) struct LocalHookRunner {
    pub(crate) environment: HashMap<String, String>,
    pub(crate) readonly_roots: Vec<PathBuf>,
}

impl LifecycleHookRunner for LocalHookRunner {
    fn run_hook(
        &self,
        handler: &LifecycleHookHandlerV1,
        event: &LifecycleHookEventV1,
    ) -> LifecycleHookCommandResultV1 {
        let run = || -> Result<_, String> {
            let workspace = event
                .cwd
                .as_deref()
                .ok_or("local hook requires a workspace")?;
            let workspace = PathBuf::from(workspace)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let cwd = handler
                .cwd
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.clone())
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if !cwd.starts_with(&workspace)
                && !self
                    .readonly_roots
                    .iter()
                    .any(|root| root.canonicalize().is_ok_and(|root| cwd.starts_with(root)))
            {
                return Err("hook working directory is outside its activated resources".to_string());
            }
            let mut policy =
                crate::local_execution_policy::for_workspace(&workspace, &self.readonly_roots);
            policy.filesystem.workspace_root = cwd.clone();
            let mut input = serde_json::to_vec(event).map_err(|e| e.to_string())?;
            input.push(b'\n');
            let runner =
                LocalExecutionHostRunner::new(None).map_err(|e| e.internal_debug_message())?;
            runner
                .run_command_with_stdin(
                    ExecutionCommandRequest {
                        program: handler.program.clone(),
                        args: handler.args.clone(),
                        cwd,
                        env: self.environment.clone(),
                        timeout_ms: handler.timeout_ms,
                        policy,
                    },
                    &input,
                )
                .map_err(|e| e.internal_debug_message())
        };
        match run() {
            Ok(output) => {
                let mut process = output.process;
                let stdout_truncated = bound_hook_stream(&mut process.stdout)
                    || process.stdout_decode.raw_byte_length > 64 * 1024;
                let stderr_truncated = bound_hook_stream(&mut process.stderr)
                    || process.stderr_decode.raw_byte_length > 64 * 1024;
                LifecycleHookCommandResultV1 {
                    exit_code: process.exit_code,
                    stdout: process.stdout,
                    stderr: process.stderr,
                    stdout_truncated,
                    stderr_truncated,
                    timed_out: process.timed_out,
                    spawn_error: None,
                }
            }
            Err(error) => LifecycleHookCommandResultV1 {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                spawn_error: Some(error),
            },
        }
    }
}

fn bound_hook_stream(value: &mut String) -> bool {
    let mut boundary = 64 * 1024;
    if value.len() <= boundary {
        return false;
    }
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    true
}
