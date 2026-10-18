use super::*;
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskEnvelope, EnvironmentTaskFence,
    EnvironmentTaskOwner, EnvironmentTaskRequest, EnvironmentTaskResponse, EnvironmentTaskResult,
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
        retained: true,
        profile_digest: Some(&a.profile_digest),
        name,
        agent_run_id: &a.allocation_agent_run_id,
        execution_id: &a.allocation_execution_id,
        authorization_digest: &a.authorization_digest,
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
    use sha2::{Digest, Sha256};
    let normalized = crate::postgres_store::execution_instances::instance_id("retained-profile");
    let mut profile = *expected;
    profile.instance_id = &normalized;
    let mut body = container_create_body(&profile)?;
    body.as_object_mut()
        .ok_or("container profile invalid")?
        .remove("Labels");
    let input = serde_json::json!({"schema":"workspace.retained_profile.v1","container":body,"plugins":plugins});
    let canonical = crate::agent_run_authorization::canonical_json(input)?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
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
fn relay(
    expected: &ContainerExpectation<'_>,
    id: &str,
    request: EnvironmentTaskRequest,
) -> Result<EnvironmentTaskResult, String> {
    let facts = inspect_container(id)?.ok_or("retained container missing")?;
    if facts.id != id
        || !facts.matches(expected)
        || !workspace_execution_sentinel_matches(
            id,
            expected.agent_run_id,
            expected.execution_id,
            expected.authorization_digest,
        )?
    {
        return Err("retained physical identity rejected".into());
    }
    let envelope = EnvironmentTaskEnvelope {
        schema: ENVIRONMENT_TASK_SCHEMA.into(),
        request,
    };
    let mut exec = ExecRequest::owned(
        id,
        expected.agent_run_id,
        expected.execution_id,
        vec![
            "/usr/bin/timeout".into(),
            "--signal=TERM".into(),
            "--kill-after=1s".into(),
            "10s".into(),
            AGENT_BINARY.into(),
            "environment-task-rpc".into(),
        ],
    );
    exec.user = Some("0:0".into());
    let output = exec_with_input(
        &exec,
        &serde_json::to_vec(&envelope).map_err(|e| e.to_string())?,
        HELPER_JSON_LIMIT,
    )?;
    if !output.status.success() {
        return Err(format!(
            "retained task relay failed: {}",
            bounded_diagnostic(&output.stderr)
        ));
    }
    let response: EnvironmentTaskResponse = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("retained task response invalid: {e}"))?;
    response.validate()?;
    match response {
        EnvironmentTaskResponse::Success { result, .. } => Ok(result),
        EnvironmentTaskResponse::Error { error, .. } => Err(error),
    }
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
        Ok(())
    } else {
        Err("retained takeover acknowledgement mismatch".into())
    }
}
impl DockerExecutionHostRunner {
    pub(crate) fn is_retained(&self) -> bool {
        self.retained_allocation.is_some()
    }
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
                relay(&self.container_expectation(), id, request)
            })
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
        origin.checked_binding(&self.allocation.allocation_agent_run_id)?;
        if origin.image_digest != self.allocation.image_digest
            || origin.session_id != authorization.payload.session_id
            || origin.workspace_id != self.cleanup.authorization.workspace_id
            || declared_usage(origin.resources) != self.resources
        {
            return Err("retained cleanup origin grant rejected".into());
        }
        validate_plugin_activation_snapshot(&origin.plugin_activation)?;
        let plugin_volume_name = docker_volume_name_from_environment(PLUGIN_VOLUME_NAME_ENV)?;
        let mounts = plugin_mounts(&origin.plugin_activation);
        let memory = memory_mount(
            &origin.user_id,
            &origin.agent_id,
            docker_volume_name_from_environment(AGENT_MEMORY_VOLUME_NAME_ENV)?,
        )?;
        let name = container_name(&self.allocation.allocation_execution_id);
        let expected = ContainerExpectation {
            network: origin.effective_network()?,
            benchmark_profile: None,
            benchmark_helper: None,
            retained: true,
            profile_digest: Some(&self.allocation.profile_digest),
            name: &name,
            agent_run_id: &self.allocation.allocation_agent_run_id,
            execution_id: &self.allocation.allocation_execution_id,
            authorization_digest: &self.allocation.authorization_digest,
            environment_id: &self.allocation.environment_id,
            instance_id: &self.allocation.instance_id,
            binding_digest: &self.allocation.binding_digest,
            image_digest: &self.allocation.image_digest,
            oci_runtime: OciRuntime::from_environment()?,
            resources: origin.resources,
            plugin_volume_name: &plugin_volume_name,
            plugin_mounts: &mounts,
            memory_mount: &memory,
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
