//! PostgreSQL-private storage encoding; Core always receives its original wire.
use serde_json::Value;

const STORAGE_KEY: &str = "__centaerisSessionStorage";
const STORAGE_SCHEMA: &str = "workspace.session_event.storage.v1";

fn contains_nul(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains('\0'),
        Value::Array(items) => items.iter().any(contains_nul),
        Value::Object(items) => items
            .iter()
            .any(|(key, value)| key.contains('\0') || contains_nul(value)),
        _ => false,
    }
}

fn query_index(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace('\0', "\u{2400}")),
        Value::Array(items) => Value::Array(items.iter().map(query_index).collect()),
        Value::Object(items) => Value::Object(
            items
                .iter()
                .map(|(key, value)| (key.replace('\0', "\u{2400}"), query_index(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

pub(super) fn encode(value: &serde_json::Value) -> Result<serde_json::Value, String> {
    if !value.is_object() || value.get(STORAGE_KEY).is_some() {
        return Err("session_payload_storage_invalid".into());
    }
    if !contains_nul(value) {
        return Ok(value.clone());
    }
    let mut stored = query_index(value);
    stored[STORAGE_KEY] =
        serde_json::json!({"schema": STORAGE_SCHEMA, "canonicalJson": value.to_string()});
    Ok(stored)
}

pub(crate) fn decode(value: &mut serde_json::Value) -> Result<(), String> {
    let Some(storage) = value.get(STORAGE_KEY) else {
        return Ok(());
    };
    let data = storage
        .as_object()
        .ok_or("session_payload_storage_invalid")?;
    if data.len() != 2 || data.get("schema").and_then(Value::as_str) != Some(STORAGE_SCHEMA) {
        return Err("session_payload_storage_invalid".into());
    }
    let encoded = data
        .get("canonicalJson")
        .and_then(Value::as_str)
        .ok_or("session_payload_storage_invalid")?;
    let wire: Value =
        serde_json::from_str(encoded).map_err(|_| "session_payload_storage_invalid")?;
    if !wire.is_object() || wire.get(STORAGE_KEY).is_some() || !contains_nul(&wire) {
        return Err("session_payload_storage_invalid".into());
    }
    let mut index = value.clone();
    index
        .as_object_mut()
        .ok_or("session_payload_storage_invalid")?
        .remove(STORAGE_KEY);
    if index != query_index(&wire) {
        return Err("session_payload_storage_index_mismatch".into());
    }
    *value = wire;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn storage_contract_matches_the_shared_rust_python_corpus() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/session_payload_storage.json"
        ))
        .unwrap();
        assert!(cases.len() >= 3);
        for case in cases {
            assert_eq!(
                encode(&case["wire"]).unwrap(),
                case["stored"],
                "{}",
                case["name"]
            );
            let mut stored = case["stored"].clone();
            decode(&mut stored).unwrap();
            assert_eq!(stored, case["wire"], "{}", case["name"]);
        }
    }

    fn nul_free(value: &Value) -> bool {
        match value {
            Value::String(text) => !text.contains('\0'),
            Value::Array(items) => items.iter().all(nul_free),
            Value::Object(items) => items
                .iter()
                .all(|(key, value)| !key.contains('\0') && nul_free(value)),
            _ => true,
        }
    }

    #[test]
    fn hosted_tool_output_round_trips_control_characters_without_failing_the_tool() {
        let wire = json!({"type":"tool_result","payload":{"callId":"owned","toolName":"bash","resultState":"successWithOutput","modelContent":"kernel\0\nLiteral \\u0000\n中文","summary":"kernel\0"}});
        let stored = encode(&wire).unwrap();
        assert!(nul_free(&stored));
        assert_eq!(stored["type"], "tool_result");
        assert_eq!(stored["payload"]["resultState"], "successWithOutput");
        let mut reloaded: Value = serde_json::from_str(&stored.to_string()).unwrap();
        decode(&mut reloaded).unwrap();
        assert_eq!(reloaded, wire);
    }

    #[test]
    fn ordinary_failed_tool_wire_keeps_its_exact_content_and_state() {
        let wire = json!({"type":"tool_result","payload":{"resultState":"error","modelContent":"Literal \\u0000"}});
        assert_eq!(encode(&wire).unwrap(), wire);
        let mut reloaded = wire.clone();
        decode(&mut reloaded).unwrap();
        assert_eq!(reloaded, wire);
    }

    #[test]
    fn hosted_storage_rejects_tampered_query_index() {
        let mut stored = encode(
            &json!({"type":"tool_result","payload":{"callId":"owned","modelContent":"a\0b"}}),
        )
        .unwrap();
        stored["payload"]["callId"] = json!("forged");
        assert_eq!(
            decode(&mut stored).unwrap_err(),
            "session_payload_storage_index_mismatch"
        );
    }
}
