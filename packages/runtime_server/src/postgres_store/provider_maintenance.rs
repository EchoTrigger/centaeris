//! Pin provider maintenance to durable controller, target, and consumer facts.
use super::provider_operations::cleanup::{ProviderExecutionDispatchError, ProviderVerifiedTarget};
use super::PostgresRuntimeStore;
use crate::agent_run_authorization::{EnvironmentProfile, WorkspaceAgentRunAuthorization};
use crate::environment_authorization::provider_maintenance::{
    ProviderMaintenanceAuthorization, ProviderMaintenanceBinding, ProviderMaintenanceTarget,
};
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskOwner, EnvironmentTaskResult,
};
use sha2::{Digest, Sha256};

pub(crate) struct ProviderMaintenanceDispatchTarget {
    pub profile: EnvironmentProfile,
    pub target: ProviderVerifiedTarget,
}

fn operation_id(binding: &ProviderMaintenanceBinding) -> Result<&str, String> {
    match &binding.controller_owner {
        EnvironmentTaskOwner::MaintenanceOperation { operation_id } => Ok(operation_id),
        _ => Err("provider_maintenance_owner_rejected".into()),
    }
}

fn clock(tx: &mut postgres::Transaction<'_>) -> Result<i64, String> {
    tx.query_one(
        "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
        &[],
    )
    .map(|r| r.get(0))
    .map_err(|_| "provider_maintenance_store_unavailable".into())
}

fn lease_digest(owner: &str) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(
            [
                b"workspace:environment-controller-lease:v1\0".as_slice(),
                owner.as_bytes()
            ]
            .concat()
        )
    )
}

fn locked_target(
    tx: &mut postgres::Transaction<'_>,
    auth: &ProviderMaintenanceAuthorization,
    key: &[u8],
    configured_digest: &str,
) -> Result<ProviderMaintenanceDispatchTarget, String> {
    let b = &auth.payload;
    b.digest()?;
    let operation = operation_id(b)?;
    tx.batch_execute("SET LOCAL lock_timeout='1s'; SET LOCAL statement_timeout='5s'")
        .map_err(|_| "provider_maintenance_store_unavailable")?;
    let workspace = tx
        .query_opt(
            "SELECT status FROM public.app_core_workspace WHERE id=$1 FOR SHARE",
            &[&b.workspace_id],
        )
        .map_err(|_| "provider_maintenance_store_unavailable")?
        .ok_or("provider_maintenance_workspace_missing")?;
    let session = tx.query_opt("SELECT workspace_id,current_environment_id,\"deletedAt\" IS NULL FROM public.app_core_session WHERE id=$1 FOR SHARE",
        &[&b.session_id]).map_err(|_| "provider_maintenance_store_unavailable")?.ok_or("provider_maintenance_session_missing")?;
    let executing = b.controller_purpose == EnvironmentTaskControllerPurpose::Execute;
    if session.get::<_, String>(0) != b.workspace_id
        || (executing
            && (workspace.get::<_, String>(0) != "active"
                || session.get::<_, Option<String>>(1).as_deref()
                    != Some(b.environment_id.as_str())
                || !session.get::<_, bool>(2)))
    {
        return Err("provider_maintenance_session_rejected".into());
    }
    let environment = tx.query_opt("SELECT session_id,controller_run_id,controller_maintenance_operation_id,controller_epoch,controller_lease_owner,controller_purpose,controller_released,environment_profile::text,environment_profile_digest FROM public.app_core_sessionenvironment WHERE id=$1 FOR SHARE",
        &[&b.environment_id]).map_err(|_| "provider_maintenance_store_unavailable")?.ok_or("provider_maintenance_environment_missing")?;
    let purpose = match environment.get::<_, String>(5).as_str() {
        "execute" => EnvironmentTaskControllerPurpose::Execute,
        "cleanup" => EnvironmentTaskControllerPurpose::Cleanup,
        _ => return Err("provider_maintenance_controller_rejected".into()),
    };
    if environment.get::<_, String>(0) != b.session_id
        || environment.get::<_, Option<String>>(1).is_some()
        || environment.get::<_, Option<String>>(2).as_deref() != Some(operation)
        || environment.get::<_, bool>(6)
    {
        return Err("provider_maintenance_controller_rejected".into());
    }
    let profile: EnvironmentProfile = serde_json::from_str(
        &environment
            .get::<_, Option<String>>(7)
            .ok_or("provider_maintenance_profile_missing")?,
    )
    .map_err(|_| "provider_maintenance_profile_invalid")?;
    profile.validate()?;
    let EnvironmentProfile::E2b(provider) = &profile else {
        return Err("provider_maintenance_profile_rejected".into());
    };
    let profile_digest = profile.digest()?;
    if provider.lifetime != "retained"
        || provider.provider_config_digest != configured_digest
        || environment.get::<_, Option<String>>(8).as_deref() != Some(profile_digest.as_str())
    {
        return Err("provider_maintenance_profile_rejected".into());
    }
    let record = tx.query_opt("SELECT workspace_id,session_id,environment_id,instance_id,container_id,host_id,allocation_execution_id,profile_digest,request_digest,current_grant_id,state,operation_kind,provider_target::text,request::text FROM public.app_core_environmentmaintenanceoperation WHERE operation_id=$1 FOR SHARE",
        &[&operation]).map_err(|_| "provider_maintenance_store_unavailable")?.ok_or("provider_maintenance_operation_missing")?;
    let provider_target: ProviderMaintenanceTarget = serde_json::from_str(
        &record
            .get::<_, Option<String>>(12)
            .ok_or("provider_maintenance_target_missing")?,
    )
    .map_err(|_| "provider_maintenance_target_invalid")?;
    let request: serde_json::Value = serde_json::from_str(&record.get::<_, String>(13))
        .map_err(|_| "provider_maintenance_request_invalid")?;
    let encoded = crate::agent_run_authorization::canonical_json(request.clone())?;
    if request.as_object().is_none_or(|v| v.is_empty())
        || encoded.len() > 64 * 1024
        || format!("sha256:{:x}", Sha256::digest(encoded.as_bytes())) != record.get::<_, String>(8)
        || record.get::<_, String>(0) != b.workspace_id
        || record.get::<_, String>(1) != b.session_id
        || record.get::<_, String>(2) != b.environment_id
        || (3..7).any(|i| record.get::<_, Option<String>>(i).is_some())
        || record.get::<_, String>(7) != profile_digest
        || record.get::<_, String>(10) != "active"
        || record.get::<_, String>(11) != "task"
    {
        return Err("provider_maintenance_operation_rejected".into());
    }
    let grant_id = record
        .get::<_, Option<String>>(9)
        .ok_or("provider_maintenance_grant_missing")?;
    let grant = tx.query_opt("SELECT operation_id,payload::text,signature,digest,controller_epoch,controller_lease_digest,controller_purpose,expires_at_ms,previous_controller_epoch FROM public.app_core_environmentmaintenancegrant WHERE id=$1 FOR SHARE",
        &[&grant_id]).map_err(|_| "provider_maintenance_store_unavailable")?.ok_or("provider_maintenance_grant_missing")?;
    let stored: ProviderMaintenanceBinding = serde_json::from_str(&grant.get::<_, String>(1))
        .map_err(|_| "provider_maintenance_grant_invalid")?;
    let current = ProviderMaintenanceBinding {
        schema: stored.schema.clone(),
        operation_kind: record.get(11),
        workspace_id: record.get(0),
        session_id: record.get(1),
        environment_id: record.get(2),
        provider_target,
        profile_digest: record.get(7),
        controller_owner: EnvironmentTaskOwner::MaintenanceOperation {
            operation_id: operation.into(),
        },
        controller_epoch: environment.get(3),
        controller_lease_digest: lease_digest(&environment.get::<_, String>(4)),
        controller_purpose: purpose,
        request_digest: record.get(8),
        expires_at_ms: grant.get(7),
    };
    let purpose_name = if current.controller_purpose == EnvironmentTaskControllerPurpose::Execute {
        "execute"
    } else {
        "cleanup"
    };
    if stored != current
        || grant.get::<_, String>(0) != operation
        || grant.get::<_, String>(2) != auth.signature
        || grant.get::<_, String>(3) != current.digest()?
        || grant.get::<_, i64>(4) != current.controller_epoch
        || grant.get::<_, String>(5) != current.controller_lease_digest
        || grant.get::<_, String>(6) != purpose_name
        || grant.get::<_, i64>(8) < 0
        || grant.get::<_, i64>(8).checked_add(1) != Some(current.controller_epoch)
    {
        return Err("provider_maintenance_grant_rejected".into());
    }
    let anchor = tx.query_opt("SELECT creation_attempt_id FROM runtime.provider_environment_bindings WHERE environment_id=$1 FOR SHARE NOWAIT",
        &[&current.environment_id]).map_err(|_| "provider_maintenance_target_unavailable")?.ok_or("provider_maintenance_target_missing")?;
    if anchor.get::<_, String>(0) != current.provider_target.creation_attempt_id {
        return Err("provider_maintenance_target_rejected".into());
    }
    let creation = tx.query_opt("SELECT dispatch_state,settlement_id,verified_at_ms,reported_sandbox_id,verified_sandbox_id,verification_digest,provider FROM runtime.provider_creation_operations WHERE attempt_id=$1 FOR SHARE NOWAIT",
        &[&current.provider_target.creation_attempt_id]).map_err(|_| "provider_maintenance_target_unavailable")?.ok_or("provider_maintenance_target_missing")?;
    let target =
        super::provider_operations::retained::load_environment_target(tx, &current.environment_id)?;
    let i = &target.identity;
    if i.environment_id != current.environment_id
        || i.session_id != current.session_id
        || i.workspace_id != current.workspace_id
        || i.profile_digest != current.profile_digest
        || i.provider_config_digest != configured_digest
        || i.instance_id != super::execution_instances::instance_id(&i.execution_id)
        || current.provider_target.provider != "e2b"
        || current.provider_target.creation_attempt_id != i.attempt_id
        || current.provider_target.instance_id != i.instance_id
        || current.provider_target.sandbox_id != target.sandbox_id
        || current.provider_target.verification_digest != target.verification_digest
        || current.provider_target.provider_config_digest != i.provider_config_digest
        || creation.get::<_, String>(0) != "dispatched"
        || creation.get::<_, Option<String>>(1).is_some()
        || creation.get::<_, Option<i64>>(2).is_none_or(|v| v < 0)
        || creation.get::<_, Option<String>>(3).as_deref() != Some(target.sandbox_id.as_str())
        || creation.get::<_, Option<String>>(4).as_deref() != Some(target.sandbox_id.as_str())
        || creation.get::<_, Option<String>>(5).as_deref()
            != Some(target.verification_digest.as_str())
        || creation.get::<_, String>(6) != "e2b"
    {
        return Err("provider_maintenance_target_rejected".into());
    }
    if tx.query_opt("SELECT operation_id FROM runtime.provider_environment_retirements WHERE environment_id=$1 FOR SHARE NOWAIT",
        &[&current.environment_id]).map_err(|_| "provider_maintenance_retirement_unavailable")?.is_some() {
        return Err("provider_maintenance_environment_retiring".into());
    }
    tx.query("SELECT operation_id FROM runtime.provider_environment_power_operations WHERE environment_id=$1 FOR SHARE NOWAIT",
        &[&current.environment_id]).map_err(|_| "provider_maintenance_power_unavailable")?;
    if tx.query_opt("SELECT blocked FROM runtime.provider_environment_power_admission WHERE environment_id=$1",
        &[&current.environment_id]).map_err(|_| "provider_maintenance_power_unavailable")?.is_some_and(|r| r.get::<_,bool>(0)) {
        return Err("provider_maintenance_power_unavailable".into());
    }
    require_original_authority(tx, &target, &profile, key)?;
    super::provider_operations::retained::require_maintenance_handoff(tx, &target)?;
    // A timestamp taken before any of the waits above cannot prove authority.
    auth.verify_current(&current, clock(tx)?, key)?;
    Ok(ProviderMaintenanceDispatchTarget { profile, target })
}

fn require_original_authority(
    tx: &mut postgres::Transaction<'_>,
    target: &ProviderVerifiedTarget,
    profile: &EnvironmentProfile,
    key: &[u8],
) -> Result<(), String> {
    let i = &target.identity;
    match &i.owner {
        EnvironmentTaskOwner::MaintenanceOperation { operation_id } => {
            let original=tx.query_opt("SELECT o.workspace_id,o.session_id,o.environment_id,o.profile_digest,o.request::text,o.request_digest,o.state,g.payload::text,g.signature,g.digest FROM public.app_core_environmentmaintenanceoperation o JOIN public.app_core_environmentmaintenancegrant g ON g.operation_id=o.operation_id AND g.digest=$2 WHERE o.operation_id=$1 AND o.operation_kind='provision' FOR SHARE OF o,g NOWAIT",
                &[&operation_id,&i.authorization_digest]).map_err(|_|"provider_maintenance_origin_unavailable")?.ok_or("provider_maintenance_origin_missing")?;
            let payload: crate::environment_authorization::provision::ProvisionBinding =
                serde_json::from_str(&original.get::<_, String>(7))
                    .map_err(|_| "provider_maintenance_origin_invalid")?;
            let source = crate::environment_authorization::provision::ProvisionAuthorization {
                payload,
                signature: original.get(8),
            };
            let request = serde_json::from_str(&original.get::<_, String>(4))
                .map_err(|_| "provider_maintenance_origin_invalid")?;
            source.payload.validate_request(&request)?;
            source.verify_signature(key)?;
            if source.digest()? != i.authorization_digest
                || original.get::<_, String>(9) != i.authorization_digest
                || source.payload.operation_id != *operation_id
                || source.payload.environment_id != i.environment_id
                || source.payload.session_id != i.session_id
                || source.payload.workspace_id != i.workspace_id
                || source.payload.environment_profile != *profile
                || source.payload.environment_profile_digest != i.profile_digest
                || original.get::<_, String>(0) != i.workspace_id
                || original.get::<_, String>(1) != i.session_id
                || original.get::<_, String>(2) != i.environment_id
                || original.get::<_, String>(3) != i.profile_digest
                || original.get::<_, String>(5) != source.payload.request_digest
                || original.get::<_, String>(6) != "released"
            {
                return Err("provider_maintenance_origin_rejected".into());
            }
        }
        EnvironmentTaskOwner::AgentRun { run_id } => {
            let original=tx.query_opt("SELECT r.workspace_id,r.session_id,r.environment_id,g.payload::text,g.signature,g.digest FROM public.app_core_agentrun r JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id WHERE r.id=$1 FOR SHARE OF r,g NOWAIT",
                &[&run_id]).map_err(|_|"provider_maintenance_origin_unavailable")?.ok_or("provider_maintenance_origin_missing")?;
            let source: WorkspaceAgentRunAuthorization =
                serde_json::from_str(&original.get::<_, String>(3))
                    .map_err(|_| "provider_maintenance_origin_invalid")?;
            source.checked_binding(run_id)?.verify(
                &i.authorization_digest,
                key,
                &original.get::<_, String>(4),
            )?;
            if source.environment_profile != *profile
                || source.environment_profile_digest != i.profile_digest
                || source.workspace_id != i.workspace_id
                || source.session_id != i.session_id
                || original.get::<_, String>(0) != i.workspace_id
                || original.get::<_, String>(1) != i.session_id
                || original.get::<_, Option<String>>(2).as_deref()
                    != Some(i.environment_id.as_str())
                || original.get::<_, String>(5) != i.authorization_digest
            {
                return Err("provider_maintenance_origin_rejected".into());
            }
        }
    }
    Ok(())
}

impl PostgresRuntimeStore {
    pub(crate) fn with_current_provider_maintenance<T: Send, E: Send>(
        &self,
        auth: &ProviderMaintenanceAuthorization,
        key: &[u8],
        configured_digest: &str,
        action: impl FnOnce(&ProviderMaintenanceDispatchTarget) -> Result<T, E> + Send,
    ) -> Result<T, ProviderExecutionDispatchError<E>> {
        self.dispatch_provider_maintenance(auth, key, configured_digest, |target| {
            action(target).map(|value| (value, None))
        })
    }

    #[cfg(test)]
    pub(crate) fn with_provider_maintenance_completion<T: Send, E: Send>(
        &self,
        auth: &ProviderMaintenanceAuthorization,
        key: &[u8],
        configured_digest: &str,
        action: impl FnOnce(&ProviderMaintenanceDispatchTarget) -> Result<(T, EnvironmentTaskResult), E>
            + Send,
    ) -> Result<T, ProviderExecutionDispatchError<E>> {
        self.dispatch_provider_maintenance(auth, key, configured_digest, |target| {
            action(target).map(|(value, result)| (value, Some(result)))
        })
    }

    fn dispatch_provider_maintenance<T: Send, E: Send>(
        &self,
        auth: &ProviderMaintenanceAuthorization,
        key: &[u8],
        configured_digest: &str,
        action: impl FnOnce(
                &ProviderMaintenanceDispatchTarget,
            ) -> Result<(T, Option<EnvironmentTaskResult>), E>
            + Send,
    ) -> Result<T, ProviderExecutionDispatchError<E>> {
        let mut started = false;
        let result: Result<T, ProviderExecutionDispatchError<E>> =
            self.with_client_error(|client| {
                let mut tx = client.transaction().map_err(|e| e.to_string())?;
                super::resource_commit::require_durable_resource_commit(&mut tx)?;
                let target = locked_target(&mut tx, auth, key, configured_digest)?;
                started = true;
                let (output, completion) =
                    action(&target).map_err(ProviderExecutionDispatchError::Callback)?;
                locked_target(&mut tx, auth, key, configured_digest)
                    .map_err(ProviderExecutionDispatchError::AfterDispatch)?;
                if let Some(completion) = completion {
                    write_completion(&mut tx, auth, &completion)
                        .map_err(ProviderExecutionDispatchError::AfterDispatch)?;
                }
                tx.commit()
                    .map_err(|e| ProviderExecutionDispatchError::AfterDispatch(e.to_string()))?;
                Ok(output)
            });
        match result {
            Err(ProviderExecutionDispatchError::Rejected(error)) if started => {
                Err(ProviderExecutionDispatchError::AfterDispatch(error))
            }
            other => other,
        }
    }

    /// Trusted Core EndOperation results enter here. The public HTTP body is
    /// never accepted as completion evidence.
    #[cfg(test)]
    pub(crate) fn record_provider_maintenance_completion(
        &self,
        auth: &ProviderMaintenanceAuthorization,
        key: &[u8],
        configured_digest: &str,
        result: &EnvironmentTaskResult,
    ) -> Result<(), String> {
        super::maintenance::completed_operation(result)?;
        self.with_provider_maintenance_completion(auth, key, configured_digest, |_| {
            Ok::<_, String>(((), result.clone()))
        })
        .map_err(|error| match error {
            ProviderExecutionDispatchError::Rejected(e)
            | ProviderExecutionDispatchError::AfterDispatch(e)
            | ProviderExecutionDispatchError::Callback(e) => e,
        })
    }
}

fn write_completion(
    tx: &mut postgres::Transaction<'_>,
    auth: &ProviderMaintenanceAuthorization,
    result: &EnvironmentTaskResult,
) -> Result<(), String> {
    let completion = super::maintenance::completed_operation(result)?;
    let b = &auth.payload;
    let t = &b.provider_target;
    let operation = operation_id(b)?;
    let values: &[&(dyn postgres::types::ToSql + Sync)] = &[
        &operation,
        &b.environment_id,
        &t.creation_attempt_id,
        &t.instance_id,
        &t.sandbox_id,
        &b.profile_digest,
        &t.provider_config_digest,
        &t.verification_digest,
        &b.controller_epoch,
        &b.controller_lease_digest,
        &b.request_digest,
        &completion,
    ];
    tx.execute("INSERT INTO runtime.provider_environment_maintenance_completions(operation_id,environment_id,creation_attempt_id,instance_id,sandbox_id,profile_digest,provider_config_digest,verification_digest,controller_epoch,controller_lease_digest,request_digest,completion_json,recorded_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint) ON CONFLICT(operation_id) DO NOTHING",values).map_err(|_|"provider_maintenance_completion_unavailable")?;
    let matched:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.provider_environment_maintenance_completions WHERE operation_id=$1 AND environment_id=$2 AND creation_attempt_id=$3 AND instance_id=$4 AND sandbox_id=$5 AND profile_digest=$6 AND provider_config_digest=$7 AND verification_digest=$8 AND controller_epoch=$9 AND controller_lease_digest=$10 AND request_digest=$11 AND completion_json=$12)",values).map_err(|_|"provider_maintenance_completion_unavailable")?.get(0);
    if !matched {
        return Err("provider_maintenance_completion_conflict".into());
    }
    Ok(())
}

pub(crate) mod direct;
