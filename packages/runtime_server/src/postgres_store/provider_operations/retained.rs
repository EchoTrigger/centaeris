//! Environment-owned provider identity and independently authorized Run consumers.
//! A creation Run remains provenance; it never grants a later Run authority.
use super::cleanup::{
    self, ProviderExecutionDispatchError, ProviderSandboxObservation, ProviderVerifiedTarget,
};
use super::*;
use sha2::{Digest, Sha256};

pub(crate) mod power;
pub(in crate::postgres_store) mod receipts;
pub(crate) mod retirement;
pub(crate) mod schema;
pub use receipts::{RetainedProviderCleanupReceipt, RetainedProviderQuiescence};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedProviderRunUse {
    pub provider_use_id: String,
    pub provider_use_digest: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub execution_id: String,
    pub target: ProviderVerifiedTarget,
}

pub(super) fn bind_creation(
    db: &mut impl GenericClient,
    identity: &ProviderCreationIdentity,
) -> Result<(), String> {
    power::require_admission(db, &identity.environment_id)?;
    if db.query_opt("SELECT operation_id FROM runtime.provider_environment_retirements WHERE environment_id=$1", &[&identity.environment_id]).map_err(|e|e.to_string())?.is_some() {
        return Err("retained_provider_environment_retiring".into());
    }
    db.execute("INSERT INTO runtime.provider_environment_bindings(environment_id,creation_attempt_id,recorded_at_ms) VALUES($1,$2,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint) ON CONFLICT(environment_id) DO NOTHING", &[&identity.environment_id,&identity.attempt_id]).map_err(|_|"retained_provider_environment_binding_conflict")?;
    let attempt: String = db.query_one("SELECT creation_attempt_id FROM runtime.provider_environment_bindings WHERE environment_id=$1 FOR SHARE", &[&identity.environment_id]).map_err(|e|e.to_string())?.get(0);
    if attempt != identity.attempt_id {
        return Err("retained_provider_environment_binding_conflict".into());
    }
    Ok(())
}

/// Live admission follows the current Environment binding; historical consumers
/// can resolve a creation fact without following the live selection.
#[derive(Debug)]
pub(in crate::postgres_store) enum TargetLookup {
    CurrentEnvironment {
        environment_id: String,
    },
    CreationAttempt {
        environment_id: String,
        creation_attempt_id: String,
    },
}

fn resolve_target(
    db: &mut impl GenericClient,
    lookup: TargetLookup,
) -> Result<ProviderVerifiedTarget, String> {
    let (environment_id, attempt) = match lookup {
        TargetLookup::CurrentEnvironment { environment_id } => {
            let row = db.query_opt("SELECT creation_attempt_id FROM runtime.provider_environment_bindings WHERE environment_id=$1 FOR SHARE", &[&environment_id]).map_err(|e|e.to_string())?.ok_or("retained_provider_environment_binding_missing")?;
            (environment_id, row.get::<_, String>(0))
        }
        TargetLookup::CreationAttempt {
            environment_id,
            creation_attempt_id,
        } => (environment_id, creation_attempt_id),
    };
    let operation =
        super::load(db, &attempt, false)?.ok_or("retained_provider_creation_missing")?;
    if operation.identity.environment_id != environment_id {
        return Err("retained_provider_environment_binding_rejected".into());
    }
    cleanup::load_target(db, &operation.identity)?
        .ok_or("retained_provider_verified_target_required".into())
}

pub(crate) fn load_environment_target(
    db: &mut impl GenericClient,
    environment_id: &str,
) -> Result<ProviderVerifiedTarget, String> {
    resolve_target(
        db,
        TargetLookup::CurrentEnvironment {
            environment_id: environment_id.into(),
        },
    )
}

fn reject_retirement(db: &mut impl GenericClient, environment_id: &str) -> Result<(), String> {
    if db.query_opt("SELECT operation_id FROM runtime.provider_environment_retirements WHERE environment_id=$1 FOR SHARE", &[&environment_id]).map_err(|e|e.to_string())?.is_some() {
        return Err("retained_provider_environment_retiring".into());
    }
    Ok(())
}

pub(super) fn require_authority(
    tx: &mut postgres::Transaction<'_>,
    authority: &ControllerAuthorityRequest<'_>,
    configured_digest: Option<&str>,
) -> Result<VerifiedOriginalGrant, String> {
    let grant = verify_controller_authority(tx, authority)?;
    let EnvironmentProfile::E2b(profile) = &grant.authorization.environment_profile else {
        return Err("retained_provider_profile_rejected".into());
    };
    if profile.lifetime != "retained"
        || configured_digest.is_some_and(|value| value != profile.provider_config_digest)
    {
        return Err("retained_provider_policy_rejected".into());
    }
    Ok(grant)
}

fn require_target_scope(
    grant: &VerifiedOriginalGrant,
    authority: &ControllerAuthorityRequest<'_>,
    target: &ProviderVerifiedTarget,
) -> Result<(), String> {
    let original = &grant.authorization;
    let EnvironmentProfile::E2b(profile) = &original.environment_profile else {
        return Err("retained_provider_profile_rejected".into());
    };
    let identity = &target.identity;
    if identity.environment_id != authority.environment_authorization.payload.environment_id
        || identity.session_id != original.session_id
        || identity.workspace_id != original.workspace_id
        || identity.profile_digest != original.environment_profile_digest
        || identity.provider_config_digest != profile.provider_config_digest
    {
        return Err("retained_provider_use_binding_rejected".into());
    }
    Ok(())
}

fn require_open_target(
    db: &mut impl GenericClient,
    target: &ProviderVerifiedTarget,
) -> Result<(), String> {
    let operation = super::load(db, &target.identity.attempt_id, true)?
        .ok_or("retained_provider_creation_missing")?;
    super::require_identity(&operation, &target.identity)?;
    if operation.dispatch_state != ProviderCreationDispatchState::Dispatched
        || cleanup::load_settlement(db, &target.identity.attempt_id)?.is_some()
    {
        return Err("retained_provider_target_unavailable".into());
    }
    if cleanup::load_target(db, &target.identity)?.as_ref() != Some(target) {
        return Err("retained_provider_target_rejected".into());
    }
    Ok(())
}

fn binding_digest(
    run: &str,
    authorization: &str,
    execution: &str,
    target: &ProviderVerifiedTarget,
) -> Result<String, String> {
    let i = &target.identity;
    let fields = serde_json::json!({"agentRunId":run,"authorizationDigest":authorization,"executionId":execution,
        "creation":{"attemptId":i.attempt_id,"executionId":i.execution_id,"instanceId":i.instance_id,"environmentId":i.environment_id,"sessionId":i.session_id,"workspaceId":i.workspace_id,"agentRunId":i.agent_run_id,"authorizationDigest":i.authorization_digest,"profileDigest":i.profile_digest,"providerConfigDigest":i.provider_config_digest,"requestDigest":i.request_digest,"idempotencyKey":i.idempotency_key,"processingJobId":i.processing_job_id},"sandboxId":target.sandbox_id,"verificationDigest":target.verification_digest});
    fact_digest("runtime:retained-provider-use:v1\0", &fields)
}

pub(super) fn fact_digest(domain: &str, fields: &serde_json::Value) -> Result<String, String> {
    let mut digest = Sha256::new();
    digest.update(domain.as_bytes());
    digest.update(serde_json::to_vec(fields).map_err(|_| "retained_provider_fact_invalid")?);
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn require_prior_uses_closed(
    db: &mut impl GenericClient,
    run: &str,
    target: &ProviderVerifiedTarget,
) -> Result<(), String> {
    if run != target.identity.agent_run_id && db.query_opt("SELECT provider_use_id FROM runtime.provider_environment_run_uses WHERE agent_run_id=$1 AND environment_id=$2", &[&target.identity.agent_run_id,&target.identity.environment_id]).map_err(|e|e.to_string())?.is_none() {
        return Err("retained_provider_creation_consumer_unconfirmed".into());
    }
    let prior = db.query(
        "SELECT agent_run_id FROM runtime.provider_environment_run_uses WHERE environment_id=$1 AND agent_run_id<>$2 ORDER BY provider_use_id FOR SHARE",
        &[&target.identity.environment_id,&run],
    ).map_err(|e|e.to_string())?;
    for row in prior {
        let prior_run: String = row.get(0);
        let receipt = receipts::load_receipt(db, &prior_run)?
            .ok_or("retained_provider_prior_consumer_pending")?;
        if receipt.environment_id != target.identity.environment_id
            || receipt.profile_digest != target.identity.profile_digest
            || receipt.provider_config_digest != target.identity.provider_config_digest
        {
            return Err("retained_provider_prior_consumer_rejected".into());
        }
        let released: bool = db.query_one(
            "SELECT EXISTS(SELECT 1 FROM public.app_core_agentrunenvironmentrelease WHERE agent_run_id=$1 AND environment_id=$2 AND authorization_digest=$3 AND controller_epoch>=$4)",
            &[&prior_run,&receipt.environment_id,&receipt.authorization_digest,&receipt.controller_epoch],
        ).map_err(|e|e.to_string())?.get(0);
        if !released {
            return Err("retained_provider_prior_consumer_pending".into());
        }
    }
    Ok(())
}

fn insert_use(
    db: &mut impl GenericClient,
    grant: &VerifiedOriginalGrant,
    execution_id: &str,
    target: &ProviderVerifiedTarget,
) -> Result<RetainedProviderRunUse, String> {
    let run = &grant.authorization.agent_run_id;
    let digest = binding_digest(run, &grant.authorization_digest, execution_id, target)?;
    let id = format!("provider_use_{}", &digest[7..]);
    db.execute("INSERT INTO runtime.provider_environment_run_uses(provider_use_id,agent_run_id,environment_id,authorization_digest,execution_id,provider_use_digest,recorded_at_ms,creation_attempt_id) VALUES($1,$2,$3,$4,$5,$6,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,$7) ON CONFLICT(agent_run_id) DO NOTHING", &[&id,run,&target.identity.environment_id,&grant.authorization_digest,&execution_id,&digest,&target.identity.attempt_id]).map_err(|_|"retained_provider_use_conflict")?;
    let value = load_use(db, run)?.ok_or("retained_provider_use_missing")?;
    if value.provider_use_id != id || value.provider_use_digest != digest || &value.target != target
    {
        return Err("retained_provider_use_conflict".into());
    }
    Ok(value)
}

pub(super) fn load_use(
    db: &mut impl GenericClient,
    run: &str,
) -> Result<Option<RetainedProviderRunUse>, String> {
    load_use_with_target_lookup(db, run, resolve_target)
}

pub(in crate::postgres_store) fn load_use_with_target_lookup<C: GenericClient>(
    db: &mut C,
    run: &str,
    lookup: impl FnOnce(&mut C, TargetLookup) -> Result<ProviderVerifiedTarget, String>,
) -> Result<Option<RetainedProviderRunUse>, String> {
    let row = db.query_opt("SELECT provider_use_id,authorization_digest,execution_id,provider_use_digest,environment_id,creation_attempt_id FROM runtime.provider_environment_run_uses WHERE agent_run_id=$1", &[&run]).map_err(|e|e.to_string())?;
    let Some(row) = row else { return Ok(None) };
    let environment: String = row.get(4);
    let attempt: String = row.get(5);
    let target = lookup(
        db,
        TargetLookup::CreationAttempt {
            environment_id: environment.clone(),
            creation_attempt_id: attempt.clone(),
        },
    )?;
    if target.identity.environment_id != environment || target.identity.attempt_id != attempt {
        return Err("retained_provider_environment_binding_rejected".into());
    }
    let value = RetainedProviderRunUse {
        provider_use_id: row.get(0),
        provider_use_digest: row.get(3),
        agent_run_id: run.into(),
        authorization_digest: row.get(1),
        execution_id: row.get(2),
        target,
    };
    let expected = binding_digest(
        run,
        &value.authorization_digest,
        &value.execution_id,
        &value.target,
    )?;
    if value.provider_use_digest != expected
        || value.provider_use_id != format!("provider_use_{}", &expected[7..])
    {
        return Err("retained_provider_use_fact_rejected".into());
    }
    require_original_use_scope(db, &value)?;
    Ok(Some(value))
}

fn require_original_use_scope(
    db: &mut impl GenericClient,
    value: &RetainedProviderRunUse,
) -> Result<(), String> {
    let run = value.agent_run_id.as_str();
    let original=db.query_opt("SELECT r.environment_id,g.digest,g.payload::text FROM public.app_core_agentrun r JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id WHERE r.id=$1",&[&run]).map_err(|e|e.to_string())?.ok_or("retained_provider_original_missing")?;
    let grant: crate::agent_run_authorization::WorkspaceAgentRunAuthorization =
        serde_json::from_str(&original.get::<_, String>(2))
            .map_err(|_| "retained_provider_original_invalid")?;
    grant
        .checked_binding(run)?
        .verify_digest_binding(&value.authorization_digest)?;
    let EnvironmentProfile::E2b(profile) = &grant.environment_profile else {
        return Err("retained_provider_original_rejected".into());
    };
    if original.get::<_, Option<String>>(0).as_deref()
        != Some(value.target.identity.environment_id.as_str())
        || original.get::<_, String>(1) != value.authorization_digest
        || grant.session_id != value.target.identity.session_id
        || grant.workspace_id != value.target.identity.workspace_id
        || grant.environment_profile_digest != value.target.identity.profile_digest
        || profile.provider_config_digest != value.target.identity.provider_config_digest
        || profile.lifetime != "retained"
    {
        return Err("retained_provider_original_rejected".into());
    }
    Ok(())
}

fn require_current_use(
    tx: &mut postgres::Transaction<'_>,
    authority: &ControllerAuthorityRequest<'_>,
    value: &RetainedProviderRunUse,
    configured_digest: &str,
) -> Result<VerifiedOriginalGrant, String> {
    require_current_use_with_target_lookup(
        tx,
        authority,
        value,
        configured_digest,
        &mut |db, selection| resolve_target(db, selection),
    )
}

fn require_current_use_with_target_lookup(
    tx: &mut postgres::Transaction<'_>,
    authority: &ControllerAuthorityRequest<'_>,
    value: &RetainedProviderRunUse,
    configured_digest: &str,
    lookup: &mut impl FnMut(
        &mut postgres::Transaction<'_>,
        TargetLookup,
    ) -> Result<ProviderVerifiedTarget, String>,
) -> Result<VerifiedOriginalGrant, String> {
    let grant = require_authority(tx, authority, Some(configured_digest))?;
    if authority.purpose == ControllerPurpose::Execute {
        power::require_admission(tx, &value.target.identity.environment_id)?;
    }
    if value.agent_run_id != grant.authorization.agent_run_id
        || value.authorization_digest != grant.authorization_digest
        || load_use_with_target_lookup(tx, &value.agent_run_id, &mut *lookup)?.as_ref()
            != Some(value)
        || lookup(
            tx,
            TargetLookup::CurrentEnvironment {
                environment_id: value.target.identity.environment_id.clone(),
            },
        )? != value.target
    {
        return Err("retained_provider_use_authority_rejected".into());
    }
    require_target_scope(&grant, authority, &value.target)?;
    reject_retirement(tx, &value.target.identity.environment_id)?;
    require_open_target(tx, &value.target)?;
    Ok(grant)
}

impl PostgresRuntimeStore {
    pub fn record_retained_provider_execution_target(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
        configured_digest: &str,
        observation: &ProviderSandboxObservation,
    ) -> Result<ProviderVerifiedTarget, String> {
        require_execute(authority)?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant = require_authority(&mut tx, authority, Some(configured_digest))?;
            require_current_binding(&grant, authority, identity)?;
            reject_retirement(&mut tx, &identity.environment_id)?;
            power::require_admission(&mut tx, &identity.environment_id)?;
            let operation = super::load(&mut tx, &identity.attempt_id, true)?
                .ok_or("retained_provider_creation_missing")?;
            require_identity(&operation, identity)?;
            bind_creation(&mut tx, identity)?;
            if operation.dispatch_state != ProviderCreationDispatchState::Dispatched
                || cleanup::load_settlement(&mut tx, &identity.attempt_id)?.is_some()
            {
                return Err("retained_provider_target_unavailable".into());
            }
            let sandbox = operation
                .reported_sandbox_id
                .as_deref()
                .ok_or("retained_provider_target_unknown")?;
            let target = cleanup::observed_target(
                &grant,
                identity,
                configured_digest,
                sandbox,
                observation,
            )?;
            require_authority(&mut tx, authority, Some(configured_digest))?;
            let target = cleanup::persist_verified_target(&mut tx, &target)?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(target)
        })
    }

    pub fn bind_retained_provider_run_use(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        execution_id: &str,
        creation_attempt_id: &str,
        configured_digest: &str,
    ) -> Result<RetainedProviderRunUse, String> {
        require_execute(authority)?;
        validate_identity(execution_id)?;
        validate_identity(creation_attempt_id)?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant = require_authority(&mut tx, authority, Some(configured_digest))?;
            let environment = &authority.environment_authorization.payload.environment_id;
            reject_retirement(&mut tx, environment)?;
            power::require_admission(&mut tx, environment)?;
            let target = load_environment_target(&mut tx, environment)?;
            if target.identity.attempt_id != creation_attempt_id {
                return Err("retained_provider_use_target_rejected".into());
            }
            require_target_scope(&grant, authority, &target)?;
            require_open_target(&mut tx, &target)?;
            require_prior_uses_closed(&mut tx, &grant.authorization.agent_run_id, &target)?;
            let value = insert_use(&mut tx, &grant, execution_id, &target)?;
            require_authority(&mut tx, authority, Some(configured_digest))?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(value)
        })
    }

    pub fn retained_provider_environment_creation(
        &self,
        environment_id: &str,
    ) -> Result<Option<ProviderCreationOperation>, String> {
        validate_identity(environment_id)?;
        self.with_execution_control_client(|client| {
            let row=client.query_opt("SELECT creation_attempt_id FROM runtime.provider_environment_bindings WHERE environment_id=$1",&[&environment_id]).map_err(|e|e.to_string())?;
            let Some(row)=row else { return Ok(None) };
            let attempt:String=row.get(0);
            let operation=super::load(client,&attempt,false)?.ok_or("retained_provider_creation_missing")?;
            if operation.identity.environment_id!=environment_id { return Err("retained_provider_environment_binding_rejected".into()); }
            Ok(Some(operation))
        })
    }

    pub fn retained_provider_run_use(
        &self,
        run_id: &str,
    ) -> Result<Option<RetainedProviderRunUse>, String> {
        validate_identity(run_id)?;
        self.with_execution_control_client(|client| load_use(client, run_id))
    }

    pub fn with_retained_provider_execution_dispatch<T: Send, E: Send>(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        value: &RetainedProviderRunUse,
        configured_digest: &str,
        dispatch: impl FnOnce(&ProviderVerifiedTarget) -> Result<T, E> + Send,
    ) -> Result<T, ProviderExecutionDispatchError<E>> {
        self.with_retained_provider_execution_dispatch_with_target_lookup(
            authority,
            value,
            configured_digest,
            |db, selection| resolve_target(db, selection),
            dispatch,
        )
    }

    pub(in crate::postgres_store) fn with_retained_provider_execution_dispatch_with_target_lookup<
        T: Send,
        E: Send,
    >(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        value: &RetainedProviderRunUse,
        configured_digest: &str,
        mut lookup: impl FnMut(
                &mut postgres::Transaction<'_>,
                TargetLookup,
            ) -> Result<ProviderVerifiedTarget, String>
            + Send,
        dispatch: impl FnOnce(&ProviderVerifiedTarget) -> Result<T, E> + Send,
    ) -> Result<T, ProviderExecutionDispatchError<E>> {
        use crate::observations::provider_dispatch::{
            DispatchKind, DispatchMetric, DispatchObservation,
        };
        require_execute(authority)?;
        let mut observation = DispatchObservation::new(DispatchKind::RetainedExecution);
        let mut started = false;
        let result = self.with_client_error(|client| {
            observation.entered_client();
            let result = (|| {
                let mut tx = observation.timed(DispatchMetric::AuthorityBefore, || {
                    let mut tx = client.transaction().map_err(|e| e.to_string())?;
                    super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
                    require_current_use_with_target_lookup(&mut tx, authority, value, configured_digest, &mut lookup)?;
                    let closed: bool = tx.query_one("SELECT cleanup_receipt_id IS NOT NULL FROM runtime.provider_environment_run_uses WHERE provider_use_id=$1 FOR SHARE", &[&value.provider_use_id]).map_err(|e| e.to_string())?.get(0);
                    if closed {
                        return Err(ProviderExecutionDispatchError::Rejected("retained_provider_use_already_closed".into()));
                    }
                    Ok::<_, ProviderExecutionDispatchError<E>>(tx)
                })?;
                started = true;
                let result = observation.callback(|| {
                    dispatch(&value.target).map_err(ProviderExecutionDispatchError::Callback)
                })?;
                observation.timed(DispatchMetric::AuthorityAfter, || {
                    require_current_use_with_target_lookup(&mut tx, authority, value, configured_digest, &mut lookup)
                        .map_err(ProviderExecutionDispatchError::AfterDispatch)
                })?;
                observation.timed(DispatchMetric::Commit, || {
                    tx.commit().map_err(|e| ProviderExecutionDispatchError::AfterDispatch(e.to_string()))
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
}

pub(crate) fn settle_environment_removed(
    db: &mut impl GenericClient,
    target: &ProviderVerifiedTarget,
    controller_epoch: i64,
    evidence: &cleanup::ProviderRemovalEvidence,
) -> Result<cleanup::ProviderCreationSettlement, String> {
    let removed = match evidence {
        cleanup::ProviderRemovalEvidence::Deleted { sandbox_id }
        | cleanup::ProviderRemovalEvidence::NotFound { sandbox_id } => sandbox_id,
    };
    if removed != &target.sandbox_id
        || load_environment_target(db, &target.identity.environment_id)? != *target
    {
        return Err("retained_provider_retirement_target_rejected".into());
    }
    let allowed:bool=db.query_one("SELECT EXISTS(SELECT 1 FROM runtime.provider_environment_retirements r JOIN public.app_core_sessionenvironment e ON e.id=r.environment_id WHERE r.environment_id=$1 AND r.expected_controller_epoch=$2 AND e.controller_epoch=$2 AND e.controller_released)",&[&target.identity.environment_id,&controller_epoch]).map_err(|e|e.to_string())?.get(0);
    if !allowed {
        return Err("retained_provider_retirement_authority_rejected".into());
    }
    if let Some(settled) = cleanup::load_settlement(db, &target.identity.attempt_id)? {
        if settled.kind != cleanup::ProviderSettlementKind::Removed
            || settled.sandbox_id.as_deref() != Some(removed)
        {
            return Err("retained_provider_retirement_settlement_conflict".into());
        }
        return Ok(settled);
    }
    cleanup::insert_settlement(
        db,
        &target.identity,
        cleanup::ProviderSettlementKind::Removed,
        Some(removed),
        controller_epoch,
    )
}

pub(crate) const DDL: &str = r#"
CREATE TABLE runtime.provider_environment_bindings(
 environment_id text PRIMARY KEY,creation_attempt_id text NOT NULL UNIQUE REFERENCES runtime.provider_creation_operations(attempt_id),
 recorded_at_ms bigint NOT NULL CHECK(recorded_at_ms>=0));
CREATE TABLE runtime.provider_environment_run_uses(
 provider_use_id text PRIMARY KEY,agent_run_id text NOT NULL UNIQUE,
 environment_id text NOT NULL REFERENCES runtime.provider_environment_bindings(environment_id),
 authorization_digest text NOT NULL,execution_id text NOT NULL UNIQUE,provider_use_digest text NOT NULL,
 recorded_at_ms bigint NOT NULL,cleanup_receipt_id text UNIQUE,cleanup_controller_epoch bigint,quiescence_digest text,cleanup_recorded_at_ms bigint,
 CONSTRAINT retained_provider_run_use_facts CHECK(recorded_at_ms>=0 AND authorization_digest ~ '^sha256:[0-9a-f]{64}$' AND provider_use_digest ~ '^sha256:[0-9a-f]{64}$'
 AND ((cleanup_receipt_id IS NULL AND cleanup_controller_epoch IS NULL AND quiescence_digest IS NULL AND cleanup_recorded_at_ms IS NULL)
 OR (cleanup_receipt_id IS NOT NULL AND cleanup_controller_epoch IS NOT NULL AND cleanup_controller_epoch>0 AND quiescence_digest IS NOT NULL AND quiescence_digest ~ '^sha256:[0-9a-f]{64}$' AND cleanup_recorded_at_ms IS NOT NULL AND cleanup_recorded_at_ms>=recorded_at_ms))));
CREATE TABLE runtime.provider_environment_retirements(
 operation_id text PRIMARY KEY,environment_id text NOT NULL UNIQUE REFERENCES runtime.provider_environment_bindings(environment_id),
 request_digest text NOT NULL,request_json text NOT NULL,expected_controller_epoch bigint NOT NULL,recorded_at_ms bigint NOT NULL,
 CONSTRAINT provider_environment_retirement_facts CHECK(request_digest ~ '^sha256:[0-9a-f]{64}$' AND expected_controller_epoch>=0 AND recorded_at_ms>=0 AND request_json<>''));
CREATE FUNCTION runtime.protect_provider_environment_binding() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP<>'INSERT' THEN RAISE EXCEPTION 'provider environment origin is immutable'; END IF;
 IF NOT EXISTS(SELECT 1 FROM runtime.provider_creation_operations WHERE attempt_id=NEW.creation_attempt_id AND environment_id=NEW.environment_id)
 THEN RAISE EXCEPTION 'provider environment origin mismatch'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER provider_environment_binding_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_environment_bindings FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_environment_binding();
CREATE FUNCTION runtime.protect_retained_provider_run_use() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'provider consumer responsibility is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.cleanup_receipt_id IS NOT NULL OR NEW.cleanup_controller_epoch IS NOT NULL OR NEW.quiescence_digest IS NOT NULL OR NEW.cleanup_recorded_at_ms IS NOT NULL
  THEN RAISE EXCEPTION 'provider consumer starts without cleanup evidence'; END IF;
 ELSE
  IF ROW(NEW.provider_use_id,NEW.agent_run_id,NEW.environment_id,NEW.authorization_digest,NEW.execution_id,NEW.provider_use_digest,NEW.recorded_at_ms)
   IS DISTINCT FROM ROW(OLD.provider_use_id,OLD.agent_run_id,OLD.environment_id,OLD.authorization_digest,OLD.execution_id,OLD.provider_use_digest,OLD.recorded_at_ms)
   OR OLD.cleanup_receipt_id IS NOT NULL OR NEW.cleanup_receipt_id IS NULL
  THEN RAISE EXCEPTION 'provider consumer identity or evidence transition rejected'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER retained_provider_run_use_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_environment_run_uses FOR EACH ROW EXECUTE FUNCTION runtime.protect_retained_provider_run_use();
CREATE FUNCTION runtime.protect_provider_environment_retirement() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 RAISE EXCEPTION 'provider environment retirement is immutable';
END $$;
CREATE TRIGGER provider_environment_retirement_facts BEFORE UPDATE OR DELETE ON runtime.provider_environment_retirements FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_environment_retirement();
"#;

pub(crate) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let history: bool = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=21)",
            &[],
        )
        .map_err(|e| e.to_string())?
        .get(0);
    for (table, name, kind) in [
        (
            "provider_environment_bindings",
            "provider_environment_binding_facts",
            31i16,
        ),
        (
            "provider_environment_run_uses",
            "retained_provider_run_use_facts",
            31i16,
        ),
        (
            "provider_environment_retirements",
            "provider_environment_retirement_facts",
            27i16,
        ),
    ] {
        let function = match table {
            "provider_environment_bindings" => "protect_provider_environment_binding",
            "provider_environment_run_uses" => "protect_retained_provider_run_use",
            _ => "protect_provider_environment_retirement",
        };
        let signature = format!("runtime.{function}()");
        let found:bool=db.query_one("SELECT EXISTS(SELECT 1 FROM pg_trigger WHERE tgrelid=to_regclass($1) AND tgname=$2 AND tgenabled='O' AND NOT tgisinternal AND tgtype=$3 AND tgfoid=to_regprocedure($4) AND tgqual IS NULL AND tgnargs=0)",&[&format!("runtime.{table}"),&name,&kind,&signature]).map_err(|e|e.to_string())?.get(0);
        if !found {
            return Err("retained_provider_schema_trigger_rejected".into());
        }
        let guard = if history && table == "provider_environment_run_uses" {
            schema::DDL
        } else {
            DDL
        };
        let expected = guard
            .split_once(&format!(
                "CREATE FUNCTION runtime.{function}() RETURNS trigger LANGUAGE plpgsql AS $$"
            ))
            .ok_or("retained_provider_guard_missing")?
            .1
            .split_once("$$;")
            .ok_or("retained_provider_guard_missing")?
            .0;
        let actual: String = db
            .query_one(
                "SELECT prosrc FROM pg_proc WHERE oid=to_regprocedure($1)",
                &[&signature],
            )
            .map_err(|e| e.to_string())?
            .get(0);
        if actual != expected {
            return Err("retained_provider_schema_guard_rejected".into());
        }
    }
    for (table, expected) in [
        ("provider_environment_bindings", 4i64),
        (
            "provider_environment_run_uses",
            if history { 7i64 } else { 6i64 },
        ),
        ("provider_environment_retirements", 4i64),
    ] {
        let row=db.query_one("SELECT count(*),bool_and(convalidated AND NOT condeferrable) FROM pg_constraint WHERE conrelid=to_regclass($1) AND contype<>'n'",&[&format!("runtime.{table}")]).map_err(|e|e.to_string())?;
        if row.get::<_, i64>(0) != expected || row.get::<_, Option<bool>>(1) != Some(true) {
            return Err("retained_provider_schema_constraints_rejected".into());
        }
    }
    if history {
        schema::validate(db)?;
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::postgres_store) fn restore_previous_fixture(db: &mut postgres::Client) {
    power::schema::restore_previous_fixture(db);
    let exists: bool = db
        .query_one(
            "SELECT to_regclass('runtime.provider_environment_bindings') IS NOT NULL",
            &[],
        )
        .unwrap()
        .get(0);
    if exists {
        assert_eq!(db.query_one("SELECT (SELECT count(*) FROM runtime.provider_environment_bindings)+(SELECT count(*) FROM runtime.provider_environment_run_uses)+(SELECT count(*) FROM runtime.provider_environment_retirements)",&[]).unwrap().get::<_,i64>(0),0,"cannot discard retained provider evidence");
        db.batch_execute("DROP TABLE runtime.provider_environment_retirements; DROP TABLE runtime.provider_environment_run_uses; DROP TABLE runtime.provider_environment_bindings; DROP FUNCTION runtime.protect_provider_environment_retirement(); DROP FUNCTION runtime.protect_retained_provider_run_use(); DROP FUNCTION runtime.protect_provider_environment_binding(); DELETE FROM runtime.schema_migrations WHERE version=19;").unwrap();
    }
}

pub(super) fn require_unsent_cleanup(
    tx: &mut postgres::Transaction<'_>,
    authority: &ControllerAuthorityRequest<'_>,
    identity: &ProviderCreationIdentity,
) -> Result<VerifiedOriginalGrant, String> {
    if authority.purpose != ControllerPurpose::Cleanup {
        return Err("provider_cleanup_requires_cleanup_authority".into());
    }
    let grant = require_authority(tx, authority, None)?;
    require_current_binding(&grant, authority, identity)?;
    let bound=tx.query_opt("SELECT creation_attempt_id FROM runtime.provider_environment_bindings WHERE environment_id=$1 FOR SHARE",&[&identity.environment_id]).map_err(|e|e.to_string())?.ok_or("retained_provider_environment_binding_missing")?;
    if bound.get::<_, String>(0) != identity.attempt_id
        || load_use(tx, &identity.agent_run_id)?.is_some()
    {
        return Err("retained_provider_unsent_scope_rejected".into());
    }
    Ok(grant)
}

pub(super) fn require_only_unsent_settlements(
    db: &mut impl GenericClient,
    run: &str,
    environment: &str,
) -> Result<(), String> {
    if load_use(db, run)?.is_some() {
        return Err("retained_provider_unsent_scope_rejected".into());
    }
    let rows=db.query("SELECT p.attempt_id,p.dispatch_state,p.settlement_kind,p.verified_sandbox_id,p.reported_sandbox_id FROM runtime.provider_creation_operations p JOIN runtime.provider_environment_bindings b ON b.environment_id=p.environment_id AND b.creation_attempt_id=p.attempt_id WHERE p.agent_run_id=$1 AND p.environment_id=$2",&[&run,&environment]).map_err(|e|e.to_string())?;
    if rows.len() != 1
        || rows[0].get::<_, String>(1) != "notDispatched"
        || rows[0].get::<_, Option<String>>(2).as_deref() != Some("notDispatched")
        || rows[0].get::<_, Option<String>>(3).is_some()
        || rows[0].get::<_, Option<String>>(4).is_some()
    {
        return Err("retained_provider_unsent_settlement_required".into());
    }
    Ok(())
}

pub(super) fn require_no_effects_environment(
    db: &mut impl GenericClient,
    run: &str,
    environment: &str,
) -> Result<(), String> {
    reject_retirement(db, environment)?;
    let anchor=db.query_opt("SELECT creation_attempt_id FROM runtime.provider_environment_bindings WHERE environment_id=$1",&[&environment]).map_err(|e|e.to_string())?;
    if anchor.is_some() {
        let target = load_environment_target(db, environment)?;
        require_open_target(db, &target)?;
        require_prior_uses_closed(db, run, &target)
    } else {
        let pending:bool=db.query_one("SELECT EXISTS(SELECT 1 FROM runtime.provider_creation_operations WHERE environment_id=$1 AND settlement_id IS NULL)",&[&environment]).map_err(|e|e.to_string())?.get(0);
        if pending {
            Err("provider_no_effects_environment_responsibility_pending".into())
        } else {
            Ok(())
        }
    }
}

impl PostgresRuntimeStore {
    pub fn verify_retained_provider_cleanup_target(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
        configured_digest: &str,
        inspect: impl FnOnce(&ProviderCreationIdentity, &str) -> Result<ProviderSandboxObservation, String>
            + Send,
    ) -> Result<ProviderVerifiedTarget, String> {
        if authority.purpose != ControllerPurpose::Cleanup {
            return Err("retained_provider_cleanup_requires_cleanup_authority".into());
        }
        self.cleanup_view().with_client(|client| { let mut tx=client.transaction().map_err(|e|e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant=require_authority(&mut tx,authority,Some(configured_digest))?;
            require_current_binding(&grant,authority,identity)?;
            let anchor=tx.query_opt("SELECT creation_attempt_id FROM runtime.provider_environment_bindings WHERE environment_id=$1 FOR SHARE",&[&identity.environment_id]).map_err(|e|e.to_string())?.ok_or("retained_provider_environment_binding_missing")?;
            if anchor.get::<_,String>(0)!=identity.attempt_id { return Err("retained_provider_environment_binding_rejected".into()); }
            reject_retirement(&mut tx,&identity.environment_id)?;
            let operation=super::load(&mut tx,&identity.attempt_id,true)?.ok_or("retained_provider_creation_missing")?;
            require_identity(&operation,identity)?;
            if operation.dispatch_state!=ProviderCreationDispatchState::Dispatched || cleanup::load_settlement(&mut tx,&identity.attempt_id)?.is_some() { return Err("retained_provider_target_unavailable".into()); }
            if let Some(target)=cleanup::load_target(&mut tx,identity)? { tx.commit().map_err(|e|e.to_string())?;return Ok(target); }
            let sandbox=operation.reported_sandbox_id.as_deref().ok_or("retained_provider_target_unknown")?;
            let observed=inspect(identity,sandbox)?;
            let target=cleanup::observed_target(&grant,identity,configured_digest,sandbox,&observed)?;
            require_authority(&mut tx,authority,Some(configured_digest))?;
            let target=cleanup::persist_verified_target(&mut tx,&target)?;
            tx.commit().map_err(|e|e.to_string())?;Ok(target)
        })
    }

    pub fn recover_retained_provider_run_use_for_cleanup(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        execution_id: &str,
        creation_attempt_id: &str,
        configured_digest: &str,
    ) -> Result<RetainedProviderRunUse, String> {
        if authority.purpose != ControllerPurpose::Cleanup {
            return Err("retained_provider_cleanup_requires_cleanup_authority".into());
        }
        self.with_execution_control_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant = require_authority(&mut tx, authority, Some(configured_digest))?;
            let environment = &authority.environment_authorization.payload.environment_id;
            reject_retirement(&mut tx, environment)?;
            let target = load_environment_target(&mut tx, environment)?;
            if target.identity.attempt_id != creation_attempt_id
                || target.identity.execution_id != execution_id
            {
                return Err("retained_provider_cleanup_use_recovery_rejected".into());
            }
            require_current_binding(&grant, authority, &target.identity)?;
            require_open_target(&mut tx, &target)?;
            let use_ = insert_use(&mut tx, &grant, execution_id, &target)?;
            require_authority(&mut tx, authority, Some(configured_digest))?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(use_)
        })
    }
}
