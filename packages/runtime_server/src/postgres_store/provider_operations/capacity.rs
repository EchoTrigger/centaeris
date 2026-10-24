//! Provider admission follows durable create/power facts, including Unknown.
use postgres::GenericClient;
pub(crate) const WAIT: &str = "e2b_environment_capacity_wait";
pub(crate) fn validate_limit(limit: Option<u32>) -> Result<(), String> {
    if limit.is_some_and(|value| value == 0 || value > 64) {
        return Err("e2b_running_limit_invalid".into());
    }
    Ok(())
}
pub(crate) fn active_count(db: &mut impl GenericClient, digest: &str) -> Result<i64, String> {
    db.query_one("SELECT count(*) FROM runtime.provider_creation_operations c LEFT JOIN LATERAL (SELECT p.action,p.ack_json FROM runtime.provider_environment_power_operations p WHERE p.environment_id=c.environment_id AND p.request_json::jsonb->>'creationAttemptId'=c.attempt_id AND p.dispatch_state<>'notDispatched' ORDER BY p.operation_sequence DESC LIMIT 1) p ON true WHERE c.provider='e2b' AND c.provider_config_digest=$1 AND c.dispatch_state='dispatched' AND c.settlement_id IS NULL AND NOT COALESCE(p.action='pause' AND p.ack_json IS NOT NULL,false)",&[&digest]).map(|row|row.get(0)).map_err(|e|e.to_string())
}
/// Called after the Environment pin and before committing create/resume intent.
/// The lock serializes admission only; no provider IO occurs under this lock.
pub(crate) fn require(
    db: &mut impl GenericClient,
    limit: Option<u32>,
    digest: &str,
) -> Result<(), String> {
    validate_limit(limit)?;
    let Some(limit) = limit else {
        return Ok(());
    };
    db.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended('runtime.provider.capacity:'||$1::text,0))",
        &[&digest],
    )
    .map_err(|e| e.to_string())?;
    if active_count(db, digest)? >= i64::from(limit) {
        return Err(WAIT.into());
    }
    Ok(())
}
