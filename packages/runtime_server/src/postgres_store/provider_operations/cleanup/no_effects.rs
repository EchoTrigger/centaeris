//! An owned terminal Run can have no provider creation attempt at all.
//! This proves cleanup only; business controller release remains independent.
use super::*;
use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;

pub const NO_PROVIDER_EFFECTS_RECEIPT_SCHEMA: &str =
    "runtime.agent_run.cleanup_receipt.no_provider_effects.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NoProviderEffectsReceipt {
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
}

impl PostgresRuntimeStore {
    pub fn confirm_provider_run_no_effects(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
    ) -> Result<NoProviderEffectsReceipt, String> {
        self.with_execution_control_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            super::super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant = if authority.environment_authorization.payload.execution_mode == "retained" {
                if authority.purpose!=ControllerPurpose::Cleanup { return Err("provider_cleanup_requires_cleanup_authority".into()); }
                super::super::retained::require_authority(&mut tx,authority,None)?
            } else { require_cleanup(&mut tx, authority, None, None)? };
            let run = &grant.authorization.agent_run_id;
            if let Some(receipt) = load_no_effects_receipt(&mut tx, run)? {
                if !(1..=grant.controller_epoch).contains(&receipt.controller_epoch) {
                    return Err("provider_no_effects_receipt_epoch_rejected".into());
                }
                tx.commit().map_err(|error| error.to_string())?;
                return Ok(receipt);
            }
            let environment = &authority.environment_authorization.payload.environment_id;
            require_no_run_responsibility(&mut tx, run)?;
            // Only the initial proof covers prior Environment responsibilities.
            // A successor admitted after real release does not invalidate it.
            let unresolved = tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.provider_creation_operations WHERE environment_id=$1 AND settlement_id IS NULL)", &[&environment]).map_err(|error| error.to_string())?.get::<_,bool>(0);
            if authority.environment_authorization.payload.execution_mode == "retained" {
                super::super::retained::require_no_effects_environment(&mut tx,run,environment)?;
            } else if unresolved {
                return Err("provider_no_effects_environment_responsibility_pending".into());
            }
            require_legacy_settled(&mut tx, "environment_id", environment)?;
            let receipt_id = receipt_id(run, &grant.authorization_digest, environment)?;
            tx.execute("INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection,recorded_at_ms,physical_removed,removed_container_id) VALUES($1,$2,$3,$4,$5,'neverAllocated',NULL,true,'deferred',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,false,NULL) ON CONFLICT(agent_run_id) DO NOTHING",
                &[&receipt_id,run,&environment,&grant.authorization_digest,&grant.controller_epoch]).map_err(|error| error.to_string())?;
            let receipt = load_no_effects_receipt(&mut tx, run)?
                .ok_or("provider_no_effects_receipt_conflict")?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok(receipt)
        })
    }
}

fn receipt_id(run: &str, authorization: &str, environment: &str) -> Result<String, String> {
    Ok(format!(
        "provider_no_effects_{}",
        &digest(
            "runtime:provider-no-effects-cleanup:v1",
            &serde_json::json!([run, authorization, environment])
        )?[7..]
    ))
}

pub(super) fn load_no_effects_receipt(
    db: &mut impl GenericClient,
    run: &str,
) -> Result<Option<NoProviderEffectsReceipt>, String> {
    let row = db.query_opt("SELECT c.receipt_id,c.environment_id,c.authorization_digest,c.controller_epoch,c.allocation_kind,c.instance_id,c.cleanup_complete,c.file_protection,c.physical_removed,c.removed_container_id,c.recorded_at_ms,r.environment_id,r.session_id,g.digest,g.payload::text,e.session_id,e.environment_profile::text,e.environment_profile_digest,e.controller_epoch FROM runtime.agent_run_cleanup_receipts c JOIN public.app_core_agentrun r ON r.id=c.agent_run_id JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id JOIN public.app_core_sessionenvironment e ON e.id=r.environment_id WHERE c.agent_run_id=$1", &[&run]).map_err(|error| error.to_string())?;
    let Some(row) = row else { return Ok(None) };
    let id: String = row.get(0);
    let environment: String = row.get(1);
    let authorization: String = row.get(2);
    if id != receipt_id(run, &authorization, &environment)? {
        // A legacy neverAllocated receipt is not this provider-scoped proof.
        return Ok(None);
    }
    let original: WorkspaceAgentRunAuthorization = serde_json::from_str(&row.get::<_, String>(14))
        .map_err(|_| "provider_no_effects_original_invalid")?;
    original
        .checked_binding(run)?
        .verify_digest_binding(&authorization)?;
    let EnvironmentProfile::E2b(profile) = &original.environment_profile else {
        return Err("provider_no_effects_profile_rejected".into());
    };
    let frozen: EnvironmentProfile = serde_json::from_str(
        &row.get::<_, Option<String>>(16)
            .ok_or("provider_no_effects_profile_missing")?,
    )
    .map_err(|_| "provider_no_effects_profile_invalid")?;
    let epoch: i64 = row.get(3);
    if row.get::<_, String>(4) != "neverAllocated"
        || row.get::<_, Option<String>>(5).is_some()
        || !row.get::<_, bool>(6)
        || row.get::<_, String>(7) != "deferred"
        || row.get::<_, bool>(8)
        || row.get::<_, Option<String>>(9).is_some()
        || row.get::<_, i64>(10) < 0
        || row.get::<_, Option<String>>(11).as_deref() != Some(environment.as_str())
        || row.get::<_, String>(12) != original.session_id
        || row.get::<_, String>(13) != authorization
        || original.agent_run_id != run
        || row.get::<_, String>(15) != original.session_id
        || frozen != original.environment_profile
        || row.get::<_, Option<String>>(17).as_deref()
            != Some(original.environment_profile_digest.as_str())
        || original.environment_profile.digest()? != original.environment_profile_digest
        || !matches!(profile.lifetime.as_str(), "perRun" | "retained")
        || !(1..=row.get::<_, i64>(18)).contains(&epoch)
    {
        return Err("provider_no_effects_receipt_original_binding_rejected".into());
    }
    require_no_run_responsibility(db, run)?;
    Ok(Some(NoProviderEffectsReceipt {
        schema: NO_PROVIDER_EFFECTS_RECEIPT_SCHEMA.into(),
        receipt_id: id,
        agent_run_id: run.into(),
        environment_id: environment,
        authorization_digest: authorization,
        controller_epoch: epoch,
        cleanup_complete: true,
        provider: "e2b".into(),
        profile_digest: original.environment_profile_digest.clone(),
        provider_config_digest: profile.provider_config_digest.clone(),
        scope: "noProviderEffectsForRun".into(),
    }))
}

fn require_no_run_responsibility(db: &mut impl GenericClient, run: &str) -> Result<(), String> {
    let effects = db.query_one("SELECT EXISTS(SELECT 1 FROM runtime.provider_creation_operations WHERE agent_run_id=$1) OR EXISTS(SELECT 1 FROM runtime.provider_run_cleanup_receipts WHERE agent_run_id=$1) OR EXISTS(SELECT 1 FROM runtime.provider_environment_run_uses WHERE agent_run_id=$1)", &[&run]).map_err(|error| error.to_string())?.get::<_,bool>(0);
    if effects {
        return Err("provider_no_effects_run_responsibility_present".into());
    }
    require_legacy_settled(db, "agent_run_id", run)
}

fn require_legacy_settled(
    db: &mut impl GenericClient,
    scope: &str,
    identity: &str,
) -> Result<(), String> {
    // scope is chosen only by the two internal call sites, never by a request.
    let query = match scope {
        "environment_id" => "SELECT allocation_execution_id,instance_id,environment_id FROM runtime.execution_instances WHERE environment_id=$1",
        "agent_run_id" => "SELECT allocation_execution_id,instance_id,environment_id FROM runtime.execution_instances WHERE agent_run_id=$1",
        _ => unreachable!("fixed legacy responsibility scope"),
    };
    let holding = match scope {
        "environment_id" => "SELECT EXISTS(SELECT 1 FROM runtime.resident_sandboxes WHERE environment_id=$1 OR execution_id IN(SELECT allocation_execution_id FROM runtime.execution_instances WHERE environment_id=$1)) OR EXISTS(SELECT 1 FROM runtime.environment_retirements WHERE environment_id=$1 AND state<>'destroyed')",
        _ => "SELECT EXISTS(SELECT 1 FROM runtime.resident_sandboxes s JOIN runtime.execution_instances i ON i.allocation_execution_id=s.execution_id AND i.instance_id=s.instance_id WHERE i.agent_run_id=$1)",
    };
    if db
        .query_one(holding, &[&identity])
        .map_err(|error| error.to_string())?
        .get::<_, bool>(0)
    {
        return Err("provider_no_effects_legacy_holding_present".into());
    }
    let rows = db
        .query(query, &[&identity])
        .map_err(|error| error.to_string())?;
    for row in rows {
        let execution: String = row.get(0);
        let instance: String = row.get(1);
        let environment: String = row.get(2);
        if super::super::super::creation_attempts::not_materialized(db, &execution, &instance)? {
            continue;
        }
        let settled = db.query_one("SELECT EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts WHERE environment_id=$1 AND instance_id=$2 AND allocation_kind='instance' AND cleanup_complete AND physical_removed AND removed_container_id IS NOT NULL) OR EXISTS(SELECT 1 FROM runtime.environment_retirements WHERE environment_id=$1 AND instance_id=$2 AND allocation_execution_id=$3 AND state='destroyed')", &[&environment,&instance,&execution]).map_err(|error| error.to_string())?.get::<_,bool>(0);
        if !settled {
            return Err("provider_no_effects_legacy_responsibility_pending".into());
        }
    }
    Ok(())
}
