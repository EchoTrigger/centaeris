use super::catalog::{list_tool_contracts, select_tool_contracts_by_names};
use super::dynamic::{list_tool_contracts_with_dynamic, DynamicToolRegistry};
use super::types::{ModelToolDefinition, ToolContract};

pub fn build_model_tool_definitions(limit: usize) -> Vec<ModelToolDefinition> {
    list_tool_contracts()
        .into_iter()
        .take(limit.max(1))
        .map(model_definition)
        .collect()
}

pub fn build_model_tool_definitions_for_names(
    limit: usize,
    names: &[String],
) -> Vec<ModelToolDefinition> {
    select_tool_contracts_by_names(names, limit.max(1))
        .into_iter()
        .map(model_definition)
        .collect()
}

pub fn build_model_tool_definitions_for_names_with_dynamic(
    limit: usize,
    names: &[String],
    registry: &DynamicToolRegistry,
) -> Vec<ModelToolDefinition> {
    list_tool_contracts_with_dynamic(registry)
        .into_iter()
        .filter(|contract| names.iter().any(|name| name == &contract.name))
        .take(limit.max(1))
        .map(model_definition)
        .collect()
}

fn model_definition(contract: ToolContract) -> ModelToolDefinition {
    ModelToolDefinition {
        name: contract.name,
        description: contract.summary,
        input_schema: contract.input_schema,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{DynamicToolContract, ToolTurnBehavior};
    use serde_json::json;

    #[test]
    fn fixed_and_enabled_dynamic_tools_project_their_registered_schema() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "example_search".to_string(),
            category: "external.context".to_string(),
            summary: "Search an external source.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"],
                "additionalProperties": false
            }),
            provider_id: "example.local".to_string(),
            scopes: vec![],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let names = vec!["read".to_string(), "example_search".to_string()];
        let definitions = build_model_tool_definitions_for_names_with_dynamic(8, &names, &registry);

        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            vec!["read", "example_search"]
        );
        assert_eq!(definitions[1].input_schema["required"], json!(["query"]));
    }

    #[test]
    fn dynamic_contract_cannot_replace_fixed_definition() {
        let error = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "read".to_string(),
            category: "external.context".to_string(),
            summary: "Read through the registered provider.".to_string(),
            input_schema: json!({"type": "object", "required": ["query"]}),
            provider_id: "example.local".to_string(),
            scopes: vec![],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect_err("built-in collision must fail");

        assert!(error.contains("collides with built-in tool"));
    }

    #[test]
    fn provider_definition_does_not_expose_turn_behavior() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "complete_turn_test_tool".to_string(),
            category: "test".to_string(),
            summary: "Complete a test turn.".to_string(),
            input_schema: json!({ "type": "object" }),
            provider_id: "test.provider".to_string(),
            scopes: vec![],
            concurrency_safe: false,
            turn_behavior: ToolTurnBehavior::CompleteTurnOnSuccess,
        }])
        .expect("dynamic registry");
        let definitions = build_model_tool_definitions_for_names_with_dynamic(
            1,
            &["complete_turn_test_tool".to_string()],
            &registry,
        );
        let projected = serde_json::to_value(&definitions[0]).expect("model definition");

        assert!(projected.get("turnBehavior").is_none());
    }
}
