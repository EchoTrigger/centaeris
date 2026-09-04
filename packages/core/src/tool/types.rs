use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::canonical_json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolTurnBehavior {
    ContinueTurn,
    CompleteTurnOnSuccess,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolContract {
    pub name: String,
    pub category: String,
    pub summary: String,
    pub input_schema: Value,
    pub concurrency_safe: bool,
    pub turn_behavior: ToolTurnBehavior,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub provider_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub schema_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dynamic: bool,
}

impl ToolContract {
    pub fn contract_digest(&self) -> Result<String, String> {
        if let Some(digest) = self.schema_hash.as_deref() {
            if digest.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }) {
                return Ok(digest.to_string());
            }
            return Err("tool contract schemaHash must be sha256:<64 lowercase hex>".to_string());
        }
        let mut preimage = self.clone();
        preimage.schema_hash = None;
        canonical_json::sha256("centaeris.tool_contract.v1", &preimage)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelToolChoice {
    None,
    Auto,
    Required,
    Specific { name: String },
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_turn_behavior_uses_exact_values() {
        assert_eq!(
            serde_json::from_value::<ToolTurnBehavior>(json!("completeTurnOnSuccess"))
                .expect("terminal behavior"),
            ToolTurnBehavior::CompleteTurnOnSuccess
        );
        assert!(serde_json::from_value::<ToolTurnBehavior>(json!("banana")).is_err());
    }

    #[test]
    fn tool_contract_requires_declared_turn_behavior() {
        let mut value = json!({
            "name": "example_tool",
            "category": "test",
            "summary": "Example tool.",
            "inputSchema": { "type": "object" },
            "concurrencySafe": true
        });

        assert!(serde_json::from_value::<ToolContract>(value.clone()).is_err());
        value["turnBehavior"] = json!("continueTurn");
        value["banana"] = json!(true);
        assert!(serde_json::from_value::<ToolContract>(value).is_err());
    }

    #[test]
    fn tool_contract_rejects_invalid_declared_schema_hash() {
        let mut contract = crate::tool::list_tool_contracts().remove(0);
        contract.schema_hash = Some(format!("sha256:{}", "G".repeat(64)));

        assert!(contract
            .contract_digest()
            .expect_err("invalid schema hash must fail")
            .contains("lowercase hex"));
    }
}
