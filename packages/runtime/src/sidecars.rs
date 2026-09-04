use centaeris_core::runtime::contracts::current_timestamp_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child as OsChild, Command, ExitStatus, Stdio};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[derive(Default)]
pub(crate) struct SidecarStoreState {
    next_seq: u64,
    sidecars: HashMap<String, RuntimeHostState>,
}

struct SidecarRecord {
    sidecar_id: String,
    name: String,
    status: String,
    created_at_ms: i64,
    workspace_root: String,
    cwd: Option<String>,
    pid: Option<u32>,
    exit_code: Option<i32>,
}

struct RuntimeHostState {
    sidecar_record: SidecarRecord,
    child_process: Option<OsChild>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SidecarStartRequest {
    pub(crate) command: String,
    pub(crate) args: Option<Vec<String>>,
    pub(crate) workspace_root: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) env: Option<HashMap<String, String>>,
    pub(crate) name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SidecarStopRequest {
    pub(crate) sidecar_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SidecarStartResponse {
    pub(crate) sidecar_id: String,
    pub(crate) pid: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SidecarSimpleResponse {
    pub(crate) ok: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SidecarListResponse {
    pub(crate) sidecars: Vec<SidecarSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SidecarSummary {
    pub(crate) sidecar_id: String,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) created_at_ms: i64,
    pub(crate) workspace_root: String,
    pub(crate) cwd: Option<String>,
    pub(crate) pid: Option<u32>,
    pub(crate) exit_code: Option<i32>,
}

fn configure_sidecar_command(_command: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        _command.creation_flags(CREATE_NO_WINDOW);
    }
}

pub(crate) fn start(
    sidecar_store: &mut SidecarStoreState,
    request: SidecarStartRequest,
) -> Result<SidecarStartResponse, String> {
    let command_text = request.command.trim();
    if command_text.is_empty() {
        return Err(String::from("sidecar command is required"));
    }
    sidecar_store.next_seq += 1;
    let sidecar_id = format!("sidecar-{}", sidecar_store.next_seq);
    let created_at_ms = current_timestamp_ms();
    let args = request.args.clone().unwrap_or_default();
    let workspace_root = resolve_sidecar_workspace_root(&request)?;
    let cwd = resolve_sidecar_cwd(&request, workspace_root.as_path())?;
    let workspace_root_text = workspace_root.to_string_lossy().to_string();
    let cwd_text = Some(cwd.to_string_lossy().to_string());

    let mut command_builder = Command::new(command_text);
    configure_sidecar_command(&mut command_builder);
    command_builder.args(&args);
    command_builder.current_dir(cwd.as_path());
    if let Some(env_map) = request.env.clone() {
        for (key, value) in env_map {
            command_builder.env(key, value);
        }
    }
    command_builder.stdin(Stdio::null());
    command_builder.stdout(Stdio::null());
    command_builder.stderr(Stdio::null());

    let spawn_result = command_builder.spawn();
    let sidecar_name = request.name.unwrap_or_else(|| command_text.to_string());

    match spawn_result {
        Ok(child) => {
            let pid = Some(child.id());
            sidecar_store.sidecars.insert(
                sidecar_id.clone(),
                RuntimeHostState {
                    sidecar_record: SidecarRecord {
                        sidecar_id: sidecar_id.clone(),
                        name: sidecar_name,
                        status: String::from("running"),
                        created_at_ms,
                        workspace_root: workspace_root_text,
                        cwd: cwd_text,
                        pid,
                        exit_code: None,
                    },
                    child_process: Some(child),
                },
            );
            Ok(SidecarStartResponse { sidecar_id, pid })
        }
        Err(error) => {
            sidecar_store.sidecars.insert(
                sidecar_id.clone(),
                RuntimeHostState {
                    sidecar_record: SidecarRecord {
                        sidecar_id: sidecar_id.clone(),
                        name: sidecar_name,
                        status: String::from("failed"),
                        created_at_ms,
                        workspace_root: workspace_root_text,
                        cwd: cwd_text,
                        pid: None,
                        exit_code: None,
                    },
                    child_process: None,
                },
            );
            Err(format!("sidecar-spawn-failed: {error}"))
        }
    }
}

pub(crate) fn stop(
    sidecar_store: &mut SidecarStoreState,
    request: SidecarStopRequest,
) -> Result<SidecarSimpleResponse, String> {
    let Some(sidecar_state) = sidecar_store.sidecars.get_mut(&request.sidecar_id) else {
        return Ok(SidecarSimpleResponse { ok: false });
    };

    if let Some(mut child) = sidecar_state.child_process.take() {
        match child.try_wait() {
            Ok(Some(exit_status)) => {
                sidecar_state.sidecar_record.status = sidecar_status_from_exit(&exit_status);
                sidecar_state.sidecar_record.exit_code = Some(sidecar_exit_code(&exit_status));
                sidecar_state.sidecar_record.pid = None;
                return Ok(SidecarSimpleResponse { ok: true });
            }
            Ok(None) => {
                if let Err(error) = child.kill() {
                    sidecar_state.sidecar_record.status = String::from("failed");
                    sidecar_state.sidecar_record.exit_code = None;
                    sidecar_state.child_process = Some(child);
                    return Err(format!("sidecar-kill-failed: {error}"));
                }

                match child.wait() {
                    Ok(exit_status) => {
                        sidecar_state.sidecar_record.status =
                            sidecar_status_from_exit(&exit_status);
                        sidecar_state.sidecar_record.exit_code =
                            Some(sidecar_exit_code(&exit_status));
                        sidecar_state.sidecar_record.pid = None;
                        return Ok(SidecarSimpleResponse { ok: true });
                    }
                    Err(error) => {
                        sidecar_state.sidecar_record.status = String::from("failed");
                        sidecar_state.sidecar_record.exit_code = None;
                        return Err(format!("sidecar-wait-failed: {error}"));
                    }
                }
            }
            Err(error) => {
                sidecar_state.sidecar_record.status = String::from("failed");
                sidecar_state.sidecar_record.exit_code = None;
                sidecar_state.child_process = Some(child);
                return Err(format!("sidecar-try-wait-failed: {error}"));
            }
        }
    }

    sidecar_state.sidecar_record.status = String::from("stopped");
    sidecar_state.sidecar_record.pid = None;
    Ok(SidecarSimpleResponse { ok: true })
}

pub(crate) fn list(sidecar_store: &mut SidecarStoreState) -> SidecarListResponse {
    let mut sidecars = Vec::with_capacity(sidecar_store.sidecars.len());

    for sidecar_state in sidecar_store.sidecars.values_mut() {
        if let Some(mut child) = sidecar_state.child_process.take() {
            match child.try_wait() {
                Ok(Some(exit_status)) => {
                    sidecar_state.sidecar_record.status = sidecar_status_from_exit(&exit_status);
                    sidecar_state.sidecar_record.exit_code = Some(sidecar_exit_code(&exit_status));
                    sidecar_state.sidecar_record.pid = None;
                }
                Ok(None) => {
                    sidecar_state.sidecar_record.pid = Some(child.id());
                    sidecar_state.child_process = Some(child);
                }
                Err(_) => {
                    sidecar_state.sidecar_record.status = String::from("failed");
                    sidecar_state.sidecar_record.exit_code = None;
                    sidecar_state.sidecar_record.pid = None;
                }
            }
        }

        sidecars.push(SidecarSummary {
            sidecar_id: sidecar_state.sidecar_record.sidecar_id.clone(),
            name: sidecar_state.sidecar_record.name.clone(),
            status: sidecar_state.sidecar_record.status.clone(),
            created_at_ms: sidecar_state.sidecar_record.created_at_ms,
            workspace_root: sidecar_state.sidecar_record.workspace_root.clone(),
            cwd: sidecar_state.sidecar_record.cwd.clone(),
            pid: sidecar_state.sidecar_record.pid,
            exit_code: sidecar_state.sidecar_record.exit_code,
        });
    }

    SidecarListResponse { sidecars }
}

fn resolve_sidecar_workspace_root(request: &SidecarStartRequest) -> Result<PathBuf, String> {
    let root = request
        .workspace_root
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "sidecar workspaceRoot is required".to_string())?;
    fs::canonicalize(root).map_err(|error| format!("sidecar workspaceRoot invalid: {error}"))
}

fn resolve_sidecar_cwd(
    request: &SidecarStartRequest,
    workspace_root: &Path,
) -> Result<PathBuf, String> {
    let candidate = request
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.to_path_buf());
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        workspace_root.join(candidate)
    };
    let canonical = fs::canonicalize(resolved.as_path())
        .map_err(|error| format!("resolve sidecar cwd failed: {error}"))?;
    if !canonical.starts_with(workspace_root) {
        return Err(format!(
            "sidecar cwd outside workspace is not allowed: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

fn sidecar_exit_code(exit_status: &ExitStatus) -> i32 {
    exit_status.code().unwrap_or(-1)
}

fn sidecar_status_from_exit(exit_status: &ExitStatus) -> String {
    if sidecar_exit_code(exit_status) == 0 {
        String::from("stopped")
    } else {
        String::from("failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "centaeris-runtime-{name}-{}",
            current_timestamp_ms()
        ));
        fs::create_dir_all(path.as_path()).expect("create temp dir");
        path
    }

    #[test]
    fn resolve_sidecar_cwd_rejects_path_outside_workspace() {
        let workspace = unique_temp_dir("workspace");
        let outside = unique_temp_dir("outside");
        let request = SidecarStartRequest {
            command: String::from("cmd"),
            args: None,
            workspace_root: Some(workspace.to_string_lossy().to_string()),
            cwd: Some(outside.to_string_lossy().to_string()),
            env: None,
            name: None,
        };
        let workspace_root = fs::canonicalize(workspace.as_path()).expect("canonical workspace");

        let error = resolve_sidecar_cwd(&request, workspace_root.as_path()).expect_err("error");

        assert!(error.contains("sidecar cwd outside workspace is not allowed"));
        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn list_reports_empty_store() {
        let mut store = SidecarStoreState::default();

        assert!(list(&mut store).sidecars.is_empty());
    }
}
