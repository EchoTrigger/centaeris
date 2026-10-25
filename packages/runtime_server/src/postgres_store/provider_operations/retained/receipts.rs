use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedProviderQuiescence {
    pub agent_run_id: String,
    pub execution_id: String,
    pub instance_id: String,
    pub sandbox_id: String,
    pub controller_epoch: i64,
    pub quiescent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedProviderCleanupReceipt {
    pub schema: String,
    pub receipt_id: String,
    pub agent_run_id: String,
    pub environment_id: String,
    pub authorization_digest: String,
    pub controller_epoch: i64,
    pub cleanup_complete: bool,
    pub provider: String,
    pub profile_digest: String,
    pub provider_config_digest: String,
    pub scope: String,
    pub provider_use_id: String,
    pub provider_use_digest: String,
}

fn quiescence_digest(value: &RetainedProviderRunUse, epoch: i64) -> Result<String, String> {
    fact_digest(
        "runtime:retained-provider-run-quiescence:v1\0",
        &serde_json::json!({"agentRunId":value.agent_run_id,"executionId":value.execution_id,"instanceId":value.target.identity.instance_id,"sandboxId":value.target.sandbox_id,"controllerEpoch":epoch,"quiescent":true,"providerUseDigest":value.provider_use_digest}),
    )
}

fn receipt_id(value: &RetainedProviderRunUse) -> Result<String, String> {
    Ok(format!(
        "provider_retained_cleanup_{}",
        &fact_digest(
            "runtime:retained-provider-cleanup:v1\0",
            &serde_json::json!([value.provider_use_id, value.provider_use_digest])
        )?[7..]
    ))
}

pub(in crate::postgres_store::provider_operations) fn load_receipt(
    db: &mut impl GenericClient,
    run: &str,
) -> Result<Option<RetainedProviderCleanupReceipt>, String> {
    load_receipt_with_target_lookup(db, run, resolve_target)
}

pub(in crate::postgres_store) fn load_receipt_with_target_lookup<C: GenericClient>(
    db: &mut C,
    run: &str,
    lookup: impl FnOnce(&mut C, TargetLookup) -> Result<ProviderVerifiedTarget, String>,
) -> Result<Option<RetainedProviderCleanupReceipt>, String> {
    let row=db.query_opt("SELECT cleanup_receipt_id,cleanup_controller_epoch,quiescence_digest FROM runtime.provider_environment_run_uses WHERE agent_run_id=$1 AND cleanup_receipt_id IS NOT NULL",&[&run]).map_err(|e|e.to_string())?;
    let Some(row) = row else { return Ok(None) };
    let value =
        load_use_with_target_lookup(db, run, lookup)?.ok_or("retained_provider_use_missing")?;
    let epoch: i64 = row.get(1);
    let recorded_quiescence: String = row.get(2);
    let id: String = row.get(0);
    if epoch < 1
        || recorded_quiescence != quiescence_digest(&value, epoch)?
        || id != receipt_id(&value)?
    {
        return Err("retained_provider_cleanup_fact_rejected".into());
    }
    Ok(Some(RetainedProviderCleanupReceipt {
        schema: "runtime.agent_run.cleanup_receipt.retained_provider.v1".into(),
        receipt_id: id,
        agent_run_id: run.into(),
        environment_id: value.target.identity.environment_id.clone(),
        authorization_digest: value.authorization_digest,
        controller_epoch: epoch,
        cleanup_complete: true,
        provider: "e2b".into(),
        profile_digest: value.target.identity.profile_digest.clone(),
        provider_config_digest: value.target.identity.provider_config_digest.clone(),
        scope: "runEffectsOnly".into(),
        provider_use_id: value.provider_use_id,
        provider_use_digest: value.provider_use_digest,
    }))
}

impl PostgresRuntimeStore {
    pub fn confirm_retained_provider_run_cleanup(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        value: &RetainedProviderRunUse,
        configured_digest: &str,
        quiesce: impl FnOnce(&RetainedProviderRunUse) -> Result<RetainedProviderQuiescence, String>
            + Send,
    ) -> Result<RetainedProviderCleanupReceipt, String> {
        use crate::observations::provider_dispatch::{
            DispatchKind, DispatchMetric, DispatchObservation,
        };
        if authority.purpose != ControllerPurpose::Cleanup {
            return Err("retained_provider_cleanup_requires_cleanup_authority".into());
        }
        let mut observation = DispatchObservation::new(DispatchKind::RetainedRunCleanup);
        let result = self.cleanup_view().with_client(|client| {
            observation.entered_client();
            let result = (|| {
                let (mut tx, grant, existing_receipt) = observation.timed(DispatchMetric::AuthorityBefore, || {
                    let mut tx = client.transaction().map_err(|e| e.to_string())?;
                    super::super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
                    let grant = require_current_use(&mut tx, authority, value, configured_digest)?;
                    if let Some(receipt) = load_receipt(&mut tx, &value.agent_run_id)? {
                        if receipt.controller_epoch > grant.controller_epoch {
                            return Err("retained_provider_cleanup_epoch_rejected".into());
                        }
                        return Ok::<_, String>((tx, grant, Some(receipt)));
                    }
                    let conflict: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.provider_run_cleanup_receipts WHERE agent_run_id=$1) OR EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts WHERE agent_run_id=$1)", &[&value.agent_run_id]).map_err(|e| e.to_string())?.get(0);
                    if conflict {
                        return Err("provider_cleanup_receipt_scope_conflict".into());
                    }
                    Ok((tx, grant, None))
                })?;
                if let Some(receipt) = existing_receipt {
                    observation.timed(DispatchMetric::Commit, || tx.commit().map_err(|e| e.to_string()))?;
                    return Ok(receipt);
                }
                let proof = if retirement::confirmed_removal(&mut tx, &value.target)? {
                    // Independent maintenance has positively removed this exact
                    // sandbox. The missing original streams are no longer needed
                    // to prove that this Run cannot leave live sandbox effects.
                    RetainedProviderQuiescence {
                        agent_run_id: value.agent_run_id.clone(),
                        execution_id: value.execution_id.clone(),
                        instance_id: value.target.identity.instance_id.clone(),
                        sandbox_id: value.target.sandbox_id.clone(),
                        controller_epoch: grant.controller_epoch,
                        quiescent: true,
                    }
                } else {
                    observation.callback(|| quiesce(value))?
                };
                // This post-callback transaction stage includes receipt SQL;
                // commitElapsedMs below remains the existing tx.commit alone.
                let receipt = observation.timed(DispatchMetric::AuthorityAfter, || {
                    if !proof.quiescent || proof.agent_run_id != value.agent_run_id || proof.execution_id != value.execution_id || proof.instance_id != value.target.identity.instance_id || proof.sandbox_id != value.target.sandbox_id || proof.controller_epoch != grant.controller_epoch {
                        return Err("retained_provider_quiescence_rejected".into());
                    }
                    require_current_use(&mut tx, authority, value, configured_digest)?;
                    let id = receipt_id(value)?;
                    let quiescence = quiescence_digest(value, grant.controller_epoch)?;
                    let changed = tx.execute("UPDATE runtime.provider_environment_run_uses SET cleanup_receipt_id=$2,cleanup_controller_epoch=$3,quiescence_digest=$4,cleanup_recorded_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE provider_use_id=$1 AND cleanup_receipt_id IS NULL", &[&value.provider_use_id,&id,&grant.controller_epoch,&quiescence]).map_err(|e| e.to_string())?;
                    if changed != 1 {
                        return Err("retained_provider_cleanup_receipt_conflict".into());
                    }
                    let receipt = load_receipt(&mut tx, &value.agent_run_id)?.ok_or("retained_provider_cleanup_receipt_missing")?;
                    Ok::<_, String>(receipt)
                })?;
                observation.timed(DispatchMetric::Commit, || tx.commit().map_err(|e| e.to_string()))?;
                Ok(receipt)
            })();
            observation.leaving_client();
            result
        });
        observation.finish(result.is_ok());
        result
    }
}
