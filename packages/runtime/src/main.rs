mod agent_runs;
mod agent_runtime;
mod atomic_file;
mod commands;
mod desktop_file_preview;
mod errors;
mod handlers;
mod host_protocol;
mod http_transport;
mod local_attachments;
mod mcp;
mod message_log;
mod operation_receipts;
mod plugins;
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
mod session_files;
mod session_projection;
mod sessions;
mod sidecars;
mod skills;
use centaeris_runtime_sqlite as sqlite_store;
mod subagent_scheduler;
mod system_skills_deployment;
mod user_config;
mod user_data_layout;
mod workspace_git;
mod workspaces;

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    if arguments.first().map(String::as_str) == Some("--local-sandbox-supervisor") {
        match centaeris_runtime::local_execution_host::run_linux_supervisor(&arguments[1..]) {
            Ok(exit_code) => std::process::exit(exit_code),
            Err(error) => {
                eprintln!("centaeris sandbox supervisor failed: {error}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    if arguments == ["--local-sandbox-filesystem-helper"] {
        match centaeris_runtime::local_execution_host::run_file_system_helper() {
            Ok(()) => return,
            Err(error) => {
                eprintln!("centaeris local filesystem sandbox helper failed: {error}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(target_os = "windows")]
    if arguments == ["--windows-host-launcher"] {
        match centaeris_runtime::local_execution_host::run_windows_host_launcher() {
            Ok(exit_code) => std::process::exit(exit_code),
            Err(error) => {
                eprintln!("centaeris Windows host launcher failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if let Err(error) = run() {
        eprintln!("centaeris Runtime Host failed: {}", error);
        std::process::exit(1);
    }
}

fn run() -> Result<(), errors::RuntimeHostError> {
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
        return runtime.block_on(runtime_server_transport::run_server());
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
