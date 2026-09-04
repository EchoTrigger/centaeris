use super::*;

pub(super) fn build_route_state(
    generate_result: GenerateResult,
    system_prompt_manifest_json: Option<&str>,
    compression_stats_json: Option<String>,
    agent_run_resource_usage: &AgentRunResourceUsageV1,
) -> AgentStateSnapshot {
    let mut state = build_agent_state(generate_result);
    attach_system_prompt_manifest(&mut state, system_prompt_manifest_json);
    state.compression_stats_json = compression_stats_json;
    state.agent_run_resource_usage = agent_run_resource_usage.clone();
    state.transition_json = Some(
        json!({
            "reason": "route_generate_result",
        })
        .to_string(),
    );
    state
}
