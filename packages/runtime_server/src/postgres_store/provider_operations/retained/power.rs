//! Durable, independently authorized provider pause/resume operations.
use super::*;
use crate::observations::power::{PowerMetric, PowerObservation, PowerOutcome};
use measurements::PowerStage;
use serde::{Deserialize, Deserializer, Serialize};

mod measurements;
pub(crate) mod schema;
mod state;
mod wake;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ProviderEnvironmentPowerAction {
    Pause,
    Resume,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderEnvironmentPowerRequest {
    pub schema: String,
    pub operation_id: String,
    pub action: ProviderEnvironmentPowerAction,
    pub environment_id: String,
    pub creation_attempt_id: String,
    pub sandbox_id: String,
    pub provider_config_digest: String,
    pub expected_controller_epoch: i64,
    #[serde(deserialize_with = "required_predecessor")]
    pub predecessor_operation_id: Option<String>,
}

fn required_predecessor<'de, D: Deserializer<'de>>(value: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(value)
}

impl ProviderEnvironmentPowerRequest {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema != "runtime.provider_environment.power.v1"
            || self.expected_controller_epoch < 0
        {
            return Err("provider_environment_power_request_invalid".into());
        }
        for value in [
            &self.operation_id,
            &self.environment_id,
            &self.creation_attempt_id,
            &self.sandbox_id,
        ] {
            super::super::validate_identity(value)
                .map_err(|_| "provider_environment_power_request_invalid")?;
        }
        if let Some(value) = &self.predecessor_operation_id {
            super::super::validate_identity(value)
                .map_err(|_| "provider_environment_power_request_invalid")?;
            if value == &self.operation_id {
                return Err("provider_environment_power_request_invalid".into());
            }
        }
        if self.action == ProviderEnvironmentPowerAction::Resume
            && self.predecessor_operation_id.is_none()
        {
            return Err("provider_environment_power_predecessor_required".into());
        }
        super::super::validate_digest(&self.provider_config_digest)
            .map_err(|_| "provider_environment_power_request_invalid".into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProviderEnvironmentPowerResult {
    pub schema: &'static str,
    pub operation_id: String,
    pub environment_id: String,
    pub creation_attempt_id: String,
    pub sandbox_id: String,
    pub action: ProviderEnvironmentPowerAction,
    pub sequence: i64,
    pub operation_status: &'static str,
    pub requested_mode: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum ProviderEnvironmentPowerAcknowledgement {
    Paused {
        sandbox_id: String,
        changed: bool,
        requested_mode: String,
    },
    Resumed {
        sandbox_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderEnvironmentPowerDispatchState {
    Prepared,
    Dispatched,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderEnvironmentPowerOperation {
    pub request: ProviderEnvironmentPowerRequest,
    pub sequence: i64,
    pub dispatch_state: ProviderEnvironmentPowerDispatchState,
    pub acknowledgement: Option<ProviderEnvironmentPowerAcknowledgement>,
}

#[derive(Debug)]
pub(crate) struct FreshProviderEnvironmentPowerDispatchPermit {
    operation: ProviderEnvironmentPowerOperation,
}

impl FreshProviderEnvironmentPowerDispatchPermit {
    #[cfg(test)]
    pub(crate) fn operation(&self) -> &ProviderEnvironmentPowerOperation {
        &self.operation
    }
}

impl ProviderEnvironmentPowerOperation {
    pub(crate) fn result(&self) -> ProviderEnvironmentPowerResult {
        ProviderEnvironmentPowerResult {
            schema: "runtime.provider_environment.power.result.v1",
            operation_id: self.request.operation_id.clone(),
            environment_id: self.request.environment_id.clone(),
            creation_attempt_id: self.request.creation_attempt_id.clone(),
            sandbox_id: self.request.sandbox_id.clone(),
            action: self.request.action,
            sequence: self.sequence,
            operation_status: if self.acknowledgement.is_some() {
                "acknowledged"
            } else if self.dispatch_state == ProviderEnvironmentPowerDispatchState::Prepared {
                "prepared"
            } else {
                "pending"
            },
            requested_mode: match self.request.action {
                ProviderEnvironmentPowerAction::Pause => Some("full"),
                ProviderEnvironmentPowerAction::Resume => None,
            },
        }
    }
}

impl PostgresRuntimeStore {
    /// Authentication of independent maintenance authority is the caller's responsibility.
    /// Preparing commits the admission barrier before any provider side effect.
    pub(crate) fn prepare_provider_environment_power(
        &self,
        request: &ProviderEnvironmentPowerRequest,
        configured_digest: &str,
    ) -> Result<ProviderEnvironmentPowerOperation, String> {
        let observation = observe(request);
        observation.timed(PowerMetric::StorePrepare, || {
        let (body, digest) = state::checked_request(request, configured_digest)?;
        self.cleanup_view().with_client(|client| {
            let mut tx=client.transaction().map_err(|e|e.to_string())?;
            super::super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            if let Some(existing)=state::load(&mut tx,&request.operation_id)? {
                state::require_request(&existing,request)?;
                tx.commit().map_err(|e|e.to_string())?;
                return Ok(existing);
            }
            state::lock_idle_target(&mut tx,request,configured_digest)?;
            // A concurrent identical preparation may have committed while this
            // caller waited for the Environment pin. It is the same intent.
            if let Some(existing)=state::load(&mut tx,&request.operation_id)? {
                state::require_request(&existing,request)?;
                tx.commit().map_err(|e|e.to_string())?;
                return Ok(existing);
            }
            let latest=state::latest(&mut tx,&request.environment_id)?;
            let sequence=state::next_sequence(request,latest.as_ref())?;
            tx.execute("INSERT INTO runtime.provider_environment_power_operations(operation_id,environment_id,operation_sequence,predecessor_operation_id,request_digest,request_json,action,dispatch_state,recorded_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,'prepared',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint)",
                &[&request.operation_id,&request.environment_id,&sequence,&request.predecessor_operation_id,&digest,&body,&state::action_name(request.action)])
                .map_err(|_|"provider_environment_power_intent_conflict")?;
            let operation=state::load(&mut tx,&request.operation_id)?.ok_or("provider_environment_power_operation_missing")?;
            tx.commit().map_err(|e|e.to_string())?;Ok(operation)
        })
        })
    }

    /// Only a successful, independently committed CAS constructs a dispatch permit.
    pub(crate) fn dispatch_provider_environment_power(
        &self,
        request: &ProviderEnvironmentPowerRequest,
        configured_digest: &str,
    ) -> Result<Option<FreshProviderEnvironmentPowerDispatchPermit>, String> {
        let observation = observe(request);
        observation.timed(PowerMetric::StoreDispatchCas, || {
        state::checked_request(request, configured_digest)?;
        self.cleanup_view().with_client(|client| {
            let mut tx=client.transaction().map_err(|e|e.to_string())?;
            super::super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            state::lock_idle_target(&mut tx,request,configured_digest)?;
            let operation=state::load(&mut tx,&request.operation_id)?.ok_or("provider_environment_power_operation_missing")?;
            state::require_request(&operation,request)?;
            state::require_current_head(&mut tx,&operation)?;
            if operation.dispatch_state!=ProviderEnvironmentPowerDispatchState::Prepared {
                tx.commit().map_err(|e|e.to_string())?;return Ok(None);
            }
            let changed=tx.execute("UPDATE runtime.provider_environment_power_operations SET dispatch_state='dispatched',dispatched_at_ms=GREATEST(recorded_at_ms,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint) WHERE operation_id=$1 AND dispatch_state='prepared'",&[&request.operation_id]).map_err(|e|e.to_string())?;
            if changed!=1 {return Err("provider_environment_power_dispatch_conflict".into());}
            let operation=state::load(&mut tx,&request.operation_id)?.ok_or("provider_environment_power_operation_missing")?;
            tx.commit().map_err(|e|e.to_string())?;Ok(Some(FreshProviderEnvironmentPowerDispatchPermit{operation}))
        })
        })
    }

    pub(crate) fn with_provider_environment_power_dispatch(
        &self,
        permit: FreshProviderEnvironmentPowerDispatchPermit,
        configured_digest: &str,
        callback: impl FnOnce(
                &ProviderVerifiedTarget,
                &ProviderEnvironmentPowerRequest,
            ) -> Result<ProviderEnvironmentPowerAcknowledgement, String>
            + Send,
    ) -> Result<ProviderEnvironmentPowerResult, String> {
        let expected = permit.operation;
        let observation = observe(&expected.request);
        // Includes pool acquisition and authority pins, ending before provider work.
        let pins = PowerStage::new(&observation, PowerMetric::StoreSideEffectPins);
        state::checked_request(&expected.request, configured_digest)?;
        self.cleanup_view().with_client(|client| {
            let mut tx=client.transaction().map_err(|e|e.to_string())?;
            super::super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let target=state::lock_idle_target(&mut tx,&expected.request,configured_digest)?;
            let operation=state::load(&mut tx,&expected.request.operation_id)?.ok_or("provider_environment_power_operation_missing")?;
            if operation!=expected || operation.dispatch_state!=ProviderEnvironmentPowerDispatchState::Dispatched || operation.acknowledgement.is_some() {
                return Err("provider_environment_power_dispatch_permit_rejected".into());
            }
            state::require_current_head(&mut tx,&operation)?;
            pins.finish(PowerOutcome::Ok);
            let callback_stage=PowerStage::new(&observation,PowerMetric::StoreCallback);
            let callback_result=callback(&target,&operation.request);
            callback_stage.finish(if callback_result.is_ok(){PowerOutcome::Ok}else{PowerOutcome::Pending});
            // Validation, durable ACK/wake writes, and COMMIT are one following
            // stage; callback errors still commit their pending responsibility.
            let acknowledgement_stage=PowerStage::new(&observation,PowerMetric::StoreAckCommit);
            let acknowledgement=match callback_result {
                Ok(value) if state::valid_acknowledgement(&operation.request,&value) => value,
                _ => {
                    tx.commit().map_err(|e|e.to_string())?;
                    acknowledgement_stage.finish(PowerOutcome::Pending);
                    return Ok(operation.result());
                }
            };
            let ack_json=serde_json::to_string(&acknowledgement).map_err(|_|"provider_environment_power_ack_invalid")?;
            let row=tx.query_opt("UPDATE runtime.provider_environment_power_operations SET acknowledged_at_ms=GREATEST(dispatched_at_ms,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint),ack_json=$2 WHERE operation_id=$1 AND dispatch_state='dispatched' AND ack_json IS NULL RETURNING acknowledged_at_ms",&[&operation.request.operation_id,&ack_json]).map_err(|e|e.to_string())?.ok_or("provider_environment_power_ack_conflict")?;
            let acknowledged_at_ms:i64=row.get(0);
            if operation.request.action==ProviderEnvironmentPowerAction::Resume {
                wake::resumed_jobs(&mut tx,&operation,&target,acknowledged_at_ms)?;
            }
            let recorded=state::load(&mut tx,&operation.request.operation_id)?.ok_or("provider_environment_power_operation_missing")?;
            tx.commit().map_err(|e|e.to_string())?;
            acknowledgement_stage.finish(PowerOutcome::Ok);
            Ok(recorded.result())
        })
    }

    pub(crate) fn advance_provider_environment_power(
        &self,
        request: &ProviderEnvironmentPowerRequest,
        configured_digest: &str,
        callback: impl FnOnce(
                &ProviderVerifiedTarget,
                &ProviderEnvironmentPowerRequest,
            ) -> Result<ProviderEnvironmentPowerAcknowledgement, String>
            + Send,
    ) -> Result<ProviderEnvironmentPowerResult, String> {
        let observation = observe(request);
        observation.timed(PowerMetric::StoreAdvance, || {
            let operation = self.prepare_provider_environment_power(request, configured_digest)?;
            if operation.acknowledgement.is_some()
                || operation.dispatch_state == ProviderEnvironmentPowerDispatchState::Dispatched
            {
                return Ok(operation.result());
            }
            match self.dispatch_provider_environment_power(request, configured_digest)? {
                Some(permit) => self.with_provider_environment_power_dispatch(
                    permit,
                    configured_digest,
                    callback,
                ),
                None => self
                    .provider_environment_power_operation(&request.operation_id)?
                    .map(|value| value.result())
                    .ok_or("provider_environment_power_operation_missing".into()),
            }
        })
    }

    pub(crate) fn provider_environment_power_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<ProviderEnvironmentPowerOperation>, String> {
        super::super::validate_identity(operation_id)?;
        self.cleanup_view()
            .with_client(|client| state::load(client, operation_id))
    }
}

pub(super) fn require_admission(
    db: &mut impl GenericClient,
    environment_id: &str,
) -> Result<(), String> {
    let blocked:bool=db.query_one("SELECT COALESCE((SELECT blocked FROM runtime.provider_environment_power_admission WHERE environment_id=$1),false)",&[&environment_id]).map_err(|e|e.to_string())?.get(0);
    if blocked {
        Err("provider_environment_power_pending".into())
    } else {
        Ok(())
    }
}

pub(super) fn require_retirement_allowed(
    db: &mut impl GenericClient,
    environment_id: &str,
) -> Result<(), String> {
    if state::latest(db, environment_id)?
        .is_some_and(|operation| operation.acknowledgement.is_none())
    {
        Err("provider_environment_power_pending".into())
    } else {
        Ok(())
    }
}

fn observe(request: &ProviderEnvironmentPowerRequest) -> PowerObservation {
    PowerObservation::new(&request.operation_id, state::action_name(request.action))
}
