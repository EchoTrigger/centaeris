use super::*;
use crate::extension::composition::{
    empty_composition_digest, AgentCompositionEnvironmentV1, ResolvedAgentCompositionV1,
};
use crate::model::prepared_prompt::ModelMessageRoleV1;
use crate::model::ModelSessionConfig;
use crate::tool::{ModelToolChoice, ToolContract, ToolTurnBehavior};

fn request_fixture(
    message_count: usize,
    tool_count: usize,
) -> (
    ModelClientRequest,
    ResolvedAgentCompositionV1,
    Vec<ModelObservationV1>,
) {
    let contracts = (0..tool_count)
        .map(|index| ToolContract {
            name: format!("tool_{index:04}"),
            category: if index % 2 == 0 { "external.mcp" } else { "builtin" }.into(),
            summary: format!("Inspect synthetic record {index}."),
            input_schema: serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            concurrency_safe: true,
            turn_behavior: ToolTurnBehavior::ContinueTurn,
            provider_id: Some(format!("provider_{}", index % 3)),
            schema_hash: None,
            scopes: vec![],
            dynamic: true,
        })
        .collect::<Vec<_>>();
    // Reverse model order so accounting must not follow the sorted composition.
    let definitions = contracts
        .iter()
        .rev()
        .map(|contract| ModelToolDefinition {
            name: contract.name.clone(),
            description: contract.summary.clone(),
            input_schema: contract.input_schema.clone(),
        })
        .collect();
    let messages = (0..message_count)
        .map(|index| ModelMessageV1 {
            message_id: format!("message:{index:04}"),
            role: ModelMessageRoleV1::User,
            content: format!(
                "Synthetic context record {index}: preserve the original ordered evidence."
            ),
            tool_calls: vec![],
            tool_call_id: None,
            reasoning_content: None,
        })
        .collect();
    let prepared_prompt = PreparedPromptV1::new(
        None,
        messages,
        definitions,
        if tool_count == 0 {
            ModelToolChoice::None
        } else {
            ModelToolChoice::Auto
        },
        4096,
    )
    .expect("valid synthetic prepared prompt");
    let context_token_estimate =
        super::super::generate_request::estimate_prepared_prompt_input_tokens(&prepared_prompt)
            .expect("context estimate");
    let request = ModelClientRequest {
        session_id: "session:test".into(),
        turn_id: "turn:test".into(),
        loop_index: 0,
        provider_prompt_cache_key: None,
        provider_prompt_cache_retention: None,
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate,
        prepared_prompt,
        session_config: ModelSessionConfig::default(),
    };
    let digest = empty_composition_digest("request-test").expect("digest");
    let composition = AgentCompositionEnvironmentV1 {
        tool_contracts: contracts,
        skill_catalog_digest: digest.clone(),
        plugin_activation_digest: digest.clone(),
        hook_composition_digest: digest.clone(),
        execution_profile_digest: digest,
        policy_version: "test.v1".into(),
        model_binding_override: None,
    }
    .resolve_request(&request)
    .expect("resolved composition");
    let mut observations = request
        .prepared_prompt
        .messages
        .iter()
        .cloned()
        .map(|message| ModelObservationV1::ContextMessage { message })
        .collect::<Vec<_>>();
    if tool_count > 0 {
        observations.push(ModelObservationV1::ToolCatalog {
            tool_definitions: request.prepared_prompt.tool_definitions.clone(),
        });
    }
    (request, composition, observations)
}

#[test]
fn model_request_context_observations_require_full_ordered_coverage() {
    let (request, composition, observations) = request_fixture(2, 0);
    ModelRequestStartedV1::from_request(
        ModelRequestPurposeV1::Main,
        &request,
        observations.clone(),
        composition.clone(),
    )
    .expect("ordered full messages accepted");
    let mut cases = Vec::new();
    let mut reordered = observations.clone();
    reordered.swap(0, 1);
    cases.push(("reordered", reordered));
    cases.push(("missing", observations[..1].to_vec()));
    let mut duplicated = observations.clone();
    duplicated[1] = duplicated[0].clone();
    cases.push(("duplicated", duplicated));
    let mut extra = observations.clone();
    extra.push(observations[0].clone());
    cases.push(("extra", extra));
    let mut altered = observations.clone();
    let ModelObservationV1::ContextMessage { message } = &mut altered[0] else {
        unreachable!()
    };
    message.content.push_str(" changed");
    cases.push(("same ID different content", altered));
    for (label, observations) in cases {
        assert!(
            ModelRequestStartedV1::from_request(
                ModelRequestPurposeV1::Main,
                &request,
                observations,
                composition.clone()
            )
            .is_err(),
            "{label}"
        );
    }
    let (request, composition, observations) = request_fixture(1, 0);
    let mut mixed = observations;
    mixed.push(ModelObservationV1::CompactionPrompt {
        message: request.prepared_prompt.messages[0].clone(),
    });
    assert!(
        ModelRequestStartedV1::from_request(
            ModelRequestPurposeV1::Compaction,
            &request,
            mixed,
            composition
        )
        .is_err(),
        "compaction rejects context observations even when they match"
    );
}

#[test]
fn context_breakdown_preserves_model_tool_order_providers_and_accounting() {
    let (request, mut composition, _) = request_fixture(1, 5);
    let breakdown = context_token_breakdown(&request, &composition).expect("tool breakdown");
    assert_eq!(breakdown.total_tokens(), request.context_token_estimate);
    let expected = [
        ("tool_0004", "provider_1"),
        ("tool_0002", "provider_2"),
        ("tool_0000", "provider_0"),
    ];
    assert_eq!(
        breakdown
            .mcp_tools
            .iter()
            .map(|tool| (tool.name.as_str(), tool.provider_id.as_str()))
            .collect::<Vec<_>>(),
        expected
    );
    assert!(breakdown.system_tool_tokens > 0);
    assert_eq!(
        breakdown
            .mcp_tools
            .iter()
            .map(|tool| tool.tokens)
            .sum::<u32>(),
        breakdown.mcp_tool_tokens
    );
    composition
        .tool_contracts
        .retain(|contract| contract.name != "tool_0004");
    assert_eq!(
        context_token_breakdown(&request, &composition).expect_err("missing contract"),
        "context breakdown tool contract missing: tool_0004"
    );
}

/// Actual functions, including per-call index construction and output destruction.
/// Fixture construction and observation/composition cloning are outside each timed call.
/// Run with cargo test -p centaeris-core --release --lib repeated_processing_timings -- --ignored --nocapture --test-threads=1
#[test]
#[ignore = "manual timing probe; no timing assertions"]
fn repeated_processing_timings() {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    fn measure(mut run: impl FnMut() -> Duration) -> f64 {
        let mut samples = Vec::new();
        for _ in 0..5 {
            let mut elapsed = Duration::ZERO;
            for _ in 0..100 {
                elapsed += run();
            }
            samples.push(elapsed.as_secs_f64() * 1e6 / 100.0);
        }
        samples.sort_by(f64::total_cmp);
        samples[2]
    }
    for count in [32, 256, 2048] {
        let (request, composition, observations) = request_fixture(count, 0);
        let micros = measure(|| {
            let composition = composition.clone();
            let observations = observations.clone();
            let start = Instant::now();
            drop(black_box(
                ModelRequestStartedV1::from_request(
                    ModelRequestPurposeV1::Main,
                    black_box(&request),
                    observations,
                    composition,
                )
                .expect("valid request"),
            ));
            start.elapsed()
        });
        println!("from_request messages={count} median_us={micros:.3}");
    }
    for count in [16, 128, 512] {
        let (request, composition, _) = request_fixture(1, count);
        let micros = measure(|| {
            let start = Instant::now();
            drop(black_box(
                context_token_breakdown(black_box(&request), black_box(&composition))
                    .expect("valid breakdown"),
            ));
            start.elapsed()
        });
        println!("context_token_breakdown tools={count} median_us={micros:.3}");
    }
}
