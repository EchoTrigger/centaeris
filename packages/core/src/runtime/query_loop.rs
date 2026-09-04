use serde::{Deserialize, Serialize};

use crate::model::GenerateResult;
#[cfg(test)]
use crate::model::ToolCallEnvelope;
use crate::runtime::contracts::JsonMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunResourceUsageV1 {
    pub provider_attempts: u32,
    pub completed_provider_rounds: u32,
    pub estimated_input_tokens: u64,
    pub actual_input_tokens: u64,
    pub prompt_cache_hit_tokens: u64,
    pub prompt_cache_miss_tokens: u64,
    pub output_tokens: u64,
    pub tool_call_count: u32,
}

impl AgentRunResourceUsageV1 {
    pub fn record_completed_provider_round(
        &mut self,
        generate_result: &GenerateResult,
        context_token_estimate: u32,
        provider_attempts: u32,
    ) {
        self.record_provider_attempts(context_token_estimate, provider_attempts);
        self.completed_provider_rounds = self.completed_provider_rounds.saturating_add(1);
        let actual_input_tokens = non_negative_u64(generate_result.input_tokens);
        self.actual_input_tokens = self.actual_input_tokens.saturating_add(actual_input_tokens);
        self.prompt_cache_hit_tokens = self
            .prompt_cache_hit_tokens
            .saturating_add(non_negative_u64(generate_result.prompt_cache_hit_tokens));
        self.prompt_cache_miss_tokens = self
            .prompt_cache_miss_tokens
            .saturating_add(non_negative_u64(generate_result.prompt_cache_miss_tokens));
        let total_tokens = non_negative_u64(generate_result.total_tokens);
        self.output_tokens = self
            .output_tokens
            .saturating_add(total_tokens.saturating_sub(actual_input_tokens));
        self.tool_call_count = self
            .tool_call_count
            .saturating_add(u32::try_from(generate_result.tool_calls.len()).unwrap_or(u32::MAX));
    }

    pub fn record_provider_attempts(
        &mut self,
        context_token_estimate: u32,
        provider_attempts: u32,
    ) {
        self.provider_attempts = self.provider_attempts.saturating_add(provider_attempts);
        self.estimated_input_tokens = self.estimated_input_tokens.saturating_add(
            u64::from(context_token_estimate.max(1)).saturating_mul(u64::from(provider_attempts)),
        );
    }

    pub fn merge(&mut self, other: &Self) {
        self.provider_attempts = self
            .provider_attempts
            .saturating_add(other.provider_attempts);
        self.completed_provider_rounds = self
            .completed_provider_rounds
            .saturating_add(other.completed_provider_rounds);
        self.estimated_input_tokens = self
            .estimated_input_tokens
            .saturating_add(other.estimated_input_tokens);
        self.actual_input_tokens = self
            .actual_input_tokens
            .saturating_add(other.actual_input_tokens);
        self.prompt_cache_hit_tokens = self
            .prompt_cache_hit_tokens
            .saturating_add(other.prompt_cache_hit_tokens);
        self.prompt_cache_miss_tokens = self
            .prompt_cache_miss_tokens
            .saturating_add(other.prompt_cache_miss_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.tool_call_count = self.tool_call_count.saturating_add(other.tool_call_count);
    }
}

fn non_negative_u64(value: Option<i64>) -> u64 {
    value
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum LoopStep {
    Generate,
    ExecuteTools,
    Finalize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DoneReason {
    FinalResponse,
    ClarificationNeeded,
    Question,
    TerminalTool,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContinueHop {
    Tools,
    Finalize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ToolsRoute {
    Execute,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopDecision {
    pub next_hop: ContinueHop,
    pub tools_route: Option<ToolsRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentStateSnapshot {
    pub loop_count: u32,
    pub done_reason: Option<DoneReason>,
    pub pending_question_json: Option<String>,
    pub generate_result: Option<GenerateResult>,
    pub tool_reports_json: Vec<String>,
    pub process_events_json: Vec<String>,
    pub transition_json: Option<String>,
    pub agent_run_resource_usage: AgentRunResourceUsageV1,
    pub compression_stats_json: Option<String>,
    pub tool_use_summary: Option<String>,
    pub tool_operations_json: Option<String>,
    pub recovery_policy_trace_json: Vec<String>,
    pub metadata: JsonMap,
}

pub fn should_continue(state: &AgentStateSnapshot) -> ContinueHop {
    if matches!(state.done_reason, Some(DoneReason::Error)) {
        return ContinueHop::Finalize;
    }
    if state
        .generate_result
        .as_ref()
        .map(|result| !result.tool_calls.is_empty())
        .unwrap_or(false)
    {
        return ContinueHop::Tools;
    }

    ContinueHop::Finalize
}

pub fn route_tools(state: &AgentStateSnapshot) -> ToolsRoute {
    let _ = state;
    ToolsRoute::Execute
}

pub fn decide_loop_step(state: &AgentStateSnapshot) -> LoopDecision {
    let next_hop = should_continue(state);
    if !matches!(next_hop, ContinueHop::Tools) {
        return LoopDecision {
            next_hop,
            tools_route: None,
        };
    }

    LoopDecision {
        next_hop,
        tools_route: Some(route_tools(state)),
    }
}

pub fn decide_next_step(_state: &AgentStateSnapshot, has_executable_tools: bool) -> LoopStep {
    if has_executable_tools {
        return LoopStep::ExecuteTools;
    }

    LoopStep::Finalize
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        decide_loop_step, decide_next_step, route_tools, should_continue, AgentRunResourceUsageV1,
        AgentStateSnapshot, ContinueHop, DoneReason, GenerateResult, LoopStep, ToolCallEnvelope,
        ToolsRoute,
    };

    fn empty_state(loop_count: u32) -> AgentStateSnapshot {
        AgentStateSnapshot {
            loop_count,
            done_reason: None,
            pending_question_json: None,
            generate_result: None,
            tool_reports_json: vec![],
            process_events_json: vec![],
            transition_json: None,
            agent_run_resource_usage: AgentRunResourceUsageV1::default(),
            compression_stats_json: None,
            tool_use_summary: None,
            tool_operations_json: None,
            recovery_policy_trace_json: vec![],
            metadata: HashMap::new(),
        }
    }

    fn with_tool_calls(state: &mut AgentStateSnapshot, calls: Vec<ToolCallEnvelope>) {
        state.generate_result = Some(GenerateResult {
            content: String::new(),
            tool_calls: calls,
            reasoning_content: None,
            input_tokens: None,
            total_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
        });
    }

    #[test]
    fn decide_tools_path() {
        let state = empty_state(1);
        assert_eq!(decide_next_step(&state, true), LoopStep::ExecuteTools);
    }

    #[test]
    fn decide_finalize_without_tools() {
        let mut state = empty_state(1);
        state.done_reason = Some(DoneReason::FinalResponse);
        assert_eq!(decide_next_step(&state, false), LoopStep::Finalize);
    }

    #[test]
    fn should_continue_matches_python_semantics() {
        let mut state = empty_state(0);
        assert_eq!(should_continue(&state), ContinueHop::Finalize);

        with_tool_calls(
            &mut state,
            vec![ToolCallEnvelope {
                id: "c1".to_string(),
                name: "read".to_string(),
                args_json: "{}".to_string(),
            }],
        );
        assert_eq!(should_continue(&state), ContinueHop::Tools);

        state.done_reason = Some(DoneReason::Error);
        assert_eq!(should_continue(&state), ContinueHop::Finalize);
    }

    #[test]
    fn route_tools_executes_declared_tools() {
        let mut state = empty_state(1);
        with_tool_calls(
            &mut state,
            vec![ToolCallEnvelope {
                id: "c1".to_string(),
                name: "write".to_string(),
                args_json: "{\"title\":\"followup\"}".to_string(),
            }],
        );
        assert_eq!(route_tools(&state), ToolsRoute::Execute);
    }

    #[test]
    fn decide_loop_step_exposes_next_hop_and_route() {
        let mut state = empty_state(1);
        with_tool_calls(
            &mut state,
            vec![ToolCallEnvelope {
                id: "c1".to_string(),
                name: "write".to_string(),
                args_json: "{\"title\":\"followup\"}".to_string(),
            }],
        );
        let decision = decide_loop_step(&state);
        assert_eq!(decision.next_hop, ContinueHop::Tools);
        assert_eq!(decision.tools_route, Some(ToolsRoute::Execute));
    }

    #[test]
    fn agent_run_resource_usage_accumulates_provider_cache_and_tool_observations() {
        let mut usage = AgentRunResourceUsageV1::default();
        usage.record_completed_provider_round(
            &GenerateResult {
                content: String::new(),
                tool_calls: vec![
                    ToolCallEnvelope {
                        id: "call-1".to_string(),
                        name: "read".to_string(),
                        args_json: "{}".to_string(),
                    },
                    ToolCallEnvelope {
                        id: "call-2".to_string(),
                        name: "bash".to_string(),
                        args_json: "{}".to_string(),
                    },
                ],
                reasoning_content: None,
                input_tokens: Some(600_000),
                total_tokens: Some(600_120),
                prompt_cache_hit_tokens: Some(580_000),
                prompt_cache_miss_tokens: Some(20_000),
            },
            30_000,
            3,
        );

        assert_eq!(usage.provider_attempts, 3);
        assert_eq!(usage.completed_provider_rounds, 1);
        assert_eq!(usage.estimated_input_tokens, 90_000);
        assert_eq!(usage.actual_input_tokens, 600_000);
        assert_eq!(usage.prompt_cache_hit_tokens, 580_000);
        assert_eq!(usage.prompt_cache_miss_tokens, 20_000);
        assert_eq!(usage.output_tokens, 120);
        assert_eq!(usage.tool_call_count, 2);
    }
}
