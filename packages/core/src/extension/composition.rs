use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::ModelClientRequest;
use crate::runtime::canonical_json;
use crate::tool::limits::{
    json_size_with_limit, validate_tool_description, ToolContractBudget,
    MAX_TOOL_INPUT_SCHEMA_BYTES,
};
use crate::tool::{ToolContract, ToolTurnBehavior};

pub const RESOLVED_AGENT_COMPOSITION_SCHEMA_V1: &str = "resolved_agent_composition_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedModelBindingV1 {
    pub provider_id: String,
    pub model_name: String,
    pub wire_protocol: String,
    pub config_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedToolContractV1 {
    pub name: String,
    pub category: String,
    pub summary: String,
    pub input_schema: Value,
    pub provider_id: String,
    pub contract_digest: String,
    pub concurrency_safe: bool,
    pub turn_behavior: ToolTurnBehavior,
    pub scopes: Vec<String>,
    pub dynamic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentCompositionInputsV1 {
    pub prompt_digest: String,
    pub model_binding: ResolvedModelBindingV1,
    pub skill_catalog_digest: String,
    pub plugin_activation_digest: String,
    pub hook_composition_digest: String,
    pub execution_profile_digest: String,
    pub policy_version: String,
}

#[derive(Debug, Clone)]
pub struct AgentCompositionEnvironmentV1 {
    pub tool_contracts: Vec<ToolContract>,
    pub skill_catalog_digest: String,
    pub plugin_activation_digest: String,
    pub hook_composition_digest: String,
    pub execution_profile_digest: String,
    pub policy_version: String,
    pub model_binding_override: Option<ResolvedModelBindingV1>,
}

impl AgentCompositionEnvironmentV1 {
    pub fn resolve_request(
        &self,
        request: &ModelClientRequest,
    ) -> Result<ResolvedAgentCompositionV1, String> {
        for definition in &request.prepared_prompt.tool_definitions {
            let contract = self
                .tool_contracts
                .iter()
                .find(|contract| contract.name == definition.name)
                .ok_or_else(|| {
                    format!(
                        "model-visible tool has no resolved contract: {}",
                        definition.name
                    )
                })?;
            if contract.summary != definition.description
                || contract.input_schema != definition.input_schema
            {
                return Err(format!(
                    "model-visible tool contract mismatch: {}",
                    definition.name
                ));
            }
        }
        let prompt_digest = canonical_json::sha256(
            "centaeris.agent_prompt.v1",
            &request.prepared_prompt.system_prompt,
        )?;
        let config_digest =
            canonical_json::sha256("centaeris.model_session_config.v1", &request.session_config)?;
        let wire_protocol = serde_json::to_value(request.session_config.provider_kind.clone())
            .map_err(|error| format!("encode model provider kind failed: {error}"))?
            .as_str()
            .ok_or_else(|| "model provider kind must serialize as a string".to_string())?
            .to_string();
        let model_binding = self
            .model_binding_override
            .clone()
            .unwrap_or(ResolvedModelBindingV1 {
                provider_id: request.session_config.provider_id.clone(),
                model_name: request.session_config.model.clone(),
                wire_protocol,
                config_digest,
            });
        if model_binding.provider_id != request.session_config.provider_id
            || model_binding.model_name != request.session_config.model
        {
            return Err("resolved model binding does not match request config".to_string());
        }
        resolve_agent_composition_borrowed(
            AgentCompositionInputsV1 {
                prompt_digest,
                model_binding,
                skill_catalog_digest: self.skill_catalog_digest.clone(),
                plugin_activation_digest: self.plugin_activation_digest.clone(),
                hook_composition_digest: self.hook_composition_digest.clone(),
                execution_profile_digest: self.execution_profile_digest.clone(),
                policy_version: self.policy_version.clone(),
            },
            &self.tool_contracts,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedAgentCompositionV1 {
    pub schema: String,
    pub prompt_digest: String,
    pub model_binding: ResolvedModelBindingV1,
    pub tool_contracts: Vec<ResolvedToolContractV1>,
    pub skill_catalog_digest: String,
    pub plugin_activation_digest: String,
    pub hook_composition_digest: String,
    pub execution_profile_digest: String,
    pub policy_version: String,
    pub composition_digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompositionDigestPreimage<'a> {
    schema: &'a str,
    prompt_digest: &'a str,
    model_binding: &'a ResolvedModelBindingV1,
    tool_contracts: &'a [ResolvedToolContractV1],
    skill_catalog_digest: &'a str,
    plugin_activation_digest: &'a str,
    hook_composition_digest: &'a str,
    execution_profile_digest: &'a str,
    policy_version: &'a str,
}

pub fn resolve_agent_composition(
    inputs: AgentCompositionInputsV1,
    tool_contracts: impl IntoIterator<Item = ToolContract>,
) -> Result<ResolvedAgentCompositionV1, String> {
    validate_inputs(&inputs)?;
    let mut budget = ToolContractBudget::default();
    let mut validated = Vec::new();
    for contract in tool_contracts {
        budget.add(&contract)?;
        validate_tool_content(&contract.summary, &contract.input_schema)?;
        validated.push(contract);
    }
    resolve_validated_agent_composition(inputs, validated)
}

fn validate_tool_content(summary: &str, schema: &Value) -> Result<(), String> {
    validate_tool_description(summary)?;
    if !schema.is_object() {
        return Err("resolved agent composition tool inputSchema must be an object".to_string());
    }
    json_size_with_limit(schema, MAX_TOOL_INPUT_SCHEMA_BYTES)
        .map_err(|error| format!("resolved agent composition tool inputSchema: {error}"))?;
    Ok(())
}

fn resolve_agent_composition_borrowed(
    inputs: AgentCompositionInputsV1,
    tool_contracts: &[ToolContract],
) -> Result<ResolvedAgentCompositionV1, String> {
    validate_inputs(&inputs)?;
    let mut budget = ToolContractBudget::default();
    for contract in tool_contracts {
        budget.add(contract)?;
        validate_tool_content(&contract.summary, &contract.input_schema)?;
    }
    resolve_validated_agent_composition(inputs, tool_contracts.iter().cloned())
}

fn resolve_validated_agent_composition(
    inputs: AgentCompositionInputsV1,
    tool_contracts: impl IntoIterator<Item = ToolContract>,
) -> Result<ResolvedAgentCompositionV1, String> {
    let mut seen_names = HashSet::new();
    let mut resolved_tools = tool_contracts
        .into_iter()
        .map(|contract| {
            if !seen_names.insert(contract.name.clone()) {
                return Err(format!(
                    "resolved agent composition has duplicate tool name: {}",
                    contract.name
                ));
            }
            let provider_id = required("tool providerId", contract.provider_id.as_deref())?;
            let contract_digest = contract.contract_digest()?;
            require_sha256("tool contractDigest", contract_digest.as_str())?;
            let mut scopes = contract.scopes;
            scopes.sort();
            if scopes.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(format!(
                    "resolved agent composition tool scopes are duplicated: {}",
                    contract.name
                ));
            }
            Ok(ResolvedToolContractV1 {
                name: required("tool name", Some(contract.name.as_str()))?,
                category: required("tool category", Some(contract.category.as_str()))?,
                summary: contract.summary,
                input_schema: contract.input_schema,
                provider_id,
                contract_digest,
                concurrency_safe: contract.concurrency_safe,
                turn_behavior: contract.turn_behavior,
                scopes,
                dynamic: contract.dynamic,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    resolved_tools.sort_by(|left, right| left.name.cmp(&right.name));
    let mut budget = ToolContractBudget::default();
    for tool in &resolved_tools {
        budget.add(tool)?;
    }

    let mut resolved = ResolvedAgentCompositionV1 {
        schema: RESOLVED_AGENT_COMPOSITION_SCHEMA_V1.to_string(),
        prompt_digest: inputs.prompt_digest,
        model_binding: inputs.model_binding,
        tool_contracts: resolved_tools,
        skill_catalog_digest: inputs.skill_catalog_digest,
        plugin_activation_digest: inputs.plugin_activation_digest,
        hook_composition_digest: inputs.hook_composition_digest,
        execution_profile_digest: inputs.execution_profile_digest,
        policy_version: inputs.policy_version,
        composition_digest: String::new(),
    };
    resolved.composition_digest = composition_digest(&resolved)?;
    Ok(resolved)
}

pub fn validate_resolved_agent_composition(
    composition: &ResolvedAgentCompositionV1,
) -> Result<(), String> {
    if composition.schema != RESOLVED_AGENT_COMPOSITION_SCHEMA_V1 {
        return Err("resolved agent composition schema mismatch".to_string());
    }
    validate_inputs(&AgentCompositionInputsV1 {
        prompt_digest: composition.prompt_digest.clone(),
        model_binding: composition.model_binding.clone(),
        skill_catalog_digest: composition.skill_catalog_digest.clone(),
        plugin_activation_digest: composition.plugin_activation_digest.clone(),
        hook_composition_digest: composition.hook_composition_digest.clone(),
        execution_profile_digest: composition.execution_profile_digest.clone(),
        policy_version: composition.policy_version.clone(),
    })?;
    let mut previous = None;
    let mut budget = ToolContractBudget::default();
    for tool in &composition.tool_contracts {
        budget.add(tool)?;
        validate_tool_content(&tool.summary, &tool.input_schema)?;
        required("tool name", Some(tool.name.as_str()))?;
        required("tool category", Some(tool.category.as_str()))?;
        required("tool providerId", Some(tool.provider_id.as_str()))?;
        require_sha256("tool contractDigest", tool.contract_digest.as_str())?;
        if tool.scopes.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(
                "resolved agent composition tool scopes must be uniquely sorted".to_string(),
            );
        }
        if previous.is_some_and(|name| name >= tool.name.as_str()) {
            return Err("resolved agent composition tools must be uniquely sorted".to_string());
        }
        previous = Some(tool.name.as_str());
    }
    require_sha256("compositionDigest", composition.composition_digest.as_str())?;
    if composition_digest(composition)? != composition.composition_digest {
        return Err("resolved agent composition digest mismatch".to_string());
    }
    Ok(())
}

pub fn validate_resolved_model_binding(binding: &ResolvedModelBindingV1) -> Result<(), String> {
    required(
        "modelBinding.providerId",
        Some(binding.provider_id.as_str()),
    )?;
    required("modelBinding.modelName", Some(binding.model_name.as_str()))?;
    required(
        "modelBinding.wireProtocol",
        Some(binding.wire_protocol.as_str()),
    )?;
    require_sha256("modelBinding.configDigest", binding.config_digest.as_str())
}

pub fn empty_composition_digest(kind: &str) -> Result<String, String> {
    canonical_json::sha256("centaeris.empty_composition.v1", &kind)
}

fn composition_digest(composition: &ResolvedAgentCompositionV1) -> Result<String, String> {
    canonical_json::sha256(
        "centaeris.resolved_agent_composition.v1",
        &CompositionDigestPreimage {
            schema: composition.schema.as_str(),
            prompt_digest: composition.prompt_digest.as_str(),
            model_binding: &composition.model_binding,
            tool_contracts: composition.tool_contracts.as_slice(),
            skill_catalog_digest: composition.skill_catalog_digest.as_str(),
            plugin_activation_digest: composition.plugin_activation_digest.as_str(),
            hook_composition_digest: composition.hook_composition_digest.as_str(),
            execution_profile_digest: composition.execution_profile_digest.as_str(),
            policy_version: composition.policy_version.as_str(),
        },
    )
}

fn validate_inputs(inputs: &AgentCompositionInputsV1) -> Result<(), String> {
    require_sha256("promptDigest", inputs.prompt_digest.as_str())?;
    validate_resolved_model_binding(&inputs.model_binding)?;
    for (name, digest) in [
        ("skillCatalogDigest", inputs.skill_catalog_digest.as_str()),
        (
            "pluginActivationDigest",
            inputs.plugin_activation_digest.as_str(),
        ),
        (
            "hookCompositionDigest",
            inputs.hook_composition_digest.as_str(),
        ),
        (
            "executionProfileDigest",
            inputs.execution_profile_digest.as_str(),
        ),
    ] {
        require_sha256(name, digest)?;
    }
    required("policyVersion", Some(inputs.policy_version.as_str()))?;
    Ok(())
}

fn required(name: &str, value: Option<&str>) -> Result<String, String> {
    let value = value.unwrap_or_default();
    if value.is_empty() || value != value.trim() {
        return Err(format!("{name} is required"));
    }
    Ok(value.to_string())
}

fn require_sha256(name: &str, value: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(format!("{name} must be sha256:<64 lowercase hex>"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{name} must be sha256:<64 lowercase hex>"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::prepared_prompt::{ModelMessageRoleV1, ModelMessageV1, PreparedPromptV1};
    use crate::model::ModelSessionConfig;
    use crate::tool::{list_tool_contracts, ModelToolChoice, ModelToolDefinition};

    #[test]
    fn composition_is_stable_and_rejects_duplicate_tools() {
        let digest = empty_composition_digest("test").expect("digest");
        let inputs = AgentCompositionInputsV1 {
            prompt_digest: digest.clone(),
            model_binding: ResolvedModelBindingV1 {
                provider_id: "centaeris.test".to_string(),
                model_name: "test-model".to_string(),
                wire_protocol: "test".to_string(),
                config_digest: digest.clone(),
            },
            skill_catalog_digest: digest.clone(),
            plugin_activation_digest: digest.clone(),
            hook_composition_digest: digest.clone(),
            execution_profile_digest: digest,
            policy_version: "runtime.v1".to_string(),
        };
        let first =
            resolve_agent_composition(inputs.clone(), list_tool_contracts()).expect("composition");
        let second =
            resolve_agent_composition(inputs.clone(), list_tool_contracts()).expect("composition");
        assert_eq!(first, second);
        validate_resolved_agent_composition(&first).expect("valid composition");

        let mut invalid = first.clone();
        invalid.tool_contracts[0].summary = " \n\t".to_string();
        assert!(validate_resolved_agent_composition(&invalid)
            .unwrap_err()
            .contains("description"));
        invalid = first.clone();
        invalid.tool_contracts[0].input_schema =
            serde_json::json!({"large": "a".repeat(MAX_TOOL_INPUT_SCHEMA_BYTES)});
        assert!(validate_resolved_agent_composition(&invalid)
            .unwrap_err()
            .contains("inputSchema"));
        let mut large = list_tool_contracts().remove(0);
        large.dynamic = false;
        large.input_schema = serde_json::json!({"large": "a".repeat(60 * 1024)});
        let pulled = std::cell::Cell::new(0);
        let oversized = (0..).map(|index| {
            pulled.set(pulled.get() + 1);
            let mut tool = large.clone();
            tool.name = format!("large_tool_{index}");
            tool
        });
        assert!(resolve_agent_composition(inputs.clone(), oversized)
            .unwrap_err()
            .contains("4194304"));
        assert!(
            pulled.get() < 70,
            "unbounded iterator must stop at byte limit"
        );
        invalid = first.clone();
        let mut large = invalid.tool_contracts[0].clone();
        large.dynamic = false;
        large.input_schema = serde_json::json!({"large": "a".repeat(60 * 1024)});
        invalid.tool_contracts = (0..70)
            .map(|index| {
                let mut tool = large.clone();
                tool.name = format!("large_tool_{index:02}");
                tool
            })
            .collect();
        assert!(validate_resolved_agent_composition(&invalid)
            .unwrap_err()
            .contains("4194304"));

        let tool = list_tool_contracts().remove(0);
        assert!(resolve_agent_composition(inputs, [tool.clone(), tool])
            .expect_err("duplicate")
            .contains("duplicate tool name"));
    }

    #[test]
    fn request_projection_is_a_subset_of_the_frozen_tool_composition() {
        let digest = empty_composition_digest("test").expect("digest");
        let contracts = list_tool_contracts();
        let read = contracts
            .iter()
            .find(|contract| contract.name == "read")
            .expect("read contract");
        let environment = AgentCompositionEnvironmentV1 {
            tool_contracts: contracts.clone(),
            skill_catalog_digest: digest.clone(),
            plugin_activation_digest: digest.clone(),
            hook_composition_digest: digest.clone(),
            execution_profile_digest: digest,
            policy_version: "test.v1".to_string(),
            model_binding_override: None,
        };
        let message = ModelMessageV1 {
            message_id: "message-1".to_string(),
            role: ModelMessageRoleV1::User,
            content: "test".to_string(),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        };
        let request = |tool_definitions, tool_choice| ModelClientRequest {
            session_id: "session-1".to_string(),
            turn_id: "turn-1".to_string(),
            loop_index: 0,
            provider_prompt_cache_key: None,
            provider_prompt_cache_retention: None,
            system_prompt_manifest_json: None,
            compression_stats_json: None,
            context_token_estimate: 1,
            prepared_prompt: PreparedPromptV1::new(
                None,
                vec![message.clone()],
                tool_definitions,
                tool_choice,
                128,
            )
            .expect("prepared prompt"),
            session_config: ModelSessionConfig::default(),
        };
        let projected = environment
            .resolve_request(&request(
                vec![ModelToolDefinition {
                    name: read.name.clone(),
                    description: read.summary.clone(),
                    input_schema: read.input_schema.clone(),
                }],
                ModelToolChoice::Auto,
            ))
            .expect("projected composition");
        let empty = environment
            .resolve_request(&request(vec![], ModelToolChoice::None))
            .expect("empty projection composition");

        assert_eq!(projected.composition_digest, empty.composition_digest);
        assert_eq!(projected.tool_contracts.len(), contracts.len());
        assert_eq!(
            projected
                .tool_contracts
                .iter()
                .find(|contract| contract.name == "read")
                .map(|contract| (&contract.summary, &contract.input_schema)),
            Some((&read.summary, &read.input_schema))
        );
    }
}
