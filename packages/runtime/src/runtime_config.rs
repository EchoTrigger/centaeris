use crate::{atomic_file::write_file_atomically, user_config, user_data_layout};
use centaeris_core::model::{
    built_in_model_profile, built_in_model_profiles, built_in_model_provider_ids,
    canonical_custom_model_provider_id, resolve_custom_model_settings,
    validate_model_settings_structure, AuthSpec, BuiltInModelProfile, CustomModelDraft,
    CustomModelProviderDraft, ModelProviderRegistry, ModelWireApi,
};
use centaeris_core::runtime::contracts::current_timestamp_ms;
use centaeris_core::runtime::{normalize_tool_parallelism, DEFAULT_TOOL_PARALLELISM};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const RUNTIME_CONFIG_UNSUPPORTED: &str = "runtime_config_unsupported";
const RUNTIME_SECRET_UNSUPPORTED: &str = "runtime_secret_unsupported";
const RUNTIME_CONFIG_IO: &str = "runtime_config_io";
const DEFAULT_MODEL_TIMEOUT_MS: u64 =
    centaeris_core::model::DEFAULT_MODEL_RESPONSE_HEADERS_TIMEOUT_MS;
const DEFAULT_MODEL_MAX_RETRIES: u32 = centaeris_core::model::DEFAULT_MODEL_MAX_RETRIES;
const DEFAULT_MODEL_RETRY_BACKOFF_MS: u64 = centaeris_core::model::DEFAULT_MODEL_RETRY_BACKOFF_MS;
static RUNTIME_CONFIG_STORE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) type CustomModelProviderApi = ModelWireApi;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRuntimeConfigGetRequest {}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRuntimeConfigSetRequest {
    pub(crate) bash_path: Option<String>,
    pub(crate) auto_continue_after_resume_wait: Option<bool>,
    pub(crate) agent_transport_mode: Option<String>,
    pub(crate) model_provider_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) model_thinking_mode: Option<String>,
    pub(crate) model_api_key: Option<String>,
    pub(crate) clear_model_api_key: Option<bool>,
    pub(crate) custom_model_providers: Option<Vec<CustomModelProviderDraft>>,
    pub(crate) tool_parallelism: Option<usize>,
    #[serde(rename = "agentPolicyMode")]
    pub(crate) removed_agent_policy_mode: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentRuntimeConfigRecord {
    #[serde(default)]
    bash_path: Option<String>,
    auto_continue_after_resume_wait: bool,
    #[serde(default = "default_agent_transport_mode")]
    agent_transport_mode: String,
    #[serde(default)]
    model_provider_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    model_api_base: Option<String>,
    #[serde(default)]
    model_timeout_ms: Option<u64>,
    #[serde(default)]
    model_max_retries: Option<u32>,
    #[serde(default)]
    model_retry_backoff_ms: Option<u64>,
    #[serde(default)]
    model_context_tokens: Option<u32>,
    #[serde(default)]
    model_max_output_tokens: Option<u32>,
    #[serde(default)]
    model_thinking_mode: Option<String>,
    #[serde(default)]
    tool_parallelism: Option<usize>,
    updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomModelRecord {
    provider_id: String,
    model: String,
    #[serde(default)]
    display_name: Option<String>,
    model_api_base: Option<String>,
    model_timeout_ms: Option<u64>,
    model_max_retries: Option<u32>,
    model_retry_backoff_ms: Option<u64>,
    model_context_tokens: String,
    model_max_output_tokens: String,
    model_thinking_mode: Option<String>,
    #[serde(default)]
    model_api: Option<CustomModelProviderApi>,
    supports_vision: bool,
    updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveModelRef {
    provider_id: String,
    model: String,
    #[serde(default)]
    model_thinking_mode: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedRuntimeConfigState {
    models: Vec<CustomModelRecord>,
    custom_model_providers: Vec<CustomModelProviderRecord>,
    active_model: Option<ActiveModelRef>,
    default_tool_parallelism: Option<usize>,
    default_bash_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomModelProviderRecord {
    provider_id: String,
    name: String,
    base_url: String,
    api: CustomModelProviderApi,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedRuntimeSecretState {
    model_api_keys: Vec<ModelProviderApiKeySecret>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelProviderApiKeySecret {
    provider_id: String,
    model_api_key: String,
    updated_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRuntimeConfigResetRequest {
    pub(crate) confirm: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentRuntimeConfigResetResponse {
    pub(crate) config: AgentRuntimeConfigResponse,
    pub(crate) quarantined_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentRuntimeConfigResponse {
    pub(crate) execution_host: String,
    pub(crate) bash_path: Option<String>,
    pub(crate) auto_continue_after_resume_wait: bool,
    pub(crate) agent_transport_mode: String,
    pub(crate) model_provider_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) model_providers: Vec<ModelProviderResponse>,
    pub(crate) selectable_models: Vec<SelectableModelResponse>,
    pub(crate) custom_model_providers: Vec<CustomModelProviderDraft>,
    pub(crate) model_api_base: Option<String>,
    pub(crate) model_timeout_ms: Option<u64>,
    pub(crate) model_max_retries: Option<u32>,
    pub(crate) model_retry_backoff_ms: Option<u64>,
    pub(crate) model_context_tokens: Option<u32>,
    pub(crate) model_max_output_tokens: Option<u32>,
    pub(crate) model_thinking_mode: Option<String>,
    pub(crate) tool_parallelism: Option<usize>,
    pub(crate) updated_at: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SelectableModelResponse {
    pub(crate) provider_id: String,
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) display_name: Option<String>,
    pub(crate) model_api_base: Option<String>,
    pub(crate) model_context_tokens: Option<u32>,
    pub(crate) model_max_output_tokens: Option<u32>,
    pub(crate) model_thinking_mode: Option<String>,
    pub(crate) model_thinking_modes: Vec<String>,
    pub(crate) model_api: Option<CustomModelProviderApi>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelCatalogItemResponse {
    pub(crate) provider_id: String,
    pub(crate) provider_name: String,
    pub(crate) model: String,
    pub(crate) display_name: Option<String>,
    pub(crate) model_api_base: Option<String>,
    pub(crate) model_context_tokens: Option<u32>,
    pub(crate) model_max_output_tokens: Option<u32>,
    pub(crate) model_thinking_mode: Option<String>,
    pub(crate) model_thinking_modes: Vec<String>,
    pub(crate) supports_vision: bool,
    pub(crate) built_in: bool,
    pub(crate) model_api: Option<CustomModelProviderApi>,
    pub(crate) diagnostic: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelProviderResponse {
    pub(crate) provider_id: String,
    pub(crate) name: String,
    pub(crate) built_in: bool,
    pub(crate) access_kind: String,
    pub(crate) configured: bool,
    pub(crate) credential_source: Option<String>,
    pub(crate) models: Vec<ModelCatalogItemResponse>,
}

pub(crate) fn get(
    _request: AgentRuntimeConfigGetRequest,
) -> Result<AgentRuntimeConfigResponse, String> {
    let _guard = runtime_config_store_guard()?;
    get_unlocked()
}

fn get_unlocked() -> Result<AgentRuntimeConfigResponse, String> {
    let state = load_state()?;
    let secrets = load_secret_state(&state)?;
    let record = normalize_record(default_record(&state), &state)?;
    Ok(into_response(record, &state, &secrets))
}

pub(crate) fn set(
    request: AgentRuntimeConfigSetRequest,
) -> Result<AgentRuntimeConfigResponse, String> {
    let _guard = runtime_config_store_guard()?;
    set_unlocked(request)
}

pub(crate) fn reset(
    request: AgentRuntimeConfigResetRequest,
) -> Result<AgentRuntimeConfigResetResponse, String> {
    if !request.confirm {
        return Err("runtime config reset requires confirm=true".to_string());
    }
    let _guard = runtime_config_store_guard()?;
    reset_unlocked(
        runtime_config_file_path().as_path(),
        runtime_secret_file_path().as_path(),
    )
}

pub(crate) fn error_code(error: &str) -> &'static str {
    if error.starts_with(RUNTIME_CONFIG_UNSUPPORTED) || error.starts_with("user_config_unsupported")
    {
        RUNTIME_CONFIG_UNSUPPORTED
    } else if error.starts_with(RUNTIME_SECRET_UNSUPPORTED) {
        RUNTIME_SECRET_UNSUPPORTED
    } else if error.starts_with(RUNTIME_CONFIG_IO) || error.starts_with("user_config_io") {
        RUNTIME_CONFIG_IO
    } else {
        "runtime_config_failed"
    }
}

fn reset_unlocked(
    config_path: &Path,
    secret_path: &Path,
) -> Result<AgentRuntimeConfigResetResponse, String> {
    let state = PersistedRuntimeConfigState::default();
    let secrets = PersistedRuntimeSecretState::default();
    persist_secret_state_at(secret_path, &secrets, &state)?;
    let quarantined_path = quarantine_runtime_config(config_path)?;
    persist_state_at(config_path, &state)?;
    let record = normalize_record(default_record(&state), &state)?;
    Ok(AgentRuntimeConfigResetResponse {
        config: into_response(record, &state, &secrets),
        quarantined_path: quarantined_path.map(|path| path.to_string_lossy().to_string()),
    })
}

fn quarantine_runtime_config(config_path: &Path) -> Result<Option<PathBuf>, String> {
    let metadata = match fs::symlink_metadata(config_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "{RUNTIME_CONFIG_IO}: inspect runtime config before reset failed: {error}"
            ));
        }
    };
    if !metadata.file_type().is_file() {
        return Err(format!(
            "{RUNTIME_CONFIG_IO}: runtime config reset target is not a regular file"
        ));
    }
    let parent = config_path
        .parent()
        .ok_or_else(|| format!("{RUNTIME_CONFIG_IO}: runtime config path has no parent"))?;
    let quarantined_path = (0..1_000i64)
        .map(|offset| {
            parent.join(format!(
                "config.unsupported-{}.toml",
                current_timestamp_ms().saturating_add(offset)
            ))
        })
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| {
            format!("{RUNTIME_CONFIG_IO}: runtime config quarantine path unavailable")
        })?;
    fs::rename(config_path, quarantined_path.as_path()).map_err(|error| {
        format!("{RUNTIME_CONFIG_IO}: quarantine runtime config failed: {error}")
    })?;
    Ok(Some(quarantined_path))
}

fn set_unlocked(
    request: AgentRuntimeConfigSetRequest,
) -> Result<AgentRuntimeConfigResponse, String> {
    let mut state = load_state()?;
    let mut secrets = load_secret_state(&state)?;
    if request
        .model_api_key
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err("modelApiKey must be non-empty".to_string());
    }
    let requested_model_api_key = normalize_optional_string(request.model_api_key.as_deref());
    let requested_clear_model_api_key = request.clear_model_api_key.unwrap_or(false);
    if requested_clear_model_api_key && requested_model_api_key.is_some() {
        return Err(String::from(
            "modelApiKey and clearModelApiKey cannot be used together",
        ));
    }
    if (requested_clear_model_api_key || requested_model_api_key.is_some())
        && (request.model.is_some()
            || request.model_thinking_mode.is_some()
            || request.custom_model_providers.is_some())
    {
        return Err(String::from(
            "credential mutation cannot be combined with model settings mutation",
        ));
    }
    let requested_model_provider_id =
        normalize_optional_string(request.model_provider_id.as_deref());
    let requested_bash_path = request.bash_path.is_some();
    let removed_custom_provider_ids = apply_custom_model_providers_request(
        &mut state,
        request.custom_model_providers.as_deref(),
    )?;
    apply_model_request(&mut state, &secrets, &request)?;
    apply_model_thinking_mode_request(&mut state, &request)?;
    let mut should_persist_secrets = !removed_custom_provider_ids.is_empty();
    for provider_id in removed_custom_provider_ids {
        clear_model_api_key(&mut secrets, provider_id.as_str());
    }
    if requested_clear_model_api_key {
        let provider_id = requested_model_provider_id
            .as_deref()
            .ok_or_else(|| String::from("modelProviderId is required to clear a model API key"))?;
        validate_settings_model_provider_id(&state, provider_id)?;
        clear_model_provider_credential(&mut state, &mut secrets, provider_id);
        should_persist_secrets = true;
    } else if let Some(model_api_key) = requested_model_api_key {
        let provider_id = requested_model_provider_id
            .as_deref()
            .ok_or_else(|| String::from("modelProviderId is required to save a model API key"))?;
        validate_settings_model_provider_id(&state, provider_id)?;
        persist_model_api_key(&mut secrets, provider_id, model_api_key);
        should_persist_secrets = true;
    }
    let mut record = default_record(&state);
    apply_request(&mut record, request)?;
    record.updated_at = current_timestamp_ms();
    let record = normalize_record(record, &state)?;
    apply_default_record(&mut state, &record, requested_bash_path);
    persist_state(&state)?;
    if should_persist_secrets {
        persist_secret_state(&secrets, &state)?;
    }
    Ok(into_response(record, &state, &secrets))
}

fn runtime_config_store_guard() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    RUNTIME_CONFIG_STORE_LOCK
        .lock()
        .map_err(|_| "runtime config store lock poisoned".to_string())
}

fn apply_model_request(
    state: &mut PersistedRuntimeConfigState,
    secrets: &PersistedRuntimeSecretState,
    request: &AgentRuntimeConfigSetRequest,
) -> Result<(), String> {
    if request.model.is_none() {
        return Ok(());
    }

    let provider_id = normalize_optional_string(request.model_provider_id.as_deref())
        .ok_or_else(|| String::from("modelProviderId is required to select a model"))?;
    let model = normalize_optional_string(request.model.as_deref())
        .ok_or_else(|| String::from("model is required to select a model"))?;
    validate_model_ready(state, provider_id.as_str(), model.as_str())?;
    if !model_provider_api_key_configured(secrets, provider_id.as_str()) {
        return Err(format!(
            "model provider is not configured: providerId={provider_id}"
        ));
    }
    let model_thinking_mode = built_in_model_profile(provider_id.as_str(), model.as_str())
        .and_then(|profile| profile.thinking_mode);
    state.active_model = Some(ActiveModelRef {
        provider_id,
        model,
        model_thinking_mode,
    });
    Ok(())
}

fn apply_model_thinking_mode_request(
    state: &mut PersistedRuntimeConfigState,
    request: &AgentRuntimeConfigSetRequest,
) -> Result<(), String> {
    let Some(raw) = request.model_thinking_mode.as_deref() else {
        return Ok(());
    };
    let normalized = raw.trim().to_ascii_lowercase();
    let active = state
        .active_model
        .as_mut()
        .ok_or_else(|| "modelThinkingMode requires an active model".to_string())?;
    let profile = built_in_model_profile(active.provider_id.as_str(), active.model.as_str());
    let mode = match normalized.as_str() {
        "default" => profile
            .as_ref()
            .and_then(|profile| profile.thinking_mode.as_deref())
            .ok_or_else(|| "modelThinkingMode=default is unsupported for this model".to_string())?,
        mode @ ("none" | "low" | "medium" | "high" | "xhigh" | "max") => mode,
        other => {
            return Err(format!(
                "unsupported modelThinkingMode={other}; expected none, low, medium, high, xhigh, or max"
            ))
        }
    };
    if let Some(profile) = profile.as_ref() {
        let valid = if profile.thinking_modes.is_empty() {
            profile.thinking_mode.as_deref() == Some(mode)
        } else {
            profile.thinking_modes.iter().any(|item| item == mode)
        };
        if !valid {
            let expected = if profile.thinking_modes.is_empty() {
                profile
                    .thinking_mode
                    .clone()
                    .unwrap_or_else(|| "provider default".to_string())
            } else {
                profile.thinking_modes.join(", ")
            };
            return Err(format!(
                "unsupported {} thinkingMode={mode}; expected {}",
                profile.display_name, expected
            ));
        }
    }
    active.model_thinking_mode = Some(mode.to_string());
    Ok(())
}

fn apply_custom_model_providers_request(
    state: &mut PersistedRuntimeConfigState,
    request: Option<&[CustomModelProviderDraft]>,
) -> Result<Vec<String>, String> {
    let Some(request) = request else {
        return Ok(Vec::new());
    };
    validate_model_settings_structure(request)?;
    let previous_provider_ids = state
        .custom_model_providers
        .iter()
        .map(|provider| provider.provider_id.clone())
        .collect::<Vec<_>>();
    let now = current_timestamp_ms();
    let mut provider_ids = Vec::with_capacity(request.len());
    let mut providers = Vec::with_capacity(request.len());
    let mut models = Vec::new();
    for requested_provider in request {
        let provider_id =
            canonical_custom_model_provider_id(requested_provider.provider_id.as_str())?;
        provider_ids.push(provider_id.clone());
        providers.push(CustomModelProviderRecord {
            provider_id: provider_id.clone(),
            name: requested_provider.name.clone(),
            base_url: requested_provider.base_url.clone(),
            api: requested_provider.api,
        });
        for requested_model in &requested_provider.models {
            models.push(CustomModelRecord {
                provider_id: provider_id.clone(),
                model: requested_model.model.clone(),
                display_name: requested_model.display_name.clone(),
                model_api_base: Some(requested_provider.base_url.clone()),
                model_timeout_ms: Some(DEFAULT_MODEL_TIMEOUT_MS),
                model_max_retries: Some(DEFAULT_MODEL_MAX_RETRIES),
                model_retry_backoff_ms: Some(DEFAULT_MODEL_RETRY_BACKOFF_MS),
                model_context_tokens: requested_model.context_tokens.clone(),
                model_max_output_tokens: requested_model.max_output_tokens.clone(),
                model_thinking_mode: None,
                model_api: requested_model
                    .api_override
                    .or(Some(requested_provider.api)),
                supports_vision: requested_model.supports_vision,
                updated_at: now,
            });
        }
    }
    state.custom_model_providers = providers;
    state.models = models;
    clear_unselectable_active_model(state);
    Ok(previous_provider_ids
        .into_iter()
        .filter(|provider_id| !provider_ids.contains(provider_id))
        .collect())
}

fn clear_unselectable_active_model(state: &mut PersistedRuntimeConfigState) {
    if state.active_model.as_ref().is_some_and(|active| {
        validate_model_ready(state, active.provider_id.as_str(), active.model.as_str()).is_err()
    }) {
        state.active_model = None;
    }
}

fn validate_settings_model_provider_id(
    state: &PersistedRuntimeConfigState,
    provider_id: &str,
) -> Result<(), String> {
    if built_in_model_provider_ids()
        .iter()
        .any(|item| item == provider_id)
        || state
            .custom_model_providers
            .iter()
            .any(|provider| provider.provider_id == provider_id)
    {
        Ok(())
    } else {
        Err(format!(
            "unsupported settings modelProviderId={provider_id}; configure a custom provider first or use deepseek.default/kimi.default"
        ))
    }
}

fn apply_request(
    record: &mut AgentRuntimeConfigRecord,
    request: AgentRuntimeConfigSetRequest,
) -> Result<(), String> {
    if request.bash_path.is_some() {
        record.bash_path = normalize_bash_path(request.bash_path.as_deref())?;
    }
    if let Some(value) = request.auto_continue_after_resume_wait {
        record.auto_continue_after_resume_wait = value;
    }
    if let Some(value) = request.agent_transport_mode.as_deref() {
        record.agent_transport_mode = normalize_agent_transport_mode(value);
    }
    set_copy(
        &mut record.tool_parallelism,
        request.tool_parallelism.map(normalize_tool_parallelism),
    );
    if request.removed_agent_policy_mode.is_some() {
        return Err("agentPolicyMode has been removed; ReAct is the only runtime base".to_string());
    }
    Ok(())
}

fn into_response(
    record: AgentRuntimeConfigRecord,
    state: &PersistedRuntimeConfigState,
    secrets: &PersistedRuntimeSecretState,
) -> AgentRuntimeConfigResponse {
    let model_providers = model_providers(state, secrets);
    let mut selectable_models = model_providers
        .iter()
        .flat_map(|provider| provider.models.iter())
        .filter(|item| item.diagnostic.is_none())
        .map(|item| SelectableModelResponse {
            provider_id: item.provider_id.clone(),
            provider_name: item.provider_name.clone(),
            model: item.model.clone(),
            display_name: item.display_name.clone(),
            model_api_base: item.model_api_base.clone(),
            model_context_tokens: item.model_context_tokens,
            model_max_output_tokens: item.model_max_output_tokens,
            model_thinking_mode: item.model_thinking_mode.clone(),
            model_thinking_modes: item.model_thinking_modes.clone(),
            model_api: item.model_api,
        })
        .collect::<Vec<_>>();
    selectable_models.sort_by_key(|item| {
        (
            item.display_name
                .as_deref()
                .unwrap_or(item.model.as_str())
                .to_ascii_lowercase(),
            item.provider_id.clone(),
        )
    });
    let custom_model_providers = custom_model_provider_drafts(state);
    AgentRuntimeConfigResponse {
        execution_host: default_execution_host(),
        bash_path: record.bash_path,
        auto_continue_after_resume_wait: record.auto_continue_after_resume_wait,
        agent_transport_mode: record.agent_transport_mode,
        model_provider_id: record.model_provider_id,
        model: record.model,
        model_providers,
        selectable_models,
        custom_model_providers,
        model_api_base: record.model_api_base,
        model_timeout_ms: record.model_timeout_ms,
        model_max_retries: record.model_max_retries,
        model_retry_backoff_ms: record.model_retry_backoff_ms,
        model_context_tokens: record.model_context_tokens,
        model_max_output_tokens: record.model_max_output_tokens,
        model_thinking_mode: record.model_thinking_mode,
        tool_parallelism: record.tool_parallelism,
        updated_at: record.updated_at,
    }
}

fn model_providers(
    state: &PersistedRuntimeConfigState,
    secrets: &PersistedRuntimeSecretState,
) -> Vec<ModelProviderResponse> {
    let catalog = model_catalog(state);
    let registry = ModelProviderRegistry::new();
    let mut providers = built_in_model_provider_ids()
        .into_iter()
        .map(|provider_id| {
            let credential_source = model_provider_credential_source(secrets, provider_id.as_str());
            ModelProviderResponse {
                provider_id: provider_id.clone(),
                name: registry
                    .get(provider_id.as_str())
                    .map(|provider| provider.name.clone())
                    .unwrap_or_else(|| provider_id.clone()),
                built_in: true,
                access_kind: "api_key".to_string(),
                configured: credential_source.is_some(),
                models: credential_source
                    .as_ref()
                    .map(|_| {
                        catalog
                            .iter()
                            .filter(|model| model.provider_id == provider_id)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default(),
                credential_source,
            }
        })
        .collect::<Vec<_>>();
    providers.extend(state.custom_model_providers.iter().map(|provider| {
        let credential_source =
            model_provider_credential_source(secrets, provider.provider_id.as_str());
        ModelProviderResponse {
            provider_id: provider.provider_id.clone(),
            name: provider.name.clone(),
            built_in: false,
            access_kind: "custom".to_string(),
            configured: credential_source.is_some(),
            models: credential_source
                .as_ref()
                .map(|_| {
                    catalog
                        .iter()
                        .filter(|model| model.provider_id == provider.provider_id)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            credential_source,
        }
    }));
    providers
}

fn model_provider_credential_source(
    secrets: &PersistedRuntimeSecretState,
    provider_id: &str,
) -> Option<String> {
    if secret_model_api_key_for(secrets, provider_id).is_some() {
        return Some("stored".to_string());
    }
    env_model_api_key_for_provider(provider_id).map(|_| "environment".to_string())
}

fn model_catalog(state: &PersistedRuntimeConfigState) -> Vec<ModelCatalogItemResponse> {
    let mut catalog = built_in_model_profiles()
        .into_iter()
        .map(model_catalog_item_from_profile)
        .collect::<Vec<_>>();
    catalog.extend(state.models.iter().filter_map(|configured| {
        let provider = state
            .custom_model_providers
            .iter()
            .find(|provider| provider.provider_id == configured.provider_id)?;
        let provider_draft = custom_model_provider_draft(state, provider);
        let model_draft = custom_model_draft(provider, configured);
        let resolved = resolve_custom_model_settings(&provider_draft, &model_draft);
        let diagnostic = resolved.as_ref().err().cloned();
        Some(ModelCatalogItemResponse {
            provider_id: configured.provider_id.clone(),
            provider_name: provider.name.clone(),
            model: resolved
                .as_ref()
                .map(|item| item.model.clone())
                .unwrap_or_else(|_| configured.model.trim().to_string()),
            display_name: resolved
                .as_ref()
                .map(|item| item.display_name.clone())
                .unwrap_or_else(|_| configured.display_name.clone()),
            model_api_base: resolved
                .as_ref()
                .map(|item| Some(item.base_url.clone()))
                .unwrap_or_else(|_| configured.model_api_base.clone()),
            model_context_tokens: resolved.as_ref().ok().map(|item| item.context_tokens),
            model_max_output_tokens: resolved.as_ref().ok().map(|item| item.max_output_tokens),
            model_thinking_mode: configured.model_thinking_mode.clone(),
            model_thinking_modes: Vec::new(),
            supports_vision: configured.supports_vision,
            built_in: false,
            model_api: resolved
                .as_ref()
                .map(|item| Some(item.api))
                .unwrap_or(configured.model_api),
            diagnostic,
        })
    }));
    catalog
}

fn model_catalog_item_from_profile(profile: BuiltInModelProfile) -> ModelCatalogItemResponse {
    let registry = ModelProviderRegistry::new();
    let provider = registry.get(profile.provider_id.as_str());
    ModelCatalogItemResponse {
        provider_id: profile.provider_id.clone(),
        provider_name: provider
            .map(|item| item.name.clone())
            .unwrap_or_else(|| profile.provider_id.clone()),
        model: profile.model,
        display_name: Some(profile.display_name),
        model_api_base: profile
            .api_base_override
            .clone()
            .or_else(|| provider.and_then(|item| item.base_url.clone())),
        model_context_tokens: Some(profile.context_tokens),
        model_max_output_tokens: Some(profile.max_output_tokens),
        model_thinking_mode: profile.thinking_mode,
        model_thinking_modes: profile.thinking_modes,
        supports_vision: profile.supports_vision,
        built_in: true,
        model_api: profile.api_override,
        diagnostic: None,
    }
}

fn custom_model_provider_drafts(
    state: &PersistedRuntimeConfigState,
) -> Vec<CustomModelProviderDraft> {
    state
        .custom_model_providers
        .iter()
        .map(|provider| custom_model_provider_draft(state, provider))
        .collect()
}

fn custom_model_provider_draft(
    state: &PersistedRuntimeConfigState,
    provider: &CustomModelProviderRecord,
) -> CustomModelProviderDraft {
    CustomModelProviderDraft {
        provider_id: provider.provider_id.clone(),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        api: provider.api,
        models: state
            .models
            .iter()
            .filter(|model| model.provider_id == provider.provider_id)
            .map(|model| custom_model_draft(provider, model))
            .collect(),
    }
}

fn custom_model_draft(
    provider: &CustomModelProviderRecord,
    model: &CustomModelRecord,
) -> CustomModelDraft {
    CustomModelDraft {
        model: model.model.clone(),
        display_name: model.display_name.clone(),
        context_tokens: model.model_context_tokens.clone(),
        max_output_tokens: model.model_max_output_tokens.clone(),
        api_override: model.model_api.filter(|api| *api != provider.api),
        supports_vision: model.supports_vision,
    }
}

fn model_provider_api_key_configured(
    secrets: &PersistedRuntimeSecretState,
    provider_id: &str,
) -> bool {
    secret_model_api_key_for(secrets, provider_id)
        .or_else(|| env_model_api_key_for_provider(provider_id))
        .is_some()
}

pub(crate) fn model_api_key_for_provider(provider_id: &str) -> Result<Option<String>, String> {
    let _guard = runtime_config_store_guard()?;
    let provider_id = normalize_model_provider_id(provider_id);
    let state = load_state()?;
    let secrets = load_secret_state(&state)?;
    Ok(secret_model_api_key_for(&secrets, provider_id.as_str())
        .or_else(|| env_model_api_key_for_provider(provider_id.as_str())))
}

fn secret_model_api_key_for(
    secrets: &PersistedRuntimeSecretState,
    provider_id: &str,
) -> Option<String> {
    provider_api_key_secret_for(secrets.model_api_keys.as_slice(), provider_id)
}

fn provider_api_key_secret_for(
    items: &[ModelProviderApiKeySecret],
    provider_id: &str,
) -> Option<String> {
    let normalized_provider_id = normalize_model_provider_id(provider_id);
    items
        .iter()
        .find(|item| {
            normalize_model_provider_id(item.provider_id.as_str()) == normalized_provider_id
        })
        .and_then(|item| normalize_optional_string(Some(item.model_api_key.as_str())))
}

fn persist_model_api_key(
    secrets: &mut PersistedRuntimeSecretState,
    provider_id: &str,
    model_api_key: String,
) {
    upsert_model_provider_api_key(
        &mut secrets.model_api_keys,
        provider_id,
        model_api_key,
        current_timestamp_ms(),
    );
}

fn upsert_model_provider_api_key(
    items: &mut Vec<ModelProviderApiKeySecret>,
    provider_id: &str,
    model_api_key: String,
    now: i64,
) {
    let provider_id = normalize_model_provider_id(provider_id);
    if let Some(existing) = items
        .iter_mut()
        .find(|item| normalize_model_provider_id(item.provider_id.as_str()) == provider_id)
    {
        existing.provider_id = provider_id;
        existing.model_api_key = model_api_key;
        existing.updated_at = now;
        return;
    }
    items.push(ModelProviderApiKeySecret {
        provider_id,
        model_api_key,
        updated_at: now,
    });
}

fn clear_model_api_key(secrets: &mut PersistedRuntimeSecretState, provider_id: &str) {
    remove_model_provider_api_key(&mut secrets.model_api_keys, provider_id);
}

fn clear_model_provider_credential(
    state: &mut PersistedRuntimeConfigState,
    secrets: &mut PersistedRuntimeSecretState,
    provider_id: &str,
) {
    clear_model_api_key(secrets, provider_id);
    if !model_provider_api_key_configured(secrets, provider_id)
        && state
            .active_model
            .as_ref()
            .is_some_and(|active| active.provider_id == provider_id)
    {
        state.active_model = None;
    }
}

fn remove_model_provider_api_key(items: &mut Vec<ModelProviderApiKeySecret>, provider_id: &str) {
    let provider_id = normalize_model_provider_id(provider_id);
    items.retain(|item| normalize_model_provider_id(item.provider_id.as_str()) != provider_id);
}

fn normalize_model_provider_id(provider_id: &str) -> String {
    provider_id.trim().to_string()
}

fn env_model_api_key_for_provider(provider_id: &str) -> Option<String> {
    provider_api_key_env_var_name(provider_id).and_then(|env_key| env_var_value(env_key.as_str()))
}

fn provider_api_key_env_var_name(provider_id: &str) -> Option<String> {
    let provider_id = normalize_model_provider_id(provider_id);
    let registry = ModelProviderRegistry::new();
    registry
        .get(provider_id.as_str())
        .and_then(|provider| match &provider.auth {
            AuthSpec::ApiKeyEnv { env_key, .. } | AuthSpec::BearerEnv { env_key } => {
                Some(env_key.clone())
            }
            AuthSpec::None | AuthSpec::StaticHeader { .. } | AuthSpec::CommandToken { .. } => None,
        })
}

fn resolve_model_record(
    state: &PersistedRuntimeConfigState,
    provider_id: &str,
    model: &str,
) -> Option<CustomModelRecord> {
    if let Some(profile) = built_in_model_profile(provider_id, model) {
        return Some(CustomModelRecord {
            provider_id: profile.provider_id,
            model: profile.model,
            display_name: Some(profile.display_name),
            model_api_base: profile.api_base_override,
            model_timeout_ms: Some(DEFAULT_MODEL_TIMEOUT_MS),
            model_max_retries: Some(DEFAULT_MODEL_MAX_RETRIES),
            model_retry_backoff_ms: Some(DEFAULT_MODEL_RETRY_BACKOFF_MS),
            model_context_tokens: profile.context_tokens.to_string(),
            model_max_output_tokens: profile.max_output_tokens.to_string(),
            model_thinking_mode: profile.thinking_mode,
            model_api: profile.api_override,
            supports_vision: profile.supports_vision,
            updated_at: 0,
        });
    }
    state
        .models
        .iter()
        .find(|item| item.provider_id == provider_id && item.model.trim() == model.trim())
        .cloned()
}

fn validate_model_ready(
    state: &PersistedRuntimeConfigState,
    provider_id: &str,
    model: &str,
) -> Result<(), String> {
    if built_in_model_profile(provider_id, model).is_some() {
        return Ok(());
    }
    let provider_record = state
        .custom_model_providers
        .iter()
        .find(|item| item.provider_id == provider_id)
        .ok_or_else(|| {
            format!("catalog model does not exist: providerId={provider_id} model={model}")
        })?;
    let configured = state
        .models
        .iter()
        .find(|item| item.provider_id == provider_id && item.model.trim() == model.trim())
        .ok_or_else(|| {
            format!("catalog model does not exist: providerId={provider_id} model={model}")
        })?;
    let provider = custom_model_provider_draft(state, provider_record);
    let model = custom_model_draft(provider_record, configured);
    resolve_custom_model_settings(&provider, &model).map(|_| ())
}

fn active_model_record(state: &PersistedRuntimeConfigState) -> Option<CustomModelRecord> {
    let active = state.active_model.as_ref()?;
    let mut record =
        resolve_model_record(state, active.provider_id.as_str(), active.model.as_str())?;
    record.model_thinking_mode = active
        .model_thinking_mode
        .clone()
        .or(record.model_thinking_mode);
    Some(record)
}

fn default_record(state: &PersistedRuntimeConfigState) -> AgentRuntimeConfigRecord {
    let active_model = active_model_record(state);
    let active_model = active_model.as_ref();
    AgentRuntimeConfigRecord {
        bash_path: state.default_bash_path.clone(),
        auto_continue_after_resume_wait: true,
        agent_transport_mode: default_agent_transport_mode(),
        model_provider_id: active_model.map(|item| item.provider_id.clone()),
        model: active_model.map(|item| item.model.clone()),
        model_api_base: active_model.and_then(|item| item.model_api_base.clone()),
        model_timeout_ms: active_model.and_then(|item| item.model_timeout_ms),
        model_max_retries: active_model.and_then(|item| item.model_max_retries),
        model_retry_backoff_ms: active_model.and_then(|item| item.model_retry_backoff_ms),
        model_context_tokens: active_model.and_then(|item| {
            centaeris_core::model::parse_model_token_quantity(item.model_context_tokens.as_str())
                .ok()
        }),
        model_max_output_tokens: active_model.and_then(|item| {
            centaeris_core::model::parse_model_token_quantity(item.model_max_output_tokens.as_str())
                .ok()
        }),
        model_thinking_mode: active_model.and_then(|item| item.model_thinking_mode.clone()),
        tool_parallelism: Some(
            state
                .default_tool_parallelism
                .map(normalize_tool_parallelism)
                .unwrap_or(DEFAULT_TOOL_PARALLELISM),
        ),
        updated_at: current_timestamp_ms(),
    }
}

fn normalize_record(
    mut record: AgentRuntimeConfigRecord,
    state: &PersistedRuntimeConfigState,
) -> Result<AgentRuntimeConfigRecord, String> {
    record.agent_transport_mode =
        normalize_agent_transport_mode(record.agent_transport_mode.as_str());
    record.bash_path = normalize_bash_path(
        record
            .bash_path
            .as_deref()
            .or(state.default_bash_path.as_deref()),
    )?;
    record.tool_parallelism = Some(normalize_tool_parallelism(
        record.tool_parallelism.unwrap_or_else(|| {
            state
                .default_tool_parallelism
                .unwrap_or(DEFAULT_TOOL_PARALLELISM)
        }),
    ));
    Ok(record)
}

fn apply_default_record(
    state: &mut PersistedRuntimeConfigState,
    record: &AgentRuntimeConfigRecord,
    persist_bash_path: bool,
) {
    state.default_tool_parallelism = record.tool_parallelism;
    if persist_bash_path {
        state.default_bash_path = record.bash_path.clone();
    }
}

fn load_state() -> Result<PersistedRuntimeConfigState, String> {
    Ok(user_config::load()?.runtime)
}

pub(crate) fn validate_persisted_model_state(
    state: &PersistedRuntimeConfigState,
) -> Result<(), String> {
    let mut custom_provider_ids = Vec::new();
    for provider in &state.custom_model_providers {
        let canonical = canonical_custom_model_provider_id(provider.provider_id.as_str())?;
        if canonical != provider.provider_id {
            return Err(format!(
                "persisted custom providerId is non-canonical: {:?}",
                provider.provider_id
            ));
        }
        if custom_provider_ids.contains(&canonical) {
            return Err(format!("duplicate persisted custom providerId={canonical}"));
        }
        custom_provider_ids.push(canonical);
    }
    let mut model_ids = HashSet::new();
    for model in &state.models {
        if model.provider_id.trim() != model.provider_id
            || model.model.trim() != model.model
            || model.model.is_empty()
        {
            return Err(format!(
                "persisted model identity is empty or non-canonical: providerId={:?} model={:?}",
                model.provider_id, model.model
            ));
        }
        let canonical_provider_id = canonical_custom_model_provider_id(model.provider_id.as_str())?;
        if canonical_provider_id != model.provider_id {
            return Err(format!(
                "persisted model providerId is non-canonical: {:?}",
                model.provider_id
            ));
        }
        if !model_ids.insert((model.provider_id.clone(), model.model.clone())) {
            return Err(format!(
                "duplicate persisted model: providerId={} model={}",
                model.provider_id, model.model
            ));
        }
    }
    validate_model_settings_structure(custom_model_provider_drafts(state).as_slice())?;
    for provider in &state.custom_model_providers {
        for model in state
            .models
            .iter()
            .filter(|model| model.provider_id == provider.provider_id)
        {
            if model.model_api_base.as_deref() != Some(provider.base_url.as_str()) {
                return Err(format!(
                    "custom model base URL does not match its provider: providerId={} model={}",
                    model.provider_id, model.model
                ));
            }
            if model.model_api.is_none() {
                return Err(format!(
                    "custom model api is missing: providerId={} model={}",
                    model.provider_id, model.model
                ));
            }
        }
    }
    if let Some(model) = state.models.iter().find(|model| {
        !state
            .custom_model_providers
            .iter()
            .any(|provider| provider.provider_id == model.provider_id)
    }) {
        return Err(format!(
            "custom model records require configured provider: providerId={} model={}",
            model.provider_id, model.model
        ));
    }

    if let Some(active) = state.active_model.as_ref() {
        if active.provider_id.trim() != active.provider_id
            || active.model.trim() != active.model
            || active.provider_id.is_empty()
            || active.model.is_empty()
        {
            return Err("active model identity is empty or non-canonical".to_string());
        }
        validate_model_ready(state, active.provider_id.as_str(), active.model.as_str()).map_err(
            |_| {
                format!(
                    "active model is not selectable: providerId={} model={}",
                    active.provider_id, active.model
                )
            },
        )?;
        if let Some(mode) = active.model_thinking_mode.as_deref() {
            if let Some(profile) =
                built_in_model_profile(active.provider_id.as_str(), active.model.as_str())
            {
                let valid = profile.thinking_modes.iter().any(|item| item == mode)
                    || (profile.thinking_modes.is_empty()
                        && profile.thinking_mode.as_deref() == Some(mode));
                if !valid {
                    return Err(format!(
                        "unsupported persisted modelThinkingMode={mode} for providerId={} model={}",
                        active.provider_id, active.model
                    ));
                }
            } else if !matches!(mode, "none" | "low" | "medium" | "high" | "xhigh" | "max") {
                return Err(format!(
                    "unsupported persisted modelThinkingMode={mode}; expected none, low, medium, high, xhigh, or max"
                ));
            }
        }
    }
    Ok(())
}

fn validate_secret_state(
    state: &PersistedRuntimeSecretState,
    config: &PersistedRuntimeConfigState,
) -> Result<(), String> {
    let mut provider_ids = Vec::new();
    for item in &state.model_api_keys {
        let provider_id = normalize_model_provider_id(item.provider_id.as_str());
        if provider_id != item.provider_id || provider_id.is_empty() {
            return Err(format!(
                "{RUNTIME_SECRET_UNSUPPORTED}: providerId is empty or non-canonical: {:?}",
                item.provider_id
            ));
        }
        let supported = built_in_model_provider_ids().contains(&provider_id)
            || config
                .custom_model_providers
                .iter()
                .any(|provider| provider.provider_id == provider_id);
        if !supported {
            return Err(format!(
                "{RUNTIME_SECRET_UNSUPPORTED}: secret has unsupported providerId={provider_id}"
            ));
        }
        if provider_ids.contains(&provider_id) {
            return Err(format!(
                "{RUNTIME_SECRET_UNSUPPORTED}: duplicate providerId={provider_id}"
            ));
        }
        provider_ids.push(provider_id);
        if item.model_api_key.is_empty()
            || item.model_api_key.trim().is_empty()
            || item.model_api_key.trim() != item.model_api_key
        {
            return Err(format!(
                "{RUNTIME_SECRET_UNSUPPORTED}: modelApiKey is empty or non-canonical for providerId={}",
                item.provider_id
            ));
        }
        if item.updated_at < 0 {
            return Err(format!(
                "{RUNTIME_SECRET_UNSUPPORTED}: updatedAt must be non-negative for providerId={}",
                item.provider_id
            ));
        }
    }
    Ok(())
}

fn load_secret_state(
    config: &PersistedRuntimeConfigState,
) -> Result<PersistedRuntimeSecretState, String> {
    load_secret_state_from_path(runtime_secret_file_path().as_path(), config)
}

fn load_secret_state_from_path(
    file_path: &std::path::Path,
    config: &PersistedRuntimeConfigState,
) -> Result<PersistedRuntimeSecretState, String> {
    match fs::read_to_string(file_path) {
        Ok(raw) => {
            let encoded =
                serde_json::from_str::<serde_json::Value>(raw.as_str()).map_err(|error| {
                    format!(
                        "{RUNTIME_SECRET_UNSUPPORTED}: parse runtime secrets JSON failed for {}: {error}",
                        file_path.display()
                    )
                })?;
            require_exact_keys(
                &encoded,
                &["modelApiKeys"],
                "runtime secrets",
                RUNTIME_SECRET_UNSUPPORTED,
            )?;
            for (index, item) in encoded
                .get("modelApiKeys")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    format!("{RUNTIME_SECRET_UNSUPPORTED}: modelApiKeys must be an array")
                })?
                .iter()
                .enumerate()
            {
                require_exact_keys(
                    item,
                    &["providerId", "modelApiKey", "updatedAt"],
                    format!("runtime secrets modelApiKeys[{index}]").as_str(),
                    RUNTIME_SECRET_UNSUPPORTED,
                )?;
            }
            let state = serde_json::from_value::<PersistedRuntimeSecretState>(encoded).map_err(
                |error| {
                    format!(
                        "{RUNTIME_SECRET_UNSUPPORTED}: parse runtime secrets failed for {}: {error}",
                        file_path.display()
                    )
                },
            )?;
            validate_secret_state(&state, config)?;
            Ok(state)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(PersistedRuntimeSecretState::default())
        }
        Err(error) => Err(format!(
            "{RUNTIME_CONFIG_IO}: read runtime secrets failed for {}: {error}",
            file_path.display()
        )),
    }
}

fn require_exact_keys(
    value: &serde_json::Value,
    expected: &[&str],
    label: &str,
    code: &str,
) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{code}: {label} must be an object"))?;
    for key in expected {
        if !object.contains_key(*key) {
            return Err(format!("{code}: {label} missing field {key}"));
        }
    }
    for key in object.keys() {
        if !expected.contains(&key.as_str()) {
            return Err(format!("{code}: {label} unknown field {key}"));
        }
    }
    Ok(())
}

fn persist_state(state: &PersistedRuntimeConfigState) -> Result<(), String> {
    validate_persisted_model_state(state)
        .map_err(|error| format!("{RUNTIME_CONFIG_UNSUPPORTED}: {error}"))?;
    user_config::update(|config| {
        config.runtime = state.clone();
        Ok(())
    })
}

fn persist_state_at(file_path: &Path, state: &PersistedRuntimeConfigState) -> Result<(), String> {
    validate_persisted_model_state(state)
        .map_err(|error| format!("{RUNTIME_CONFIG_UNSUPPORTED}: {error}"))?;
    let mut config = user_config::load_at(file_path)?;
    config.runtime = state.clone();
    user_config::persist_at(file_path, &config)
}

fn persist_secret_state(
    state: &PersistedRuntimeSecretState,
    config: &PersistedRuntimeConfigState,
) -> Result<(), String> {
    persist_secret_state_at(runtime_secret_file_path().as_path(), state, config)
}

fn persist_secret_state_at(
    file_path: &Path,
    state: &PersistedRuntimeSecretState,
    config: &PersistedRuntimeConfigState,
) -> Result<(), String> {
    validate_secret_state(state, config)?;
    let encoded = serde_json::to_string_pretty(state)
        .map_err(|error| format!("serialize runtime secrets failed: {error}"))?;
    write_file_atomically(file_path, encoded.as_bytes(), "runtime secrets")
        .map_err(|error| format!("{RUNTIME_CONFIG_IO}: {error}"))
}

fn runtime_config_file_path() -> PathBuf {
    user_data_layout::user_config_file_path()
}

fn runtime_secret_file_path() -> PathBuf {
    if let Some(path_raw) = std::env::var_os("CENTAERIS_AGENT_RUNTIME_SECRETS_PATH") {
        return PathBuf::from(path_raw);
    }
    user_data_layout::runtime_secret_file_path()
}

fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
}

fn set_copy<T: Copy>(target: &mut Option<T>, value: Option<T>) {
    if let Some(value) = value {
        *target = Some(value);
    }
}

fn normalize_agent_transport_mode(value: &str) -> String {
    match value.trim() {
        "desktop_primary" => String::from("desktop_primary"),
        _ => default_agent_transport_mode(),
    }
}

fn normalize_bash_path(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    #[cfg(target_os = "windows")]
    return centaeris_runtime::local_execution_host::resolve_bash_path(Some(PathBuf::from(value)))
        .map(|path| Some(path.to_string_lossy().to_string()))
        .map_err(|error| error.internal_debug_message());
    #[cfg(not(target_os = "windows"))]
    {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err("bashPath must be an absolute path".to_string());
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("canonicalize bashPath failed: {error}"))?;
        if !canonical.is_file() {
            return Err(format!(
                "bashPath is not an executable file: {}",
                canonical.display()
            ));
        }
        Ok(Some(canonical.to_string_lossy().to_string()))
    }
}

fn default_execution_host() -> String {
    "localUser".to_string()
}

fn default_agent_transport_mode() -> String {
    String::from("desktop_primary")
}

fn env_var_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .and_then(|value| normalize_optional_string(Some(value.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::model::DEEPSEEK_PROVIDER_ID;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "centaeris-runtime-config-{name}-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        fs::create_dir_all(path.as_path()).expect("create temp dir");
        path
    }

    #[test]
    fn execution_host_is_local_user_on_every_desktop_platform() {
        assert_eq!(default_execution_host(), "localUser");
    }

    #[test]
    fn bash_path_requires_an_existing_absolute_file() {
        let current_exe = std::env::current_exe().expect("current executable");
        #[cfg(not(target_os = "windows"))]
        assert_eq!(
            normalize_bash_path(Some(current_exe.to_string_lossy().as_ref()))
                .expect("normalize executable"),
            Some(
                current_exe
                    .canonicalize()
                    .expect("canonical executable")
                    .to_string_lossy()
                    .to_string()
            )
        );
        #[cfg(target_os = "windows")]
        {
            let bash = centaeris_runtime::local_execution_host::resolve_bash_path(None)
                .expect("Git for Windows Bash is required by the Windows test gate");
            assert_eq!(
                normalize_bash_path(Some(bash.to_string_lossy().as_ref()))
                    .expect("normalize Git Bash override"),
                Some(bash.to_string_lossy().to_string())
            );
            assert!(
                normalize_bash_path(Some(current_exe.to_string_lossy().as_ref()))
                    .expect_err("non-Git Bash override must loud-fail")
                    .contains("Git for Windows Bash")
            );
        }
        assert!(normalize_bash_path(Some("banana/bash.exe")).is_err());
    }

    #[test]
    fn default_record_has_no_implicit_model() {
        let state = PersistedRuntimeConfigState::default();
        let record = default_record(&state);

        assert_eq!(record.model_provider_id, None);
        assert_eq!(record.model, None);
        assert_eq!(record.model_context_tokens, None);
        assert_eq!(record.model_max_output_tokens, None);
        assert_eq!(record.tool_parallelism, Some(DEFAULT_TOOL_PARALLELISM));
        assert_eq!(record.bash_path, None);
    }

    #[test]
    fn runtime_config_persists_invalid_model_token_limits_without_selecting_them() {
        let mut state = PersistedRuntimeConfigState::default();
        let request = CustomModelProviderDraft {
            provider_id: "custom.test".to_string(),
            name: "Custom".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api: CustomModelProviderApi::OpenAiCompletions,
            models: vec![CustomModelDraft {
                model: "model-a".to_string(),
                display_name: Some("Model A".to_string()),
                context_tokens: "32k".to_string(),
                max_output_tokens: "32k".to_string(),
                api_override: None,
                supports_vision: false,
            }],
        };

        apply_custom_model_providers_request(&mut state, Some(&[request]))
            .expect("invalid runtime values remain persistable settings");
        validate_persisted_model_state(&state).expect("incomplete settings are structurally valid");
        let mut secrets = PersistedRuntimeSecretState::default();
        persist_model_api_key(&mut secrets, "custom.test", "secret".to_string());
        let response = into_response(default_record(&state), &state, &secrets);
        let provider = response
            .model_providers
            .iter()
            .find(|provider| provider.provider_id == "custom.test")
            .expect("custom provider projection");
        assert!(provider.models[0].diagnostic.is_some());
        assert!(response.selectable_models.is_empty());
    }

    #[test]
    fn custom_models_stay_hidden_until_the_provider_has_a_credential() {
        let mut state = PersistedRuntimeConfigState::default();
        apply_custom_model_providers_request(
            &mut state,
            Some(&[CustomModelProviderDraft {
                provider_id: "custom.local".to_string(),
                name: "Local".to_string(),
                base_url: "http://127.0.0.1:8000/v1".to_string(),
                api: CustomModelProviderApi::OpenAiCompletions,
                models: vec![CustomModelDraft {
                    model: "local-model".to_string(),
                    display_name: None,
                    context_tokens: "32k".to_string(),
                    max_output_tokens: "4k".to_string(),
                    api_override: None,
                    supports_vision: false,
                }],
            }]),
        )
        .expect("persist custom model without credential");

        let response = into_response(
            default_record(&state),
            &state,
            &PersistedRuntimeSecretState::default(),
        );
        assert!(response.selectable_models.is_empty());
        let provider = response
            .model_providers
            .iter()
            .find(|provider| provider.provider_id == "custom.local")
            .expect("custom provider projection");
        assert!(!provider.configured);
        assert!(provider.models.is_empty());
        let wire = serde_json::to_value(&response).expect("serialize config response");
        assert!(wire.get("modelProviders").is_some());
        assert!(wire.get("model_providers").is_none());
        assert!(wire["modelProviders"][0].get("credentialSource").is_some());
    }

    #[test]
    fn batch_model_api_keys_field_is_rejected() {
        let error = serde_json::from_value::<AgentRuntimeConfigSetRequest>(serde_json::json!({
            "modelApiKeys": [{ "providerId": "deepseek.default", "modelApiKey": "secret" }]
        }))
        .expect_err("removed batch credential field must fail");

        assert!(error.to_string().contains("modelApiKeys"));
    }

    #[test]
    fn atomic_file_write_replaces_existing_content() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-runtime-config-atomic-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        let path = root.join("config.toml");

        write_file_atomically(path.as_path(), b"old", "test config").expect("first write");
        write_file_atomically(path.as_path(), b"new", "test config").expect("replace write");
        assert_eq!(fs::read(path.as_path()).expect("read result"), b"new");

        fs::remove_file(path).expect("remove test file");
        fs::remove_dir(root).expect("remove test directory");
    }

    #[test]
    fn runtime_config_tool_parallelism_uses_core_hard_cap() {
        let state = PersistedRuntimeConfigState {
            default_tool_parallelism: Some(usize::MAX),
            ..PersistedRuntimeConfigState::default()
        };
        let record = normalize_record(default_record(&state), &state)
            .expect("normalize tool parallelism config");

        assert_eq!(
            record.tool_parallelism,
            Some(centaeris_core::runtime::MAX_TOOL_PARALLELISM)
        );
    }

    #[test]
    fn removed_runtime_access_mode_loud_fails_for_requests_and_persisted_records() {
        let request_error =
            serde_json::from_value::<AgentRuntimeConfigSetRequest>(serde_json::json!({
                "runtimeAccessMode": "banana"
            }))
            .expect_err("removed request field must fail");
        assert!(request_error.to_string().contains("runtimeAccessMode"));

        let record = default_record(&PersistedRuntimeConfigState::default());
        let mut persisted = serde_json::to_value(record).expect("serialize runtime config record");
        persisted
            .as_object_mut()
            .expect("runtime config record object")
            .insert("runtimeAccessMode".to_string(), serde_json::json!("banana"));
        let persisted_error = serde_json::from_value::<AgentRuntimeConfigRecord>(persisted)
            .expect_err("removed persisted field must fail");
        assert!(persisted_error.to_string().contains("runtimeAccessMode"));
    }

    #[test]
    fn removed_host_policy_fields_are_rejected_as_unknown() {
        for field in [
            "sandboxBackend",
            "sandboxWslDistro",
            "sandboxAllowedWriteDirectories",
            "sandboxDeniedReadPaths",
            "sandboxNetworkPolicy",
        ] {
            let request_error = serde_json::from_value::<AgentRuntimeConfigSetRequest>(
                serde_json::json!({ (field): "banana" }),
            )
            .expect_err("removed execution-host field must fail");
            assert!(request_error.to_string().contains(field));
        }

        let mut persisted =
            serde_json::to_value(default_record(&PersistedRuntimeConfigState::default()))
                .expect("serialize runtime config record");
        persisted
            .as_object_mut()
            .expect("runtime config record object")
            .insert("banana".to_string(), serde_json::json!(["example.com"]));
        let persisted_error = serde_json::from_value::<AgentRuntimeConfigRecord>(persisted)
            .expect_err("unknown persisted network field must fail");
        assert!(persisted_error.to_string().contains("banana"));
    }

    #[test]
    fn removed_http_provider_fields_are_rejected_as_unknown() {
        for field in [
            "httpProviderConfigs",
            "providerPollingHostEnabled",
            "providerPollingTickMs",
            "providerPollingLeaseMs",
            "providerPollingClaimLimit",
            "providerPollingMaxJobsPerTick",
        ] {
            let error = serde_json::from_value::<AgentRuntimeConfigSetRequest>(
                serde_json::json!({ (field): null }),
            )
            .expect_err("removed HTTP provider field must fail");
            assert!(error.to_string().contains(field));
        }
    }

    #[test]
    fn apply_request_rejects_removed_agent_policy_field() {
        let state = PersistedRuntimeConfigState::default();
        let mut record = normalize_record(default_record(&state), &state)
            .expect("normalize default policy record");
        let request = AgentRuntimeConfigSetRequest {
            bash_path: None,
            auto_continue_after_resume_wait: None,
            agent_transport_mode: None,
            model_provider_id: None,
            model: None,
            model_thinking_mode: None,
            model_api_key: None,
            clear_model_api_key: None,
            custom_model_providers: None,
            tool_parallelism: None,
            removed_agent_policy_mode: Some(serde_json::json!("ralphExperimental")),
        };

        let err = apply_request(&mut record, request).expect_err("removed policy must fail");
        assert!(err.contains("agentPolicyMode has been removed"));
    }

    #[test]
    fn provider_scoped_secret_does_not_reuse_custom_key_for_deepseek() {
        let mut secrets = PersistedRuntimeSecretState::default();
        persist_model_api_key(&mut secrets, "custom.test", "custom-key".to_string());

        assert_eq!(
            secret_model_api_key_for(&secrets, "custom.test").as_deref(),
            Some("custom-key")
        );
        assert_eq!(secret_model_api_key_for(&secrets, "deepseek.default"), None);
    }

    #[test]
    fn provider_scoped_secret_overwrites_only_selected_provider() {
        let mut secrets = PersistedRuntimeSecretState::default();
        persist_model_api_key(&mut secrets, "custom.test", "custom-key".to_string());
        persist_model_api_key(&mut secrets, "deepseek.default", "deepseek-key".to_string());
        persist_model_api_key(
            &mut secrets,
            "deepseek.default",
            "deepseek-key-2".to_string(),
        );

        assert_eq!(
            secret_model_api_key_for(&secrets, "custom.test").as_deref(),
            Some("custom-key")
        );
        assert_eq!(
            secret_model_api_key_for(&secrets, "deepseek.default").as_deref(),
            Some("deepseek-key-2")
        );
    }

    #[test]
    fn clear_model_api_key_removes_only_selected_provider() {
        let mut secrets = PersistedRuntimeSecretState::default();
        persist_model_api_key(&mut secrets, "custom.test", "custom-key".to_string());
        persist_model_api_key(&mut secrets, "deepseek.default", "deepseek-key".to_string());

        clear_model_api_key(&mut secrets, "deepseek.default");

        assert_eq!(
            secret_model_api_key_for(&secrets, "custom.test").as_deref(),
            Some("custom-key")
        );
        assert_eq!(secret_model_api_key_for(&secrets, "deepseek.default"), None);
    }

    #[test]
    fn built_in_model_selection_uses_core_profile_without_persisting_duplicates() {
        let mut state = PersistedRuntimeConfigState::default();
        let mut secrets = PersistedRuntimeSecretState::default();
        persist_model_api_key(&mut secrets, DEEPSEEK_PROVIDER_ID, "secret".to_string());
        let request = AgentRuntimeConfigSetRequest {
            model_provider_id: Some(DEEPSEEK_PROVIDER_ID.to_string()),
            model: Some("deepseek-v4-pro".to_string()),
            model_thinking_mode: Some("default".to_string()),
            ..AgentRuntimeConfigSetRequest::default()
        };
        apply_model_request(&mut state, &secrets, &request).expect("select built-in model");
        apply_model_thinking_mode_request(&mut state, &request)
            .expect("normalize default thinking");

        assert!(state.models.is_empty());
        assert_eq!(
            active_model_record(&state).map(|item| item.model),
            Some("deepseek-v4-pro".to_string())
        );
        let record = default_record(&state);
        assert_eq!(record.model_context_tokens, Some(1_000_000));
        assert_eq!(record.model_max_output_tokens, Some(384_000));
        assert_eq!(record.model_thinking_mode.as_deref(), Some("high"));

        let invalid = AgentRuntimeConfigSetRequest {
            model_thinking_mode: Some("xhigh".to_string()),
            ..AgentRuntimeConfigSetRequest::default()
        };
        assert!(apply_model_thinking_mode_request(&mut state, &invalid)
            .expect_err("DeepSeek must reject xhigh")
            .contains("expected high, max"));
        apply_model_thinking_mode_request(
            &mut state,
            &AgentRuntimeConfigSetRequest {
                model_thinking_mode: Some("max".to_string()),
                ..AgentRuntimeConfigSetRequest::default()
            },
        )
        .expect("DeepSeek max thinking");
        assert_eq!(
            default_record(&state).model_thinking_mode.as_deref(),
            Some("max")
        );
    }

    #[test]
    fn catalog_model_route_overrides_are_projected_without_persisted_copies() {
        let item = model_catalog(&PersistedRuntimeConfigState::default())
            .into_iter()
            .find(|item| item.provider_id == "opencode-go.default" && item.model == "minimax-m3")
            .expect("OpenCode MiniMax catalog item");

        assert_eq!(item.model_api, Some(ModelWireApi::AnthropicMessages));
        assert_eq!(
            item.model_api_base.as_deref(),
            Some("https://opencode.ai/zen/go")
        );
    }

    #[test]
    fn builtin_models_require_a_credential_and_disappear_when_it_is_cleared() {
        let mut state = PersistedRuntimeConfigState::default();
        let mut secrets = PersistedRuntimeSecretState::default();
        let request = AgentRuntimeConfigSetRequest {
            model_provider_id: Some(DEEPSEEK_PROVIDER_ID.to_string()),
            model: Some("deepseek-v4-pro".to_string()),
            ..AgentRuntimeConfigSetRequest::default()
        };
        assert!(apply_model_request(&mut state, &secrets, &request).is_err());

        persist_model_api_key(&mut secrets, DEEPSEEK_PROVIDER_ID, "secret".to_string());
        apply_model_request(&mut state, &secrets, &request).expect("select available model");
        clear_model_provider_credential(&mut state, &mut secrets, DEEPSEEK_PROVIDER_ID);

        assert!(state.active_model.is_none());
        let response = into_response(default_record(&state), &state, &secrets);
        assert!(response.model_provider_id.is_none());
        assert!(response.model.is_none());
        assert!(response.selectable_models.is_empty());
    }

    #[test]
    fn persisted_active_model_rejects_unknown_thinking_mode() {
        let mut state = PersistedRuntimeConfigState::default();
        let mut secrets = PersistedRuntimeSecretState::default();
        persist_model_api_key(&mut secrets, DEEPSEEK_PROVIDER_ID, "secret".to_string());
        apply_model_request(
            &mut state,
            &secrets,
            &AgentRuntimeConfigSetRequest {
                model_provider_id: Some(DEEPSEEK_PROVIDER_ID.to_string()),
                model: Some("deepseek-v4-pro".to_string()),
                ..AgentRuntimeConfigSetRequest::default()
            },
        )
        .expect("select model");
        state
            .active_model
            .as_mut()
            .expect("active model")
            .model_thinking_mode = Some("banana".to_string());

        assert!(validate_persisted_model_state(&state).is_err());
    }

    #[test]
    fn credential_only_request_does_not_select_or_create_a_model() {
        let mut state = PersistedRuntimeConfigState::default();
        let request = AgentRuntimeConfigSetRequest {
            model_provider_id: Some(DEEPSEEK_PROVIDER_ID.to_string()),
            model_api_key: Some("secret".to_string()),
            ..AgentRuntimeConfigSetRequest::default()
        };

        apply_model_request(
            &mut state,
            &PersistedRuntimeSecretState::default(),
            &request,
        )
        .expect("credential-only request");

        assert!(state.active_model.is_none());
        assert!(state.models.is_empty());
    }

    #[test]
    fn persisted_builtin_duplicate_loud_fails() {
        let mut state = PersistedRuntimeConfigState::default();
        let duplicate = resolve_model_record(&state, DEEPSEEK_PROVIDER_ID, "deepseek-v4-flash")
            .expect("built-in profile");
        state.models.push(duplicate);
        state.active_model = Some(ActiveModelRef {
            provider_id: DEEPSEEK_PROVIDER_ID.to_string(),
            model: "deepseek-v4-flash".to_string(),
            model_thinking_mode: Some("high".to_string()),
        });

        let error = validate_persisted_model_state(&state)
            .expect_err("persisted built-in duplicate must fail");
        assert!(error.contains("custom providerId") || error.contains("active model"));
    }

    #[test]
    fn custom_providers_are_independent_and_models_inherit_or_override_api() {
        let mut state = PersistedRuntimeConfigState::default();
        apply_custom_model_providers_request(
            &mut state,
            Some(&[CustomModelProviderDraft {
                provider_id: "custom.gateway".to_string(),
                name: "Private Gateway".to_string(),
                base_url: "https://gateway.example.com/v1".to_string(),
                api: CustomModelProviderApi::OpenAiCompletions,
                models: Vec::new(),
            }]),
        )
        .expect("add provider before models");
        assert!(state.models.is_empty());
        validate_persisted_model_state(&state).expect("provider-only state");

        let request = vec![CustomModelProviderDraft {
            provider_id: "custom.gateway".to_string(),
            name: "Private Gateway".to_string(),
            base_url: "https://gateway.example.com/v1/".to_string(),
            api: CustomModelProviderApi::OpenAiCompletions,
            models: vec![
                CustomModelDraft {
                    model: "z-model".to_string(),
                    display_name: Some("Zulu".to_string()),
                    context_tokens: "125k".to_string(),
                    max_output_tokens: "31.25k".to_string(),
                    api_override: Some(CustomModelProviderApi::AnthropicMessages),
                    supports_vision: true,
                },
                CustomModelDraft {
                    model: "a-model".to_string(),
                    display_name: Some("Alpha".to_string()),
                    context_tokens: "62.5k".to_string(),
                    max_output_tokens: "15.625k".to_string(),
                    api_override: None,
                    supports_vision: false,
                },
            ],
        }];

        apply_custom_model_providers_request(&mut state, Some(&request))
            .expect("configure custom provider");

        assert_eq!(
            state
                .models
                .iter()
                .map(|item| (
                    item.provider_id.as_str(),
                    item.model.as_str(),
                    item.model_api
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    "custom.gateway",
                    "z-model",
                    Some(CustomModelProviderApi::AnthropicMessages)
                ),
                (
                    "custom.gateway",
                    "a-model",
                    Some(CustomModelProviderApi::OpenAiCompletions)
                ),
            ]
        );
        assert_eq!(
            state
                .custom_model_providers
                .first()
                .map(|provider| provider.base_url.as_str()),
            Some("https://gateway.example.com/v1/")
        );
        validate_persisted_model_state(&state).expect("custom provider state");
    }

    #[test]
    fn removed_per_model_token_override_fields_and_unknown_provider_loud_fail() {
        for field in ["modelApiBase", "modelContextTokens"] {
            let error = serde_json::from_value::<AgentRuntimeConfigSetRequest>(
                serde_json::json!({ (field): "banana" }),
            )
            .expect_err("removed model override must fail");
            assert!(error.to_string().contains(field));
        }
        assert!(validate_settings_model_provider_id(
            &PersistedRuntimeConfigState::default(),
            "banana"
        )
        .expect_err("unknown settings provider must fail")
        .contains("unsupported settings modelProviderId"));
    }

    #[test]
    fn removed_added_builtin_field_loud_fails_and_custom_model_name_is_optional() {
        let mut state = PersistedRuntimeConfigState::default();
        let error = serde_json::from_value::<AgentRuntimeConfigSetRequest>(serde_json::json!({
            "addedBuiltinModelProviderIds": ["deepseek.default"]
        }))
        .expect_err("removed added-provider field must fail");
        assert!(error.to_string().contains("addedBuiltinModelProviderIds"));

        apply_custom_model_providers_request(
            &mut state,
            Some(&[CustomModelProviderDraft {
                provider_id: "custom.test".to_string(),
                name: "Custom".to_string(),
                base_url: "https://api.example.com/v1".to_string(),
                api: CustomModelProviderApi::OpenAiCompletions,
                models: vec![CustomModelDraft {
                    model: "model-id".to_string(),
                    display_name: None,
                    context_tokens: "125k".to_string(),
                    max_output_tokens: "31.25k".to_string(),
                    api_override: None,
                    supports_vision: false,
                }],
            }]),
        )
        .expect("optional custom model name");
        assert_eq!(state.models[0].display_name, None);
        validate_persisted_model_state(&state).expect("optional name remains valid");
    }

    #[test]
    fn persisted_runtime_secrets_reject_empty_noncanonical_duplicate_and_banana() {
        let root = unique_temp_dir("strict-secrets");
        let file_path = root.join("runtime-secrets.json");
        let config = PersistedRuntimeConfigState::default();
        let cases = [
            serde_json::json!({"modelApiKeys":[{"providerId":"deepseek.default","modelApiKey":"","updatedAt":1}]}),
            serde_json::json!({"modelApiKeys":[{"providerId":" deepseek.default","modelApiKey":"secret","updatedAt":1}]}),
            serde_json::json!({"modelApiKeys":[
                {"providerId":"deepseek.default","modelApiKey":"one","updatedAt":1},
                {"providerId":"deepseek.default","modelApiKey":"two","updatedAt":2}
            ]}),
            serde_json::json!({"modelApiKeys":[{"providerId":"deepseek.default","modelApiKey":"secret","updatedAt":1,"banana":true}]}),
            serde_json::json!({"modelApiKeys":[{"providerId":"deepseek.default","modelApiKey":"secret"}]}),
        ];
        for value in cases {
            fs::write(
                file_path.as_path(),
                serde_json::to_vec(&value).expect("encode invalid secrets"),
            )
            .expect("write invalid secrets");
            let error = load_secret_state_from_path(file_path.as_path(), &config)
                .expect_err("invalid secret state rejected");
            assert!(error.starts_with(RUNTIME_SECRET_UNSUPPORTED));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_config_reset_deletes_keys_and_only_quarantines_config() {
        let root = unique_temp_dir("reset");
        let config_path = root.join("config.toml");
        let secret_path = root.join("runtime-secrets.json");
        let unrelated_path = root.join("keep.txt");
        fs::write(config_path.as_path(), "{unsupported").expect("write unsupported config");
        fs::write(unrelated_path.as_path(), "keep").expect("write unrelated file");
        let config = PersistedRuntimeConfigState::default();
        let mut secrets = PersistedRuntimeSecretState::default();
        persist_model_api_key(
            &mut secrets,
            DEEPSEEK_PROVIDER_ID,
            "managed-secret".to_string(),
        );
        persist_secret_state_at(secret_path.as_path(), &secrets, &config)
            .expect("write secret fixture");

        let response = reset_unlocked(config_path.as_path(), secret_path.as_path())
            .expect("reset unsupported config");
        let quarantined_path = response.quarantined_path.expect("quarantined config path");
        assert_eq!(
            fs::read_to_string(quarantined_path).expect("read quarantined config"),
            "{unsupported"
        );
        assert!(user_config::load_at(config_path.as_path())
            .expect("canonical empty config")
            .runtime
            .models
            .is_empty());
        assert!(load_secret_state_from_path(secret_path.as_path(), &config)
            .expect("canonical empty secrets")
            .model_api_keys
            .is_empty());
        assert_eq!(
            fs::read_to_string(unrelated_path).expect("read unrelated"),
            "keep"
        );
        let names = fs::read_dir(root.as_path())
            .expect("list reset directory")
            .map(|entry| {
                entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert!(!names
            .iter()
            .any(|name| name.contains("runtime-secrets") && name != "runtime-secrets.json"));
        assert!(!names.iter().any(|name| name.ends_with(".tmp")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_config_reset_stops_after_secret_clear_when_config_quarantine_fails() {
        let root = unique_temp_dir("reset-partial");
        let config_path = root.join("config.toml");
        let secret_path = root.join("runtime-secrets.json");
        fs::create_dir(config_path.as_path()).expect("create invalid config target");
        let config = PersistedRuntimeConfigState::default();
        let mut secrets = PersistedRuntimeSecretState::default();
        persist_model_api_key(
            &mut secrets,
            DEEPSEEK_PROVIDER_ID,
            "managed-secret".to_string(),
        );
        persist_secret_state_at(secret_path.as_path(), &secrets, &config)
            .expect("write secret fixture");

        let error = reset_unlocked(config_path.as_path(), secret_path.as_path())
            .expect_err("invalid quarantine target must fail");
        assert!(error.starts_with(RUNTIME_CONFIG_IO));
        assert!(load_secret_state_from_path(secret_path.as_path(), &config)
            .expect("secrets cleared before quarantine")
            .model_api_keys
            .is_empty());
        assert!(config_path.is_dir());
        let _ = fs::remove_dir_all(root);
    }
}
