//! Provider cleanup facts are separate from historical dispatch and business release.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

mod no_effects;
pub use no_effects::NoProviderEffectsReceipt;

#[derive(Debug, Clone)]
pub struct ProviderSandboxObservation {
    pub sandbox_id: String,
    pub template_id: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderVerifiedTarget {
    pub identity: ProviderCreationIdentity,
    pub sandbox_id: String,
    pub verification_digest: String,
}

#[derive(Debug)]
pub enum ProviderExecutionDispatchError<E> {
    /// Current authority was rejected before entering the physical callback.
    Rejected(String),
    /// Preserve the adapter's own known/unknown dispatch classification.
    Callback(E),
    /// Physical dispatch may have occurred before a fence or DB failure.
    AfterDispatch(String),
}

impl<E> From<String> for ProviderExecutionDispatchError<E> {
    fn from(error: String) -> Self {
        Self::Rejected(error)
    }
}

#[derive(Debug)]
pub enum ProviderRemovalEvidence {
    Deleted { sandbox_id: String },
    NotFound { sandbox_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderSettlementKind {
    NotDispatched,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCreationSettlement {
    pub settlement_id: String,
    pub kind: ProviderSettlementKind,
    pub sandbox_id: Option<String>,
    pub controller_epoch: i64,
    pub recorded_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCleanupReceipt {
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
    pub settlement_digest: String,
    pub operation_count: u32,
}

/// The schema in each exact response is its explicit wire discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ProviderRunCleanupReceipt {
    Settled(ProviderCleanupReceipt),
    NoProviderEffects(NoProviderEffectsReceipt),
    RetainedProvider(super::retained::RetainedProviderCleanupReceipt),
}

impl From<ProviderCleanupReceipt> for ProviderRunCleanupReceipt {
    fn from(value: ProviderCleanupReceipt) -> Self {
        Self::Settled(value)
    }
}
impl From<NoProviderEffectsReceipt> for ProviderRunCleanupReceipt {
    fn from(value: NoProviderEffectsReceipt) -> Self {
        Self::NoProviderEffects(value)
    }
}
impl ProviderRunCleanupReceipt {
    pub fn receipt_id(&self) -> &str {
        self.binding().0
    }
    fn binding(&self) -> (&str, &str, &str, i64) {
        match self {
            Self::Settled(value) => (
                &value.receipt_id,
                &value.authorization_digest,
                &value.environment_id,
                value.controller_epoch,
            ),
            Self::RetainedProvider(value) => (
                &value.receipt_id,
                &value.authorization_digest,
                &value.environment_id,
                value.controller_epoch,
            ),
            Self::NoProviderEffects(value) => (
                &value.receipt_id,
                &value.authorization_digest,
                &value.environment_id,
                value.controller_epoch,
            ),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCleanupReceiptQuery {
    pub schema: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub cleanup_receipt_id: String,
}

impl PostgresRuntimeStore {
    /// Persist observed ownership before ordinary execution can start. This is
    /// the same immutable cleanup target, not a Ready or live-resource fact.
    pub fn record_provider_execution_target(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
        configured_digest: &str,
        observation: &ProviderSandboxObservation,
    ) -> Result<ProviderVerifiedTarget, String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant = require_execution(&mut tx, authority, identity, configured_digest)?;
            let operation = super::load(&mut tx, &identity.attempt_id, true)?
                .ok_or("provider_creation_missing")?;
            require_identity(&operation, identity)?;
            require_unsettled_dispatch(&mut tx, &operation)?;
            let sandbox_id = operation
                .reported_sandbox_id
                .as_deref()
                .ok_or("provider_execution_target_unknown")?;
            let target =
                observed_target(&grant, identity, configured_digest, sandbox_id, observation)?;
            // An operation-row lock can wait after the first authority check.
            // Verify the lease again using the database's current clock.
            require_execution(&mut tx, authority, identity, configured_digest)?;
            let target = persist_verified_target(&mut tx, &target)?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok(target)
        })
    }

    pub fn with_provider_execution_dispatch<T: Send, E: Send>(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
        configured_digest: &str,
        dispatch: impl FnOnce(&ProviderVerifiedTarget) -> Result<T, E> + Send,
    ) -> Result<T, ProviderExecutionDispatchError<E>> {
        use crate::observations::provider_dispatch::{
            DispatchKind, DispatchMetric, DispatchObservation,
        };
        let mut observation = DispatchObservation::new(DispatchKind::PerRunExecution);
        let mut callback_started = false;
        let result: Result<T, ProviderExecutionDispatchError<E>> =
            self.with_client_error(|client| {
                observation.entered_client();
                let result = (|| {
                    let (mut tx, target) =
                        observation.timed(DispatchMetric::AuthorityBefore, || {
                            let mut tx = client.transaction().map_err(|error| error.to_string())?;
                            super::super::resource_commit::require_durable_resource_commit(
                                &mut tx,
                            )?;
                            require_execution(&mut tx, authority, identity, configured_digest)?;
                            let operation = super::load(&mut tx, &identity.attempt_id, true)?
                                .ok_or_else(|| "provider_creation_missing".to_string())?;
                            require_identity(&operation, identity)?;
                            require_unsettled_dispatch(&mut tx, &operation)?;
                            let target = load_target(&mut tx, identity)?.ok_or_else(|| {
                                "provider_execution_verified_target_required".to_string()
                            })?;
                            require_execution(&mut tx, authority, identity, configured_digest)?;
                            Ok::<_, ProviderExecutionDispatchError<E>>((tx, target))
                        })?;

                    // Preserve the existing authority and transaction scope. A
                    // framed-helper callback includes all of its process waits.
                    callback_started = true;
                    let output = observation.callback(|| {
                        dispatch(&target).map_err(ProviderExecutionDispatchError::Callback)
                    })?;
                    observation.timed(DispatchMetric::AuthorityAfter, || {
                        require_execution(&mut tx, authority, identity, configured_digest)
                            .map_err(ProviderExecutionDispatchError::AfterDispatch)
                    })?;
                    observation.timed(DispatchMetric::Commit, || {
                        tx.commit().map_err(|error| {
                            ProviderExecutionDispatchError::AfterDispatch(error.to_string())
                        })
                    })?;
                    Ok(output)
                })();
                observation.leaving_client();
                result
            });
        observation.finish(result.is_ok());
        match result {
            // Pool-return and scoped blocking errors can occur after the
            // callback. They cannot prove that its physical effect was absent.
            Err(ProviderExecutionDispatchError::Rejected(error)) if callback_started => {
                Err(ProviderExecutionDispatchError::AfterDispatch(error))
            }
            other => other,
        }
    }

    pub fn verify_provider_cleanup_target(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
        configured_digest: &str,
        inspect: impl FnOnce(&ProviderCreationIdentity, &str) -> Result<ProviderSandboxObservation, String>
            + Send,
    ) -> Result<ProviderVerifiedTarget, String> {
        self.cleanup_view().with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant =
                require_cleanup(&mut tx, authority, Some(identity), Some(configured_digest))?;
            let operation = super::load(&mut tx, &identity.attempt_id, true)?
                .ok_or("provider_creation_missing")?;
            require_identity(&operation, identity)?;
            if let Some(target) = load_target(&mut tx, identity)? {
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(target);
            }
            if operation.dispatch_state != ProviderCreationDispatchState::Dispatched {
                return Err("provider_cleanup_requires_dispatched_target".into());
            }
            let sandbox_id = operation
                .reported_sandbox_id
                .ok_or("provider_cleanup_target_unknown")?;
            let observation = inspect(identity, &sandbox_id)?;
            let target = observed_target(
                &grant,
                identity,
                configured_digest,
                &sandbox_id,
                &observation,
            )?;
            // Pins prevent another controller replacing us during inspection;
            // wall-clock lease expiry still has to be checked after the I/O.
            require_cleanup(&mut tx, authority, Some(identity), Some(configured_digest))?;
            let target = persist_verified_target(&mut tx, &target)?;
            // This committed identity is required by the separate delete step.
            tx.commit().map_err(|e| e.to_string())?;
            Ok(target)
        })
    }

    pub fn remove_provider_creation(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
        configured_digest: &str,
        remove: impl FnOnce(&ProviderVerifiedTarget) -> Result<ProviderRemovalEvidence, String> + Send,
    ) -> Result<ProviderCreationSettlement, String> {
        self.cleanup_view().with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant =
                require_cleanup(&mut tx, authority, Some(identity), Some(configured_digest))?;
            let operation = super::load(&mut tx, &identity.attempt_id, true)?
                .ok_or("provider_creation_missing")?;
            require_identity(&operation, identity)?;
            if let Some(settlement) = load_settlement(&mut tx, &identity.attempt_id)? {
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(settlement);
            }
            let target = load_target(&mut tx, identity)?
                .ok_or("provider_cleanup_verified_target_required")?;
            let removed = match remove(&target)? {
                ProviderRemovalEvidence::Deleted { sandbox_id }
                | ProviderRemovalEvidence::NotFound { sandbox_id } => sandbox_id,
            };
            if removed != target.sandbox_id {
                return Err("provider_cleanup_removed_identity_rejected".into());
            }
            require_cleanup(&mut tx, authority, Some(identity), Some(configured_digest))?;
            let settlement = insert_settlement(
                &mut tx,
                identity,
                ProviderSettlementKind::Removed,
                Some(&removed),
                grant.controller_epoch,
            )?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(settlement)
        })
    }

    pub fn settle_provider_creation_not_dispatched(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
    ) -> Result<ProviderCreationSettlement, String> {
        self.with_execution_control_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant = if authority.environment_authorization.payload.execution_mode == "retained" {
                super::retained::require_unsent_cleanup(&mut tx,authority,identity)?
            } else { require_cleanup(&mut tx, authority, Some(identity), None)? };
            let operation = super::load(&mut tx, &identity.attempt_id, true)?.ok_or("provider_creation_missing")?;
            require_identity(&operation, identity)?;
            if let Some(settlement) = load_settlement(&mut tx, &identity.attempt_id)? {
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(settlement);
            }
            if operation.dispatch_state == ProviderCreationDispatchState::Dispatched {
                return Err("provider_creation_not_dispatched_unproven".into());
            }
            if operation.dispatch_state == ProviderCreationDispatchState::Prepared {
                tx.execute("UPDATE runtime.provider_creation_operations SET dispatch_state='notDispatched' WHERE attempt_id=$1", &[&identity.attempt_id]).map_err(|e| e.to_string())?;
            }
            let settlement = insert_settlement(&mut tx, identity, ProviderSettlementKind::NotDispatched, None, grant.controller_epoch)?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(settlement)
        })
    }

    pub fn confirm_provider_run_cleanup(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
    ) -> Result<ProviderCleanupReceipt, String> {
        self.with_execution_control_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant = if authority.environment_authorization.payload.execution_mode == "retained" {
                if authority.purpose!=ControllerPurpose::Cleanup { return Err("provider_cleanup_requires_cleanup_authority".into()); }
                super::retained::require_authority(&mut tx,authority,None)?
            } else { require_cleanup(&mut tx, authority, None, None)? };
            let run = &grant.authorization.agent_run_id;
            if authority.environment_authorization.payload.execution_mode == "retained" {
                super::retained::require_only_unsent_settlements(&mut tx,run,&authority.environment_authorization.payload.environment_id)?;
            }
            if no_effects::load_no_effects_receipt(&mut tx, run)?.is_some() {
                return Err("provider_cleanup_receipt_scope_conflict".into());
            }
            if let Some(receipt) = load_receipt(&mut tx, run)? {
                if !(1..=grant.controller_epoch).contains(&receipt.controller_epoch) {
                    return Err("provider_cleanup_receipt_epoch_rejected".into());
                }
                require_receipt_facts(&mut tx, &receipt)?;
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(receipt);
            }
            let EnvironmentProfile::E2b(profile) = &grant.authorization.environment_profile else {
                return Err("provider_cleanup_requires_e2b_profile".into());
            };
            let (operation_count, settlement_digest) = settlement_summary(&mut tx, run, &authority.environment_authorization.payload.environment_id, &grant.authorization_digest, &grant.authorization.environment_profile_digest, &profile.provider_config_digest, grant.controller_epoch)?;
            let receipt_id = format!("provider_cleanup_{}", &digest("runtime:provider-run-cleanup:v1", &serde_json::json!([run, grant.authorization_digest, authority.environment_authorization.payload.environment_id]))?[7..]);
            tx.execute("INSERT INTO runtime.provider_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,profile_digest,provider_config_digest,controller_epoch,settlement_digest,operation_count,recorded_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint)",
                &[&receipt_id, run, &authority.environment_authorization.payload.environment_id, &grant.authorization_digest, &grant.authorization.environment_profile_digest, &profile.provider_config_digest, &grant.controller_epoch, &settlement_digest, &i64::from(operation_count)]).map_err(|e| e.to_string())?;
            let receipt = load_receipt(&mut tx, run)?.ok_or("provider_cleanup_receipt_commit_missing")?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(receipt)
        })
    }

    /// Read observations only. Each subsequent action rechecks its own authority.
    pub fn provider_creation_operations_for_cleanup(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        configured_digest: &str,
    ) -> Result<Vec<ProviderCreationOperation>, String> {
        self.with_execution_control_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let grant = require_cleanup(&mut tx, authority, None, Some(configured_digest))?;
            let attempts = tx.query("SELECT attempt_id FROM runtime.provider_creation_operations WHERE agent_run_id=$1 ORDER BY attempt_id", &[&grant.authorization.agent_run_id]).map_err(|error| error.to_string())?;
            let mut operations = Vec::with_capacity(attempts.len());
            for attempt in attempts {
                let operation = super::load(&mut tx, &attempt.get::<_,String>(0), false)?
                    .ok_or("provider_creation_missing")?;
                require_current_binding(&grant, authority, &operation.identity)?;
                operations.push(operation);
            }
            tx.commit().map_err(|error| error.to_string())?;
            Ok(operations)
        })
    }

    pub fn get_provider_cleanup_receipt(
        &self,
        request: &ProviderCleanupReceiptQuery,
    ) -> Result<Option<ProviderRunCleanupReceipt>, String> {
        request.validate()?;
        self.with_execution_control_client(|client| {
            let Some(receipt) = load_run_receipt(client, &request.agent_run_id)? else {
                return Ok(None);
            };
            let (id, authorization, _, _) = receipt.binding();
            if id != request.cleanup_receipt_id || authorization != request.authorization_digest {
                return Ok(None);
            }
            Ok(Some(receipt))
        })
    }
}

impl ProviderCleanupReceiptQuery {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "runtime.agent_run.cleanup_receipt.get.v2" {
            return Err("provider_cleanup_receipt_query_invalid".into());
        }
        validate_identity(&self.agent_run_id)?;
        validate_identity(&self.cleanup_receipt_id)?;
        validate_digest(&self.authorization_digest)
    }
}

fn require_execution(
    tx: &mut postgres::Transaction<'_>,
    authority: &ControllerAuthorityRequest<'_>,
    identity: &ProviderCreationIdentity,
    configured_digest: &str,
) -> Result<VerifiedOriginalGrant, String> {
    require_execute(authority)?;
    let grant = verify_controller_authority(tx, authority)?;
    let EnvironmentProfile::E2b(profile) = &grant.authorization.environment_profile else {
        return Err("provider_execution_requires_e2b_profile".into());
    };
    if profile.lifetime != "perRun" || profile.provider_config_digest != configured_digest {
        return Err("provider_execution_policy_rejected".into());
    }
    require_current_binding(&grant, authority, identity)?;
    Ok(grant)
}

fn require_unsettled_dispatch(
    tx: &mut postgres::Transaction<'_>,
    operation: &ProviderCreationOperation,
) -> Result<(), String> {
    if operation.dispatch_state != ProviderCreationDispatchState::Dispatched {
        return Err("provider_execution_requires_dispatched_target".into());
    }
    if load_settlement(tx, &operation.identity.attempt_id)?.is_some() {
        return Err("provider_execution_target_settled".into());
    }
    Ok(())
}

pub(super) fn observed_target(
    grant: &VerifiedOriginalGrant,
    identity: &ProviderCreationIdentity,
    configured_digest: &str,
    sandbox_id: &str,
    observation: &ProviderSandboxObservation,
) -> Result<ProviderVerifiedTarget, String> {
    let EnvironmentProfile::E2b(profile) = &grant.authorization.environment_profile else {
        return Err("provider_cleanup_requires_e2b_profile".into());
    };
    if observation.sandbox_id != sandbox_id
        || observation.template_id != profile.template_ref.template_id
        || observation.metadata != expected_metadata(identity)
    {
        return Err("provider_cleanup_observation_binding_rejected".into());
    }
    let verification_digest = target_verification_digest(
        identity,
        &observation.template_id,
        configured_digest,
        &observation.sandbox_id,
    )?;
    Ok(ProviderVerifiedTarget {
        identity: identity.clone(),
        sandbox_id: sandbox_id.into(),
        verification_digest,
    })
}

pub(super) fn target_verification_digest(
    identity: &ProviderCreationIdentity,
    template_id: &str,
    configured_digest: &str,
    sandbox_id: &str,
) -> Result<String, String> {
    // Preserve the canonical hash domain of already persisted ownership evidence.
    digest(
        "runtime:provider-cleanup-target:v1",
        &serde_json::json!({
            "sandboxId": sandbox_id,
            "templateId": template_id,
            "metadata": expected_metadata(identity),
            "requestDigest": identity.request_digest,
            "providerConfigDigest": configured_digest,
        }),
    )
}

pub(super) fn persist_verified_target(
    tx: &mut postgres::Transaction<'_>,
    target: &ProviderVerifiedTarget,
) -> Result<ProviderVerifiedTarget, String> {
    if let Some(existing) = load_target(tx, &target.identity)? {
        if existing != *target {
            return Err("provider_cleanup_verification_conflict".into());
        }
        return Ok(existing);
    }
    let updated = tx
        .execute(
            "UPDATE runtime.provider_creation_operations
             SET verified_sandbox_id=$2,verification_digest=$3,
                 verified_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint
             WHERE attempt_id=$1 AND verified_sandbox_id IS NULL",
            &[
                &target.identity.attempt_id,
                &target.sandbox_id,
                &target.verification_digest,
            ],
        )
        .map_err(|error| error.to_string())?;
    if updated != 1 {
        return Err("provider_cleanup_target_commit_missing".into());
    }
    load_target(tx, &target.identity)?.ok_or("provider_cleanup_target_commit_missing".into())
}

fn require_cleanup(
    tx: &mut postgres::Transaction<'_>,
    authority: &ControllerAuthorityRequest<'_>,
    identity: Option<&ProviderCreationIdentity>,
    configured_digest: Option<&str>,
) -> Result<VerifiedOriginalGrant, String> {
    if authority.purpose != ControllerPurpose::Cleanup {
        return Err("provider_cleanup_requires_cleanup_authority".into());
    }
    let grant = verify_controller_authority(tx, authority)?;
    let EnvironmentProfile::E2b(profile) = &grant.authorization.environment_profile else {
        return Err("provider_cleanup_requires_e2b_profile".into());
    };
    if profile.lifetime != "perRun"
        || configured_digest.is_some_and(|value| value != profile.provider_config_digest)
    {
        return Err("provider_cleanup_policy_rejected".into());
    }
    if let Some(identity) = identity {
        require_current_binding(&grant, authority, identity)?;
    }
    Ok(grant)
}

pub(crate) fn expected_metadata(identity: &ProviderCreationIdentity) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("centaerisAttemptId".into(), identity.attempt_id.clone()),
        ("environmentId".into(), identity.environment_id.clone()),
        ("instanceId".into(), identity.instance_id.clone()),
        ("executionId".into(), identity.execution_id.clone()),
        ("agentRunId".into(), identity.agent_run_id.clone()),
        ("sessionId".into(), identity.session_id.clone()),
        ("workspaceId".into(), identity.workspace_id.clone()),
        ("profileDigest".into(), identity.profile_digest.clone()),
        (
            "authorizationDigest".into(),
            identity.authorization_digest.clone(),
        ),
        ("processingJobId".into(), identity.processing_job_id.clone()),
    ])
}

fn digest(domain: &str, value: &serde_json::Value) -> Result<String, String> {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    hash.update(crate::agent_run_authorization::canonical_json(value.clone())?.as_bytes());
    Ok(format!("sha256:{:x}", hash.finalize()))
}

pub(super) fn load_target(
    db: &mut impl GenericClient,
    identity: &ProviderCreationIdentity,
) -> Result<Option<ProviderVerifiedTarget>, String> {
    let row = db.query_one("SELECT verified_sandbox_id,verification_digest FROM runtime.provider_creation_operations WHERE attempt_id=$1", &[&identity.attempt_id]).map_err(|e| e.to_string())?;
    let Some(sandbox_id) = row.get::<_, Option<String>>(0) else {
        return Ok(None);
    };
    Ok(Some(ProviderVerifiedTarget {
        identity: identity.clone(),
        sandbox_id,
        verification_digest: row
            .get::<_, Option<String>>(1)
            .ok_or("provider_cleanup_verification_missing")?,
    }))
}

pub(super) fn load_settlement(
    db: &mut impl GenericClient,
    attempt: &str,
) -> Result<Option<ProviderCreationSettlement>, String> {
    let row = db.query_one("SELECT settlement_id,settlement_kind,settlement_sandbox_id,settlement_controller_epoch,settled_at_ms FROM runtime.provider_creation_operations WHERE attempt_id=$1", &[&attempt]).map_err(|e| e.to_string())?;
    let Some(settlement_id) = row.get::<_, Option<String>>(0) else {
        return Ok(None);
    };
    let kind = match row.get::<_, Option<String>>(1).as_deref() {
        Some("notDispatched") => ProviderSettlementKind::NotDispatched,
        Some("removed") => ProviderSettlementKind::Removed,
        _ => return Err("provider_cleanup_settlement_invalid".into()),
    };
    Ok(Some(ProviderCreationSettlement {
        settlement_id,
        kind,
        sandbox_id: row.get(2),
        controller_epoch: row
            .get::<_, Option<i64>>(3)
            .ok_or("provider_cleanup_epoch_missing")?,
        recorded_at_ms: row
            .get::<_, Option<i64>>(4)
            .ok_or("provider_cleanup_time_missing")?,
    }))
}

pub(super) fn insert_settlement(
    db: &mut impl GenericClient,
    identity: &ProviderCreationIdentity,
    kind: ProviderSettlementKind,
    sandbox: Option<&str>,
    epoch: i64,
) -> Result<ProviderCreationSettlement, String> {
    let id = format!(
        "provider_settlement_{}",
        &digest(
            "runtime:provider-creation-settlement:v1",
            &serde_json::json!([
                identity.attempt_id,
                identity.request_digest,
                identity.provider_config_digest
            ])
        )?[7..]
    );
    let kind = match kind {
        ProviderSettlementKind::NotDispatched => "notDispatched",
        ProviderSettlementKind::Removed => "removed",
    };
    db.execute("UPDATE runtime.provider_creation_operations SET settlement_id=$2,settlement_kind=$3,settlement_sandbox_id=$4,settlement_controller_epoch=$5,settled_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE attempt_id=$1 AND settlement_id IS NULL", &[&identity.attempt_id, &id, &kind, &sandbox, &epoch]).map_err(|e| e.to_string())?;
    load_settlement(db, &identity.attempt_id)?
        .ok_or("provider_cleanup_settlement_commit_missing".into())
}

fn settlement_summary(
    db: &mut impl GenericClient,
    run: &str,
    environment: &str,
    authorization: &str,
    profile: &str,
    config: &str,
    epoch: i64,
) -> Result<(u32, String), String> {
    let rows = db.query("SELECT row_to_json(f)::text FROM (SELECT attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,agent_run_id,authorization_digest,profile_digest,provider_config_digest,request_digest,idempotency_key,processing_job_id,dispatch_state,verified_sandbox_id,verification_digest,settlement_id,settlement_kind,settlement_sandbox_id,settlement_controller_epoch,settled_at_ms FROM runtime.provider_creation_operations WHERE agent_run_id=$1 ORDER BY attempt_id) f", &[&run]).map_err(|e| e.to_string())?;
    if rows.is_empty() {
        return Err("provider_cleanup_zero_operations_requires_no_effects".into());
    }
    let mut facts = Vec::with_capacity(rows.len());
    for row in rows {
        let fact: serde_json::Value =
            serde_json::from_str(&row.get::<_, String>(0)).map_err(|e| e.to_string())?;
        if fact["environment_id"] != environment
            || fact["authorization_digest"] != authorization
            || fact["profile_digest"] != profile
            || fact["provider_config_digest"] != config
            || fact["settlement_id"].is_null()
            || !fact["settlement_controller_epoch"]
                .as_i64()
                .is_some_and(|settled_epoch| (1..=epoch).contains(&settled_epoch))
        {
            return Err("provider_cleanup_unsettled_or_foreign_operation".into());
        }
        facts.push(fact);
    }
    let count =
        u32::try_from(facts.len()).map_err(|_| "provider_cleanup_operation_count_invalid")?;
    Ok((
        count,
        digest(
            "runtime:provider-run-settlements:v1",
            &serde_json::Value::Array(facts),
        )?,
    ))
}

fn load_receipt(
    db: &mut impl GenericClient,
    run: &str,
) -> Result<Option<ProviderCleanupReceipt>, String> {
    db.query_opt("SELECT receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,profile_digest,provider_config_digest,settlement_digest,operation_count FROM runtime.provider_run_cleanup_receipts WHERE agent_run_id=$1", &[&run]).map_err(|e| e.to_string())?.map(|row| {
        Ok(ProviderCleanupReceipt { schema: "runtime.agent_run.cleanup_receipt.v2".into(), receipt_id: row.get(0), agent_run_id: row.get(1), environment_id: row.get(2), authorization_digest: row.get(3), controller_epoch: row.get(4), cleanup_complete: true, provider: "e2b".into(), profile_digest: row.get(5), provider_config_digest: row.get(6), settlement_digest: row.get(7), operation_count: u32::try_from(row.get::<_, i64>(8)).map_err(|_| "provider_cleanup_operation_count_invalid")? })
    }).transpose()
}

fn require_receipt_facts(
    db: &mut impl GenericClient,
    receipt: &ProviderCleanupReceipt,
) -> Result<(), String> {
    let original = db.query_opt("SELECT r.environment_id,g.digest,g.payload->>'environmentProfileDigest',g.payload->'environmentProfile'->>'providerConfigDigest',g.payload->'environmentProfile'->>'lifetime',g.payload->'environmentProfile'->>'schema' FROM public.app_core_agentrun r JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id WHERE r.id=$1", &[&receipt.agent_run_id]).map_err(|e| e.to_string())?.ok_or("provider_cleanup_original_missing")?;
    if original.get::<_, Option<String>>(0).as_deref() != Some(&receipt.environment_id)
        || original.get::<_, String>(1) != receipt.authorization_digest
        || original.get::<_, Option<String>>(2).as_deref() != Some(&receipt.profile_digest)
        || original.get::<_, Option<String>>(3).as_deref() != Some(&receipt.provider_config_digest)
        || !matches!(
            original.get::<_, Option<String>>(4).as_deref(),
            Some("perRun" | "retained")
        )
        || original.get::<_, Option<String>>(5).as_deref()
            != Some("runtime.e2b_environment_profile.v1")
    {
        return Err("provider_cleanup_receipt_original_binding_rejected".into());
    }
    if original.get::<_, Option<String>>(4).as_deref() == Some("retained") {
        super::retained::require_only_unsent_settlements(
            db,
            &receipt.agent_run_id,
            &receipt.environment_id,
        )?;
    }
    let (count, digest) = settlement_summary(
        db,
        &receipt.agent_run_id,
        &receipt.environment_id,
        &receipt.authorization_digest,
        &receipt.profile_digest,
        &receipt.provider_config_digest,
        receipt.controller_epoch,
    )?;
    if count != receipt.operation_count || digest != receipt.settlement_digest {
        return Err("provider_cleanup_receipt_settlement_rejected".into());
    }
    Ok(())
}

fn load_run_receipt(
    db: &mut impl GenericClient,
    run: &str,
) -> Result<Option<ProviderRunCleanupReceipt>, String> {
    let physical = load_receipt(db, run)?;
    let no_effects = no_effects::load_no_effects_receipt(db, run)?;
    let retained = super::retained::receipts::load_receipt(db, run)?;
    match (physical, no_effects, retained) {
        (Some(receipt), None, None) => {
            require_receipt_facts(db, &receipt)?;
            Ok(Some(receipt.into()))
        }
        (None, Some(receipt), None) => Ok(Some(receipt.into())),
        (None, None, Some(receipt)) => {
            Ok(Some(ProviderRunCleanupReceipt::RetainedProvider(receipt)))
        }
        (None, None, None) => Ok(None),
        _ => Err("provider_cleanup_receipt_scope_conflict".into()),
    }
}

pub(crate) fn require_lifecycle_receipt(
    db: &mut impl GenericClient,
    run: &str,
    authorization: &str,
) -> Result<(), String> {
    let receipt =
        load_run_receipt(db, run)?.ok_or("lifecycle provider cleanup receipt required")?;
    let (_, original, environment, epoch) = receipt.binding();
    if original != authorization {
        return Err("lifecycle provider cleanup binding rejected".into());
    }
    let released = db.query_one("SELECT EXISTS(SELECT 1 FROM public.app_core_agentrunenvironmentrelease WHERE agent_run_id=$1 AND environment_id=$2 AND authorization_digest=$3 AND controller_epoch>=$4)", &[&run, &environment, &authorization, &epoch]).map_err(|e| e.to_string())?.get::<_, bool>(0);
    if !released {
        return Err("lifecycle provider release receipt required".into());
    }
    Ok(())
}

pub(crate) const DDL: &str = r#"
ALTER TABLE runtime.provider_creation_operations
 ADD COLUMN verified_sandbox_id text,
 ADD COLUMN verification_digest text,
 ADD COLUMN verified_at_ms bigint,
 ADD COLUMN settlement_id text UNIQUE,
 ADD COLUMN settlement_kind text,
 ADD COLUMN settlement_sandbox_id text,
 ADD COLUMN settlement_controller_epoch bigint,
 ADD COLUMN settled_at_ms bigint;
ALTER TABLE runtime.provider_creation_operations ADD CONSTRAINT provider_creation_cleanup_facts CHECK(
 ((verified_sandbox_id IS NULL AND verification_digest IS NULL AND verified_at_ms IS NULL)
  OR (dispatch_state='dispatched' AND verified_sandbox_id=reported_sandbox_id AND verified_sandbox_id IS NOT NULL
   AND verification_digest ~ '^sha256:[0-9a-f]{64}$' AND verification_digest IS NOT NULL AND verified_at_ms>=0 AND verified_at_ms IS NOT NULL))
 AND ((settlement_id IS NULL AND settlement_kind IS NULL AND settlement_sandbox_id IS NULL AND settlement_controller_epoch IS NULL AND settled_at_ms IS NULL)
  OR (settlement_id IS NOT NULL AND settlement_kind IS NOT NULL AND settlement_controller_epoch>0 AND settlement_controller_epoch IS NOT NULL AND settled_at_ms>=0 AND settled_at_ms IS NOT NULL
   AND ((settlement_kind='notDispatched' AND dispatch_state='notDispatched' AND verified_sandbox_id IS NULL AND settlement_sandbox_id IS NULL)
    OR (settlement_kind='removed' AND dispatch_state='dispatched' AND verified_sandbox_id IS NOT NULL AND settlement_sandbox_id=verified_sandbox_id AND settlement_sandbox_id IS NOT NULL AND settled_at_ms>=verified_at_ms)))));
DROP INDEX runtime.provider_creation_operations_open_key;
CREATE UNIQUE INDEX provider_creation_operations_open_key ON runtime.provider_creation_operations(environment_id) WHERE dispatch_state IN('prepared','dispatched') AND settlement_id IS NULL;
CREATE TABLE runtime.provider_run_cleanup_receipts(
 receipt_id text PRIMARY KEY,agent_run_id text NOT NULL UNIQUE,environment_id text NOT NULL,
 authorization_digest text NOT NULL,profile_digest text NOT NULL,provider_config_digest text NOT NULL,
 controller_epoch bigint NOT NULL,settlement_digest text NOT NULL,operation_count bigint NOT NULL,recorded_at_ms bigint NOT NULL,
 CONSTRAINT provider_run_cleanup_receipt_facts CHECK(controller_epoch>0 AND operation_count>0 AND recorded_at_ms>=0
  AND authorization_digest ~ '^sha256:[0-9a-f]{64}$' AND profile_digest ~ '^sha256:[0-9a-f]{64}$'
  AND provider_config_digest ~ '^sha256:[0-9a-f]{64}$' AND settlement_digest ~ '^sha256:[0-9a-f]{64}$'));
CREATE FUNCTION runtime.protect_provider_run_cleanup_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 RAISE EXCEPTION 'provider cleanup receipts are immutable';
END $$;
CREATE TRIGGER provider_run_cleanup_receipt_facts BEFORE UPDATE OR DELETE ON runtime.provider_run_cleanup_receipts FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_run_cleanup_receipt();
"#;

pub(crate) const GUARD_DDL: &str = r#"
CREATE OR REPLACE FUNCTION runtime.protect_provider_creation_operation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'provider creation responsibility is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.dispatch_state<>'prepared' OR NEW.reported_sandbox_id IS NOT NULL OR NEW.verified_sandbox_id IS NOT NULL OR NEW.settlement_id IS NOT NULL THEN
   RAISE EXCEPTION 'provider creation requires a prepared intent'; END IF;
 ELSE
  IF ROW(NEW.attempt_id,NEW.execution_id,NEW.instance_id,NEW.environment_id,NEW.session_id,NEW.workspace_id,NEW.agent_run_id,NEW.authorization_digest,NEW.profile_digest,NEW.provider,NEW.provider_config_digest,NEW.request_digest,NEW.idempotency_key,NEW.processing_job_id,NEW.recorded_at_ms)
    IS DISTINCT FROM ROW(OLD.attempt_id,OLD.execution_id,OLD.instance_id,OLD.environment_id,OLD.session_id,OLD.workspace_id,OLD.agent_run_id,OLD.authorization_digest,OLD.profile_digest,OLD.provider,OLD.provider_config_digest,OLD.request_digest,OLD.idempotency_key,OLD.processing_job_id,OLD.recorded_at_ms)
   OR OLD.settlement_id IS NOT NULL
   OR NOT (
    (OLD.dispatch_state='prepared' AND NEW.dispatch_state IN('dispatched','notDispatched') AND NEW.reported_sandbox_id IS NULL AND NEW.verified_sandbox_id IS NULL AND NEW.settlement_id IS NULL)
    OR (OLD.dispatch_state='dispatched' AND NEW.dispatch_state='dispatched' AND OLD.reported_sandbox_id IS NULL AND NEW.reported_sandbox_id IS NOT NULL AND NEW.verified_sandbox_id IS NULL AND NEW.settlement_id IS NULL)
    OR (OLD.dispatch_state='dispatched' AND NEW.dispatch_state='dispatched' AND OLD.verified_sandbox_id IS NULL AND NEW.verified_sandbox_id IS NOT NULL AND NEW.reported_sandbox_id=OLD.reported_sandbox_id AND NEW.settlement_id IS NULL)
    OR (NEW.dispatch_state=OLD.dispatch_state AND NEW.reported_sandbox_id IS NOT DISTINCT FROM OLD.reported_sandbox_id
     AND ROW(NEW.verified_sandbox_id,NEW.verification_digest,NEW.verified_at_ms) IS NOT DISTINCT FROM ROW(OLD.verified_sandbox_id,OLD.verification_digest,OLD.verified_at_ms)
     AND NEW.settlement_id IS NOT NULL))
  THEN RAISE EXCEPTION 'provider creation identity or evidence transition rejected'; END IF;
 END IF;
 RETURN NEW;
END $$;
"#;

pub(crate) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let body = DDL.split_once("CREATE FUNCTION runtime.protect_provider_run_cleanup_receipt() RETURNS trigger LANGUAGE plpgsql AS $$").ok_or("provider cleanup function missing")?.1.split_once("$$;").ok_or("provider cleanup body missing")?.0;
    let actual = db.query_opt("SELECT prosrc FROM pg_proc WHERE oid=to_regprocedure('runtime.protect_provider_run_cleanup_receipt()')", &[]).map_err(|e| e.to_string())?;
    if actual.is_none_or(|row| row.get::<_, String>(0) != body) {
        return Err("Postgres provider cleanup function mismatch".into());
    }
    let valid = db.query_one("SELECT count(*)=1 FROM pg_trigger WHERE tgrelid='runtime.provider_run_cleanup_receipts'::regclass AND tgname='provider_run_cleanup_receipt_facts' AND tgenabled='O' AND NOT tgisinternal AND tgtype=27 AND tgfoid='runtime.protect_provider_run_cleanup_receipt()'::regprocedure AND tgqual IS NULL AND tgnargs=0", &[]).map_err(|e| e.to_string())?.get::<_, bool>(0);
    if !valid {
        return Err("Postgres provider cleanup trigger mismatch".into());
    }
    let constraints = db.query_one("SELECT count(*)=3 AND bool_and(convalidated AND NOT condeferrable) AND count(*) FILTER(WHERE contype='p' AND conkey=ARRAY[1]::smallint[])=1 AND count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[2]::smallint[])=1 AND count(*) FILTER(WHERE contype='c')=1 FROM pg_constraint WHERE conrelid='runtime.provider_run_cleanup_receipts'::regclass AND contype<>'n'", &[]).map_err(|e| e.to_string())?.get::<_, bool>(0);
    if !constraints {
        return Err("Postgres provider cleanup constraints mismatch".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn restore_previous_fixture(db: &mut postgres::Client) {
    super::retained::restore_previous_fixture(db);
    if !db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=18)",
            &[],
        )
        .unwrap()
        .get::<_, bool>(0)
    {
        return;
    }
    assert_eq!(db.query_one("SELECT (SELECT count(*) FROM runtime.provider_run_cleanup_receipts)+(SELECT count(*) FROM runtime.provider_creation_operations WHERE verified_sandbox_id IS NOT NULL OR settlement_id IS NOT NULL)", &[]).unwrap().get::<_, i64>(0), 0, "cannot discard provider cleanup facts");
    db.batch_execute("DROP TABLE runtime.provider_run_cleanup_receipts; DROP FUNCTION runtime.protect_provider_run_cleanup_receipt(); DROP INDEX runtime.provider_creation_operations_open_key; ALTER TABLE runtime.provider_creation_operations DROP CONSTRAINT provider_creation_cleanup_facts; ALTER TABLE runtime.provider_creation_operations DROP COLUMN verified_sandbox_id,DROP COLUMN verification_digest,DROP COLUMN verified_at_ms,DROP COLUMN settlement_id,DROP COLUMN settlement_kind,DROP COLUMN settlement_sandbox_id,DROP COLUMN settlement_controller_epoch,DROP COLUMN settled_at_ms; CREATE UNIQUE INDEX provider_creation_operations_open_key ON runtime.provider_creation_operations(environment_id) WHERE dispatch_state IN('prepared','dispatched'); DELETE FROM runtime.schema_migrations WHERE version=18;").unwrap();
    let original = super::DDL
        .split_once("CREATE FUNCTION runtime.protect_provider_creation_operation()")
        .unwrap()
        .1
        .split_once("CREATE TRIGGER")
        .unwrap()
        .0;
    db.batch_execute(&format!(
        "CREATE OR REPLACE FUNCTION runtime.protect_provider_creation_operation(){original}"
    ))
    .unwrap();
}
