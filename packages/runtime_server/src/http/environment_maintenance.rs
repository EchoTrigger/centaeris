//! Independently authorized maintenance uses the ordinary environment task channel.
use super::*;
use crate::environment_authorization::maintenance::MaintenanceAuthorization;
use centaeris_core::execution::{
    EnvironmentTaskEnvelope, EnvironmentTaskFence, EnvironmentTaskOwner, EnvironmentTaskRequest,
    EnvironmentTaskResult, EnvironmentTaskScope,
};

/// The command projection of Core Start. Controller identity, scope and operation
/// receipts come from the authorized fence, never from this signed request body.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Command {
    program: String,
    args: Vec<String>,
    cwd: String,
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Dispatch {
    authorization: MaintenanceAuthorization,
    request: Command,
    task: EnvironmentTaskEnvelope,
}

pub(crate) fn body_authority<B>(request: &Request<B>) -> Result<(), (u16, &'static str)> {
    for name in ["x-internal-token", "x-environment-maintenance-token"] {
        if request.headers().get_all(name).iter().count() > 1 {
            return Err((400, "request_header_duplicate"));
        }
    }
    let internal = env::var("INTERNAL_API_TOKEN").ok();
    if internal.as_deref().is_none_or(|token| {
        token.is_empty()
            || request
                .headers()
                .get("x-internal-token")
                .and_then(|v| v.to_str().ok())
                != Some(token)
    }) {
        return Err((401, "unauthorized"));
    }
    if !super::environment_retirement::maintenance_authorized(
        env::var("ENVIRONMENT_MAINTENANCE_TOKEN").ok().as_deref(),
        internal.as_deref(),
        request
            .headers()
            .get("x-environment-maintenance-token")
            .and_then(|v| v.to_str().ok()),
    ) {
        return Err((403, "environment_maintenance_authority_required"));
    }
    Ok(())
}

impl Dispatch {
    fn validate(&self) -> Result<EnvironmentTaskFence, &'static str> {
        self.task
            .validate()
            .map_err(|_| "maintenance_task_invalid")?;
        self.task
            .request
            .validate_authority()
            .map_err(|_| "maintenance_task_invalid")?;
        let body = crate::agent_run_authorization::canonical_json(
            serde_json::to_value(&self.request).map_err(|_| "maintenance_request_invalid")?,
        )
        .map_err(|_| "maintenance_request_invalid")?;
        if body.len() > 64 * 1024
            || format!("sha256:{:x}", sha2::Sha256::digest(body.as_bytes()))
                != self.authorization.payload.request_digest
        {
            return Err("maintenance_request_binding_rejected");
        }
        let b = &self.authorization.payload;
        let EnvironmentTaskOwner::MaintenanceOperation { operation_id } = &b.controller_owner
        else {
            return Err("maintenance_task_owner_rejected");
        };
        let expected = EnvironmentTaskFence {
            environment_instance_id: b.instance_id.clone(),
            controller_owner: b.controller_owner.clone(),
            controller_epoch: u64::try_from(b.controller_epoch)
                .map_err(|_| "maintenance_task_invalid")?,
            controller_purpose: b.controller_purpose.clone(),
        };
        expected
            .validate()
            .map_err(|_| "maintenance_task_invalid")?;
        let actual = match &self.task.request {
            EnvironmentTaskRequest::Takeover {
                environment_instance_id,
                controller_owner,
                controller_epoch,
                controller_purpose,
            } => EnvironmentTaskFence {
                environment_instance_id: environment_instance_id.clone(),
                controller_owner: controller_owner.clone(),
                controller_epoch: *controller_epoch,
                controller_purpose: controller_purpose.clone(),
            },
            EnvironmentTaskRequest::Start {
                fence,
                scope,
                program,
                args,
                env,
                cwd,
                timeout_ms,
                ..
            } => {
                if scope
                    != &(EnvironmentTaskScope::Operation {
                        operation_id: operation_id.clone(),
                    })
                    || !env.is_empty()
                    || program != &self.request.program
                    || args != &self.request.args
                    || cwd != &self.request.cwd
                    || timeout_ms != &self.request.timeout_ms
                {
                    return Err("maintenance_command_binding_rejected");
                }
                fence.clone()
            }
            EnvironmentTaskRequest::Query { fence, .. }
            | EnvironmentTaskRequest::List { fence }
            | EnvironmentTaskRequest::Input { fence, .. }
            | EnvironmentTaskRequest::QueryInputReceipt { fence, .. }
            | EnvironmentTaskRequest::Terminate { fence, .. }
            | EnvironmentTaskRequest::EndOperation { fence, .. } => fence.clone(),
            EnvironmentTaskRequest::EndRun { .. } => return Err("maintenance_task_owner_rejected"),
        };
        if actual != expected {
            return Err("maintenance_task_fence_rejected");
        }
        Ok(expected)
    }
}

pub(super) fn task(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if let Err((status, code)) = body_authority(&request) {
        return json_error_response(status, code);
    }
    let dispatch: Dispatch = match serde_json::from_slice(request.body()) {
        Ok(value) => value,
        Err(_) => return json_error_response(400, "maintenance_request_invalid"),
    };
    let fence = match dispatch.validate() {
        Ok(fence) => fence,
        Err(code) => return json_error_response(400, code),
    };
    let key = match env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => return json_error_response(503, "maintenance_signing_key_unavailable"),
    };
    let result = state.job_store.with_current_maintenance(
        &dispatch.authorization,
        key.as_bytes(),
        |target| {
            if target.fence != fence {
                return Err("maintenance_task_fence_rejected".into());
            }
            crate::docker_execution_host::maintenance_task(
                &dispatch.authorization.payload,
                target,
                &dispatch.task,
            )
        },
    );
    let result = match result {
        Ok(result) => result,
        Err(_) => return json_error_response(409, "maintenance_dispatch_rejected"),
    };
    if matches!(
        dispatch.task.request,
        EnvironmentTaskRequest::EndOperation { .. }
    ) {
        if !matches!(
            result,
            EnvironmentTaskResult::OperationEnded {
                cleanup_complete: true,
                ..
            }
        ) {
            return json_error_response(409, "maintenance_completion_unconfirmed");
        }
        if state
            .job_store
            .record_maintenance_completion(&dispatch.authorization, key.as_bytes(), &result)
            .is_err()
        {
            return json_error_response(409, "maintenance_completion_unconfirmed");
        }
    }
    Ok(http_response(
        200,
        "application/json",
        serde_json::to_vec(
            &centaeris_core::execution::EnvironmentTaskResponse::from_result(Ok(result)),
        )
        .map_err(|_| "maintenance_response_encoding_failed")?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileDispatch {
    authorization: MaintenanceAuthorization,
    request: hosted_execution::protocol::FileSystemFrameMetadata,
}

fn file_dispatch(bytes: &[u8]) -> Result<(FileDispatch, Vec<u8>), &'static str> {
    if bytes.len() < 4 {
        return Err("maintenance_frame_invalid");
    }
    let length = u32::from_be_bytes(
        bytes[..4]
            .try_into()
            .map_err(|_| "maintenance_frame_invalid")?,
    ) as usize;
    if length == 0 || length > 64 * 1024 || bytes.len() < 4 + length {
        return Err("maintenance_frame_invalid");
    }
    let dispatch: FileDispatch =
        serde_json::from_slice(&bytes[4..4 + length]).map_err(|_| "maintenance_frame_invalid")?;
    dispatch
        .request
        .validate()
        .map_err(|_| "maintenance_frame_invalid")?;
    if dispatch.request.content_length > 32 * 1024 * 1024 {
        return Err("maintenance_content_too_large");
    }
    let raw = bytes[4 + length..].to_vec();
    dispatch
        .request
        .clone()
        .into_request(raw.clone())
        .map_err(|_| "maintenance_content_rejected")?;
    let canonical = crate::agent_run_authorization::canonical_json(
        serde_json::to_value(&dispatch.request).map_err(|_| "maintenance_request_invalid")?,
    )
    .map_err(|_| "maintenance_request_invalid")?;
    if canonical.len() > 64 * 1024
        || format!("sha256:{:x}", sha2::Sha256::digest(canonical.as_bytes()))
            != dispatch.authorization.payload.request_digest
    {
        return Err("maintenance_request_binding_rejected");
    }
    Ok((dispatch, raw))
}

pub(super) fn filesystem(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    if let Err((status, code)) = body_authority(&request) {
        return json_error_response(status, code);
    }
    let (dispatch, raw) = match file_dispatch(request.body()) {
        Ok(value) => value,
        Err(code) => return json_error_response(400, code),
    };
    let key = match env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY") {
        Ok(key) if !key.is_empty() => key,
        _ => return json_error_response(503, "maintenance_signing_key_unavailable"),
    };
    let result = state.job_store.with_maintenance_completion(
        &dispatch.authorization,
        key.as_bytes(),
        |target| {
            let core = &dispatch.request.request;
            if core.cwd != std::path::Path::new(&target.profile.workspace_root)
                || core.policy != maintenance_policy(&target.profile)
            {
                return Err("maintenance_filesystem_policy_rejected".into());
            }
            let fence = &target.fence;
            let request_task = |request| EnvironmentTaskEnvelope {
                schema: centaeris_core::execution::ENVIRONMENT_TASK_SCHEMA.into(),
                request,
            };
            let taken = crate::docker_execution_host::maintenance_task(
                &dispatch.authorization.payload,
                target,
                &request_task(EnvironmentTaskRequest::Takeover {
                    environment_instance_id: fence.environment_instance_id.clone(),
                    controller_owner: fence.controller_owner.clone(),
                    controller_epoch: fence.controller_epoch,
                    controller_purpose: fence.controller_purpose.clone(),
                }),
            )?;
            if !matches!(taken,EnvironmentTaskResult::TakenOver{fence:ref actual}if actual==fence) {
                return Err("maintenance_takeover_unconfirmed".into());
            }
            let EnvironmentTaskOwner::MaintenanceOperation { operation_id } =
                &fence.controller_owner
            else {
                return Err("maintenance_owner_rejected".into());
            };
            let mut input = Vec::new();
            hosted_execution::protocol::write_filesystem_request(
                &mut input,
                dispatch
                    .request
                    .clone()
                    .into_request(raw)
                    .map_err(|_| "maintenance_content_rejected")?,
            )
            .map_err(|_| "maintenance_frame_invalid")?;
            let helper = hosted_execution::host_supervisor::HostHelperRequest {
                fence: fence.clone(),
                operation_id: fence.operation_id(&format!(
                    "filesystem:{}",
                    dispatch.authorization.payload.request_digest
                )),
                scope: EnvironmentTaskScope::Operation {
                    operation_id: operation_id.clone(),
                },
                kind: hosted_execution::host_supervisor::TrustedHelperKind::FileSystem,
                input_digest: format!("sha256:{:x}", sha2::Sha256::digest(&input)),
                input_length: input.len() as u64,
            };
            let output = crate::docker_execution_host::maintenance_helper(
                &dispatch.authorization.payload,
                target,
                helper,
                &input,
            )?;
            hosted_execution::protocol::read_filesystem_result(&mut std::io::Cursor::new(&output))?;
            let ended = crate::docker_execution_host::maintenance_task(
                &dispatch.authorization.payload,
                target,
                &request_task(EnvironmentTaskRequest::EndOperation {
                    fence: fence.clone(),
                    owner_operation_id: operation_id.clone(),
                    operation_id: fence.operation_id("filesystem-completed"),
                }),
            )?;
            Ok((output, ended))
        },
    );
    match result {
        Ok(output) => Ok(http_response(200, "application/octet-stream", output)),
        Err(_) => json_error_response(409, "maintenance_filesystem_unconfirmed"),
    }
}

fn maintenance_policy(
    profile: &crate::agent_run_authorization::EnvironmentProfile,
) -> centaeris_core::execution::ExecutionPolicy {
    let mut policy = crate::docker_execution_host::environment_policy(profile);
    policy.filesystem.read_only_roots = vec![std::path::PathBuf::from("/")];
    policy.filesystem.writable_roots = vec![std::path::PathBuf::from("/")];
    policy.filesystem.tmp_root = None;
    policy
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    use hosted_execution::protocol::FileSystemFrameMetadata;
    fn frame(content: Vec<u8>) -> Vec<u8> {
        let mut auth:MaintenanceAuthorization=serde_json::from_str(include_str!("../../../../tests/workspace/fixtures/environment_maintenance_authorization/v1/valid.json")).unwrap();
        let profile:crate::agent_run_authorization::EnvironmentProfile=serde_json::from_value(serde_json::from_str::<serde_json::Value>(include_str!("../../../../tests/workspace/fixtures/environment_provision_authorization/v1/valid.json")).unwrap()["payload"]["environmentProfile"].clone()).unwrap();
        let (metadata, raw) = FileSystemFrameMetadata::from_request(
            centaeris_core::execution::ExecutionFileSystemRequest {
                operation_id: None,
                cwd: profile.workspace_root.clone().into(),
                model_path: "/app/input".into(),
                operation: centaeris_core::execution::ExecutionFileSystemOperation::WriteFile {
                    content,
                    observed_file_hash: None,
                    create_only: false,
                },
                policy: maintenance_policy(&profile),
            },
        );
        let canonical = crate::agent_run_authorization::canonical_json(
            serde_json::to_value(&metadata).unwrap(),
        )
        .unwrap();
        auth.payload.request_digest =
            format!("sha256:{:x}", sha2::Sha256::digest(canonical.as_bytes()));
        let header = serde_json::to_vec(&json!({"authorization":auth,"request":metadata})).unwrap();
        assert!(header.len() < 64 * 1024);
        let mut bytes = (header.len() as u32).to_be_bytes().to_vec();
        bytes.extend(header);
        bytes.extend(raw);
        bytes
    }
    #[test]
    fn maintenance_command_cannot_add_unsigned_process_environment() {
        let mut auth:MaintenanceAuthorization=serde_json::from_str(include_str!("../../../../tests/workspace/fixtures/environment_maintenance_authorization/v1/valid.json")).unwrap();
        let command = Command {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "true".into()],
            cwd: "/app".into(),
            timeout_ms: 1000,
        };
        auth.payload.request_digest = format!(
            "sha256:{:x}",
            sha2::Sha256::digest(
                crate::agent_run_authorization::canonical_json(
                    serde_json::to_value(&command).unwrap()
                )
                .unwrap()
                .as_bytes()
            )
        );
        let b = &auth.payload;
        let fence = EnvironmentTaskFence {
            environment_instance_id: b.instance_id.clone(),
            controller_owner: b.controller_owner.clone(),
            controller_epoch: b.controller_epoch as u64,
            controller_purpose: b.controller_purpose.clone(),
        };
        let EnvironmentTaskOwner::MaintenanceOperation { operation_id } = &fence.controller_owner
        else {
            unreachable!()
        };
        let request = EnvironmentTaskRequest::Start {
            fence: fence.clone(),
            operation_id: fence.operation_id("command"),
            scope: EnvironmentTaskScope::Operation {
                operation_id: operation_id.clone(),
            },
            program: command.program.clone(),
            args: command.args.clone(),
            env: Default::default(),
            cwd: command.cwd.clone(),
            timeout_ms: command.timeout_ms,
        };
        let mut dispatch = Dispatch {
            authorization: auth,
            request: command,
            task: EnvironmentTaskEnvelope {
                schema: centaeris_core::execution::ENVIRONMENT_TASK_SCHEMA.into(),
                request,
            },
        };
        dispatch.validate().unwrap();
        if let EnvironmentTaskRequest::Start { env, .. } = &mut dispatch.task.request {
            env.insert("BASH_ENV".into(), "/tmp/unsigned-script".into());
        }
        assert_eq!(
            dispatch.validate(),
            Err("maintenance_command_binding_rejected")
        );
    }
    #[test]
    fn maintenance_filesystem_frame_binds_bounded_raw_binary_and_rejects_extra_or_changed_bytes() {
        let content = vec![0xff; 11_508_561];
        let bytes = frame(content.clone());
        let (dispatch, raw) = file_dispatch(&bytes).unwrap();
        assert_eq!(raw, content);
        let mut changed = bytes.clone();
        *changed.last_mut().unwrap() = 0;
        assert!(file_dispatch(&changed).is_err());
        let mut extra = bytes;
        extra.push(0);
        assert!(file_dispatch(&extra).is_err());
        assert_eq!(dispatch.request.content_length, 11_508_561);
    }
    #[test]
    fn maintenance_filesystem_policy_keeps_guest_control_private_and_profile_identity() {
        let bytes = frame(vec![]);
        let (dispatch, _) = file_dispatch(&bytes).unwrap();
        let policy = dispatch.request.request.policy;
        assert_eq!(
            policy.filesystem.writable_roots,
            vec![std::path::PathBuf::from("/")]
        );
        assert_eq!(
            policy.filesystem.denied_read_paths,
            vec![
                std::path::PathBuf::from("/run/centaeris"),
                std::path::PathBuf::from("/run/centaeris-host")
            ]
        );
        assert_eq!(
            policy.filesystem.denied_write_paths,
            policy.filesystem.denied_read_paths
        );
    }
}
