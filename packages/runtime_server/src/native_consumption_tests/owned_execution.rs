//! Empty physical adapter; allocation, authority, receipts and resource accounting
//! remain in the same PostgreSQL APIs used by the Docker adapter.
use super::owns_run;
use crate::docker_execution_host::DockerExecutionHostRequest;
use crate::http::agent_runs::retained_cleanup::{confirm_run_cleanup, RetainedCleanupHost};
use crate::postgres_store::cleanup_receipts::CleanupPhysicalResult;
use crate::postgres_store::execution_instances::{
    RetainedInstanceAllocation, RetainedInstanceRequest,
};
use crate::postgres_store::PostgresRuntimeStore;
use crate::resident_capacity::{declared_usage, ObservedResidentSandbox, ResidentCapacity};
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskEnvelope, EnvironmentTaskFence,
    EnvironmentTaskOwner, EnvironmentTaskRequest, EnvironmentTaskResult, ResidentResourceUsage,
};
use centaeris_core::session::RuntimeJobLeaseFence;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

const HOST_ID: &str = "native-consumption-owned-test-host";

#[derive(Default)]
struct OwnedPhysicalHost {
    instances: Mutex<BTreeMap<String, PhysicalInstance>>,
}

struct PhysicalInstance {
    allocation: RetainedInstanceAllocation,
    resources: ResidentResourceUsage,
    fence: EnvironmentTaskFence,
    present: bool,
    cleanup_confirmed: bool,
}

fn host() -> &'static OwnedPhysicalHost {
    static HOST: OnceLock<OwnedPhysicalHost> = OnceLock::new();
    HOST.get_or_init(OwnedPhysicalHost::default)
}

pub(crate) fn owned_host_context() -> Result<(String, ResidentCapacity), String> {
    Ok((HOST_ID.into(), ResidentCapacity::from_env()?))
}

pub(crate) fn owned_inventory() -> Result<Vec<ObservedResidentSandbox>, String> {
    host().inventory()
}

pub(crate) fn prepare_owned_allocation(
    request: &DockerExecutionHostRequest<'_>,
    before_create: &mut (dyn FnMut(&mut postgres::Client) -> Result<(), String> + Send),
) -> Result<RetainedInstanceAllocation, String> {
    let key = std::env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
        .map_err(|_| "owned fixture signing key unavailable")?;
    let profile = request
        .environment_profile
        .as_ref()
        .ok_or("owned fixture environment profile missing")?;
    let profile_digest = profile.digest()?;
    let controller = request
        .environment_authorization
        .payload
        .controller
        .as_ref()
        .ok_or("owned fixture controller missing")?;
    let resources = declared_usage(request.resources);
    let lease = RuntimeJobLeaseFence {
        job_id: request.lifecycle_job_id.clone(),
        job_kind: "agent_run.lifecycle".into(),
        lease_owner: request.lifecycle_lease_owner.clone(),
    };
    request.resident_store.prepare_retained_instance(
        &RetainedInstanceRequest {
            environment_authorization: &request.environment_authorization,
            signing_key: key.as_bytes(),
            execution_id: &request.execution_id,
            workspace_id: &request.workspace_id,
            host_id: HOST_ID,
            image_digest: &request.image_digest,
            profile_digest: &profile_digest,
            resources,
            lease: &lease,
        },
        owned_host_context()?.1,
        owned_inventory,
        |client, _| before_create(client),
        |allocation| {
            let fence = EnvironmentTaskFence {
                environment_instance_id: allocation.instance_id.clone(),
                controller_owner: EnvironmentTaskOwner::AgentRun {
                    run_id: controller.controller_id.clone(),
                },
                controller_epoch: controller
                    .controller_epoch
                    .try_into()
                    .map_err(|_| "owned fixture controller epoch invalid")?,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            };
            host().prepare(allocation, resources, fence)
        },
    )
}

impl OwnedPhysicalHost {
    fn inventory(&self) -> Result<Vec<ObservedResidentSandbox>, String> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| "owned host lock poisoned")?;
        Ok(instances
            .values()
            .filter(|instance| instance.present)
            .map(|instance| ObservedResidentSandbox {
                execution_id: instance.allocation.allocation_execution_id.clone(),
                allocation_owner: instance.allocation.allocation_owner.clone(),
                container_id: instance.allocation.container_id.clone().unwrap(),
                resources: instance.resources,
            })
            .collect())
    }

    fn prepare(
        &self,
        allocation: &RetainedInstanceAllocation,
        resources: ResidentResourceUsage,
        fence: EnvironmentTaskFence,
    ) -> Result<String, String> {
        fence.validate()?;
        let mut instances = self
            .instances
            .lock()
            .map_err(|_| "owned host lock poisoned")?;
        if allocation.reused {
            let instance = instances
                .get_mut(&allocation.instance_id)
                .ok_or("owned physical instance missing")?;
            instance.require_allocation(allocation, resources)?;
            if !instance.present {
                return Err("owned physical instance absent".into());
            }
            instance.fence = fence;
            instance.cleanup_confirmed = false;
            return Ok(instance.allocation.container_id.clone().unwrap());
        }
        if instances.contains_key(&allocation.instance_id) {
            return Err("owned physical instance already exists".into());
        }
        let container_id = format!(
            "{:x}",
            Sha256::digest(format!("native-owned-physical:{}", allocation.instance_id).as_bytes())
        );
        let mut physical = allocation.clone();
        physical.container_id = Some(container_id.clone());
        instances.insert(
            allocation.instance_id.clone(),
            PhysicalInstance {
                allocation: physical,
                resources,
                fence,
                present: true,
                cleanup_confirmed: false,
            },
        );
        Ok(container_id)
    }

    fn cleanup(
        &self,
        allocation: &RetainedInstanceAllocation,
        resources: ResidentResourceUsage,
        fence: &EnvironmentTaskFence,
        per_run: bool,
    ) -> Result<CleanupPhysicalResult, String> {
        fence.validate()?;
        if fence.environment_instance_id != allocation.instance_id
            || fence.controller_purpose != EnvironmentTaskControllerPurpose::Cleanup
        {
            return Err("owned cleanup fence rejected".into());
        }
        let already_removed = {
            let instances = self
                .instances
                .lock()
                .map_err(|_| "owned host lock poisoned")?;
            let instance = instances
                .get(&allocation.instance_id)
                .ok_or("owned physical instance missing")?;
            instance.require_allocation(allocation, resources)?;
            if !instance.present
                && (fence.controller_epoch < instance.fence.controller_epoch
                    || fence.controller_owner != instance.fence.controller_owner)
            {
                return Err("owned cleanup fence rejected".into());
            }
            !instance.present && instance.cleanup_confirmed
        };
        if !already_removed {
            confirm_run_cleanup(
                &OwnedCleanupHost {
                    host: self,
                    allocation,
                    resources,
                    expected_fence: fence,
                },
                fence,
            )?;
        }
        if !per_run {
            return Ok(CleanupPhysicalResult::Retained);
        }
        let container_id = allocation
            .container_id
            .as_ref()
            .ok_or("owned cleanup container identity missing")?;
        {
            let mut instances = self
                .instances
                .lock()
                .map_err(|_| "owned host lock poisoned")?;
            let instance = instances
                .get_mut(&allocation.instance_id)
                .ok_or("owned physical instance missing")?;
            instance.require_allocation(allocation, resources)?;
            if !instance.cleanup_confirmed || (instance.present && &instance.fence != fence) {
                return Err("owned physical cleanup unconfirmed".into());
            }
            instance.present = false;
        }
        self.require_absent(
            &allocation.instance_id,
            &allocation.allocation_execution_id,
            &allocation.environment_id,
            container_id,
        )?;
        Ok(CleanupPhysicalResult::Removed {
            instance_id: allocation.instance_id.clone(),
            container_id: container_id.clone(),
        })
    }

    fn require_absent(
        &self,
        instance_id: &str,
        execution_id: &str,
        environment_id: &str,
        container_id: &str,
    ) -> Result<(), String> {
        let instances = self
            .instances
            .lock()
            .map_err(|_| "owned host lock poisoned")?;
        let instance = instances
            .get(instance_id)
            .ok_or("owned physical absence identity unknown")?;
        if instance.allocation.allocation_execution_id != execution_id
            || instance.allocation.environment_id != environment_id
            || instance.allocation.container_id.as_deref() != Some(container_id)
            || instance.present
            || !instance.cleanup_confirmed
        {
            return Err("owned physical absence unconfirmed".into());
        }
        Ok(())
    }
}

impl PhysicalInstance {
    fn require_allocation(
        &self,
        allocation: &RetainedInstanceAllocation,
        resources: ResidentResourceUsage,
    ) -> Result<(), String> {
        let mut recorded = self.allocation.clone();
        recorded.reused = allocation.reused;
        if recorded != *allocation || self.resources != resources {
            return Err("owned physical allocation identity rejected".into());
        }
        Ok(())
    }
}

struct OwnedCleanupHost<'a> {
    host: &'a OwnedPhysicalHost,
    allocation: &'a RetainedInstanceAllocation,
    resources: ResidentResourceUsage,
    expected_fence: &'a EnvironmentTaskFence,
}

impl RetainedCleanupHost for OwnedCleanupHost<'_> {
    fn task_rpc(
        &self,
        instance_id: &str,
        envelope: &EnvironmentTaskEnvelope,
    ) -> Result<EnvironmentTaskResult, String> {
        envelope.validate()?;
        envelope.request.validate_authority()?;
        if instance_id != self.allocation.instance_id {
            return Err("owned cleanup instance rejected".into());
        }
        let mut instances = self
            .host
            .instances
            .lock()
            .map_err(|_| "owned host lock poisoned")?;
        let instance = instances
            .get_mut(instance_id)
            .ok_or("owned physical instance missing")?;
        instance.require_allocation(self.allocation, self.resources)?;
        if !instance.present {
            return Err("owned physical instance absent".into());
        }
        match &envelope.request {
            EnvironmentTaskRequest::Takeover {
                environment_instance_id,
                controller_owner,
                controller_epoch,
                controller_purpose,
            } => {
                let supplied = EnvironmentTaskFence {
                    environment_instance_id: environment_instance_id.clone(),
                    controller_owner: controller_owner.clone(),
                    controller_epoch: *controller_epoch,
                    controller_purpose: controller_purpose.clone(),
                };
                if &supplied != self.expected_fence
                    || supplied.controller_epoch < instance.fence.controller_epoch
                    || (supplied.controller_epoch == instance.fence.controller_epoch
                        && supplied != instance.fence)
                {
                    return Err("owned cleanup takeover fence rejected".into());
                }
                instance.fence = supplied.clone();
                Ok(EnvironmentTaskResult::TakenOver { fence: supplied })
            }
            EnvironmentTaskRequest::EndRun { fence, .. }
                if fence == self.expected_fence && fence == &instance.fence =>
            {
                instance.cleanup_confirmed = true;
                Ok(EnvironmentTaskResult::RunEnded {
                    task_ids: vec![],
                    cleanup_complete: true,
                })
            }
            EnvironmentTaskRequest::List { fence }
                if fence == self.expected_fence && fence == &instance.fence =>
            {
                Ok(EnvironmentTaskResult::Listed { tasks: vec![] })
            }
            _ => Err("empty owned host task operation unsupported".into()),
        }
    }
}

pub(crate) fn cleanup_owned_physical_execution(
    allocation: &RetainedInstanceAllocation,
    resources: ResidentResourceUsage,
    fence: &EnvironmentTaskFence,
    per_run: bool,
) -> Result<CleanupPhysicalResult, String> {
    host().cleanup(allocation, resources, fence, per_run)
}

pub(super) fn verify_cleanup(store: &PostgresRuntimeStore, run_id: &str) -> Result<Value, String> {
    if !owns_run(run_id) {
        return Err("owned cleanup proof run unknown".into());
    }
    store.with_client(|client| {
        let rows = client.query(
            "SELECT i.execution_id,i.instance_id,i.environment_id,i.allocation_execution_id,i.host_id,i.authorization_digest,c.instance_id AS receipt_instance_id,c.environment_id AS receipt_environment_id,c.authorization_digest AS receipt_authorization_digest,c.allocation_kind,c.cleanup_complete,c.physical_removed,c.removed_container_id,c.receipt_id,NOT EXISTS(SELECT 1 FROM runtime.resident_sandboxes s WHERE s.instance_id=i.instance_id OR (s.host_id=i.host_id AND s.execution_id=i.allocation_execution_id)) AS no_resident_charge FROM runtime.execution_instances i JOIN runtime.agent_run_cleanup_receipts c ON c.agent_run_id=i.agent_run_id WHERE i.agent_run_id=$1",
            &[&run_id],
        ).map_err(|error| error.to_string())?;
        if rows.is_empty() {
            return Err("owned cleanup proof allocation missing".into());
        }
        let mut allocations = Vec::new();
        for row in rows {
            let instance_id: String = row.get("instance_id");
            let environment_id: String = row.get("environment_id");
            let container_id: Option<String> = row.get("removed_container_id");
            if row.get::<_, String>("host_id") != HOST_ID
                || row.get::<_, Option<String>>("receipt_instance_id").as_ref() != Some(&instance_id)
                || row.get::<_, String>("receipt_environment_id") != environment_id
                || row.get::<_, String>("receipt_authorization_digest") != row.get::<_, String>("authorization_digest")
                || row.get::<_, String>("allocation_kind") != "instance"
                || !row.get::<_, bool>("cleanup_complete")
                || !row.get::<_, bool>("physical_removed")
                || !row.get::<_, bool>("no_resident_charge")
            {
                return Err("owned cleanup proof facts rejected".into());
            }
            let container_id = container_id.ok_or("owned cleanup proof container missing")?;
            host().require_absent(
                &instance_id,
                &row.get::<_, String>("allocation_execution_id"),
                &environment_id,
                &container_id,
            )?;
            allocations.push(json!({
                "executionId": row.get::<_, String>("execution_id"),
                "instanceId": instance_id,
                "removedContainerId": container_id,
                "allocationKind": "instance",
                "cleanupComplete": true,
                "physicalRemoved": true,
                "noResidentCharge": true,
                "physicalAbsent": true,
                "cleanupReceiptId": row.get::<_, String>("receipt_id"),
            }));
        }
        Ok(json!({"agentRunId": run_id, "allocations": allocations}))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_physical_host_requires_current_cleanup_fence_and_exact_instance() {
        let host = OwnedPhysicalHost::default();
        let resources = ResidentResourceUsage {
            sandbox_count: 1,
            memory_bytes: 1,
            cpu_milli: 1,
            pids: 1,
            workspace_bytes: 1,
        };
        let mut allocation = RetainedInstanceAllocation {
            allocation_execution_id: "owned-execution".into(),
            allocation_agent_run_id: Some("owned-run".into()),
            allocation_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "owned-run".into(),
            },
            instance_id: "owned-instance".into(),
            environment_id: "owned-environment".into(),
            host_id: HOST_ID.into(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            authorization_digest: format!("sha256:{}", "b".repeat(64)),
            binding_digest: format!("sha256:{}", "c".repeat(64)),
            profile_digest: format!("sha256:{}", "d".repeat(64)),
            container_id: None,
            reused: false,
        };
        let execute = EnvironmentTaskFence {
            environment_instance_id: allocation.instance_id.clone(),
            controller_owner: allocation.allocation_owner.clone(),
            controller_epoch: 1,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        };
        let container = host
            .prepare(&allocation, resources, execute.clone())
            .unwrap();
        allocation.container_id = Some(container.clone());
        let cleanup = EnvironmentTaskFence {
            controller_epoch: 2,
            controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
            ..execute.clone()
        };
        let mut wrong = allocation.clone();
        wrong.container_id = Some("wrong-container".into());
        assert!(host.cleanup(&wrong, resources, &cleanup, true).is_err());
        assert!(host
            .cleanup(
                &allocation,
                resources,
                &EnvironmentTaskFence {
                    controller_epoch: 1,
                    ..cleanup.clone()
                },
                true
            )
            .is_err());
        assert_eq!(host.inventory().unwrap().len(), 1);
        assert!(host
            .require_absent(
                &allocation.instance_id,
                &allocation.allocation_execution_id,
                &allocation.environment_id,
                &container
            )
            .is_err());
        assert_eq!(
            host.cleanup(&allocation, resources, &cleanup, true)
                .unwrap(),
            CleanupPhysicalResult::Removed {
                instance_id: allocation.instance_id.clone(),
                container_id: container.clone()
            }
        );
        assert!(host.inventory().unwrap().is_empty());
        assert!(host
            .cleanup(
                &allocation,
                resources,
                &EnvironmentTaskFence {
                    controller_epoch: 1,
                    ..cleanup
                },
                true,
            )
            .is_err());
        host.require_absent(
            &allocation.instance_id,
            &allocation.allocation_execution_id,
            &allocation.environment_id,
            &container,
        )
        .unwrap();
        assert!(host
            .require_absent(
                &allocation.instance_id,
                &allocation.allocation_execution_id,
                &allocation.environment_id,
                "wrong-container"
            )
            .is_err());
    }
}
