use super::*;

pub(super) fn action_name(action: ProviderEnvironmentPowerAction) -> &'static str {
    match action {
        ProviderEnvironmentPowerAction::Pause => "pause",
        ProviderEnvironmentPowerAction::Resume => "resume",
    }
}

pub(super) fn checked_request(
    request: &ProviderEnvironmentPowerRequest,
    configured_digest: &str,
) -> Result<(String, String), String> {
    request.validate()?;
    if request.provider_config_digest != configured_digest {
        return Err("provider_environment_power_configuration_rejected".into());
    }
    let body =
        serde_json::to_string(request).map_err(|_| "provider_environment_power_request_invalid")?;
    let digest = fact_digest(
        "runtime:provider-environment-power:v1\0",
        &serde_json::to_value(request).map_err(|_| "provider_environment_power_request_invalid")?,
    )?;
    Ok((body, digest))
}

pub(super) fn require_request(
    operation: &ProviderEnvironmentPowerOperation,
    request: &ProviderEnvironmentPowerRequest,
) -> Result<(), String> {
    if &operation.request != request {
        Err("provider_environment_power_intent_conflict".into())
    } else {
        Ok(())
    }
}

pub(super) fn load(
    db: &mut impl GenericClient,
    id: &str,
) -> Result<Option<ProviderEnvironmentPowerOperation>, String> {
    let row=db.query_opt("SELECT environment_id,operation_sequence,predecessor_operation_id,request_digest,request_json,action,dispatch_state,ack_json,automatic_demand_json,not_dispatched_at_ms FROM runtime.provider_environment_power_operations WHERE operation_id=$1",&[&id]).map_err(|e|e.to_string())?;
    let Some(row) = row else { return Ok(None) };
    decode(&row, id, 0)
}

pub(super) fn decode(
    row: &postgres::Row,
    id: &str,
    offset: usize,
) -> Result<Option<ProviderEnvironmentPowerOperation>, String> {
    let body: String = row.get(offset + 4);
    let request: ProviderEnvironmentPowerRequest =
        serde_json::from_str(&body).map_err(|_| "provider_environment_power_fact_rejected")?;
    let (canonical, digest) = checked_request(&request, &request.provider_config_digest)?;
    if canonical != body
        || request.operation_id != id
        || request.environment_id != row.get::<_, String>(offset)
        || request.predecessor_operation_id != row.get::<_, Option<String>>(offset + 2)
        || digest != row.get::<_, String>(offset + 3)
        || action_name(request.action) != row.get::<_, String>(offset + 5)
    {
        return Err("provider_environment_power_fact_rejected".into());
    }
    let dispatch_state = match row.get::<_, String>(offset + 6).as_str() {
        "prepared" => ProviderEnvironmentPowerDispatchState::Prepared,
        "dispatched" => ProviderEnvironmentPowerDispatchState::Dispatched,
        "notDispatched" => ProviderEnvironmentPowerDispatchState::NotDispatched,
        _ => return Err("provider_environment_power_fact_rejected".into()),
    };
    let acknowledgement = row
        .get::<_, Option<String>>(offset + 7)
        .map(|json| {
            serde_json::from_str::<ProviderEnvironmentPowerAcknowledgement>(&json)
                .map_err(|_| "provider_environment_power_ack_rejected")
        })
        .transpose()?;
    if acknowledgement
        .as_ref()
        .is_some_and(|ack| !valid_acknowledgement(&request, ack))
    {
        return Err("provider_environment_power_ack_rejected".into());
    }
    let automatic_demand = row
        .get::<_, Option<String>>(offset + 8)
        .map(|body| {
            serde_json::from_str::<automatic_resume::AutomaticResumeDemand>(&body)
                .map_err(|_| "provider_automatic_resume_demand_rejected".to_string())
        })
        .transpose()?;
    if let Some(demand) = &automatic_demand {
        demand.validate()?;
    }
    let not_dispatched_at_ms: Option<i64> = row.get(offset + 9);
    if (dispatch_state == ProviderEnvironmentPowerDispatchState::NotDispatched)
        != (not_dispatched_at_ms.is_some())
        || not_dispatched_at_ms.is_some()
            && (automatic_demand.is_none() || acknowledgement.is_some())
    {
        return Err("provider_automatic_resume_terminal_fact_rejected".into());
    }
    Ok(Some(ProviderEnvironmentPowerOperation {
        request,
        sequence: row.get(offset + 1),
        dispatch_state,
        acknowledgement,
        automatic_demand,
        not_dispatched_at_ms,
    }))
}

pub(super) fn latest(
    db: &mut impl GenericClient,
    environment_id: &str,
) -> Result<Option<ProviderEnvironmentPowerOperation>, String> {
    let row=db.query_opt("SELECT operation_id FROM runtime.provider_environment_power_operations WHERE environment_id=$1 ORDER BY operation_sequence DESC LIMIT 1",&[&environment_id]).map_err(|e|e.to_string())?;
    match row {
        Some(row) => load(db, &row.get::<_, String>(0)),
        None => Ok(None),
    }
}

pub(super) fn next_sequence(
    request: &ProviderEnvironmentPowerRequest,
    latest: Option<&ProviderEnvironmentPowerOperation>,
) -> Result<i64, String> {
    match latest {
        None if request.action == ProviderEnvironmentPowerAction::Pause
            && request.predecessor_operation_id.is_none() =>
        {
            Ok(1)
        }
        Some(previous)
            if ((previous.acknowledgement.is_some()
                && previous.request.action != request.action)
                || (previous.dispatch_state
                    == ProviderEnvironmentPowerDispatchState::NotDispatched
                    && request.action == ProviderEnvironmentPowerAction::Resume))
                && request.predecessor_operation_id.as_deref()
                    == Some(previous.request.operation_id.as_str()) =>
        {
            previous
                .sequence
                .checked_add(1)
                .ok_or("provider_environment_power_sequence_exhausted".into())
        }
        _ => Err("provider_environment_power_predecessor_rejected".into()),
    }
}

pub(super) fn require_current_head(
    db: &mut impl GenericClient,
    operation: &ProviderEnvironmentPowerOperation,
) -> Result<(), String> {
    if latest(db, &operation.request.environment_id)?.as_ref() != Some(operation) {
        return Err("provider_environment_power_head_rejected".into());
    }
    Ok(())
}

pub(super) fn lock_idle_target(
    tx: &mut postgres::Transaction<'_>,
    request: &ProviderEnvironmentPowerRequest,
    configured_digest: &str,
) -> Result<ProviderVerifiedTarget, String> {
    let target = retirement::lock_idle_maintenance_target(
        tx,
        &retirement::IdleProviderEnvironmentTarget {
            environment_id: &request.environment_id,
            creation_attempt_id: &request.creation_attempt_id,
            sandbox_id: &request.sandbox_id,
            provider_config_digest: &request.provider_config_digest,
            expected_controller_epoch: request.expected_controller_epoch,
        },
        configured_digest,
    )?;
    reject_retirement(tx, &request.environment_id)?;
    require_open_target(tx, &target)?;
    // A verified container alone does not prove initial workspace preparation completed.
    match &target.identity.owner {
        EnvironmentTaskOwner::AgentRun { run_id } => {
            let creator = load_use(tx, run_id)?
                .ok_or("provider_environment_power_creator_cleanup_required")?;
            if creator.target != target
                || receipts::load_receipt(tx, &creator.agent_run_id)?.is_none()
            {
                return Err("provider_environment_power_creator_cleanup_required".into());
            }
        }
        EnvironmentTaskOwner::MaintenanceOperation { .. } => {
            super::super::super::provision::require_completed_handoff(tx, &target)?
        }
    }
    Ok(target)
}

pub(super) fn valid_acknowledgement(
    request: &ProviderEnvironmentPowerRequest,
    ack: &ProviderEnvironmentPowerAcknowledgement,
) -> bool {
    match (request.action, ack) {
        (
            ProviderEnvironmentPowerAction::Pause,
            ProviderEnvironmentPowerAcknowledgement::Paused {
                sandbox_id,
                changed,
                requested_mode,
            },
        ) => sandbox_id == &request.sandbox_id && *changed && requested_mode == "full",
        (
            ProviderEnvironmentPowerAction::Resume,
            ProviderEnvironmentPowerAcknowledgement::Resumed { sandbox_id },
        ) => sandbox_id == &request.sandbox_id,
        _ => false,
    }
}
