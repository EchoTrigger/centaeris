//! Shared local product services for Desktop and CLI hosts.

mod agent_runs;
mod agent_runtime;
mod atomic_file;
mod commands;
mod errors;
mod handlers;
mod host_automation;
mod host_protocol;
mod http_transport;
mod local_attachments;
mod local_execution_policy;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod local_hook_runner;
mod local_host;
mod local_work_store;
mod mcp;
mod message_log;
mod model_budget;
mod operation_receipts;
mod plugins;
mod process_agent;
mod process_sessions;
mod processes;
mod protocol;
mod runtime_bridge;
mod runtime_command_registry;
mod runtime_config;
mod runtime_garbage;
mod runtime_ops;
mod runtime_rpc;
mod runtime_rpc_transport;
mod runtime_server;
mod runtime_server_transport;
#[cfg(windows)]
mod runtime_server_windows_security;
mod schedules;
mod session_api;
mod session_catalog;
mod session_files;
mod sessions;
mod sidecars;
mod skills;
mod source_reconstruction;
mod terminal_sessions;
use centaeris_runtime_sqlite as sqlite_store;
#[cfg(feature = "native-job-store")]
mod subagent_delivery;
mod subagent_scheduler;
mod system_skills_deployment;
mod transcript_runtime;
mod user_config;
mod user_data_layout;
mod workspace_git;
mod workspaces;

use centaeris_agent::{local_execution_host, openssh};
pub mod rpc_client;
pub use local_host::LocalAgentHost;
pub use rpc_client::run_rpc_stdio;

/// Dispatches the execution helper before an entry point opens its profile.
/// The entry point exits with the returned code after a helper invocation.
pub fn dispatch_execution_helper() -> Result<Option<i32>, errors::RuntimeHostError> {
    #[cfg(windows)]
    {
        let arguments = std::env::args().skip(1).collect::<Vec<_>>();
        if arguments == ["--windows-host-launcher"] {
            return local_execution_host::run_windows_host_launcher()
                .map(Some)
                .map_err(|error| {
                    errors::RuntimeHostError::new("windows_host_launcher_failed", error)
                });
        }
    }
    Ok(None)
}

pub fn run_host() -> Result<(), errors::RuntimeHostError> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("centaeris-runtime")
        .build()
        .map_err(|error| {
            errors::RuntimeHostError::new("tokio_runtime_failed", error.to_string())
        })?;
    if arguments == ["--runtime-server-endpoint"] {
        return runtime_server_transport::print_endpoint();
    }
    if arguments == ["--runtime-server"] {
        let result = runtime.block_on(runtime_server_transport::run_server());
        // Host/tool blocking tasks may outlive cooperative cancellation. Do not
        // let Runtime::drop turn the bounded server drain into an unbounded wait.
        runtime.shutdown_timeout(std::time::Duration::from_millis(100));
        return result;
    }
    if !arguments.is_empty() {
        return Err(errors::RuntimeHostError::new(
            "unknown_startup_option",
            format!(
                "unknown Runtime Host startup option: {}",
                arguments.join(" ")
            ),
        ));
    }
    Err(errors::RuntimeHostError::new(
        "runtime_server_mode_required",
        "start with --runtime-server or --runtime-server-endpoint",
    ))
}
