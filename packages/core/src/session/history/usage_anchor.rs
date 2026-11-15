//! Explicit upgrade of persisted input-only prompt usage calibrations.

use serde::Deserialize;

use super::validate_session_snapshot_history;
use crate::session::state::SessionStateSnapshot;

const INPUT_ANCHOR_SCHEMA: &str = "runtime.prompt_usage_anchor.v1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputUsageAnchor {
    schema: String,
    binding_digest: String,
    messages: Vec<InputMessageCost>,
    actual_input_tokens: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputMessageCost {
    digest: String,
    #[serde(rename = "estimatedTokens")]
    _estimated_tokens: u32,
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

/// Discards a verified input-only calibration while preserving the complete Session state.
///
/// This is an explicit persistence upgrade. Current readers accept only the current
/// anchor contract; historical input usage cannot establish a total-usage calibration.
///
/// # Errors
///
/// Rejects unsupported or malformed anchors, snapshots, and history references.
pub fn migrate_prompt_usage_anchor_snapshot(raw: &str) -> Result<String, String> {
    let mut wire: serde_json::Value = serde_json::from_str(raw)
        .map_err(|error| format!("decode usage anchor migration snapshot failed: {error}"))?;
    let anchor = wire
        .get_mut("history")
        .and_then(|history| history.get_mut("promptUsageAnchor"))
        .ok_or("usage anchor migration requires current Session history")?;
    let input_only =
        anchor.get("schema").and_then(serde_json::Value::as_str) == Some(INPUT_ANCHOR_SCHEMA);
    if input_only {
        let previous: InputUsageAnchor = serde_json::from_value(anchor.clone())
            .map_err(|error| format!("decode input usage anchor failed: {error}"))?;
        if previous.schema != INPUT_ANCHOR_SCHEMA
            || previous.actual_input_tokens == 0
            || !is_digest(&previous.binding_digest)
            || previous
                .messages
                .iter()
                .any(|message| !is_digest(&message.digest))
        {
            return Err("input_usage_anchor_invalid".into());
        }
        // Zero-cost messages were valid in the previous input-only contract.
        *anchor = serde_json::Value::Null;
    }
    let snapshot: SessionStateSnapshot = serde_json::from_value(wire.clone())
        .map_err(|error| format!("decode upgraded Session snapshot failed: {error}"))?;
    validate_session_snapshot_history(&snapshot)?;
    if input_only {
        String::from_utf8(crate::runtime::canonical_json::to_vec(&wire)?)
            .map_err(|error| format!("encode upgraded Session snapshot failed: {error}"))
    } else {
        Ok(raw.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::history::prepare_session_snapshot_write;
    use crate::session::state::{ChatMessage, MessageRole, ModelMessageSemanticsV1};

    fn snapshot() -> serde_json::Value {
        let mut state = SessionStateSnapshot::new("usage-migration".into(), 19);
        state.metadata.insert("private-state".into(), "keep".into());
        state.messages.push(ChatMessage {
            message_id: "request".into(),
            role: MessageRole::User,
            content: "Exact original request\n".into(),
            created_at_ms: 11,
            metadata: [("message_semantic_kind".into(), "user_request".into())]
                .into_iter()
                .collect(),
        });
        state
            .model_semantics
            .insert("request".into(), ModelMessageSemanticsV1::Plain);
        let write =
            prepare_session_snapshot_write(&serde_json::to_string(&state).unwrap()).unwrap();
        serde_json::from_str(write.snapshot_json()).unwrap()
    }

    fn input_anchor() -> serde_json::Value {
        serde_json::json!({
            "schema": INPUT_ANCHOR_SCHEMA,
            "bindingDigest": format!("sha256:{}", "a".repeat(64)),
            "messages": [{"digest": format!("sha256:{}", "b".repeat(64)), "estimatedTokens": 0}],
            "actualInputTokens": 123
        })
    }

    #[test]
    fn input_usage_upgrade_only_discards_derived_anchor_and_is_idempotent() {
        let expected = snapshot();
        let mut previous = expected.clone();
        previous["history"]["promptUsageAnchor"] = input_anchor();
        let upgraded = migrate_prompt_usage_anchor_snapshot(&previous.to_string()).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&upgraded).unwrap(),
            expected
        );
        assert_eq!(
            migrate_prompt_usage_anchor_snapshot(&upgraded).unwrap(),
            upgraded
        );
    }

    #[test]
    fn current_total_usage_anchor_is_preserved_without_reencoding() {
        let mut current = snapshot();
        current["history"]["promptUsageAnchor"] = serde_json::json!({
            "schema": "runtime.prompt_usage_anchor.v2",
            "bindingDigest": format!("sha256:{}", "a".repeat(64)),
            "coveredMessageDigests": [format!("sha256:{}", "b".repeat(64)), format!("sha256:{}", "c".repeat(64))],
            "actualTotalTokens": 456
        });
        let raw = serde_json::to_string_pretty(&current).unwrap();
        assert_eq!(migrate_prompt_usage_anchor_snapshot(&raw).unwrap(), raw);
    }

    #[test]
    fn input_usage_upgrade_rejects_malformed_or_unknown_anchors_and_history() {
        for patch in [
            serde_json::json!({"actualInputTokens": 0}),
            serde_json::json!({"bindingDigest": "invalid"}),
            serde_json::json!({"unexpected": true}),
            serde_json::json!({"schema": "runtime.prompt_usage_anchor.unknown"}),
            serde_json::json!({"messages": [{"digest": "invalid", "estimatedTokens": 0}]}),
            serde_json::json!({"messages": [{"digest": format!("sha256:{}", "b".repeat(64)), "estimatedTokens": -1}]}),
        ] {
            let mut previous = snapshot();
            let mut anchor = input_anchor();
            anchor
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            previous["history"]["promptUsageAnchor"] = anchor;
            assert!(
                migrate_prompt_usage_anchor_snapshot(&previous.to_string()).is_err(),
                "{patch}"
            );
        }
        let mut previous = snapshot();
        previous["history"]["promptUsageAnchor"] = input_anchor();
        previous["history"]["contextRevision"] = "sha256:damaged".into();
        assert!(migrate_prompt_usage_anchor_snapshot(&previous.to_string()).is_err());
    }
}
