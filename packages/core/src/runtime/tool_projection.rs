use super::*;
use crate::execution::ExecutionHostKind;
use crate::tool::list_tool_contracts_with_dynamic;
use std::collections::HashSet;

const TASK_OUTPUT_TOOL_NAME: &str = "task_output";
const AGENT_TOOL_NAME: &str = "agent";
const WORKSPACE_CODING_TOOL_NAMES: &[&str] = &["read", "bash", "edit", "write"];

pub(super) fn build_generate_tool_projection(
    _session: &SessionStateSnapshot,
    dynamic_registry: &DynamicToolRegistry,
    allowed_tools: Option<&[String]>,
    execution_host_kind: Option<ExecutionHostKind>,
) -> Result<(Vec<ModelToolDefinition>, String), String> {
    let definitions =
        build_generate_tool_contracts(dynamic_registry, allowed_tools, execution_host_kind)?
            .into_iter()
            .map(|contract| ModelToolDefinition {
                name: contract.name,
                description: contract.summary,
                input_schema: contract.input_schema,
            })
            .collect();
    Ok((definitions, String::new()))
}

pub(super) fn build_generate_tool_contracts(
    dynamic_registry: &DynamicToolRegistry,
    allowed_tools: Option<&[String]>,
    _execution_host_kind: Option<ExecutionHostKind>,
) -> Result<Vec<ToolContract>, String> {
    let all_contracts = list_tool_contracts_with_dynamic(dynamic_registry);
    let mut names = match allowed_tools {
        Some(allowed) => {
            if allowed.is_empty() {
                return Err("allowed tools must not be empty".to_string());
            }
            let mut names = Vec::with_capacity(allowed.len());
            let mut seen = HashSet::with_capacity(allowed.len());
            for name in allowed {
                if matches!(name.as_str(), AGENT_TOOL_NAME | TASK_OUTPUT_TOOL_NAME) {
                    return Err(format!("tool is not delegatable: {name}"));
                }
                if !seen.insert(name.as_str()) {
                    return Err(format!("allowed tools contains duplicate tool: {name}"));
                }
                if !all_contracts.iter().any(|contract| contract.name == *name) {
                    return Err(format!("allowed tools contains unknown tool: {name}"));
                }
                names.push(name.clone());
            }
            names
        }
        None => WORKSPACE_CODING_TOOL_NAMES
            .iter()
            .copied()
            .chain([AGENT_TOOL_NAME, TASK_OUTPUT_TOOL_NAME])
            .map(str::to_string)
            .collect::<Vec<_>>(),
    };
    if allowed_tools.is_none() {
        names.extend(
            dynamic_registry
                .list_contracts()
                .into_iter()
                .map(|contract| contract.name),
        );
    }
    let contracts = all_contracts
        .into_iter()
        .filter(|contract| names.contains(&contract.name))
        .collect::<Vec<_>>();
    if contracts.len() > 64 {
        return Err(format!(
            "model tool projection exceeds contract limit: {}",
            contracts.len()
        ));
    }
    Ok(contracts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{DynamicToolContract, ToolTurnBehavior};

    #[test]
    fn default_projection_uses_runtime_catalog_and_dynamic_tools() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "weather_lookup".to_string(),
            category: "weather.read".to_string(),
            summary: "Look up weather.".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            provider_id: "example.weather".to_string(),
            scopes: vec![],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let (definitions, guidance) = build_generate_tool_projection(
            &SessionStateSnapshot::new("chat-tools".to_string(), 0),
            &registry,
            None,
            None,
        )
        .expect("tool projection");

        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "read",
                "bash",
                "edit",
                "write",
                "task_output",
                "agent",
                "weather_lookup"
            ]
        );
        assert!(guidance.is_empty());
    }

    #[test]
    fn root_projection_always_includes_agent_and_task_output() {
        let registry = DynamicToolRegistry::empty();
        let session = SessionStateSnapshot::new("chat-task-output".to_string(), 0);
        let (definitions, _) = build_generate_tool_projection(&session, &registry, None, None)
            .expect("root projection");
        assert!(definitions
            .iter()
            .any(|definition| definition.name == TASK_OUTPUT_TOOL_NAME));
        assert!(definitions
            .iter()
            .any(|definition| definition.name == AGENT_TOOL_NAME));
    }

    #[test]
    fn child_projection_is_the_exact_allowed_parent_tool_subset() {
        let registry = DynamicToolRegistry::from_contracts(vec![DynamicToolContract {
            name: "weather_lookup".to_string(),
            category: "weather.read".to_string(),
            summary: "Look up weather.".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            provider_id: "example.weather".to_string(),
            scopes: vec![],
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
        }])
        .expect("dynamic registry");
        let session = SessionStateSnapshot::new("chat-child".to_string(), 0);
        let allowed = vec!["read".to_string(), "weather_lookup".to_string()];
        let (definitions, _) =
            build_generate_tool_projection(&session, &registry, Some(allowed.as_slice()), None)
                .expect("child projection");
        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            vec!["read", "weather_lookup"]
        );

        let error = build_generate_tool_projection(
            &session,
            &registry,
            Some(&["banana".to_string()]),
            None,
        )
        .expect_err("unknown child tool must fail");
        assert!(error.contains("banana"));

        let error =
            build_generate_tool_projection(&session, &registry, Some(&["agent".to_string()]), None)
                .expect_err("recursive delegation must fail");
        assert!(error.contains("not delegatable"));
    }

    #[test]
    fn root_projection_keeps_the_sixty_four_tool_limit_with_six_core_tools() {
        let dynamic_contracts = |count: usize| {
            (0..count)
                .map(|index| DynamicToolContract {
                    name: format!("plugin_tool_{index}"),
                    category: "plugin.read".to_string(),
                    summary: format!("Plugin tool {index}."),
                    input_schema: serde_json::json!({"type": "object"}),
                    provider_id: "example.plugin".to_string(),
                    scopes: vec![],
                    concurrency_safe: true,
                    turn_behavior: ToolTurnBehavior::ContinueTurn,
                })
                .collect::<Vec<_>>()
        };
        let session = SessionStateSnapshot::new("chat-tool-limit".to_string(), 0);
        let registry =
            DynamicToolRegistry::from_contracts(dynamic_contracts(58)).expect("58 dynamic tools");
        let (definitions, _) = build_generate_tool_projection(&session, &registry, None, None)
            .expect("six Core plus 58 dynamic tools");
        assert_eq!(definitions.len(), 64);

        let registry =
            DynamicToolRegistry::from_contracts(dynamic_contracts(59)).expect("59 dynamic tools");
        assert_eq!(
            build_generate_tool_projection(&session, &registry, None, None)
                .expect_err("six Core plus 59 dynamic tools must exceed the limit"),
            "model tool projection exceeds contract limit: 65"
        );
    }

    #[test]
    fn host_process_projection_exposes_bash_without_claiming_a_sandbox() {
        let registry = DynamicToolRegistry::empty();
        let session = SessionStateSnapshot::new("chat-host-process".to_string(), 0);
        let (definitions, _) = build_generate_tool_projection(
            &session,
            &registry,
            None,
            Some(ExecutionHostKind::LocalProcess),
        )
        .expect("host process projection");

        assert!(definitions
            .iter()
            .any(|definition| definition.name == "bash"));
        let (explicit, _) = build_generate_tool_projection(
            &session,
            &registry,
            Some(&["bash".to_string()]),
            Some(ExecutionHostKind::LocalProcess),
        )
        .expect("explicit Bash projection on a host process");
        assert_eq!(
            explicit
                .iter()
                .map(|definition| definition.name.as_str())
                .collect::<Vec<_>>(),
            vec!["bash"]
        );
    }
}
