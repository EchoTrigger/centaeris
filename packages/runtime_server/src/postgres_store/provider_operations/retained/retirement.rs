//! Destructive Environment retirement under separately authenticated maintenance authority.
use super::*;
use cleanup::ProviderRemovalEvidence;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderEnvironmentRetirementRequest {
    pub schema: String,
    pub operation_id: String,
    pub environment_id: String,
    pub creation_attempt_id: String,
    pub sandbox_id: String,
    pub provider_config_digest: String,
    pub expected_controller_epoch: i64,
    pub acknowledge_data_loss: bool,
}

impl ProviderEnvironmentRetirementRequest {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema != "runtime.provider_environment.retire.v1"
            || self.expected_controller_epoch < 0
            || !self.acknowledge_data_loss
        {
            return Err("provider_environment_retirement_request_invalid".into());
        }
        for value in [
            &self.operation_id,
            &self.environment_id,
            &self.creation_attempt_id,
            &self.sandbox_id,
        ] {
            super::super::validate_identity(value)
                .map_err(|_| "provider_environment_retirement_request_invalid")?;
        }
        super::super::validate_digest(&self.provider_config_digest)
            .map_err(|_| "provider_environment_retirement_request_invalid".into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderEnvironmentRetirementResult {
    pub schema: &'static str,
    pub operation_id: String,
    pub environment_id: String,
    pub creation_attempt_id: String,
    pub sandbox_id: String,
    pub status: &'static str,
}

impl PostgresRuntimeStore {
    /// Caller must authenticate independent Environment maintenance authority;
    /// a Run's execute or cleanup lease does not authorize this operation.
    pub(crate) fn retire_provider_environment(
        &self,
        request: &ProviderEnvironmentRetirementRequest,
        configured_digest: &str,
        callback: impl FnOnce(&ProviderVerifiedTarget) -> Result<ProviderRemovalEvidence, String> + Send,
    ) -> Result<ProviderEnvironmentRetirementResult, String> {
        request.validate()?;
        if request.provider_config_digest != configured_digest {
            return Err("provider_environment_retirement_configuration_rejected".into());
        }
        let body = serde_json::to_string(request)
            .map_err(|_| "provider_environment_retirement_request_invalid")?;
        let digest = fact_digest(
            "runtime:provider-environment-retirement:v1\0",
            &serde_json::to_value(request)
                .map_err(|_| "provider_environment_retirement_request_invalid")?,
        )?;
        self.cleanup_view().with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let target = lock_idle_target(&mut tx, request, configured_digest)?;
            let existing = require_matching_intent(&mut tx, request, &body, &digest)?;
            if existing && already_removed(&mut tx, request)? {
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(result(request, "removed"));
            }
            require_open_target(&mut tx, &target)?;
            if !existing {
                tx.execute(
                    "INSERT INTO runtime.provider_environment_retirements(operation_id,environment_id,request_digest,request_json,expected_controller_epoch,recorded_at_ms) VALUES($1,$2,$3,$4,$5,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint)",
                    &[&request.operation_id,&request.environment_id,&digest,&body,&request.expected_controller_epoch],
                ).map_err(|_| "provider_environment_retirement_intent_conflict")?;
            }
            // Commit the immutable barrier before any provider side effect. It
            // survives a lost response, process restart, or later DB failure.
            tx.commit().map_err(|e| e.to_string())?;

            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let current = lock_idle_target(&mut tx, request, configured_digest)?;
            if current != target || !require_matching_intent(&mut tx, request, &body, &digest)? {
                return Err("provider_environment_retirement_target_changed".into());
            }
            if already_removed(&mut tx, request)? {
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(result(request, "removed"));
            }
            require_open_target(&mut tx, &current)?;
            // Slow provider work holds the Environment pin in the separate
            // cleanup connection budget. No current Run lease is borrowed.
            let evidence = match callback(&current) {
                Ok(evidence) => evidence,
                Err(_) => {
                    tx.commit().map_err(|e| e.to_string())?;
                    return Ok(result(request, "pending"));
                }
            };
            settle_environment_removed(&mut tx, &current, request.expected_controller_epoch, &evidence)?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(result(request, "removed"))
        })
    }
}

fn lock_idle_target(
    tx: &mut postgres::Transaction<'_>,
    request: &ProviderEnvironmentRetirementRequest,
    configured_digest: &str,
) -> Result<ProviderVerifiedTarget, String> {
    let target = lock_idle_maintenance_target(
        tx,
        &IdleProviderEnvironmentTarget {
            environment_id: &request.environment_id,
            creation_attempt_id: &request.creation_attempt_id,
            sandbox_id: &request.sandbox_id,
            provider_config_digest: &request.provider_config_digest,
            expected_controller_epoch: request.expected_controller_epoch,
        },
        configured_digest,
    )?;
    super::power::require_retirement_allowed(tx, &request.environment_id)?;
    Ok(target)
}

pub(super) struct IdleProviderEnvironmentTarget<'a> {
    pub environment_id: &'a str,
    pub creation_attempt_id: &'a str,
    pub sandbox_id: &'a str,
    pub provider_config_digest: &'a str,
    pub expected_controller_epoch: i64,
}

pub(super) fn lock_idle_maintenance_target(
    tx: &mut postgres::Transaction<'_>,
    request: &IdleProviderEnvironmentTarget<'_>,
    configured_digest: &str,
) -> Result<ProviderVerifiedTarget, String> {
    let env = tx.query_opt(
        "SELECT session_id,controller_epoch,controller_released,environment_profile::text,environment_profile_digest FROM public.app_core_sessionenvironment WHERE id=$1 FOR UPDATE",
        &[&request.environment_id],
    ).map_err(|e| e.to_string())?.ok_or("provider_environment_retirement_environment_missing")?;
    if env.get::<_, i64>(1) != request.expected_controller_epoch || !env.get::<_, bool>(2) {
        return Err("provider_environment_retirement_controller_rejected".into());
    }
    let frozen: EnvironmentProfile = serde_json::from_str(
        &env.get::<_, Option<String>>(3)
            .ok_or("provider_environment_retirement_profile_missing")?,
    )
    .map_err(|_| "provider_environment_retirement_profile_rejected")?;
    let frozen_digest = frozen.digest()?;
    let EnvironmentProfile::E2b(profile) = &frozen else {
        return Err("provider_environment_retirement_profile_rejected".into());
    };
    let target = load_environment_target(tx, request.environment_id)?;
    if profile.lifetime != "retained"
        || profile.provider_config_digest != configured_digest
        || env.get::<_, Option<String>>(4).as_deref() != Some(frozen_digest.as_str())
        || target.identity.profile_digest != frozen_digest
        || target.identity.environment_id != request.environment_id
        || target.identity.session_id != env.get::<_, String>(0)
        || target.identity.attempt_id != request.creation_attempt_id
        || target.identity.provider_config_digest != request.provider_config_digest
        || target.sandbox_id != request.sandbox_id
        || target.verification_digest
            != cleanup::target_verification_digest(
                &target.identity,
                &profile.template_ref.template_id,
                configured_digest,
                &target.sandbox_id,
            )?
    {
        return Err("provider_environment_retirement_target_rejected".into());
    }
    let consumers = tx.query(
        "SELECT agent_run_id FROM runtime.provider_environment_run_uses WHERE environment_id=$1 ORDER BY provider_use_id FOR SHARE",
        &[&request.environment_id],
    ).map_err(|e| e.to_string())?;
    for row in consumers {
        let run: String = row.get(0);
        let receipt = receipts::load_receipt(tx, &run)?
            .ok_or("provider_environment_retirement_consumer_pending")?;
        if receipt.environment_id != request.environment_id
            || receipt.profile_digest != frozen_digest
            || receipt.provider_config_digest != configured_digest
        {
            return Err("provider_environment_retirement_consumer_rejected".into());
        }
        let released: bool = tx.query_one(
            "SELECT EXISTS(SELECT 1 FROM public.app_core_agentrunenvironmentrelease WHERE agent_run_id=$1 AND environment_id=$2 AND authorization_digest=$3 AND controller_epoch>=$4 AND controller_epoch<=$5)",
            &[&run,&request.environment_id,&receipt.authorization_digest,&receipt.controller_epoch,&request.expected_controller_epoch],
        ).map_err(|e| e.to_string())?.get(0);
        if !released {
            return Err("provider_environment_retirement_release_pending".into());
        }
    }
    Ok(target)
}

fn require_matching_intent(
    db: &mut impl GenericClient,
    request: &ProviderEnvironmentRetirementRequest,
    body: &str,
    digest: &str,
) -> Result<bool, String> {
    let rows = db.query(
        "SELECT operation_id,environment_id,request_digest,request_json,expected_controller_epoch FROM runtime.provider_environment_retirements WHERE operation_id=$1 OR environment_id=$2 FOR SHARE",
        &[&request.operation_id,&request.environment_id],
    ).map_err(|e| e.to_string())?;
    if rows.is_empty() {
        return Ok(false);
    }
    if rows.len() != 1
        || rows[0].get::<_, String>(0) != request.operation_id
        || rows[0].get::<_, String>(1) != request.environment_id
        || rows[0].get::<_, String>(2) != digest
        || rows[0].get::<_, String>(3) != body
        || rows[0].get::<_, i64>(4) != request.expected_controller_epoch
    {
        return Err("provider_environment_retirement_intent_conflict".into());
    }
    Ok(true)
}

fn already_removed(
    db: &mut impl GenericClient,
    request: &ProviderEnvironmentRetirementRequest,
) -> Result<bool, String> {
    let Some(settlement) = cleanup::load_settlement(db, &request.creation_attempt_id)? else {
        return Ok(false);
    };
    if settlement.kind != cleanup::ProviderSettlementKind::Removed
        || settlement.sandbox_id.as_deref() != Some(request.sandbox_id.as_str())
        || settlement.controller_epoch != request.expected_controller_epoch
    {
        return Err("provider_environment_retirement_settlement_conflict".into());
    }
    Ok(true)
}

fn result(
    request: &ProviderEnvironmentRetirementRequest,
    status: &'static str,
) -> ProviderEnvironmentRetirementResult {
    ProviderEnvironmentRetirementResult {
        schema: "runtime.provider_environment.retire.result.v1",
        operation_id: request.operation_id.clone(),
        environment_id: request.environment_id.clone(),
        creation_attempt_id: request.creation_attempt_id.clone(),
        sandbox_id: request.sandbox_id.clone(),
        status,
    }
}
