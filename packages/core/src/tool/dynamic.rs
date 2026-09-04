use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::runtime::canonical_json;

use super::catalog::list_tool_contracts;
use super::limits::{
    json_size_with_limit, validate_tool_description, ToolContractBudget,
    MAX_TOOL_INPUT_SCHEMA_BYTES,
};
use super::types::{ToolContract, ToolTurnBehavior};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DynamicToolContract {
    pub name: String,
    pub category: String,
    pub summary: String,
    pub input_schema: Value,
    pub provider_id: String,
    pub scopes: Vec<String>,
    pub concurrency_safe: bool,
    pub turn_behavior: ToolTurnBehavior,
}

#[derive(Debug, Clone, Default)]
pub struct DynamicToolRegistry {
    contracts: Vec<DynamicToolContract>,
    budget: ToolContractBudget,
}

impl DynamicToolRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_contracts(contracts: Vec<DynamicToolContract>) -> Result<Self, String> {
        let mut registry = Self::empty();
        for contract in contracts {
            registry.register(contract)?;
        }
        Ok(registry)
    }

    pub fn register(&mut self, mut contract: DynamicToolContract) -> Result<(), String> {
        let mut budget = self.budget.clone();
        budget.add(&contract)?;
        validate_contract(&contract)?;
        contract.scopes.sort();
        if list_tool_contracts()
            .iter()
            .any(|existing| existing.name == contract.name)
        {
            return Err(format!(
                "dynamic tool identity collides with built-in tool: {}",
                contract.name
            ));
        }
        if self
            .contracts
            .iter()
            .any(|existing| existing.name == contract.name)
        {
            return Err(format!(
                "duplicate dynamic tool identity: {}",
                contract.name
            ));
        }
        self.budget = budget;
        self.contracts.push(contract);
        self.contracts
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(())
    }

    pub fn list_contracts(&self) -> Vec<ToolContract> {
        self.contracts.iter().map(tool_contract).collect()
    }

    pub fn list_dynamic_contracts(&self) -> Vec<DynamicToolContract> {
        self.contracts.clone()
    }

    pub fn find_contract(&self, name: &str) -> Option<ToolContract> {
        self.contracts
            .iter()
            .find(|contract| contract.name == name)
            .map(tool_contract)
    }

    pub fn select_contracts_by_names(&self, names: &[String], limit: usize) -> Vec<ToolContract> {
        names
            .iter()
            .filter_map(|name| self.find_contract(name))
            .take(limit)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.contracts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.contracts.is_empty()
    }
}

pub fn list_tool_contracts_with_dynamic(registry: &DynamicToolRegistry) -> Vec<ToolContract> {
    let mut contracts = list_tool_contracts();
    contracts.extend(registry.list_contracts());
    contracts
}

fn tool_contract(contract: &DynamicToolContract) -> ToolContract {
    ToolContract {
        name: contract.name.clone(),
        category: contract.category.clone(),
        summary: contract.summary.clone(),
        input_schema: contract.input_schema.clone(),
        concurrency_safe: contract.concurrency_safe,
        turn_behavior: contract.turn_behavior,
        provider_id: Some(contract.provider_id.clone()),
        schema_hash: Some(schema_hash(contract)),
        scopes: contract.scopes.clone(),
        dynamic: true,
    }
}

fn validate_contract(contract: &DynamicToolContract) -> Result<(), String> {
    if contract.name.trim().is_empty() {
        return Err("dynamic tool name is required".to_string());
    }
    if contract.name != contract.name.trim() {
        return Err("dynamic tool name must not contain surrounding whitespace".to_string());
    }
    let name = contract.name.as_bytes();
    if !name.first().is_some_and(u8::is_ascii_lowercase)
        || name.last() == Some(&b'_')
        || name.windows(2).any(|pair| pair == b"__")
        || !name
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        return Err("dynamic tool name must use canonical lower_snake_case".to_string());
    }
    if contract.provider_id.trim().is_empty() {
        return Err("dynamic tool providerId is required".to_string());
    }
    if contract.provider_id != contract.provider_id.trim() {
        return Err("dynamic tool providerId must not contain surrounding whitespace".to_string());
    }
    validate_tool_description(&contract.summary)?;
    if contract.category.trim().is_empty() || contract.category != contract.category.trim() {
        return Err("dynamic tool category must be exact and non-empty".to_string());
    }
    if !contract.input_schema.is_object() {
        return Err("dynamic tool inputSchema must be an object".to_string());
    }
    json_size_with_limit(&contract.input_schema, MAX_TOOL_INPUT_SCHEMA_BYTES)
        .map_err(|error| format!("dynamic tool inputSchema: {error}"))?;
    let mut scopes = contract.scopes.clone();
    scopes.sort();
    if scopes
        .iter()
        .any(|scope| scope.trim().is_empty() || scope != scope.trim())
        || scopes.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err("dynamic tool scopes must be exact and unique".to_string());
    }
    Ok(())
}

fn schema_hash(contract: &DynamicToolContract) -> String {
    canonical_json::sha256("centaeris.dynamic_tool_contract.v1", contract)
        .expect("dynamic tool contract must serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_rejects_oversized_schema_and_aggregate_without_consuming_budget() {
        let mut contract = DynamicToolContract {
            name: "large_tool".to_string(),
            category: "external.context".to_string(),
            summary: "Large tool.".to_string(),
            input_schema: json!({"description": "a".repeat(MAX_TOOL_INPUT_SCHEMA_BYTES)}),
            provider_id: "test".to_string(),
            scopes: vec![],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        };
        let mut registry = DynamicToolRegistry::empty();
        assert!(registry
            .register(contract.clone())
            .unwrap_err()
            .contains("inputSchema"));
        contract.input_schema = json!({"description": "a".repeat(60 * 1024)});
        for index in 0..68 {
            contract.name = format!("large_tool_{index}");
            registry.register(contract.clone()).unwrap();
        }
        contract.name = "overflow_tool".to_string();
        assert!(registry
            .register(contract.clone())
            .unwrap_err()
            .contains("4194304"));
        assert_eq!(registry.len(), 68);
        contract.input_schema = json!({"type": "object"});
        registry.register(contract).unwrap();
    }

    #[test]
    fn registry_preserves_registered_definition() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "example_search".to_string(),
            category: "external.context".to_string(),
            summary: "\n  Search an external source.\n".to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: "example.local".to_string(),
            scopes: vec!["source:read".to_string()],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let contract = registry.find_contract("example_search").expect("contract");

        assert_eq!(contract.input_schema, json!({"type": "object"}));
        assert_eq!(contract.summary, "\n  Search an external source.\n");
        assert_eq!(contract.provider_id.as_deref(), Some("example.local"));
        assert!(contract.dynamic);
    }

    #[test]
    fn registry_rejects_dynamic_and_builtin_identity_collisions() {
        let contract = |name: &str, provider_id: &str| DynamicToolContract {
            name: name.to_string(),
            category: "external.context".to_string(),
            summary: "Search an external source.".to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: provider_id.to_string(),
            scopes: vec![],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        };

        assert!(
            DynamicToolRegistry::from_contracts(vec![contract("read", "one")])
                .expect_err("built-in collision must fail")
                .contains("collides with built-in tool")
        );
        assert!(DynamicToolRegistry::from_contracts(vec![
            contract("example_search", "one"),
            contract("example_search", "two"),
        ])
        .expect_err("dynamic collision must fail")
        .contains("duplicate dynamic tool identity"));
        assert!(
            DynamicToolRegistry::from_contracts(vec![contract(" external_lookup", "one")])
                .expect_err("dynamic identity must be exact")
                .contains("surrounding whitespace")
        );
        assert!(
            DynamicToolRegistry::from_contracts(vec![contract("NotCanonical", "one")])
                .expect_err("dynamic identity must be canonical")
                .contains("lower_snake_case")
        );
    }

    #[test]
    fn registry_normalizes_scopes_before_deriving_identity() {
        let contract = |scopes| DynamicToolContract {
            name: "example_search".to_string(),
            category: "external.context".to_string(),
            summary: "Search an external source.".to_string(),
            input_schema: json!({"type": "object"}),
            provider_id: "example.local".to_string(),
            scopes,
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        };
        let first = DynamicToolRegistry::from_contracts(vec![contract(vec![
            "source:write".to_string(),
            "source:read".to_string(),
        ])])
        .expect("first registry")
        .find_contract("example_search")
        .expect("first contract");
        let second = DynamicToolRegistry::from_contracts(vec![contract(vec![
            "source:read".to_string(),
            "source:write".to_string(),
        ])])
        .expect("second registry")
        .find_contract("example_search")
        .expect("second contract");

        assert_eq!(first.scopes, second.scopes);
        assert_eq!(first.schema_hash, second.schema_hash);
        assert!(DynamicToolRegistry::from_contracts(vec![contract(vec![
            "source:read".to_string(),
            "source:read".to_string(),
        ])])
        .expect_err("duplicate scopes must fail")
        .contains("exact and unique"));
    }

    #[test]
    fn dynamic_contract_requires_exact_turn_behavior() {
        let base = json!({
            "name": "example_tool",
            "category": "test",
            "summary": "Example tool.",
            "inputSchema": { "type": "object" },
            "providerId": "test.provider",
            "scopes": [],
            "concurrencySafe": true
        });
        assert!(serde_json::from_value::<DynamicToolContract>(base.clone()).is_err());

        let mut declared_hash = base.clone();
        declared_hash["turnBehavior"] = json!("continueTurn");
        declared_hash["schemaHash"] = json!(format!("sha256:{}", "0".repeat(64)));
        assert!(serde_json::from_value::<DynamicToolContract>(declared_hash).is_err());

        let mut unknown = base;
        unknown["turnBehavior"] = json!("banana");
        assert!(serde_json::from_value::<DynamicToolContract>(unknown).is_err());

        let mut extra = json!({
            "name": "example_tool",
            "category": "test",
            "summary": "Example tool.",
            "inputSchema": { "type": "object" },
            "providerId": "test.provider",
            "scopes": [],
            "concurrencySafe": true,
            "turnBehavior": "continueTurn"
        });
        extra["banana"] = json!(true);
        assert!(serde_json::from_value::<DynamicToolContract>(extra).is_err());
    }

    #[test]
    fn turn_behavior_changes_dynamic_contract_schema_hash() {
        let contract = |turn_behavior| DynamicToolContract {
            name: "example_tool".to_string(),
            category: "test".to_string(),
            summary: "Example tool.".to_string(),
            input_schema: json!({ "type": "object" }),
            provider_id: "test.provider".to_string(),
            scopes: vec![],
            concurrency_safe: true,
            turn_behavior,
        };
        let continuing = tool_contract(&contract(ToolTurnBehavior::ContinueTurn));
        let completing = tool_contract(&contract(ToolTurnBehavior::CompleteTurnOnSuccess));

        assert_ne!(continuing.schema_hash, completing.schema_hash);
    }
}
