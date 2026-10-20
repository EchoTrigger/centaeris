//! Native host control protocol. No control path or OCI security setting comes
//! from a task request; the host attests an immutable Docker/runc target first.
use centaeris_core::execution::{EnvironmentTaskEnvelope, EnvironmentTaskResponse};
use serde::{Deserialize, Serialize};
mod helper;
pub use helper::{HostHelperRequest, TrustedHelperKind, HELPER_STREAM_LIMIT};

pub const HOST_SUPERVISOR_SCHEMA: &str = "environment.host.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HostOciRuntime {
    Runc,
    Runsc,
}

impl HostOciRuntime {
    pub fn name(self) -> &'static str {
        match self {
            Self::Runc => "runc",
            Self::Runsc => "runsc",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostSupervisorConfig {
    pub schema: String,
    pub socket_path: String,
    pub control_root: String,
    pub output_root: String,
    pub oci_runtime: HostOciRuntime,
    pub runtime_path: String,
    pub runtime_root: String,
    pub docker_path: String,
    pub docker_socket: String,
    pub controller_uid: u32,
    pub helper_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostLaunchTarget {
    pub environment_instance_id: String,
    pub container_id: String,
    pub environment_id: String,
    pub profile_digest: String,
    pub image_digest: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostSupervisorEnvelope {
    pub schema: String,
    pub request: HostSupervisorRequest,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HostSupervisorRequest {
    Register {
        target: HostLaunchTarget,
    },
    Retire {
        target: HostLaunchTarget,
    },
    Task {
        envelope: EnvironmentTaskEnvelope,
    },
    Inspect {
        environment_instance_id: String,
    },
    Helper {
        helper: HostHelperRequest,
    },
    HelperOutput {
        helper: HostHelperRequest,
        task_id: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HostSupervisorResponse {
    Success {
        schema: String,
        result: HostSupervisorResult,
    },
    Error {
        schema: String,
        error: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HostSupervisorResult {
    Registered {
        target: HostLaunchTarget,
    },
    Retired {
        target: HostLaunchTarget,
    },
    Task {
        response: EnvironmentTaskResponse,
    },
    Inspection {
        inspection: serde_json::Value,
    },
    HelperFinished {
        task: centaeris_core::execution::EnvironmentTaskSnapshot,
        stdout_length: u64,
        stderr_length: u64,
    },
}

impl HostSupervisorConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != HOST_SUPERVISOR_SCHEMA {
            return Err("host_schema_unsupported".into());
        }
        if self.controller_uid != 0 {
            return Err("host_controller_uid_unsupported".into());
        }
        for path in [
            &self.socket_path,
            &self.control_root,
            &self.output_root,
            &self.runtime_path,
            &self.runtime_root,
            &self.docker_path,
            &self.docker_socket,
        ] {
            if !absolute_path(path) || path == "/" {
                return Err("host_configuration_invalid".into());
            }
        }
        let paths = [&self.control_root, &self.output_root, &self.runtime_root];
        for (i, a) in paths.iter().enumerate() {
            if paths.iter().skip(i + 1).any(|b| overlaps(a, b)) || overlaps(a, &self.socket_path) {
                return Err("host_control_paths_overlap".into());
            }
        }
        if self
            .helper_path
            .as_ref()
            .is_some_and(|path| !absolute_path(path) || path == "/")
        {
            return Err("host_helper_configuration_invalid".into());
        }
        if self.oci_runtime == HostOciRuntime::Runsc && self.helper_path.is_none() {
            return Err("host_runsc_runner_required".into());
        }
        Ok(())
    }
}
impl HostLaunchTarget {
    pub fn validate(&self) -> Result<(), String> {
        if !self
            .environment_instance_id
            .strip_prefix("instance_")
            .is_some_and(hex_id)
            || !hex_id(&self.container_id)
            || !digest_id(&self.profile_digest)
            || !digest_id(&self.image_digest)
            || self.environment_id.is_empty()
            || self.environment_id.len() > 256
            || self.environment_id.chars().any(char::is_control)
        {
            return Err("host_target_invalid".into());
        }
        Ok(())
    }
}
impl HostSupervisorEnvelope {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != HOST_SUPERVISOR_SCHEMA {
            return Err("host_schema_unsupported".into());
        }
        match &self.request {
            HostSupervisorRequest::Register { target }
            | HostSupervisorRequest::Retire { target } => target.validate(),
            HostSupervisorRequest::Task { envelope } => {
                envelope.validate()?;
                if !request_instance(&envelope.request)
                    .strip_prefix("instance_")
                    .is_some_and(hex_id)
                {
                    return Err("host_target_invalid".into());
                }
                Ok(())
            }
            HostSupervisorRequest::Helper { helper }
            | HostSupervisorRequest::HelperOutput { helper, .. } => {
                helper.validate()?;
                if !helper
                    .fence
                    .environment_instance_id
                    .strip_prefix("instance_")
                    .is_some_and(hex_id)
                {
                    return Err("host_target_invalid".into());
                }
                Ok(())
            }
            HostSupervisorRequest::Inspect {
                environment_instance_id,
            } => {
                if environment_instance_id
                    .strip_prefix("instance_")
                    .is_some_and(hex_id)
                {
                    Ok(())
                } else {
                    Err("host_target_invalid".into())
                }
            }
        }
    }
}
impl HostSupervisorResponse {
    pub fn validate(&self) -> Result<(), String> {
        let schema = match self {
            Self::Success { schema, .. } | Self::Error { schema, .. } => schema,
        };
        if schema != HOST_SUPERVISOR_SCHEMA {
            return Err("host_schema_unsupported".into());
        }
        match self {
            Self::Success {
                result:
                    HostSupervisorResult::Registered { target }
                    | HostSupervisorResult::Retired { target },
                ..
            } => target.validate(),
            Self::Success {
                result: HostSupervisorResult::Task { response },
                ..
            } => response.validate(),
            _ => Ok(()),
        }
    }
}

/// Build only the process portion. Linux security and namespaces stay in the
/// runc container configuration; process security fields are copied verbatim.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn task_process(
    baseline: &serde_json::Value,
    program: &str,
    args: &[String],
    cwd: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<serde_json::Value, String> {
    centaeris_core::execution::validate_environment_task_env(env)?;
    let valid_program = !program.is_empty()
        && program.len() <= 4096
        && !program.chars().any(char::is_control)
        && !program.contains('\\')
        && (absolute_path(program)
            || (!program.contains('/') && program != "." && program != ".."));
    if !valid_program
        || !absolute_path(cwd)
        || args.iter().any(|a| a.contains('\0'))
        || baseline["noNewPrivileges"] != true
        || baseline.get("terminal").is_some_and(|value| value != false)
        || !baseline["user"]["uid"].is_u64()
        || !baseline["user"]["gid"].is_u64()
        || !baseline["env"].is_array()
    {
        return Err("host_process_invalid".into());
    }
    let mut process = baseline.clone();
    let mut argv = vec![program.to_owned()];
    argv.extend_from_slice(args);
    process["args"] = serde_json::json!(argv);
    process["cwd"] = serde_json::json!(cwd);
    if !env.is_empty() {
        let mut merged = std::collections::BTreeMap::new();
        for value in baseline["env"].as_array().ok_or("host_process_invalid")? {
            let (key, value) = value
                .as_str()
                .and_then(|v| v.split_once('='))
                .filter(|(key, value)| {
                    !key.is_empty() && !key.contains('\0') && !value.contains('\0')
                })
                .ok_or("host_process_invalid")?;
            merged.insert(key.to_owned(), value.to_owned());
        }
        merged.extend(env.clone());
        process["env"] = serde_json::json!(merged
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>());
    }
    Ok(process)
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn workspace_context(baseline: &serde_json::Value) -> Result<(String, String), String> {
    let values = baseline["env"].as_array().ok_or("host_process_invalid")?;
    let one = |name: &str| -> Result<String, String> {
        let prefix = format!("{name}=");
        let mut found = values
            .iter()
            .filter_map(|value| value.as_str()?.strip_prefix(&prefix));
        let value = found.next().ok_or("host_workspace_context_invalid")?;
        if found.next().is_some() {
            return Err("host_workspace_context_invalid".into());
        }
        Ok(value.into())
    };
    let root = one("CENTAERIS_WORKSPACE_ROOT")?;
    let user = one("CENTAERIS_WORKSPACE_USER")?;
    let expected_user = format!(
        "{}:{}",
        baseline["user"]["uid"]
            .as_u64()
            .ok_or("host_process_invalid")?,
        baseline["user"]["gid"]
            .as_u64()
            .ok_or("host_process_invalid")?
    );
    if !absolute_path(&root)
        || root == "/run/centaeris"
        || root.starts_with("/run/centaeris/")
        || user != expected_user
    {
        return Err("host_workspace_context_invalid".into());
    }
    Ok((root, user))
}

pub(crate) fn request_instance(
    request: &centaeris_core::execution::EnvironmentTaskRequest,
) -> &str {
    use centaeris_core::execution::EnvironmentTaskRequest::*;
    match request {
        Takeover {
            environment_instance_id,
            ..
        } => environment_instance_id,
        Start { fence, .. }
        | Query { fence, .. }
        | List { fence, .. }
        | Input { fence, .. }
        | QueryInputReceipt { fence, .. }
        | Terminate { fence, .. }
        | EndRun { fence, .. }
        | EndOperation { fence, .. } => &fence.environment_instance_id,
    }
}

pub(crate) fn absolute_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.contains('\0')
        && !path.contains('\\')
        && (path == "/"
            || !path
                .split('/')
                .skip(1)
                .any(|p| p.is_empty() || p == "." || p == ".."))
}
pub(crate) fn overlaps(a: &str, b: &str) -> bool {
    a == b
        || a == "/"
        || b == "/"
        || a.strip_prefix(b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|s| s.starts_with('/'))
}
fn hex_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest_id(id: &str) -> bool {
    id.strip_prefix("sha256:").is_some_and(hex_id)
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn physical_snapshot(
    snapshot: &mut centaeris_core::execution::EnvironmentTaskSnapshot,
    leader: Option<Option<i32>>,
    output_eof: bool,
    domain_empty: bool,
) {
    if let Some(exit) = leader {
        if snapshot.state != centaeris_core::execution::EnvironmentTaskState::Unknown {
            snapshot.state = centaeris_core::execution::EnvironmentTaskState::Exited;
        }
        snapshot.exit_code = exit;
    }
    snapshot.output_complete = output_eof;
    snapshot.cleanup_complete = domain_empty && leader.is_some();
}

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "linux")]
pub use linux::serve;

#[cfg(test)]
mod tests;
