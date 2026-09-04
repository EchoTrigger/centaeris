use super::*;
#[derive(Debug, Clone)]
pub struct AgentRuntimeConfig {
    pub agent_instructions: String,
    pub max_message_chars: usize,
    pub max_recovery_attempts: u32,
    pub tool_parallelism: usize,
    pub provider_prompt_cache_retention: Option<String>,
    pub enable_prompt_compaction: bool,
    pub model_context_tokens: u32,
    pub model_max_output_tokens: u32,
    pub prompt_token_estimate_scale_basis_points: u32,
    pub prompt_compaction_trigger_headroom_tokens: u32,
    pub prompt_compaction_user_replay_tokens: u32,
    pub prompt_compaction_summary_max_tokens: u32,
    pub enable_system_prompt_template: bool,
    pub enable_tool_use_summary: bool,
    pub auto_continue_after_resume_wait: bool,
    pub allowed_tools: Option<Vec<String>>,
    pub plugin_activation_digest: Option<String>,
    pub resolved_model_binding: Option<crate::extension::composition::ResolvedModelBindingV1>,
}

impl Default for AgentRuntimeConfig {
    fn default() -> Self {
        Self {
            agent_instructions: String::new(),
            max_message_chars: 72_000,
            max_recovery_attempts: 2,
            tool_parallelism: DEFAULT_TOOL_PARALLELISM,
            provider_prompt_cache_retention: Some(
                DEFAULT_PROVIDER_PROMPT_CACHE_RETENTION.to_string(),
            ),
            enable_prompt_compaction: true,
            model_context_tokens: DEFAULT_MODEL_CONTEXT_TOKENS,
            model_max_output_tokens: DEFAULT_MODEL_OUTPUT_TOKENS,
            prompt_token_estimate_scale_basis_points: 10_000,
            prompt_compaction_trigger_headroom_tokens: PROMPT_COMPACTION_TRIGGER_HEADROOM_TOKENS,
            prompt_compaction_user_replay_tokens: PROMPT_COMPACTION_USER_REPLAY_TOKENS,
            prompt_compaction_summary_max_tokens: PROMPT_COMPACTION_MAX_OUTPUT_TOKENS,
            enable_system_prompt_template: true,
            enable_tool_use_summary: true,
            auto_continue_after_resume_wait: false,
            allowed_tools: None,
            plugin_activation_digest: None,
            resolved_model_binding: None,
        }
    }
}
