fn main() {
    let result = match std::env::args().nth(1).as_deref() {
        Some("filesystem-once") => hosted_execution::agent::run_filesystem_once(),
        Some("input-inventory") => hosted_execution::agent::run_input_inventory_once(),
        Some("materialize-input") => hosted_execution::agent::run_materialize_input_once(),
        Some("revoke-input") => hosted_execution::agent::run_revoke_input_once(),
        Some("read-artifact") => hosted_execution::agent::run_read_artifact_once(),
        Some("snapshot-collect") => hosted_execution::agent::run_snapshot_collect_once(),
        Some("snapshot-restore") => hosted_execution::agent::run_snapshot_restore_once(),
        Some("workspace-watch") => hosted_execution::agent::run_workspace_watch(),
        Some("workspace-generation") => {
            hosted_execution::agent::run_workspace_generation_once()
        }
        Some("workspace-generation-rpc") => {
            hosted_execution::agent::run_workspace_generation_rpc()
        }
        Some("quiesce-agent-processes") => {
            hosted_execution::agent::run_quiesce_agent_processes_once()
        }
        _ => Err("usage: execution_agent <filesystem-once|input-inventory|materialize-input|revoke-input|read-artifact|snapshot-collect|snapshot-restore|workspace-watch|workspace-generation|workspace-generation-rpc|quiesce-agent-processes>".to_string()),
    };
    if let Err(error) = result {
        eprintln!("execution_agent failed: {error}");
        std::process::exit(1);
    }
}
