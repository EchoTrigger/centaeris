use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub const MODEL_CATALOG_SCHEMA: &str = "centaeris.model_catalog.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCatalog {
    pub schema: String,
    pub providers: Vec<ModelProviderDefinition>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelProviderDefinition {
    pub provider_id: String,
    pub catalog_id: String,
    pub display_name: String,
    pub tier: ModelProviderTier,
    pub logo_id: String,
    #[serde(default, skip_deserializing)]
    pub logo_svg: Option<String>,
    pub provider_kind: String,
    pub api: ModelApi,
    pub api_base: String,
    pub credential: ModelCredentialDefinition,
    #[serde(default)]
    pub http_headers: HashMap<String, String>,
    pub models: Vec<ModelDefinition>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderTier {
    DirectApi,
    CodingPlan,
    TokenPlan,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCredentialDefinition {
    pub env: String,
    pub header: String,
    pub prefix: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum ModelApi {
    #[serde(rename = "openai-completions")]
    OpenAiCompletions,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelDefinition {
    pub model: String,
    pub display_name: String,
    pub context_tokens: u32,
    pub max_output_tokens: u32,
    pub thinking_mode: Option<String>,
    #[serde(default)]
    pub thinking_modes: Vec<String>,
    #[serde(default)]
    pub supports_vision: bool,
    pub api_override: Option<ModelApi>,
    pub api_base_override: Option<String>,
}

pub fn model_catalog() -> &'static ModelCatalog {
    static CATALOG: OnceLock<ModelCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut catalog: ModelCatalog =
            serde_json::from_str(include_str!("../centaeris_model_catalog/catalog.json"))
                .expect("embedded model catalog must be valid JSON");
        assert_eq!(catalog.schema, MODEL_CATALOG_SCHEMA, "model catalog schema");
        for provider in &mut catalog.providers {
            provider.logo_svg = Some(
                logo_svg(provider.logo_id.as_str())
                    .unwrap_or_else(|| panic!("unknown provider logo: {}", provider.logo_id))
                    .to_string(),
            );
        }
        catalog
    })
}

pub fn logo_svg(logo_id: &str) -> Option<&'static str> {
    let svg = match logo_id {
        "openai" => include_str!("../centaeris_model_catalog/logos/openai.svg"),
        "anthropic" => include_str!("../centaeris_model_catalog/logos/anthropic.svg"),
        "deepseek" => include_str!("../centaeris_model_catalog/logos/deepseek.svg"),
        "moonshot" => include_str!("../centaeris_model_catalog/logos/moonshot.svg"),
        "minimax" => include_str!("../centaeris_model_catalog/logos/minimax.svg"),
        "xiaomimimo" => include_str!("../centaeris_model_catalog/logos/xiaomimimo.svg"),
        "zai" => include_str!("../centaeris_model_catalog/logos/zai.svg"),
        "zhipu" => include_str!("../centaeris_model_catalog/logos/zhipu.svg"),
        "qwen" => include_str!("../centaeris_model_catalog/logos/qwen.svg"),
        "opencode" => include_str!("../centaeris_model_catalog/logos/opencode.svg"),
        "kimi" => include_str!("../centaeris_model_catalog/logos/kimi.svg"),
        "xai" => include_str!("../centaeris_model_catalog/logos/xai.svg"),
        "mistral" => include_str!("../centaeris_model_catalog/logos/mistral.svg"),
        "google" => include_str!("../centaeris_model_catalog/logos/google.svg"),
        "commandcode" => include_str!("../centaeris_model_catalog/logos/commandcode.svg"),
        _ => return None,
    };
    Some(svg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_identity_and_routes_are_unique_and_bounded() {
        let catalog = model_catalog();
        let mut provider_ids = HashSet::new();
        let mut catalog_ids = HashSet::new();
        assert!(!catalog.providers.is_empty());
        for provider in &catalog.providers {
            assert!(provider_ids.insert(provider.provider_id.as_str()));
            assert!(catalog_ids.insert(provider.catalog_id.as_str()));
            assert!(provider
                .logo_svg
                .as_deref()
                .is_some_and(|svg| svg.contains("<svg")));
            assert!(matches!(
                provider.provider_kind.as_str(),
                "open_ai"
                    | "anthropic"
                    | "kimi"
                    | "deep_seek"
                    | "zai"
                    | "xai"
                    | "mistral"
                    | "gemini"
                    | "command_code"
                    | "custom"
            ));
            assert!(!provider.credential.env.is_empty());
            assert!(!provider.credential.header.is_empty());
            assert!(provider.api_base.starts_with("https://"));
            assert!(!provider.models.is_empty());
            let mut model_ids = HashSet::new();
            for model in &provider.models {
                assert!(model_ids.insert(model.model.as_str()));
                assert!(model.max_output_tokens > 0);
                assert!(model.max_output_tokens < model.context_tokens);
                assert!(model
                    .api_base_override
                    .as_deref()
                    .is_none_or(|value| value.starts_with("https://")));
                assert!(
                    model.thinking_modes.is_empty()
                        || model
                            .thinking_mode
                            .as_ref()
                            .is_none_or(|value| model.thinking_modes.contains(value))
                );
            }
        }
    }

    #[test]
    fn approved_direct_api_and_opencode_go_models_are_exact() {
        let catalog = model_catalog();
        let direct = catalog
            .providers
            .iter()
            .filter(|provider| provider.tier == ModelProviderTier::DirectApi)
            .collect::<Vec<_>>();
        assert_eq!(direct.len(), 15);
        let go = catalog
            .providers
            .iter()
            .find(|provider| provider.catalog_id == "opencode_zen_go")
            .expect("OpenCode Go catalog entry");
        assert_eq!(go.tier, ModelProviderTier::CodingPlan);
        assert_eq!(
            go.models
                .iter()
                .map(|model| model.model.as_str())
                .collect::<Vec<_>>(),
            ["glm-5.3-flash", "deepseek-v4.1-flash"]
        );
        assert!(go
            .models
            .iter()
            .all(|model| model.thinking_mode.is_none() && model.thinking_modes.is_empty()));
        assert_eq!(
            catalog
                .providers
                .iter()
                .find(|provider| provider.catalog_id == "zai")
                .expect("existing Z.AI coding route")
                .api_base,
            "https://api.z.ai/api/coding/paas/v4"
        );
        assert_eq!(
            catalog
                .providers
                .iter()
                .find(|provider| provider.catalog_id == "zai_standard")
                .expect("new Z.AI standard route")
                .api_base,
            "https://api.z.ai/api/paas/v4"
        );
    }

    #[test]
    fn approved_second_wave_entries_have_exact_models_and_routes() {
        let catalog = model_catalog();
        let expected = [
            (
                "xai",
                ModelProviderTier::DirectApi,
                "openai-responses",
                "https://api.x.ai/v1",
                &["grok-4.7"][..],
            ),
            (
                "mistral",
                ModelProviderTier::DirectApi,
                "openai-completions",
                "https://api.mistral.ai/v1",
                &["mistral-small-2603", "mistral-medium-3-5"][..],
            ),
            (
                "google",
                ModelProviderTier::DirectApi,
                "openai-completions",
                "https://generativelanguage.googleapis.com/v1beta/openai",
                &["gemini-3.8-flash", "gemini-3.1-pro-preview"][..],
            ),
            (
                "command_code_goat",
                ModelProviderTier::CodingPlan,
                "openai-responses",
                "https://api.commandcode.ai/provider/v1",
                &[
                    "deepseek/deepseek-v4.1-flash",
                    "z-ai/glm-5.3-flash",
                    "xiaomi/mimo-v2.6-flash",
                    "MiniMaxAI/MiniMax-M3",
                ][..],
            ),
        ];
        for (id, tier, api, base, models) in expected {
            let provider = catalog
                .providers
                .iter()
                .find(|provider| provider.catalog_id == id)
                .unwrap_or_else(|| panic!("missing provider {id}"));
            assert_eq!(provider.tier, tier, "{id}");
            assert_eq!(serde_json::to_value(provider.api).unwrap(), api, "{id}");
            assert_eq!(provider.api_base, base, "{id}");
            assert_eq!(
                provider
                    .models
                    .iter()
                    .map(|model| model.model.as_str())
                    .collect::<Vec<_>>(),
                models,
                "{id}",
            );
        }
    }

    #[test]
    fn configured_second_wave_reasoning_levels_and_limits() {
        let catalog = model_catalog();
        let expected = [
            (
                "xai",
                "grok-4.7",
                500_000,
                65_536,
                "high",
                &["low", "medium", "high", "xhigh"][..],
            ),
            (
                "google",
                "gemini-3.8-flash",
                1_048_576,
                65_536,
                "medium",
                &["low", "medium", "high"][..],
            ),
            (
                "google",
                "gemini-3.1-pro-preview",
                1_048_576,
                65_536,
                "high",
                &["low", "medium", "high"][..],
            ),
        ];
        for (provider_id, model_id, context, output_cap, default_effort, efforts) in expected {
            let model = catalog
                .providers
                .iter()
                .find(|provider| provider.catalog_id == provider_id)
                .and_then(|provider| provider.models.iter().find(|model| model.model == model_id))
                .unwrap_or_else(|| panic!("missing {provider_id}/{model_id}"));
            assert_eq!(model.context_tokens, context, "{model_id}");
            assert_eq!(model.max_output_tokens, output_cap, "{model_id}");
            assert_eq!(
                model.thinking_mode.as_deref(),
                Some(default_effort),
                "{model_id}"
            );
            assert_eq!(
                model
                    .thinking_modes
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                efforts,
                "{model_id}"
            );
        }
    }
}
