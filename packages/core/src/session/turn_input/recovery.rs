use crate::session::state::SessionStateSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

const INPUT_RECOVERY_KEY: &str = "turnInputRecovery";

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputRecovery {
    committed_by_run: BTreeMap<String, BTreeSet<String>>,
    prepared_by_turn: BTreeMap<String, PreparedInputBatch>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreparedInputBatch {
    agent_run_id: String,
    input_ids: Vec<String>,
}

fn load(session: &SessionStateSnapshot) -> Result<InputRecovery, String> {
    match session.metadata.get(INPUT_RECOVERY_KEY) {
        Some(raw) => serde_json::from_str(raw)
            .map_err(|error| format!("decode turn input recovery failed: {error}")),
        None => Ok(InputRecovery::default()),
    }
}

fn save(session: &mut SessionStateSnapshot, recovery: &InputRecovery) -> Result<(), String> {
    session.metadata.insert(
        INPUT_RECOVERY_KEY.into(),
        serde_json::to_string(recovery)
            .map_err(|error| format!("encode turn input recovery failed: {error}"))?,
    );
    Ok(())
}

pub(crate) fn committed_ids(
    session: &SessionStateSnapshot,
    agent_run_id: &str,
) -> Result<HashSet<String>, String> {
    Ok(load(session)?
        .committed_by_run
        .remove(agent_run_id)
        .unwrap_or_default()
        .into_iter()
        .collect())
}

pub(crate) fn prepared_ids(
    session: &SessionStateSnapshot,
    agent_run_id: &str,
    turn_id: &str,
) -> Result<Option<Vec<String>>, String> {
    let recovery = load(session)?;
    let Some(prepared) = recovery.prepared_by_turn.get(turn_id) else {
        return Ok(None);
    };
    if prepared.agent_run_id != agent_run_id {
        return Err("turn_input_prepared_owner_mismatch".into());
    }
    Ok(Some(prepared.input_ids.clone()))
}

pub(crate) fn current_prepared_turn(
    session: &SessionStateSnapshot,
    agent_run_id: &str,
) -> Result<Option<String>, String> {
    let recovery = load(session)?;
    let mut turns = recovery
        .prepared_by_turn
        .iter()
        .filter(|(_, prepared)| prepared.agent_run_id == agent_run_id);
    let current = turns.next().map(|(turn, _)| turn.clone());
    if turns.next().is_some() {
        return Err("turn_input_prepared_context_conflict".into());
    }
    Ok(current)
}

/// Preparation membership is operational state, not a Read fact. The already
/// persisted current message retains the immutable batch body.
pub(crate) fn freeze_prepared(
    session: &mut SessionStateSnapshot,
    agent_run_id: &str,
    turn_id: &str,
    input_ids: &[String],
) -> Result<(), String> {
    if !input_ids.is_empty() {
        crate::runtime::validate_input_uptake_ids(input_ids)?;
    }
    let mut recovery = load(session)?;
    if let Some(existing) = recovery.prepared_by_turn.get(turn_id) {
        if existing.agent_run_id != agent_run_id || existing.input_ids != input_ids {
            return Err("turn_input_prepared_identity_conflict".into());
        }
    } else {
        recovery.prepared_by_turn.insert(
            turn_id.into(),
            PreparedInputBatch {
                agent_run_id: agent_run_id.into(),
                input_ids: input_ids.to_vec(),
            },
        );
    }
    // Only the current request's membership can constrain recovery. Older
    // committed batches may no longer have messages after compaction.
    recovery
        .prepared_by_turn
        .retain(|turn, prepared| prepared.agent_run_id != agent_run_id || turn == turn_id);
    save(session, &recovery)
}

/// Called only after a successful main safe-point commit or while reducing
/// confirmed committed main records. Claim/materialization/ACK never call this.
pub(crate) fn record_committed(
    session: &mut SessionStateSnapshot,
    agent_run_id: &str,
    input_ids: &[String],
) -> Result<(), String> {
    if input_ids.is_empty() {
        return Ok(());
    }
    crate::runtime::validate_input_uptake_ids(input_ids)?;
    let mut recovery = load(session)?;
    recovery
        .committed_by_run
        .entry(agent_run_id.into())
        .or_default()
        .extend(input_ids.iter().cloned());
    save(session, &recovery)
}
