//! Maintenance-owned provider create responsibility; no synthetic Run or Job.
//! API ownership is checked under current-authority locks rather than a cross-schema FK.
//! Runtime responsibility survives API source retention; source absence cannot settle it.
use super::cleanup::{ProviderSandboxObservation, ProviderVerifiedTarget};
use super::*;
use crate::environment_authorization::provision::ProvisionAuthorization;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderProvisionCompletion {
    pub operation_id: String,
    pub creation_attempt_id: String,
    pub environment_id: String,
    pub session_id: String,
    pub workspace_id: String,
    pub instance_id: String,
    pub sandbox_id: String,
    pub profile_digest: String,
    pub provider_config_digest: String,
    pub authorization_digest: String,
    pub controller_epoch: i64,
    pub controller_lease_digest: String,
    pub request_digest: String,
    pub preparation_digest: String,
    pub recorded_at_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderProvisionPreparation {
    pub instance_id: String,
    pub sandbox_id: String,
    pub preparation_digest: String,
}

fn require_authority(
    tx: &mut postgres::Transaction<'_>,
    auth: &ProvisionAuthorization,
    key: &[u8],
    config: &str,
) -> Result<(), String> {
    crate::postgres_store::provision::locked(tx, auth, key)?;
    let EnvironmentProfile::E2b(profile) = &auth.payload.environment_profile else {
        return Err("provider_provision_requires_e2b_profile".into());
    };
    if profile.lifetime != "retained" || profile.provider_config_digest != config {
        return Err("provider_provision_policy_rejected".into());
    }
    Ok(())
}

fn require_scope(
    auth: &ProvisionAuthorization,
    identity: &ProviderCreationIdentity,
) -> Result<(), String> {
    let b = &auth.payload;
    if identity.owner != b.controller_owner
        || identity.processing_job_id.is_some()
        || identity.environment_id != b.environment_id
        || identity.session_id != b.session_id
        || identity.workspace_id != b.workspace_id
        || identity.profile_digest != b.environment_profile_digest
        || identity.authorization_digest != auth.digest()?
    {
        return Err("provider_provision_origin_rejected".into());
    }
    origin::owner_metadata(&identity.owner, None)?;
    Ok(())
}

impl PostgresRuntimeStore {
    pub(crate) fn with_provider_provision_execution_dispatch<T: Send, E: Send>(
        &self,
        auth: &ProvisionAuthorization,
        key: &[u8],
        identity: &ProviderCreationIdentity,
        configured_digest: &str,
        dispatch: impl FnOnce(&ProviderVerifiedTarget) -> Result<T, E> + Send,
    ) -> Result<T, cleanup::ProviderExecutionDispatchError<E>> {
        use crate::observations::provider_dispatch::{
            DispatchKind, DispatchMetric, DispatchObservation,
        };
        use cleanup::ProviderExecutionDispatchError;
        let mut observation = DispatchObservation::new(DispatchKind::RetainedExecution);
        let mut started = false;
        let result = self.with_client_error(|client| {
            observation.entered_client();
            let result = (|| {
                let (mut tx, target) =
                    observation.timed(DispatchMetric::AuthorityBefore, || {
                        let mut tx = client.transaction().map_err(|e| e.to_string())?;
                        super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
                        require_authority(&mut tx, auth, key, configured_digest)?;
                        require_scope(auth, identity)?;
                        if identity.provider_config_digest != configured_digest {
                            return Err(ProviderExecutionDispatchError::Rejected(
                                "provider_provision_config_rejected".into(),
                            ));
                        }
                        let target = retained::require_provision_target(&mut tx, identity)?;
                        Ok::<_, ProviderExecutionDispatchError<E>>((tx, target))
                    })?;
                started = true;
                let result = observation.callback(|| {
                    dispatch(&target).map_err(ProviderExecutionDispatchError::Callback)
                })?;
                observation.timed(DispatchMetric::AuthorityAfter, || {
                    require_authority(&mut tx, auth, key, configured_digest)
                        .map_err(ProviderExecutionDispatchError::AfterDispatch)?;
                    require_scope(auth, identity)
                        .map_err(ProviderExecutionDispatchError::AfterDispatch)?;
                    let after = retained::require_provision_target(&mut tx, identity)
                        .map_err(ProviderExecutionDispatchError::AfterDispatch)?;
                    if after != target {
                        return Err(ProviderExecutionDispatchError::AfterDispatch(
                            "provider_provision_target_changed".into(),
                        ));
                    }
                    Ok(())
                })?;
                observation.timed(DispatchMetric::Commit, || {
                    tx.commit()
                        .map_err(|e| ProviderExecutionDispatchError::AfterDispatch(e.to_string()))
                })?;
                Ok(result)
            })();
            observation.leaving_client();
            result
        });
        observation.finish(result.is_ok());
        match result {
            Err(ProviderExecutionDispatchError::Rejected(error)) if started => {
                Err(ProviderExecutionDispatchError::AfterDispatch(error))
            }
            other => other,
        }
    }

    pub(crate) fn prepare_provider_provision_creation(
        &self,
        auth: &ProvisionAuthorization,
        key: &[u8],
        request: &ProviderCreationRequest<'_>,
    ) -> Result<ProviderCreationOperation, String> {
        for value in [
            request.execution_id,
            request.attempt_id,
            request.idempotency_key,
        ] {
            validate_identity(value)?;
        }
        validate_digest(request.request_digest)?;
        self.with_client(|client| {
            let mut tx=client.transaction().map_err(|e|e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            require_authority(&mut tx,auth,key,request.configured_provider_config_digest)?;
            let b=&auth.payload;
            let identity=ProviderCreationIdentity {
                attempt_id:request.attempt_id.into(),execution_id:request.execution_id.into(),instance_id:super::super::execution_instances::instance_id(request.execution_id),
                environment_id:b.environment_id.clone(),session_id:b.session_id.clone(),workspace_id:b.workspace_id.clone(),owner:b.controller_owner.clone(),
                authorization_digest:auth.digest()?,profile_digest:b.environment_profile_digest.clone(),provider_config_digest:request.configured_provider_config_digest.into(),
                request_digest:request.request_digest.into(),idempotency_key:request.idempotency_key.into(),processing_job_id:None,
            };
            if let Some(existing)=load(&mut tx,request.attempt_id,true)? {
                require_identity(&existing,&identity)?;retained::bind_creation(&mut tx,&identity)?;
                tx.commit().map_err(|e|e.to_string())?;return Ok(existing);
            }
            let owner=serde_json::to_string(&identity.owner).map_err(|_|"provider_creation_owner_invalid")?;
            tx.execute("INSERT INTO runtime.provider_creation_operations(attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,agent_run_id,authorization_digest,profile_digest,provider,provider_config_digest,request_digest,idempotency_key,processing_job_id,dispatch_state,recorded_at_ms,creation_owner_json,maintenance_operation_id) VALUES($1,$2,$3,$4,$5,$6,NULL,$7,$8,'e2b',$9,$10,$11,NULL,'prepared',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,$12,$13)",&[&identity.attempt_id,&identity.execution_id,&identity.instance_id,&identity.environment_id,&identity.session_id,&identity.workspace_id,&identity.authorization_digest,&identity.profile_digest,&identity.provider_config_digest,&identity.request_digest,&identity.idempotency_key,&owner,&b.operation_id]).map_err(|_|"provider_provision_identity_or_open_operation_conflict")?;
            retained::bind_creation(&mut tx,&identity)?;
            let operation=load(&mut tx,&identity.attempt_id,false)?.ok_or("provider_creation_missing")?;
            tx.commit().map_err(|e|e.to_string())?;Ok(operation)
        })
    }

    #[cfg(test)]
    pub(crate) fn dispatch_provider_provision_creation(
        &self,
        auth: &ProvisionAuthorization,
        key: &[u8],
        identity: &ProviderCreationIdentity,
    ) -> Result<FreshProviderCreationDispatchPermit, String> {
        self.dispatch_provider_provision_creation_limited(auth, key, identity, None)
    }
    pub(crate) fn dispatch_provider_provision_creation_limited(
        &self,
        auth: &ProvisionAuthorization,
        key: &[u8],
        identity: &ProviderCreationIdentity,
        limit: Option<u32>,
    ) -> Result<FreshProviderCreationDispatchPermit, String> {
        self.with_client(|client| {
            let mut tx=client.transaction().map_err(|e|e.to_string())?;super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            require_authority(&mut tx,auth,key,&identity.provider_config_digest)?;require_scope(auth,identity)?;
            let operation=load(&mut tx,&identity.attempt_id,true)?.ok_or("provider_creation_missing")?;require_identity(&operation,identity)?;
            if operation.dispatch_state!=ProviderCreationDispatchState::Prepared {return Err("provider_creation_dispatch_not_authorized".into());}
            // Operation-row waits must not revive an expired authority.
            require_authority(&mut tx,auth,key,&identity.provider_config_digest)?;
            capacity::require(&mut tx,limit,&identity.provider_config_digest)?;
            if tx.execute("UPDATE runtime.provider_creation_operations SET dispatch_state='dispatched' WHERE attempt_id=$1 AND dispatch_state='prepared'",&[&identity.attempt_id]).map_err(|e|e.to_string())?!=1 {return Err("provider_creation_dispatch_not_authorized".into());}
            let operation=load(&mut tx,&identity.attempt_id,false)?.ok_or("provider_creation_missing")?;
            tx.commit().map_err(|e|e.to_string())?;Ok(FreshProviderCreationDispatchPermit{operation})
        })
    }

    pub(crate) fn close_provider_provision_creation_before_dispatch(
        &self,
        auth: &ProvisionAuthorization,
        key: &[u8],
        identity: &ProviderCreationIdentity,
    ) -> Result<ProviderCreationOperation, String> {
        self.with_execution_control_client(|client| {
            let mut tx=client.transaction().map_err(|e|e.to_string())?;super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            require_authority(&mut tx,auth,key,&identity.provider_config_digest)?;require_scope(auth,identity)?;
            let operation=load(&mut tx,&identity.attempt_id,true)?.ok_or("provider_creation_missing")?;require_identity(&operation,identity)?;
            if operation.dispatch_state==ProviderCreationDispatchState::Dispatched {return Err("provider_creation_not_dispatched_unproven".into());}
            if operation.dispatch_state==ProviderCreationDispatchState::Prepared {tx.execute("UPDATE runtime.provider_creation_operations SET dispatch_state='notDispatched' WHERE attempt_id=$1 AND dispatch_state='prepared'",&[&identity.attempt_id]).map_err(|e|e.to_string())?;}
            let result=load(&mut tx,&identity.attempt_id,false)?.ok_or("provider_creation_missing")?;tx.commit().map_err(|e|e.to_string())?;Ok(result)
        })
    }

    pub(crate) fn record_provider_provision_target(
        &self,
        auth: &ProvisionAuthorization,
        key: &[u8],
        identity: &ProviderCreationIdentity,
        observation: &ProviderSandboxObservation,
    ) -> Result<ProviderVerifiedTarget, String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            require_authority(&mut tx, auth, key, &identity.provider_config_digest)?;
            require_scope(auth, identity)?;
            let operation =
                load(&mut tx, &identity.attempt_id, true)?.ok_or("provider_creation_missing")?;
            require_identity(&operation, identity)?;
            let EnvironmentProfile::E2b(profile) = &auth.payload.environment_profile else {
                return Err("provider_provision_requires_e2b_profile".into());
            };
            if operation.dispatch_state != ProviderCreationDispatchState::Dispatched
                || cleanup::load_settlement(&mut tx, &identity.attempt_id)?.is_some()
                || operation.reported_sandbox_id.as_deref() != Some(observation.sandbox_id.as_str())
                || observation.template_id != profile.template_ref.template_id
                || observation.metadata != cleanup::expected_metadata(identity)
            {
                return Err("provider_provision_observation_rejected".into());
            }
            validate_identity(&observation.sandbox_id)?;
            let target = ProviderVerifiedTarget {
                identity: identity.clone(),
                sandbox_id: observation.sandbox_id.clone(),
                verification_digest: cleanup::target_verification_digest(
                    identity,
                    &observation.template_id,
                    &identity.provider_config_digest,
                    &observation.sandbox_id,
                )?,
            };
            require_authority(&mut tx, auth, key, &identity.provider_config_digest)?;
            let target = cleanup::persist_verified_target(&mut tx, &target)?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(target)
        })
    }

    pub(crate) fn complete_provider_provision(
        &self,
        auth: &ProvisionAuthorization,
        key: &[u8],
        target: &ProviderVerifiedTarget,
        preparation: &ProviderProvisionPreparation,
    ) -> Result<ProviderProvisionCompletion, String> {
        validate_digest(&preparation.preparation_digest)?;
        if preparation.instance_id != target.identity.instance_id
            || preparation.sandbox_id != target.sandbox_id
        {
            return Err("provider_provision_preparation_target_rejected".into());
        }
        self.with_client(|client| {
            let mut tx=client.transaction().map_err(|e|e.to_string())?;super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            require_authority(&mut tx,auth,key,&target.identity.provider_config_digest)?;require_scope(auth,&target.identity)?;
            let operation=load(&mut tx,&target.identity.attempt_id,true)?.ok_or("provider_creation_missing")?;require_identity(&operation,&target.identity)?;
            if operation.dispatch_state!=ProviderCreationDispatchState::Dispatched || cleanup::load_settlement(&mut tx,&target.identity.attempt_id)?.is_some() || cleanup::load_target(&mut tx,&target.identity)?.as_ref()!=Some(target) {return Err("provider_provision_target_unavailable".into());}
            require_authority(&mut tx,auth,key,&target.identity.provider_config_digest)?;
            let b=&auth.payload;let i=&target.identity;
            tx.execute("INSERT INTO runtime.provider_environment_provision_completions(operation_id,creation_attempt_id,environment_id,session_id,workspace_id,instance_id,sandbox_id,profile_digest,provider_config_digest,authorization_digest,controller_epoch,controller_lease_digest,request_digest,preparation_digest,recorded_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint) ON CONFLICT(operation_id) DO NOTHING",&[&b.operation_id,&i.attempt_id,&i.environment_id,&i.session_id,&i.workspace_id,&i.instance_id,&target.sandbox_id,&i.profile_digest,&i.provider_config_digest,&i.authorization_digest,&b.controller_epoch,&b.controller_lease_digest,&b.request_digest,&preparation.preparation_digest]).map_err(|e|e.to_string())?;
            let receipt=load_completion(&mut tx,&b.operation_id)?.ok_or("provider_provision_completion_missing")?;
            if receipt.creation_attempt_id!=i.attempt_id || receipt.preparation_digest!=preparation.preparation_digest {return Err("provider_provision_completion_conflict".into());}
            tx.commit().map_err(|e|e.to_string())?;Ok(receipt)
        })
    }

    pub(crate) fn provider_provision_completion(
        &self,
        operation_id: &str,
    ) -> Result<Option<ProviderProvisionCompletion>, String> {
        validate_identity(operation_id)?;
        self.with_execution_control_client(|client| load_completion(client, operation_id))
    }
}

fn load_completion(
    db: &mut impl GenericClient,
    operation: &str,
) -> Result<Option<ProviderProvisionCompletion>, String> {
    let row=db.query_opt("SELECT operation_id,creation_attempt_id,environment_id,session_id,workspace_id,instance_id,sandbox_id,profile_digest,provider_config_digest,authorization_digest,controller_epoch,controller_lease_digest,request_digest,preparation_digest,recorded_at_ms FROM runtime.provider_environment_provision_completions WHERE operation_id=$1",&[&operation]).map_err(|e|e.to_string())?;
    let Some(r) = row else {
        return Ok(None);
    };
    let receipt = ProviderProvisionCompletion {
        operation_id: r.get(0),
        creation_attempt_id: r.get(1),
        environment_id: r.get(2),
        session_id: r.get(3),
        workspace_id: r.get(4),
        instance_id: r.get(5),
        sandbox_id: r.get(6),
        profile_digest: r.get(7),
        provider_config_digest: r.get(8),
        authorization_digest: r.get(9),
        controller_epoch: r.get(10),
        controller_lease_digest: r.get(11),
        request_digest: r.get(12),
        preparation_digest: r.get(13),
        recorded_at_ms: r.get(14),
    };
    validate_identity(&receipt.sandbox_id)?;
    validate_digest(&receipt.preparation_digest)?;
    let source=db.query_opt("SELECT g.payload::text,o.request::text FROM runtime.provider_environment_provision_completions c JOIN runtime.provider_creation_operations p ON p.attempt_id=c.creation_attempt_id JOIN runtime.provider_environment_bindings b ON b.environment_id=p.environment_id AND b.creation_attempt_id=p.attempt_id JOIN public.app_core_environmentmaintenanceoperation o ON o.operation_id=p.maintenance_operation_id JOIN public.app_core_environmentmaintenancegrant g ON g.operation_id=o.operation_id AND g.digest=p.authorization_digest WHERE c.operation_id=$1 AND p.maintenance_operation_id=c.operation_id AND p.agent_run_id IS NULL AND p.processing_job_id IS NULL AND p.creation_owner_json::jsonb=jsonb_build_object('kind','maintenanceOperation','operationId',c.operation_id) AND p.environment_id=c.environment_id AND p.session_id=c.session_id AND p.workspace_id=c.workspace_id AND p.instance_id=c.instance_id AND p.verified_sandbox_id=c.sandbox_id AND p.verification_digest IS NOT NULL AND p.verified_at_ms<=c.recorded_at_ms AND p.profile_digest=c.profile_digest AND p.provider_config_digest=c.provider_config_digest AND p.authorization_digest=c.authorization_digest AND o.operation_kind='provision' AND o.environment_id=c.environment_id AND o.session_id=c.session_id AND o.workspace_id=c.workspace_id AND o.profile_digest=c.profile_digest AND o.request_digest=c.request_digest AND (g.payload->>'controllerEpoch')::bigint=c.controller_epoch AND g.payload->>'controllerLeaseDigest'=c.controller_lease_digest",&[&operation]).map_err(|e|e.to_string())?.ok_or("provider_provision_completion_fact_rejected")?;
    let payload: crate::environment_authorization::provision::ProvisionBinding =
        serde_json::from_str(&source.get::<_, String>(0))
            .map_err(|_| "provider_provision_original_invalid")?;
    let request: crate::environment_authorization::provision::ProvisionRequest =
        serde_json::from_str(&source.get::<_, String>(1))
            .map_err(|_| "provider_provision_original_invalid")?;
    payload.validate_request(&request)?;
    let original = ProvisionAuthorization {
        payload,
        signature: String::new(),
    };
    if original.digest()? != receipt.authorization_digest
        || original.payload.operation_id != receipt.operation_id
        || original.payload.workspace_id != receipt.workspace_id
        || original.payload.session_id != receipt.session_id
        || original.payload.environment_id != receipt.environment_id
        || original.payload.environment_profile_digest != receipt.profile_digest
        || original.payload.request_digest != receipt.request_digest
    {
        return Err("provider_provision_completion_fact_rejected".into());
    }
    Ok(Some(receipt))
}

pub(in crate::postgres_store) fn require_completed_handoff(
    db: &mut impl GenericClient,
    target: &ProviderVerifiedTarget,
) -> Result<(), String> {
    let EnvironmentTaskOwner::MaintenanceOperation { operation_id } = &target.identity.owner else {
        return Err("provider_provision_owner_required".into());
    };
    let operation =
        super::load(db, &target.identity.attempt_id, false)?.ok_or("provider_creation_missing")?;
    super::require_identity(&operation, &target.identity)?;
    if cleanup::load_target(db, &operation.identity)?.as_ref() != Some(target) {
        return Err("provider_provision_handoff_target_rejected".into());
    }
    let c = load_completion(db, operation_id)?
        .ok_or("retained_provider_provision_completion_required")?;
    let released:bool=db.query_one("SELECT EXISTS(SELECT 1 FROM public.app_core_environmentmaintenanceoperation WHERE operation_id=$1 AND state='released')",&[&operation_id]).map_err(|e|e.to_string())?.get(0);
    if !released
        || c.creation_attempt_id != target.identity.attempt_id
        || c.instance_id != target.identity.instance_id
        || c.sandbox_id != target.sandbox_id
        || c.authorization_digest != target.identity.authorization_digest
    {
        return Err("retained_provider_provision_handoff_pending".into());
    }
    Ok(())
}
