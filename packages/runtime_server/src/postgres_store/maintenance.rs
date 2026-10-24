//! Pin independently authorized maintenance against the same environment controller.
use super::PostgresRuntimeStore;
use crate::environment_authorization::maintenance::{MaintenanceAuthorization, MaintenanceBinding};
use centaeris_core::execution::{
    EnvironmentTaskFence, EnvironmentTaskOwner, EnvironmentTaskResult,
};
use sha2::{Digest, Sha256};

pub(crate) struct MaintenanceTarget {
    pub fence: EnvironmentTaskFence,
    pub profile: crate::agent_run_authorization::EnvironmentProfile,
    pub host_id: String,
    pub container_id: String,
    pub allocation_execution_id: String,
    pub allocation_agent_run_id: Option<String>,
    pub allocation_owner: centaeris_core::execution::EnvironmentTaskOwner,
    pub binding_digest: String,
    pub image_digest: String,
    pub profile_digest: String,
}

fn operation_id(binding: &MaintenanceBinding) -> Result<&str, String> {
    match &binding.controller_owner {
        EnvironmentTaskOwner::MaintenanceOperation { operation_id } => Ok(operation_id),
        _ => Err("maintenance_owner_rejected".into()),
    }
}

// All mutable authority is pinned in the API's Workspace -> Session ->
// Environment -> operation -> grant order. Allocation pins never wait behind
// a Runtime holder which may itself be waiting for the environment lock.
fn locked_target(
    tx: &mut postgres::Transaction<'_>,
    auth: &MaintenanceAuthorization,
    key: &[u8],
) -> Result<MaintenanceTarget, String> {
    let b = &auth.payload;
    let operation = operation_id(b)?;
    tx.batch_execute("SET LOCAL lock_timeout='1s'; SET LOCAL statement_timeout='5s'")
        .map_err(|_| "maintenance_store_unavailable")?;
    let workspace = tx
        .query_opt(
            "SELECT status FROM public.app_core_workspace WHERE id=$1 FOR SHARE",
            &[&b.workspace_id],
        )
        .map_err(|_| "maintenance_store_unavailable")?
        .ok_or("maintenance_workspace_missing")?;
    let session = tx.query_opt("SELECT workspace_id,current_environment_id,\"deletedAt\" IS NULL FROM public.app_core_session WHERE id=$1 FOR SHARE", &[&b.session_id])
        .map_err(|_| "maintenance_store_unavailable")?.ok_or("maintenance_session_missing")?;
    let executing = b.controller_purpose
        == centaeris_core::execution::EnvironmentTaskControllerPurpose::Execute;
    if session.get::<_, String>(0) != b.workspace_id
        || (executing
            && (workspace.get::<_, String>(0) != "active"
                || session.get::<_, Option<String>>(1).as_deref()
                    != Some(b.environment_id.as_str())
                || !session.get::<_, bool>(2)))
    {
        return Err("maintenance_session_rejected".into());
    }
    let environment = tx.query_opt("SELECT session_id,controller_run_id,controller_maintenance_operation_id,controller_epoch,controller_lease_owner,controller_purpose,controller_released,environment_profile::text,environment_profile_digest FROM public.app_core_sessionenvironment WHERE id=$1 FOR SHARE", &[&b.environment_id])
        .map_err(|_| "maintenance_store_unavailable")?.ok_or("maintenance_environment_missing")?;
    let profile: crate::agent_run_authorization::EnvironmentProfile = serde_json::from_str(
        &environment
            .get::<_, Option<String>>(7)
            .ok_or("maintenance_profile_missing")?,
    )
    .map_err(|_| "maintenance_profile_invalid")?;
    if profile.digest()? != b.profile_digest
        || environment.get::<_, Option<String>>(8).as_deref() != Some(&b.profile_digest)
    {
        return Err("maintenance_profile_rejected".into());
    }
    let mut lease_hash = Sha256::new();
    lease_hash.update(b"workspace:environment-controller-lease:v1\0");
    lease_hash.update(environment.get::<_, String>(4).as_bytes());
    let purpose = if executing { "execute" } else { "cleanup" };
    if environment.get::<_, String>(0) != b.session_id
        || environment.get::<_, Option<String>>(1).is_some()
        || environment.get::<_, Option<String>>(2).as_deref() != Some(operation)
        || environment.get::<_, i64>(3) != b.controller_epoch
        || format!("sha256:{:x}", lease_hash.finalize()) != b.controller_lease_digest
        || environment.get::<_, String>(5) != purpose
        || environment.get::<_, bool>(6)
    {
        return Err("maintenance_controller_rejected".into());
    }
    let record = tx.query_opt("SELECT workspace_id,session_id,environment_id,instance_id,container_id,host_id,allocation_execution_id,profile_digest,request_digest,current_grant_id,state,operation_kind FROM public.app_core_environmentmaintenanceoperation WHERE operation_id=$1 FOR SHARE", &[&operation])
        .map_err(|_| "maintenance_store_unavailable")?.ok_or("maintenance_operation_missing")?;
    if record.get::<_, String>(0) != b.workspace_id
        || record.get::<_, String>(1) != b.session_id
        || record.get::<_, String>(2) != b.environment_id
        || record.get::<_, String>(3) != b.instance_id
        || record.get::<_, String>(4) != b.container_id
        || record.get::<_, String>(7) != b.profile_digest
        || record.get::<_, String>(8) != b.request_digest
        || record.get::<_, String>(10) != "active"
        || record.get::<_, String>(11) != "task"
    {
        return Err("maintenance_operation_rejected".into());
    }
    let grant_id: String = record
        .get::<_, Option<String>>(9)
        .ok_or("maintenance_grant_missing")?;
    let grant = tx.query_opt("SELECT operation_id,payload::text,signature,digest FROM public.app_core_environmentmaintenancegrant WHERE id=$1 FOR SHARE", &[&grant_id])
        .map_err(|_| "maintenance_store_unavailable")?.ok_or("maintenance_grant_missing")?;
    let current: MaintenanceBinding = serde_json::from_str(&grant.get::<_, String>(1))
        .map_err(|_| "maintenance_grant_invalid")?;
    if grant.get::<_, String>(0) != operation
        || grant.get::<_, String>(2) != auth.signature
        || grant.get::<_, String>(3) != auth.payload.digest()?
    {
        return Err("maintenance_grant_rejected".into());
    }
    let origin: String = record.get(6);
    let host: String = record.get(5);
    let allocation = tx.query_opt("SELECT i.instance_id,i.environment_id,i.session_id,i.host_id,i.agent_run_id,i.authorization_digest,i.binding_digest,i.image_digest,i.profile_digest,s.container_id,s.workspace_id,s.state,i.allocation_owner_json FROM runtime.execution_instances i JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id AND s.host_id=i.host_id AND s.instance_id=i.instance_id AND s.environment_id=i.environment_id WHERE i.execution_id=$1 AND i.allocation_execution_id=i.execution_id AND i.allocation_mode='retained' AND s.allocation_mode='retained' FOR SHARE OF i,s NOWAIT", &[&origin])
        .map_err(|_| "maintenance_allocation_unavailable")?.ok_or("maintenance_allocation_missing")?;
    if allocation.get::<_, String>(0) != b.instance_id
        || allocation.get::<_, String>(1) != b.environment_id
        || allocation.get::<_, String>(2) != b.session_id
        || allocation.get::<_, String>(3) != host
        || allocation.get::<_, Option<String>>(8).as_deref() != Some(b.profile_digest.as_str())
        || allocation.get::<_, Option<String>>(9).as_deref() != Some(b.container_id.as_str())
        || allocation.get::<_, String>(10) != b.workspace_id
        || allocation.get::<_, String>(11) != "resident"
    {
        return Err("maintenance_allocation_rejected".into());
    }
    if tx.query_opt("SELECT operation_id FROM runtime.environment_retirements WHERE environment_id=$1 FOR SHARE NOWAIT", &[&b.environment_id])
        .map_err(|_| "maintenance_retirement_unavailable")?.is_some() {
        return Err("maintenance_environment_retiring".into());
    }
    // Fresh clock after every potentially waiting lock, never a pre-lock timestamp.
    let now: i64 = tx
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .map_err(|_| "maintenance_store_unavailable")?
        .get(0);
    let fence = auth.verify_current(&current, now, key)?;
    Ok(MaintenanceTarget {
        fence,
        profile,
        host_id: host,
        container_id: b.container_id.clone(),
        allocation_execution_id: origin,
        allocation_agent_run_id: allocation.get(4),
        allocation_owner: serde_json::from_str(&allocation.get::<_, String>(12))
            .map_err(|_| "allocation_owner_invalid")?,
        binding_digest: allocation.get(6),
        image_digest: allocation.get(7),
        profile_digest: b.profile_digest.clone(),
    })
}

impl PostgresRuntimeStore {
    /// The closure performs only a bounded dispatch or receipt query and must
    /// verify physical host/container facts before touching the task service.
    pub(crate) fn with_current_maintenance<T: Send>(
        &self,
        auth: &MaintenanceAuthorization,
        key: &[u8],
        action: impl FnOnce(&MaintenanceTarget) -> Result<T, String> + Send,
    ) -> Result<T, String> {
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|_| "maintenance_store_unavailable")?;
            let target = locked_target(&mut tx, auth, key)?;
            let result = action(&target)?;
            tx.commit().map_err(|_| "maintenance_store_unavailable")?;
            Ok(result)
        })
    }

    pub(crate) fn with_maintenance_completion<T: Send>(
        &self,
        auth: &MaintenanceAuthorization,
        key: &[u8],
        action: impl FnOnce(&MaintenanceTarget) -> Result<(T, EnvironmentTaskResult), String> + Send,
    ) -> Result<T, String> {
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|_| "maintenance_store_unavailable")?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let target = locked_target(&mut tx, auth, key)?;
            let (output, result) = action(&target)?;
            write_completion(&mut tx, auth, &result)?;
            tx.commit()
                .map_err(|_| "maintenance_completion_unavailable")?;
            Ok(output)
        })
    }

    /// Called only with a confirmed EndOperation result from the trusted service.
    /// An HTTP caller cannot upload a completion or release authority by assertion.
    pub(crate) fn record_maintenance_completion(
        &self,
        auth: &MaintenanceAuthorization,
        key: &[u8],
        result: &EnvironmentTaskResult,
    ) -> Result<(), String> {
        completed_operation(result)?;
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|_| "maintenance_store_unavailable")?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            locked_target(&mut tx, auth, key)?;
            write_completion(&mut tx, auth, result)?;
            tx.commit()
                .map_err(|_| "maintenance_completion_unavailable".to_string())
        })
    }
}

fn write_completion(
    tx: &mut postgres::Transaction<'_>,
    auth: &MaintenanceAuthorization,
    result: &EnvironmentTaskResult,
) -> Result<(), String> {
    let completion = completed_operation(result)?;
    let b = &auth.payload;
    let operation = operation_id(b)?;
    let values: &[&(dyn postgres::types::ToSql + Sync)] = &[
        &operation,
        &b.environment_id,
        &b.instance_id,
        &b.container_id,
        &b.profile_digest,
        &b.controller_epoch,
        &b.controller_lease_digest,
        &b.request_digest,
        &completion,
    ];
    tx.execute("INSERT INTO runtime.environment_maintenance_completions(operation_id,environment_id,instance_id,container_id,profile_digest,controller_epoch,controller_lease_digest,request_digest,completion_json,recorded_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint) ON CONFLICT(operation_id) DO NOTHING", values)
        .map_err(|_| "maintenance_completion_unavailable")?;
    let matched: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.environment_maintenance_completions WHERE operation_id=$1 AND environment_id=$2 AND instance_id=$3 AND container_id=$4 AND profile_digest=$5 AND controller_epoch=$6 AND controller_lease_digest=$7 AND request_digest=$8 AND completion_json=$9)", values)
        .map_err(|_| "maintenance_completion_unavailable")?.get(0);
    if !matched {
        return Err("maintenance_completion_conflict".into());
    }
    Ok(())
}

pub(super) fn completed_operation(result: &EnvironmentTaskResult) -> Result<String, String> {
    let EnvironmentTaskResult::OperationEnded {
        task_ids,
        cleanup_complete: true,
    } = result
    else {
        return Err("maintenance_completion_unconfirmed".into());
    };
    let ids: std::collections::BTreeSet<_> = task_ids.iter().collect();
    if ids.len() != task_ids.len()
        || task_ids.len() > centaeris_core::execution::ENVIRONMENT_TASK_MAX_RECORDS
        || task_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 160 || id.chars().any(char::is_control))
    {
        return Err("maintenance_completion_invalid".into());
    }
    serde_json::to_string(result).map_err(|_| "maintenance_completion_invalid".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_confirmed_operation_completion_can_release_authority() {
        let result = EnvironmentTaskResult::OperationEnded {
            task_ids: vec!["task-original".into()],
            cleanup_complete: true,
        };
        let receipt = completed_operation(&result).unwrap();
        assert_eq!(
            serde_json::from_str::<EnvironmentTaskResult>(&receipt).unwrap(),
            result
        );
        for rejected in [
            EnvironmentTaskResult::RunEnded {
                task_ids: vec![],
                cleanup_complete: true,
            },
            EnvironmentTaskResult::OperationEnded {
                task_ids: vec!["task-original".into()],
                cleanup_complete: false,
            },
            EnvironmentTaskResult::OperationEnded {
                task_ids: vec!["task-original".into(), "task-original".into()],
                cleanup_complete: true,
            },
        ] {
            assert!(completed_operation(&rejected).is_err());
        }
    }
}
