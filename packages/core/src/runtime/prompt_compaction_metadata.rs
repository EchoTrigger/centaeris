use super::*;

pub(super) fn prompt_compaction_circuit_is_open(session: &SessionStateSnapshot) -> bool {
    session
        .metadata
        .get(PROMPT_COMPACTION_CIRCUIT_META_KEY)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| {
            value
                .get("status")
                .and_then(Value::as_str)
                .map(|status| status == "open")
        })
        .unwrap_or(false)
}

pub(super) fn prompt_compaction_stats_match_turn(
    session: &SessionStateSnapshot,
    turn_id: &str,
) -> bool {
    session
        .metadata
        .get("prompt_compaction_stats_json")
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| {
            value
                .get("turn_id")
                .and_then(Value::as_str)
                .map(|stats_turn_id| stats_turn_id == turn_id)
        })
        .unwrap_or(false)
}

pub(super) fn prompt_compaction_failure_matches_turn(
    session: &SessionStateSnapshot,
    turn_id: &str,
) -> bool {
    session
        .metadata
        .get(PROMPT_COMPACTION_FAILURE_META_KEY)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .is_some_and(|failure| {
            failure.as_object().is_some_and(|object| object.len() == 7)
                && failure.get("schema").and_then(Value::as_str)
                    == Some("prompt_compaction_failure_v1")
                && failure.get("turnId").and_then(Value::as_str) == Some(turn_id)
                && failure.get("phase").and_then(Value::as_str).is_some()
                && failure.get("reason").and_then(Value::as_str).is_some()
                && failure
                    .get("failureCount")
                    .and_then(Value::as_u64)
                    .is_some_and(|count| count > 0 && count <= u64::from(u32::MAX))
                && failure
                    .get("circuitOpen")
                    .and_then(Value::as_bool)
                    .is_some()
                && failure
                    .get("recordedAtMs")
                    .and_then(Value::as_i64)
                    .is_some()
        })
}

pub(super) fn clear_prompt_compaction_failure_metadata(session: &mut SessionStateSnapshot) {
    session.metadata.remove(PROMPT_COMPACTION_FAILURE_META_KEY);
    session
        .metadata
        .remove(PROMPT_COMPACTION_FAILURE_COUNT_META_KEY);
    session.metadata.remove(PROMPT_COMPACTION_CIRCUIT_META_KEY);
}
