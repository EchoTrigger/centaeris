use super::*;
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskEnvelope, EnvironmentTaskFence,
    EnvironmentTaskOwner, EnvironmentTaskRequest, EnvironmentTaskResult, EnvironmentTaskSnapshot,
    ENVIRONMENT_TASK_SCHEMA,
};

pub(super) fn signing_key() -> Result<String, String> {
    env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| "retained signing key unavailable".into())
}
pub(super) fn origin_expectation<'a>(
    base: &ContainerExpectation<'a>,
    a: &'a RetainedInstanceAllocation,
    name: &'a str,
) -> ContainerExpectation<'a> {
    ContainerExpectation {
        allocation_owner: Some(&a.allocation_owner),
        profile_digest: Some(&a.profile_digest),
        name,
        agent_run_id: a.allocation_agent_run_id.as_deref().unwrap_or(""),
        execution_id: &a.allocation_execution_id,
        environment_id: &a.environment_id,
        instance_id: &a.instance_id,
        binding_digest: &a.binding_digest,
        image_digest: &a.image_digest,
        ..*base
    }
}
pub(super) fn profile_digest(
    expected: &ContainerExpectation<'_>,
    plugins: &PluginActivationSnapshotV1,
) -> Result<String, String> {
    validate_plugin_activation_snapshot(plugins)?;
    expected
        .environment_profile
        .ok_or("environment_profile_required")?
        .digest()
}
pub(super) fn fence(
    auth: &crate::environment_authorization::EnvironmentAuthorization,
    instance: &str,
) -> Result<EnvironmentTaskFence, String> {
    let c = auth
        .payload
        .controller
        .as_ref()
        .ok_or("retained controller required")?;
    if c.controller_purpose != "execute" {
        return Err("retained execute authority required".into());
    }
    Ok(EnvironmentTaskFence {
        environment_instance_id: instance.into(),
        controller_owner: EnvironmentTaskOwner::AgentRun {
            run_id: c.controller_id.clone(),
        },
        controller_epoch: u64::try_from(c.controller_epoch)
            .map_err(|_| "controller epoch invalid")?,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    })
}
pub(super) fn relay(
    expected: &ContainerExpectation<'_>,
    id: &str,
    request: EnvironmentTaskRequest,
) -> Result<EnvironmentTaskResult, String> {
    let facts = inspect_container(id)?.ok_or("retained container missing")?;
    if facts.id != id || !facts.matches(expected) {
        return Err("retained physical identity rejected".into());
    }
    let envelope = EnvironmentTaskEnvelope {
        schema: ENVIRONMENT_TASK_SCHEMA.into(),
        request,
    };
    crate::environment_supervisor::task(
        hosted_execution::host_supervisor::HostLaunchTarget {
            environment_instance_id: expected.instance_id.into(),
            container_id: id.into(),
            environment_id: expected.environment_id.into(),
            profile_digest: expected
                .profile_digest
                .ok_or("environment profile digest missing")?
                .into(),
            image_digest: expected.image_digest.into(),
        },
        envelope,
    )
}
pub(super) fn takeover(
    expected: &ContainerExpectation<'_>,
    id: &str,
    fence: &EnvironmentTaskFence,
) -> Result<(), String> {
    let result = relay(
        expected,
        id,
        EnvironmentTaskRequest::Takeover {
            environment_instance_id: fence.environment_instance_id.clone(),
            controller_owner: fence.controller_owner.clone(),
            controller_epoch: fence.controller_epoch,
            controller_purpose: fence.controller_purpose.clone(),
        },
    )?;
    if matches!(result,EnvironmentTaskResult::TakenOver{fence:ref actual} if actual==fence) {
        use hosted_execution::host_supervisor::{
            HostHelperRequest, HostLaunchTarget, TrustedHelperKind,
        };
        let scope = match &fence.controller_owner {
            EnvironmentTaskOwner::AgentRun { run_id } => {
                centaeris_core::execution::EnvironmentTaskScope::Run {
                    run_id: run_id.clone(),
                }
            }
            EnvironmentTaskOwner::MaintenanceOperation { operation_id } => {
                centaeris_core::execution::EnvironmentTaskScope::Operation {
                    operation_id: operation_id.clone(),
                }
            }
        };
        let request = HostHelperRequest {
            fence: fence.clone(),
            operation_id: fence.operation_id("prepare-workspace"),
            scope,
            kind: TrustedHelperKind::PrepareWorkspace,
            input_digest: format!("sha256:{:x}", Sha256::digest([])),
            input_length: 0,
        };
        let target = HostLaunchTarget {
            environment_instance_id: expected.instance_id.into(),
            container_id: id.into(),
            environment_id: expected.environment_id.into(),
            profile_digest: expected
                .profile_digest
                .ok_or("environment_profile_required")?
                .into(),
            image_digest: expected.image_digest.into(),
        };
        let mut error = "environment_workspace_preparation_unknown".to_owned();
        for _ in 0..2 {
            match crate::environment_supervisor::helper(
                &target,
                request.clone(),
                &mut std::io::empty(),
                &mut Vec::new(),
                &mut Vec::new(),
            ) {
                Ok(task) if task.exit_code == Some(0) => return Ok(()),
                Ok(_) => return Err("environment_workspace_preparation_failed".into()),
                Err(e) => error = e,
            }
        }
        Err(error)
    } else {
        Err("retained takeover acknowledgement mismatch".into())
    }
}
impl DockerExecutionHostRunner {
    pub(crate) fn is_hot_reuse(&self) -> bool {
        self.retained_allocation.as_ref().is_some_and(|a| a.reused)
    }
    pub(crate) fn environment_task_fence(&self) -> Result<EnvironmentTaskFence, String> {
        fence(
            self.environment_authorization
                .as_ref()
                .ok_or("retained authority unavailable")?,
            &self.instance_id,
        )
    }
    pub(crate) fn environment_task_rpc(
        &self,
        request: EnvironmentTaskRequest,
    ) -> Result<EnvironmentTaskResult, String> {
        let expected_fence = self.environment_task_fence()?;
        let supplied = match &request {
            EnvironmentTaskRequest::Takeover { .. }
            | EnvironmentTaskRequest::EndRun { .. }
            | EnvironmentTaskRequest::EndOperation { .. } => {
                return Err("retained lifecycle operation denied".into())
            }
            EnvironmentTaskRequest::Start { fence, .. }
            | EnvironmentTaskRequest::List { fence }
            | EnvironmentTaskRequest::Query { fence, .. }
            | EnvironmentTaskRequest::Input { fence, .. }
            | EnvironmentTaskRequest::QueryInputReceipt { fence, .. }
            | EnvironmentTaskRequest::Terminate { fence, .. } => fence,
        };
        if supplied != &expected_fence {
            return Err("retained task fence mismatch".into());
        }
        request.validate_authority()?;
        self.with_supervisor_target(|target| {
            crate::environment_supervisor::task(
                target.clone(),
                EnvironmentTaskEnvelope {
                    schema: ENVIRONMENT_TASK_SCHEMA.into(),
                    request,
                },
            )
        })
    }
    pub(super) fn with_supervisor_target<T: Send>(
        &self,
        action: impl FnOnce(&hosted_execution::host_supervisor::HostLaunchTarget) -> Result<T, String>
            + Send,
    ) -> Result<T, String> {
        let a = self
            .retained_allocation
            .as_ref()
            .ok_or("retained allocation missing")?;
        let auth = self
            .environment_authorization
            .as_ref()
            .ok_or("retained authorization missing")?;
        let key = signing_key()?;
        let lease = self
            .resident_lease
            .as_ref()
            .ok_or("retained lease missing")?;
        let workspace = self
            .retained_workspace_id
            .as_ref()
            .ok_or("retained workspace missing")?;
        let operation = crate::postgres_store::execution_instances::RetainedInstanceRequest {
            environment_authorization: auth,
            signing_key: key.as_bytes(),
            execution_id: &self.execution_id,
            workspace_id: workspace,
            host_id: &a.host_id,
            image_digest: &self.image_digest,
            profile_digest: &a.profile_digest,
            resources: declared_usage(self.resources),
            lease,
        };
        self.resident_store
            .as_ref()
            .ok_or("retained store missing")?
            .with_retained_instance_operation(&operation, a, |actual| {
                let id = actual
                    .container_id
                    .as_deref()
                    .ok_or("retained operation container unknown")?;
                let expected = self.container_expectation();
                let facts = inspect_container(id)?.ok_or("environment_container_missing")?;
                if facts.id != id || !facts.matches(&expected) {
                    return Err("environment_physical_identity_rejected".into());
                }
                action(&hosted_execution::host_supervisor::HostLaunchTarget {
                    environment_instance_id: actual.instance_id.clone(),
                    container_id: id.into(),
                    environment_id: actual.environment_id.clone(),
                    profile_digest: actual.profile_digest.clone(),
                    image_digest: actual.image_digest.clone(),
                })
            })
    }
    fn start_helper_current(
        &self,
        request: &hosted_execution::host_supervisor::HostHelperRequest,
        input: &mut (impl Read + Seek + Send),
    ) -> Result<EnvironmentTaskSnapshot, String> {
        let attempt = || {
            input
                .rewind()
                .map_err(|_| "environment_helper_input_unavailable")?;
            self.with_supervisor_target(|target| {
                crate::environment_supervisor::helper_start(target, request.clone(), input)
            })
        };
        if request.fence.controller_purpose != EnvironmentTaskControllerPurpose::Execute {
            return {
                let mut attempt = attempt;
                attempt()
            };
        }
        super::admission_wait::start(
            attempt,
            || {
                let store = self
                    .resident_store
                    .as_ref()
                    .ok_or("retained control store missing")?;
                let lease = self
                    .resident_lease
                    .as_ref()
                    .ok_or("retained control lease missing")?;
                let state = store.agent_run_execution_control_state(
                    &self.agent_run_id,
                    &lease.job_id,
                    &lease.lease_owner,
                    crate::now_ms()?,
                )?;
                Ok(if state.cancellation_requested {
                    Some("agent_run_cancel_requested".into())
                } else if !state.lifecycle_lease_current {
                    Some("agent_run_lifecycle_lease_lost".into())
                } else {
                    None
                })
            },
            thread::sleep,
        )
    }
    fn finish_helper_current(
        &self,
        request: &hosted_execution::host_supervisor::HostHelperRequest,
        mut task: EnvironmentTaskSnapshot,
    ) -> Result<crate::environment_supervisor::HelperOutput, String> {
        let started = Instant::now();
        loop {
            if task.state == centaeris_core::execution::EnvironmentTaskState::Unknown {
                return Err("environment_helper_outcome_unknown".into());
            }
            if task.state == centaeris_core::execution::EnvironmentTaskState::Exited
                && task.cleanup_complete
                && task.output_complete
            {
                break;
            }
            if started.elapsed()
                > Duration::from_millis(request.kind.timeout_ms().saturating_add(5000))
            {
                let _ = self.environment_task_rpc(EnvironmentTaskRequest::Terminate {
                    fence: request.fence.clone(),
                    task_id: task.task_id.clone(),
                    operation_id: request
                        .fence
                        .operation_id(&format!("helper-timeout:{}", task.task_id)),
                });
                return Err("environment_helper_outcome_unknown".into());
            }
            thread::sleep(Duration::from_millis(10));
            let response = self.environment_task_rpc(EnvironmentTaskRequest::Query {
                fence: request.fence.clone(),
                task_id: task.task_id.clone(),
                cursor: 0,
                max_bytes: centaeris_core::execution::ENVIRONMENT_TASK_READ_BYTES as u32,
            })?;
            let EnvironmentTaskResult::Task { task: current, .. } = response else {
                return Err("environment_helper_outcome_unknown".into());
            };
            if current.task_id != task.task_id
                || current.scope != request.scope
                || current.operation_id != request.operation_id
            {
                return Err("environment_helper_outcome_unknown".into());
            }
            task = current;
        }
        // Authorize the bounded response header, then release DB pins before raw I/O.
        self.with_supervisor_target(|target| {
            crate::environment_supervisor::prepare_helper_output(
                target,
                request.clone(),
                &task.task_id,
            )
        })
    }
    fn helper_request(
        &self,
        kind: hosted_execution::host_supervisor::TrustedHelperKind,
        length: u64,
        digest: &str,
    ) -> Result<hosted_execution::host_supervisor::HostHelperRequest, String> {
        let fence = self.environment_task_fence()?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "environment_helper_identity_invalid")?
            .as_nanos();
        let request = hosted_execution::host_supervisor::HostHelperRequest {
            operation_id: fence.operation_id(&format!("helper:{}:{stamp}", std::process::id())),
            scope: centaeris_core::execution::EnvironmentTaskScope::Run {
                run_id: self.agent_run_id.clone(),
            },
            kind,
            input_digest: digest.into(),
            input_length: length,
            fence,
        };
        request.validate()?;
        Ok(request)
    }
    pub(super) fn supervised_helper(
        &self,
        kind: hosted_execution::host_supervisor::TrustedHelperKind,
        input: &[u8],
        output_limit: usize,
    ) -> Result<Vec<u8>, String> {
        if output_limit as u64 > kind.output_limit() {
            return Err("environment_helper_budget_invalid".into());
        }
        let request = self.helper_request(
            kind,
            input.len() as u64,
            &format!("sha256:{:x}", Sha256::digest(input)),
        )?;
        let task = self.start_helper_current(&request, &mut Cursor::new(input))?;
        let output = self.finish_helper_current(&request, task)?;
        if output.task.exit_code != Some(0) {
            return Err("environment_helper_failed".into());
        }
        let mut stdout = Vec::new();
        let mut stderr = BoundedDiagnosticWriter(Vec::new());
        output.copy_to(&mut stdout, &mut stderr)?;
        if stdout.len() > output_limit {
            return Err("environment_helper_output_too_large".into());
        }
        Ok(stdout)
    }
    pub(super) fn supervised_helper_file(
        &self,
        kind: hosted_execution::host_supervisor::TrustedHelperKind,
        input: &mut fs::File,
        input_length: u64,
        input_digest: &str,
    ) -> Result<(fs::File, Vec<u8>, Option<i32>), String> {
        let request = self.helper_request(kind, input_length, input_digest)?;
        let task = self.start_helper_current(&request, input)?;
        let output = self.finish_helper_current(&request, task)?;
        let exit = output.task.exit_code;
        let mut stdout =
            tempfile::tempfile().map_err(|_| "environment_helper_spool_unavailable")?;
        let mut stderr = BoundedDiagnosticWriter(Vec::new());
        output.copy_to(&mut stdout, &mut stderr)?;
        stdout
            .rewind()
            .map_err(|_| "environment_helper_spool_unavailable")?;
        Ok((stdout, stderr.0, exit))
    }
}

// The cleanup store holds host admission and authoritative controller/job row
// pins throughout this adapter call. It supplies the independently verified
// original allocation grant, which can differ from the Run being cleaned up.
impl crate::http::agent_runs::retained_cleanup::RetainedCleanupHost
    for crate::http::agent_runs::retained_cleanup::DockerCleanupHost<'_>
{
    fn task_rpc(
        &self,
        instance_id: &str,
        envelope: &EnvironmentTaskEnvelope,
    ) -> Result<EnvironmentTaskResult, String> {
        use sha2::{Digest, Sha256};
        envelope.validate()?;
        envelope.request.validate_authority()?;
        let authorization = &self.cleanup.environment_authorization;
        let controller = authorization
            .payload
            .controller
            .as_ref()
            .ok_or("retained cleanup controller missing")?;
        let key = signing_key()?;
        self.cleanup.clone().validate(key.as_bytes())?;
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(
                [
                    b"workspace:environment-controller-lease:v1\0".as_slice(),
                    self.lease.lease_owner.as_bytes()
                ]
                .concat()
            )
        );
        if controller.controller_purpose != "cleanup"
            || controller.controller_lease_digest != digest
            || self.lease.job_id != crate::agent_run_lifecycle_job_id(&self.cleanup.agent_run_id)?
            || self.lease.job_kind != "agent_run.lifecycle"
            || instance_id != self.allocation.instance_id
            || authorization.payload.environment_id != self.allocation.environment_id
        {
            return Err("retained cleanup physical authority rejected".into());
        }
        let expected_fence = EnvironmentTaskFence {
            environment_instance_id: instance_id.into(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: self.cleanup.agent_run_id.clone(),
            },
            controller_epoch: u64::try_from(controller.controller_epoch)
                .map_err(|_| "cleanup epoch invalid")?,
            controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
        };
        let supplied = match &envelope.request {
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
            EnvironmentTaskRequest::List { fence }
            | EnvironmentTaskRequest::Query { fence, .. }
            | EnvironmentTaskRequest::EndRun { fence, .. }
            | EnvironmentTaskRequest::Terminate { fence, .. } => fence.clone(),
            _ => return Err("retained cleanup mutation denied".into()),
        };
        if supplied != expected_fence {
            return Err("retained cleanup task fence mismatch".into());
        }
        let origin = self.origin_authorization;
        // resolve_target verifies this immutable origin grant's digest and
        // signature against the allocation while the cleanup guard is held.
        if origin.environment_profile.image_digest != self.allocation.image_digest
            || origin.session_id != authorization.payload.session_id
            || origin.workspace_id != self.cleanup.authorization.workspace_id
            || declared_usage(origin.environment_profile.resources.sandbox_resources())
                != self.resources
        {
            return Err("retained cleanup origin grant rejected".into());
        }
        validate_plugin_activation_snapshot(&origin.plugin_activation)?;
        let plugin_volume_name = docker_volume_name_from_environment(PLUGIN_VOLUME_NAME_ENV)?;
        let mounts = plugin_mounts(&origin.plugin_activation)?;
        let name = container_name(&self.allocation.allocation_execution_id);
        let helper = environment_container::helper_path()?;
        let expected = ContainerExpectation {
            network: origin.effective_network()?,
            environment_profile: Some(&origin.environment_profile),
            benchmark_helper: Some(&helper),
            profile_digest: Some(&self.allocation.profile_digest),
            name: &name,
            agent_run_id: self
                .allocation
                .allocation_agent_run_id
                .as_deref()
                .unwrap_or(""),
            allocation_owner: Some(&self.allocation.allocation_owner),
            execution_id: &self.allocation.allocation_execution_id,
            environment_id: &self.allocation.environment_id,
            instance_id: &self.allocation.instance_id,
            binding_digest: &self.allocation.binding_digest,
            image_digest: &self.allocation.image_digest,
            oci_runtime: OciRuntime::from_environment()?,
            resources: origin.environment_profile.resources.sandbox_resources(),
            plugin_volume_name: &plugin_volume_name,
            plugin_mounts: &mounts,
        };
        if profile_digest(&expected, &origin.plugin_activation)? != self.allocation.profile_digest {
            return Err("retained cleanup profile rejected".into());
        }
        let id = self
            .allocation
            .container_id
            .as_deref()
            .ok_or("retained cleanup container outcome unknown")?;
        relay(&expected, id, envelope.request.clone())
    }
}

struct BoundedDiagnosticWriter(Vec<u8>);
impl Write for BoundedDiagnosticWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let keep = bytes
            .len()
            .min(DOCKER_DIAGNOSTIC_LIMIT.saturating_sub(self.0.len()));
        self.0.extend_from_slice(&bytes[..keep]);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
