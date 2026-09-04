use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum ModelWireApi {
    #[serde(rename = "openai-completions")]
    OpenAiCompletions,
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomModelDraft {
    pub model: String,
    pub display_name: Option<String>,
    pub context_tokens: String,
    pub max_output_tokens: String,
    pub api_override: Option<ModelWireApi>,
    pub supports_vision: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomModelProviderDraft {
    pub provider_id: String,
    pub name: String,
    pub base_url: String,
    pub api: ModelWireApi,
    pub models: Vec<CustomModelDraft>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCustomModelSettings {
    pub provider_id: String,
    pub base_url: String,
    pub model: String,
    pub display_name: Option<String>,
    pub context_tokens: u32,
    pub max_output_tokens: u32,
    pub api: ModelWireApi,
    pub supports_vision: bool,
}

pub fn canonical_custom_model_provider_id(value: &str) -> Result<String, String> {
    let provider_id = value.trim();
    if provider_id.starts_with("custom.")
        && provider_id.len() > "custom.".len()
        && provider_id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        })
    {
        Ok(provider_id.to_string())
    } else {
        Err(format!(
            "custom providerId must use canonical custom.* lowercase identifier: {provider_id}"
        ))
    }
}

pub fn validate_model_settings_structure(
    providers: &[CustomModelProviderDraft],
) -> Result<(), String> {
    let mut provider_ids = Vec::with_capacity(providers.len());
    for provider in providers {
        let provider_id = canonical_custom_model_provider_id(provider.provider_id.as_str())?;
        if provider_ids.iter().any(|item| item == &provider_id) {
            return Err(format!("duplicate custom providerId: {provider_id}"));
        }
        provider_ids.push(provider_id.clone());

        let mut model_ids = Vec::new();
        for model in &provider.models {
            let model_id = model.model.trim();
            if model_id.is_empty() {
                continue;
            }
            if model_ids.iter().any(|item| item == model_id) {
                return Err(format!(
                    "duplicate custom model id: providerId={provider_id} model={model_id}"
                ));
            }
            model_ids.push(model_id.to_string());
        }
    }
    Ok(())
}

pub fn resolve_custom_model_settings(
    provider: &CustomModelProviderDraft,
    model: &CustomModelDraft,
) -> Result<ResolvedCustomModelSettings, String> {
    let provider_id = canonical_custom_model_provider_id(provider.provider_id.as_str())?;
    let base_url = provider.base_url.trim();
    if base_url.is_empty() {
        return Err(format!(
            "custom provider baseUrl is empty: providerId={provider_id}"
        ));
    }
    if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
        return Err(format!(
            "custom provider baseUrl must use http:// or https://: providerId={provider_id}"
        ));
    }
    if base_url.chars().any(char::is_whitespace) {
        return Err(format!(
            "custom provider baseUrl must not contain whitespace: providerId={provider_id}"
        ));
    }

    let model_id = model.model.trim();
    if model_id.is_empty() {
        return Err(format!(
            "custom model id is empty: providerId={provider_id}"
        ));
    }
    let context_tokens = parse_model_token_quantity(model.context_tokens.as_str()).map_err(|error| {
        format!("custom model contextTokens is invalid: providerId={provider_id} model={model_id}: {error}")
    })?;
    let max_output_tokens =
        parse_model_token_quantity(model.max_output_tokens.as_str()).map_err(|error| {
            format!("custom model maxOutputTokens is invalid: providerId={provider_id} model={model_id}: {error}")
        })?;
    if context_tokens == 0 || max_output_tokens == 0 || max_output_tokens >= context_tokens {
        return Err(format!(
            "custom model token limits are invalid: providerId={provider_id} model={model_id}"
        ));
    }

    Ok(ResolvedCustomModelSettings {
        provider_id,
        base_url: base_url.trim_end_matches('/').to_string(),
        model: model_id.to_string(),
        display_name: model
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        context_tokens,
        max_output_tokens,
        api: model.api_override.unwrap_or(provider.api),
        supports_vision: model.supports_vision,
    })
}

pub fn parse_model_token_quantity(value: &str) -> Result<u32, String> {
    let normalized = value.trim().replace(char::is_whitespace, "");
    if normalized.is_empty() {
        return Err("value is empty".to_string());
    }
    let (number, multiplier) = match normalized.chars().last() {
        Some('k' | 'K') => (&normalized[..normalized.len() - 1], 1_024_f64),
        Some('m' | 'M') => (&normalized[..normalized.len() - 1], 1_048_576_f64),
        _ => (normalized.as_str(), 1_f64),
    };
    let mut parts = number.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    if whole.is_empty()
        || whole.chars().any(|character| !character.is_ascii_digit())
        || fraction.is_some_and(|value| {
            value.is_empty() || value.chars().any(|character| !character.is_ascii_digit())
        })
        || parts.next().is_some()
    {
        return Err("expected an integer or a k/M quantity".to_string());
    }
    let value = number
        .parse::<f64>()
        .map_err(|_| "expected an integer or a k/M quantity".to_string())?;
    if !value.is_finite() || value < 0.0 || (multiplier == 1.0 && value.fract() != 0.0) {
        return Err("value must resolve to a non-negative integer".to_string());
    }
    let tokens = value * multiplier;
    if tokens.fract() != 0.0 || tokens > u32::MAX as f64 {
        return Err("value is outside the u32 token range".to_string());
    }
    Ok(tokens as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> CustomModelProviderDraft {
        CustomModelProviderDraft {
            provider_id: "custom.test".to_string(),
            name: String::new(),
            base_url: "https://example.com/v1/".to_string(),
            api: ModelWireApi::OpenAiCompletions,
            models: vec![CustomModelDraft {
                model: "model-a".to_string(),
                display_name: None,
                context_tokens: "125k".to_string(),
                max_output_tokens: "31.25k".to_string(),
                api_override: None,
                supports_vision: false,
            }],
        }
    }

    #[test]
    fn incomplete_values_are_valid_settings_but_not_runnable() {
        let mut provider = provider();
        provider.base_url.clear();
        provider.models[0].context_tokens.clear();

        validate_model_settings_structure(&[provider.clone()])
            .expect("incomplete draft remains persistable");
        assert!(resolve_custom_model_settings(&provider, &provider.models[0]).is_err());
    }

    #[test]
    fn runtime_resolution_normalizes_only_the_runnable_projection() {
        let provider = provider();
        let resolved = resolve_custom_model_settings(&provider, &provider.models[0])
            .expect("resolve runnable custom model");

        assert_eq!(resolved.base_url, "https://example.com/v1");
        assert_eq!(resolved.max_output_tokens, 32_000);
    }

    #[test]
    fn structural_identity_remains_strict() {
        let mut provider = provider();
        provider.provider_id = "banana".to_string();
        assert!(validate_model_settings_structure(&[provider]).is_err());
    }

    #[test]
    fn token_quantity_parser_matches_settings_shorthand() {
        assert_eq!(parse_model_token_quantity("256k").unwrap(), 262_144);
        assert_eq!(parse_model_token_quantity("1M").unwrap(), 1_048_576);
        assert!(parse_model_token_quantity("banana").is_err());
        assert!(parse_model_token_quantity("1e3").is_err());
    }
}
