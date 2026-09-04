use crate::{agent_runtime, message_log, runtime_config};
use centaeris_core::model::provider_polling::{
    parse_provider_poll_payload_ref, PROVIDER_POLL_RUNTIME_JOB_KIND,
};
use centaeris_core::model::DEFAULT_MODEL_CONTEXT_TOKENS;
use centaeris_core::runtime::contracts::{
    current_timestamp_ms, CheckpointRecord, ProviderTokenUsageV1, RuntimeAwaitJobCheckpointV1,
    RuntimeAwaitQuestionCheckpointV1,
};
use centaeris_core::session::reliability::{
    DeadLetterRecord, DeadLetterStatus, DismissDeadLetterRequest, ListDeadLettersRequest,
    ListRuntimeJobsRequest, ReplayDeadLetterRequest, ReplayDeadLetterResult, RuntimeBackoffPolicy,
    RuntimeJobRecord, RuntimeJobStatus,
};
use centaeris_core::session::store::{RuntimeStore, RuntimeStoreActor};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentStateGetRequest {
    pub(crate) session_id: String,
    pub(crate) include_runtime_state: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentContextUsageGetRequest {
    pub(crate) session_id: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct PendingQuestionSummaryResponse {
    pub(crate) question_id: String,
    pub(crate) created_at: i64,
    pub(crate) turn_id: String,
    pub(crate) question_request: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentCheckpointSummaryResponse {
    pub(crate) turn_id: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) done_reason: Option<String>,
    pub(crate) updated_at: Option<i64>,
    pub(crate) error: Option<String>,
    pub(crate) message_count: Option<u32>,
    pub(crate) loop_count: Option<u32>,
    pub(crate) web_search_count: Option<u32>,
    pub(crate) state: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentStateSummaryResponse {
    pub(crate) session_id: String,
    pub(crate) pending_question_count: usize,
    pub(crate) pending_questions: Vec<PendingQuestionSummaryResponse>,
    pub(crate) checkpoint: Option<AgentCheckpointSummaryResponse>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentContextUsageResponse {
    pub(crate) session_id: String,
    pub(crate) used_tokens: Option<u64>,
    pub(crate) max_context_tokens: Option<u32>,
    pub(crate) used_percentage: Option<u32>,
    pub(crate) updated_at: Option<i64>,
    pub(crate) is_compacting: bool,
    pub(crate) latest_usage: AgentTokenUsageSummary,
    pub(crate) breakdown: Option<AgentContextTokenBreakdownResponse>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentContextTokenBreakdownResponse {
    pub(crate) system_prompt_tokens: u32,
    pub(crate) system_tool_tokens: u32,
    pub(crate) mcp_tool_tokens: u32,
    pub(crate) skills_tokens: u32,
    pub(crate) message_tokens: u32,
    pub(crate) auto_compact_buffer_tokens: u32,
    pub(crate) free_space_tokens: u64,
    pub(crate) mcp_tools: Vec<centaeris_core::runtime::ContextToolTokenEstimateV1>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentTokenUsageSummary {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
    pub(crate) prompt_cache_hit_tokens: Option<u64>,
    pub(crate) prompt_cache_miss_tokens: Option<u64>,
    pub(crate) prompt_cache_total_tokens: Option<u64>,
    pub(crate) prompt_cache_hit_rate: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRuntimeJobListRequest {
    pub(crate) statuses: Option<Vec<String>>,
    pub(crate) job_kind: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) branch_id: Option<String>,
    pub(crate) provider_id: Option<String>,
    pub(crate) provider_tool_name: Option<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) offset: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRuntimeJobListResponse {
    pub(crate) jobs: Vec<RuntimeJobRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRuntimeJobGetRequest {
    pub(crate) job_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRuntimeJobGetResponse {
    pub(crate) job: Option<RuntimeJobRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentDeadLetterListRequest {
    pub(crate) statuses: Option<Vec<String>>,
    pub(crate) job_kind: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) branch_id: Option<String>,
    pub(crate) provider_id: Option<String>,
    pub(crate) provider_tool_name: Option<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) offset: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentDeadLetterListResponse {
    pub(crate) dead_letters: Vec<DeadLetterRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentDeadLetterGetRequest {
    pub(crate) dead_letter_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentDeadLetterGetResponse {
    pub(crate) dead_letter: Option<DeadLetterRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentDeadLetterDismissRequest {
    pub(crate) dead_letter_id: String,
    pub(crate) dismissed_by: Option<String>,
    pub(crate) dismissed_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentDeadLetterDismissResponse {
    pub(crate) ok: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentDeadLetterReplayRequest {
    pub(crate) dead_letter_id: String,
    pub(crate) replay_key: Option<String>,
    pub(crate) job_id: Option<String>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) run_at_ms: Option<i64>,
    pub(crate) max_retries: Option<u32>,
}

pub(crate) fn agent_state_get(
    request: AgentStateGetRequest,
) -> Result<AgentStateSummaryResponse, String> {
    let session_id = required_string(request.session_id.as_str(), "sessionId")?;
    build_agent_state_summary(
        session_id.as_str(),
        request.include_runtime_state.unwrap_or(true),
    )
}

pub(crate) fn agent_context_usage_get(
    request: AgentContextUsageGetRequest,
) -> Result<AgentContextUsageResponse, String> {
    let session_id = required_string(request.session_id.as_str(), "sessionId")?;
    let config = runtime_config::get(runtime_config::AgentRuntimeConfigGetRequest {})?;
    build_agent_context_usage(session_id.as_str(), config.model_context_tokens)
}

pub(crate) async fn runtime_job_list(
    request: AgentRuntimeJobListRequest,
) -> Result<AgentRuntimeJobListResponse, String> {
    let store = agent_runtime::agent_runtime_store_actor()?;
    let statuses = parse_runtime_job_statuses(request.statuses)?;
    let session_id = normalize_optional_string(request.session_id.as_deref());
    let branch_id = normalize_optional_string(request.branch_id.as_deref());
    let provider_id = normalize_optional_string(request.provider_id.as_deref());
    let provider_tool_name = normalize_optional_string(request.provider_tool_name.as_deref());
    let has_provider_filter =
        provider_poll_filters_present(provider_id.as_deref(), provider_tool_name.as_deref());
    let job_kind =
        provider_poll_job_kind_for_list(request.job_kind.as_deref(), has_provider_filter);
    let limit = normalize_list_limit(request.limit, 50, 500);
    let offset = normalize_list_offset(request.offset);
    let list_request = ListRuntimeJobsRequest {
        statuses,
        job_kind,
        session_id,
        branch_id,
        limit,
        offset,
    };
    let jobs = if has_provider_filter {
        filter_runtime_jobs_by_provider_poll_payload(
            &store,
            list_request,
            provider_id,
            provider_tool_name,
        )
        .await?
    } else {
        store.list_runtime_jobs(list_request).await?
    };
    Ok(AgentRuntimeJobListResponse { jobs })
}

pub(crate) async fn runtime_job_get(
    request: AgentRuntimeJobGetRequest,
) -> Result<AgentRuntimeJobGetResponse, String> {
    let job_id = required_string(request.job_id.as_str(), "jobId")?;
    let store = agent_runtime::agent_runtime_store_actor()?;
    Ok(AgentRuntimeJobGetResponse {
        job: store.get_runtime_job(job_id.as_str()).await?,
    })
}

pub(crate) async fn dead_letter_list(
    request: AgentDeadLetterListRequest,
) -> Result<AgentDeadLetterListResponse, String> {
    let store = agent_runtime::agent_runtime_store_actor()?;
    let statuses = parse_dead_letter_statuses(request.statuses)?;
    let session_id = normalize_optional_string(request.session_id.as_deref());
    let branch_id = normalize_optional_string(request.branch_id.as_deref());
    let provider_id = normalize_optional_string(request.provider_id.as_deref());
    let provider_tool_name = normalize_optional_string(request.provider_tool_name.as_deref());
    let has_provider_filter =
        provider_poll_filters_present(provider_id.as_deref(), provider_tool_name.as_deref());
    let job_kind =
        provider_poll_job_kind_for_list(request.job_kind.as_deref(), has_provider_filter);
    let limit = normalize_list_limit(request.limit, 50, 500);
    let offset = normalize_list_offset(request.offset);
    let list_request = ListDeadLettersRequest {
        statuses,
        job_kind,
        session_id,
        branch_id,
        limit,
        offset,
    };
    let dead_letters = if has_provider_filter {
        filter_dead_letters_by_provider_poll_payload(
            &store,
            list_request,
            provider_id,
            provider_tool_name,
        )
        .await?
    } else {
        store.list_dead_letters(list_request).await?
    };
    Ok(AgentDeadLetterListResponse { dead_letters })
}

pub(crate) async fn dead_letter_get(
    request: AgentDeadLetterGetRequest,
) -> Result<AgentDeadLetterGetResponse, String> {
    let dead_letter_id = required_string(request.dead_letter_id.as_str(), "deadLetterId")?;
    let store = agent_runtime::agent_runtime_store_actor()?;
    Ok(AgentDeadLetterGetResponse {
        dead_letter: store.get_dead_letter(dead_letter_id.as_str()).await?,
    })
}

pub(crate) async fn dead_letter_dismiss(
    request: AgentDeadLetterDismissRequest,
) -> Result<AgentDeadLetterDismissResponse, String> {
    let dead_letter_id = required_string(request.dead_letter_id.as_str(), "deadLetterId")?;
    let dismissed_by = normalize_optional_string(request.dismissed_by.as_deref())
        .unwrap_or_else(|| String::from("electron-sidecar"));
    let dismissed_reason = normalize_optional_string(request.dismissed_reason.as_deref())
        .unwrap_or_else(|| String::from("dismissed from electron runtime ops"));
    let store = agent_runtime::agent_runtime_store_actor()?;
    store
        .dismiss_dead_letter(DismissDeadLetterRequest {
            dead_letter_id,
            dismissed_by,
            dismissed_reason,
            updated_at_ms: current_timestamp_ms(),
        })
        .await?;
    Ok(AgentDeadLetterDismissResponse { ok: true })
}

pub(crate) async fn dead_letter_replay(
    request: AgentDeadLetterReplayRequest,
) -> Result<ReplayDeadLetterResult, String> {
    let dead_letter_id = required_string(request.dead_letter_id.as_str(), "deadLetterId")?;
    let store = agent_runtime::agent_runtime_store_actor()?;
    let dead_letter = store
        .get_dead_letter(dead_letter_id.as_str())
        .await?
        .ok_or_else(|| format!("deadLetterId not found: {dead_letter_id}"))?;
    let now_ms = current_timestamp_ms();
    let replay_job = build_dead_letter_replay_runtime_job(&dead_letter, &request, now_ms);
    store
        .replay_dead_letter(ReplayDeadLetterRequest {
            dead_letter_id,
            replay_job,
            replayed_at_ms: now_ms,
        })
        .await
}

fn build_agent_state_summary(
    session_id: &str,
    include_runtime_state: bool,
) -> Result<AgentStateSummaryResponse, String> {
    let store = agent_runtime::agent_runtime_store_actor()?;
    let latest_checkpoint = RuntimeStore::load_latest_checkpoint(&store, session_id)
        .map_err(|error| error.to_string())?;
    let parsed_checkpoint = latest_checkpoint
        .as_ref()
        .map(parse_wait_checkpoint)
        .transpose()?;
    let checkpoint_summary = if include_runtime_state {
        latest_checkpoint
            .zip(parsed_checkpoint)
            .map(|(checkpoint, (state, question_id))| {
                let pending_question_summary =
                    question_id.map(|question_id| PendingQuestionSummaryResponse {
                        question_request: serde_json::json!({ "id": question_id }),
                        question_id,
                        created_at: checkpoint.updated_at_ms,
                        turn_id: checkpoint.turn_id.clone(),
                    });
                let pending_questions = pending_question_summary.into_iter().collect::<Vec<_>>();
                (
                    AgentCheckpointSummaryResponse {
                        turn_id: Some(checkpoint.turn_id.clone()),
                        status: Some(checkpoint.status.clone()),
                        done_reason: checkpoint.done_reason.clone(),
                        updated_at: Some(checkpoint.updated_at_ms),
                        error: None,
                        message_count: None,
                        loop_count: None,
                        web_search_count: None,
                        state: Some(state),
                    },
                    pending_questions,
                )
            })
    } else {
        None
    };
    let (checkpoint, pending_questions) =
        if let Some((checkpoint, pending_questions)) = checkpoint_summary {
            (Some(checkpoint), pending_questions)
        } else {
            (None, vec![])
        };

    Ok(AgentStateSummaryResponse {
        session_id: session_id.to_string(),
        pending_question_count: pending_questions.len(),
        pending_questions,
        checkpoint,
    })
}

fn build_agent_context_usage(
    session_id: &str,
    configured_max_context_tokens: Option<u32>,
) -> Result<AgentContextUsageResponse, String> {
    let max_context_tokens = configured_max_context_tokens.or(Some(DEFAULT_MODEL_CONTEXT_TOKENS));
    let state = message_log::project_agent_context_state(session_id)?;
    let usage = state.provider_usage;
    let latest_usage = usage
        .as_ref()
        .filter(|usage| usage.latest.has_values())
        .map(|usage| agent_token_usage_from_provider(&usage.latest))
        .transpose()?
        .unwrap_or_default();
    let configured_auto_compact_buffer_tokens =
        centaeris_core::model::PROMPT_COMPACTION_TRIGGER_HEADROOM_TOKENS;
    let context_content_tokens = state.context_token_estimate;
    let auto_compact_buffer_tokens = match (context_content_tokens, max_context_tokens) {
        (Some(tokens), Some(max_tokens)) => u64::from(max_tokens)
            .saturating_sub(tokens)
            .min(u64::from(configured_auto_compact_buffer_tokens))
            as u32,
        _ => configured_auto_compact_buffer_tokens,
    };
    let used_tokens = context_content_tokens.map(|tokens| {
        tokens
            .saturating_add(u64::from(auto_compact_buffer_tokens))
            .min(u64::from(max_context_tokens.unwrap_or(u32::MAX)))
    });
    let used_percentage = match (used_tokens, max_context_tokens) {
        (Some(tokens), Some(max_tokens)) if max_tokens > 0 => {
            let percentage = ((tokens as f64 / f64::from(max_tokens)) * 100.0).round();
            Some(percentage.clamp(0.0, 100.0) as u32)
        }
        _ => None,
    };
    Ok(AgentContextUsageResponse {
        session_id: session_id.to_string(),
        used_tokens,
        max_context_tokens,
        used_percentage,
        updated_at: state.context_token_estimate_updated_at_ms,
        is_compacting: state.is_compacting,
        latest_usage,
        breakdown: state.context_token_breakdown.map(|breakdown| {
            AgentContextTokenBreakdownResponse {
                system_prompt_tokens: breakdown.system_prompt_tokens,
                system_tool_tokens: breakdown.system_tool_tokens,
                mcp_tool_tokens: breakdown.mcp_tool_tokens,
                skills_tokens: breakdown.skills_tokens,
                message_tokens: breakdown.message_tokens,
                auto_compact_buffer_tokens,
                free_space_tokens: max_context_tokens
                    .map(u64::from)
                    .unwrap_or_default()
                    .saturating_sub(used_tokens.unwrap_or_default()),
                mcp_tools: breakdown.mcp_tools,
            }
        }),
    })
}

fn agent_token_usage_from_provider(
    usage: &ProviderTokenUsageV1,
) -> Result<AgentTokenUsageSummary, String> {
    let prompt_cache_total_tokens = match (
        usage.prompt_cache_hit_tokens,
        usage.prompt_cache_miss_tokens,
    ) {
        (Some(hit), Some(miss)) => Some(
            hit.checked_add(miss)
                .ok_or_else(|| "provider_usage_prompt_cache_total_overflow".to_string())?,
        ),
        _ => None,
    };
    Ok(AgentTokenUsageSummary {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        prompt_cache_hit_tokens: usage.prompt_cache_hit_tokens,
        prompt_cache_miss_tokens: usage.prompt_cache_miss_tokens,
        prompt_cache_total_tokens,
        prompt_cache_hit_rate: match (usage.prompt_cache_hit_tokens, prompt_cache_total_tokens) {
            (Some(hit), Some(total)) if total > 0 => Some(hit as f64 / total as f64),
            _ => None,
        },
    })
}

fn parse_wait_checkpoint(
    checkpoint: &CheckpointRecord,
) -> Result<(serde_json::Value, Option<String>), String> {
    let (value, turn_id, question_id) = match checkpoint.done_reason.as_deref() {
        Some("question") if checkpoint.status == "paused_question" => {
            let wait = serde_json::from_str::<RuntimeAwaitQuestionCheckpointV1>(
                checkpoint.payload_json.as_str(),
            )
            .map_err(|error| format!("parse question wait checkpoint failed: {error}"))?;
            wait.validate()?;
            (
                serde_json::to_value(&wait)
                    .map_err(|error| format!("encode question wait checkpoint failed: {error}"))?,
                wait.turn_id,
                Some(wait.question_id),
            )
        }
        Some("runtime_job") if checkpoint.status == "waiting" => {
            let wait = serde_json::from_str::<RuntimeAwaitJobCheckpointV1>(
                checkpoint.payload_json.as_str(),
            )
            .map_err(|error| format!("parse runtime job wait checkpoint failed: {error}"))?;
            wait.validate()?;
            (
                serde_json::to_value(&wait).map_err(|error| {
                    format!("encode runtime job wait checkpoint failed: {error}")
                })?,
                wait.turn_id,
                None,
            )
        }
        _ => {
            return Err(format!(
                "unsupported runtime checkpoint state: status={} doneReason={:?}",
                checkpoint.status, checkpoint.done_reason
            ))
        }
    };
    if turn_id != checkpoint.turn_id {
        return Err(format!(
            "runtime checkpoint turn identity mismatch: record={} payload={turn_id}",
            checkpoint.turn_id
        ));
    }
    Ok((value, question_id))
}

async fn filter_runtime_jobs_by_provider_poll_payload(
    store: &RuntimeStoreActor,
    request: ListRuntimeJobsRequest,
    provider_id: Option<String>,
    provider_tool_name: Option<String>,
) -> Result<Vec<RuntimeJobRecord>, String> {
    let mut matched = Vec::new();
    let mut scan_offset = 0usize;
    let page_limit = 500usize;
    loop {
        let page = store
            .list_runtime_jobs(ListRuntimeJobsRequest {
                statuses: request.statuses.clone(),
                job_kind: request.job_kind.clone(),
                session_id: request.session_id.clone(),
                branch_id: request.branch_id.clone(),
                limit: page_limit,
                offset: scan_offset,
            })
            .await?;
        if page.is_empty() {
            break;
        }
        let page_len = page.len();
        for job in page {
            if provider_polling_payload_matches(
                job.job_kind.as_str(),
                job.payload_ref.as_deref(),
                provider_id.as_deref(),
                provider_tool_name.as_deref(),
            ) {
                matched.push(job);
            }
        }
        if matched.len() >= request.offset.saturating_add(request.limit) || page_len < page_limit {
            break;
        }
        scan_offset = scan_offset.saturating_add(page_len);
    }
    Ok(matched
        .into_iter()
        .skip(request.offset)
        .take(request.limit)
        .collect())
}

async fn filter_dead_letters_by_provider_poll_payload(
    store: &RuntimeStoreActor,
    request: ListDeadLettersRequest,
    provider_id: Option<String>,
    provider_tool_name: Option<String>,
) -> Result<Vec<DeadLetterRecord>, String> {
    let mut matched = Vec::new();
    let mut scan_offset = 0usize;
    let page_limit = 500usize;
    loop {
        let page = store
            .list_dead_letters(ListDeadLettersRequest {
                statuses: request.statuses.clone(),
                job_kind: request.job_kind.clone(),
                session_id: request.session_id.clone(),
                branch_id: request.branch_id.clone(),
                limit: page_limit,
                offset: scan_offset,
            })
            .await?;
        if page.is_empty() {
            break;
        }
        let page_len = page.len();
        for dead_letter in page {
            if provider_polling_payload_matches(
                dead_letter.job_kind.as_str(),
                dead_letter.payload_ref.as_deref(),
                provider_id.as_deref(),
                provider_tool_name.as_deref(),
            ) {
                matched.push(dead_letter);
            }
        }
        if matched.len() >= request.offset.saturating_add(request.limit) || page_len < page_limit {
            break;
        }
        scan_offset = scan_offset.saturating_add(page_len);
    }
    Ok(matched
        .into_iter()
        .skip(request.offset)
        .take(request.limit)
        .collect())
}

fn build_dead_letter_replay_runtime_job(
    dead_letter: &DeadLetterRecord,
    request: &AgentDeadLetterReplayRequest,
    now_ms: i64,
) -> RuntimeJobRecord {
    let replay_key = normalize_optional_string(request.replay_key.as_deref())
        .unwrap_or_else(|| format!("manual-{now_ms}"));
    let replay_segment = sanitize_runtime_identifier_segment(replay_key.as_str());
    let dead_letter_segment =
        sanitize_runtime_identifier_segment(dead_letter.dead_letter_id.as_str());
    let job_id = normalize_optional_string(request.job_id.as_deref())
        .unwrap_or_else(|| format!("runtime_job_replay:{dead_letter_segment}:{replay_segment}"));
    let idempotency_key = normalize_optional_string(request.idempotency_key.as_deref())
        .unwrap_or_else(|| format!("dlq_replay:{}:{replay_key}", dead_letter.dead_letter_id));
    RuntimeJobRecord {
        job_id,
        job_kind: dead_letter.job_kind.clone(),
        status: RuntimeJobStatus::Queued,
        run_at_ms: request.run_at_ms.unwrap_or(now_ms),
        lease_owner: None,
        lease_expires_at_ms: None,
        heartbeat_at_ms: None,
        retry_count: 0,
        max_retries: request
            .max_retries
            .unwrap_or_else(|| dead_letter.replay_policy.max_replays.max(1)),
        backoff_policy: RuntimeBackoffPolicy::default(),
        idempotency_key,
        session_id: dead_letter.session_id.clone(),
        branch_id: dead_letter.branch_id.clone(),
        checkpoint_id: dead_letter.checkpoint_id.clone(),
        payload_ref: dead_letter.payload_ref.clone(),
        output_refs: vec![],
        last_error: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }
}

fn parse_runtime_job_statuses(
    raw_statuses: Option<Vec<String>>,
) -> Result<Vec<RuntimeJobStatus>, String> {
    let mut statuses = Vec::new();
    for raw in raw_statuses.unwrap_or_default() {
        statuses.push(parse_runtime_job_status(raw.as_str())?);
    }
    Ok(statuses)
}

fn parse_runtime_job_status(raw: &str) -> Result<RuntimeJobStatus, String> {
    match raw {
        "queued" => Ok(RuntimeJobStatus::Queued),
        "leased" => Ok(RuntimeJobStatus::Leased),
        "running" => Ok(RuntimeJobStatus::Running),
        "succeeded" => Ok(RuntimeJobStatus::Succeeded),
        "failed" => Ok(RuntimeJobStatus::Failed),
        "dead_lettered" => Ok(RuntimeJobStatus::DeadLettered),
        "cancelled" => Ok(RuntimeJobStatus::Cancelled),
        other => Err(format!("unsupported runtime job status: {other}")),
    }
}

fn parse_dead_letter_statuses(
    raw_statuses: Option<Vec<String>>,
) -> Result<Vec<DeadLetterStatus>, String> {
    let mut statuses = Vec::new();
    for raw in raw_statuses.unwrap_or_default() {
        let normalized = raw.trim();
        if normalized.is_empty() {
            continue;
        }
        statuses.push(parse_dead_letter_status(normalized)?);
    }
    Ok(statuses)
}

fn parse_dead_letter_status(raw: &str) -> Result<DeadLetterStatus, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "open" => Ok(DeadLetterStatus::Open),
        "replaying" => Ok(DeadLetterStatus::Replaying),
        "replayed" => Ok(DeadLetterStatus::Replayed),
        "dismissed" => Ok(DeadLetterStatus::Dismissed),
        other => Err(format!("unsupported dead letter status: {other}")),
    }
}

fn provider_poll_filters_present(
    provider_id: Option<&str>,
    provider_tool_name: Option<&str>,
) -> bool {
    normalize_optional_string(provider_id).is_some()
        || normalize_optional_string(provider_tool_name).is_some()
}

fn provider_poll_job_kind_for_list(
    job_kind: Option<&str>,
    has_provider_filter: bool,
) -> Option<String> {
    let normalized = normalize_optional_string(job_kind);
    if has_provider_filter {
        Some(
            normalized
                .as_deref()
                .unwrap_or(PROVIDER_POLL_RUNTIME_JOB_KIND)
                .to_string(),
        )
    } else {
        normalized
    }
}

fn provider_polling_payload_matches(
    job_kind: &str,
    payload_ref: Option<&str>,
    provider_id: Option<&str>,
    provider_tool_name: Option<&str>,
) -> bool {
    let normalized_provider_id = normalize_optional_string(provider_id);
    let normalized_provider_tool_name = normalize_optional_string(provider_tool_name);
    if normalized_provider_id.is_none() && normalized_provider_tool_name.is_none() {
        return true;
    }
    if job_kind != PROVIDER_POLL_RUNTIME_JOB_KIND {
        return false;
    }
    let Ok(payload) = parse_provider_poll_payload_ref(payload_ref) else {
        return false;
    };
    if normalized_provider_id
        .as_deref()
        .is_some_and(|expected| payload.provider_id != expected)
    {
        return false;
    }
    if normalized_provider_tool_name
        .as_deref()
        .is_some_and(|expected| payload.tool_name != expected)
    {
        return false;
    }
    true
}

fn required_string(raw: &str, field_name: &str) -> Result<String, String> {
    raw.trim()
        .is_empty()
        .then(|| format!("{field_name} is required"))
        .map_or_else(|| Ok(raw.trim().to_string()), Err)
}

fn normalize_optional_string(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn normalize_list_limit(limit: Option<usize>, default_limit: usize, max_limit: usize) -> usize {
    limit.unwrap_or(default_limit).clamp(1, max_limit)
}

fn normalize_list_offset(offset: Option<usize>) -> usize {
    offset.unwrap_or(0)
}

fn sanitize_runtime_identifier_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_job_status_parser_accepts_only_canonical_values() {
        for value in [
            "queued",
            "leased",
            "running",
            "succeeded",
            "failed",
            "dead_lettered",
            "cancelled",
        ] {
            parse_runtime_job_status(value).expect("canonical runtime job status");
        }
        for value in [
            "success",
            "failure",
            "dead-lettered",
            "deadlettered",
            "canceled",
            "unknown",
            "SUCCEEDED",
            " succeeded ",
            "",
        ] {
            assert!(parse_runtime_job_status(value).is_err(), "accepted {value}");
        }
    }
}
