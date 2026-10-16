//! Durable hosted control facts for failures before Core Session admission.
use postgres::GenericClient;
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = "runtime.agent_run.pre_admission_failure.v1";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreAdmissionFailure {
    pub schema: String,
    pub workspace_id: String,
    pub session_id: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
    pub job_id: String,
    pub lease_owner: String,
    pub failed_at_ms: i64,
}

pub(crate) fn event_id(run_id: &str) -> String {
    format!("agent_run_pre_admission_failure:{run_id}")
}

/// Absence is not a terminal fact. A present but malformed or differently bound
/// record fails closed rather than becoming either execution or cleanup authority.
pub(crate) fn has_pre_admission_failure<C: GenericClient>(
    client: &mut C,
    workspace_id: &str,
    session_id: &str,
    run_id: &str,
    digest: &str,
) -> Result<bool, String> {
    load_pre_admission_failure(client, workspace_id, session_id, run_id, digest)
        .map(|receipt| receipt.is_some())
}

pub(crate) fn load_pre_admission_failure<C: GenericClient>(
    client: &mut C,
    workspace_id: &str,
    session_id: &str,
    run_id: &str,
    digest: &str,
) -> Result<Option<PreAdmissionFailure>, String> {
    let Some(row) = client.query_opt(
        "SELECT session_id,task_id,event_type,at_ms,visibility,payload_json FROM runtime.runtime_events WHERE event_id=$1",
        &[&event_id(run_id)],
    ).map_err(|e| e.to_string())? else { return Ok(None); };
    let receipt: PreAdmissionFailure = serde_json::from_str(&row.get::<_, String>(5))
        .map_err(|_| "pre-admission failure receipt malformed".to_string())?;
    if receipt.schema != SCHEMA
        || receipt.workspace_id != workspace_id
        || receipt.session_id != session_id
        || receipt.agent_run_id != run_id
        || receipt.authorization_digest != digest
        || receipt.job_id != format!("agent_run.lifecycle:{run_id}")
        || receipt.lease_owner.trim().is_empty()
        || receipt.failed_at_ms <= 0
        || receipt.failed_at_ms > 253_402_300_799_999
        || row.get::<_, String>(0) != session_id
        || row.get::<_, Option<String>>(1).as_deref() != Some(run_id)
        || row.get::<_, String>(2) != SCHEMA
        || row.get::<_, i64>(3) != receipt.failed_at_ms
        || row.get::<_, String>(4) != "internal"
    {
        return Err("pre-admission failure receipt binding mismatch".into());
    }
    let conflict: bool = client.query_one(
        "SELECT EXISTS(SELECT 1 FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND NOT session_level) OR NOT EXISTS(SELECT 1 FROM public.app_core_agentrun WHERE id=$1 AND session_id=$2 AND workspace_id=$3 AND \"preAdmissionCancelledAt\" IS NULL)",
        &[&run_id, &session_id, &workspace_id],
    ).map_err(|e| e.to_string())?.get(0);
    if conflict {
        return Err(
            "pre-admission failure receipt conflicts with admission or cancellation".into(),
        );
    }
    Ok(Some(receipt))
}

pub(super) fn reject_history_after_failure<C: GenericClient>(
    client: &mut C,
    run_id: &str,
) -> Result<(), String> {
    // Even a corrupt receipt must block execution. Only the fully validated
    // reader above can turn the record into cleanup/terminal authority.
    if client
        .query_opt(
            "SELECT event_id FROM runtime.runtime_events WHERE event_id=$1",
            &[&event_id(run_id)],
        )
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err("session admission rejected after pre-admission failure".into());
    }
    Ok(())
}
