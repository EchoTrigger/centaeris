use super::*;

#[test]
fn responses_requests_summary_when_reasoning_is_enabled() {
    let high =
        serde_json::to_value(build_openai_responses_reasoning(Some("high")).unwrap()).unwrap();
    assert_eq!(high, json!({"effort":"high", "summary":"auto"}));
    let none =
        serde_json::to_value(build_openai_responses_reasoning(Some("none")).unwrap()).unwrap();
    assert_eq!(none, json!({"effort":"none"}));
    assert!(build_openai_responses_reasoning(None).unwrap().is_none());
}

#[tokio::test]
async fn provider_reasoning_completion_corpus() {
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/model_reasoning.json"
    ))
    .unwrap();
    for case in cases {
        for mode in ["json", "stream", "interrupted"] {
            let streaming = mode != "json";
            let interrupted = mode == "interrupted";
            let response = JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: case["response"].to_string(),
            };
            let mut frames: Vec<String> = case["frames"]
                .as_array()
                .unwrap()
                .iter()
                .map(Value::to_string)
                .collect();
            if interrupted {
                frames.pop();
            }
            let transport = if streaming {
                MockJsonHttpTransport::with_sse_response_chunks(
                    vec![Ok(response.clone()), Ok(response)],
                    vec![frames.clone(), frames],
                )
            } else {
                MockJsonHttpTransport::with_response(Ok(response))
            };
            let (client, provider): (Box<dyn ModelClient>, &str) =
                match case["protocol"].as_str().unwrap() {
                    "anthropic" => {
                        std::env::set_var("ANTHROPIC_API_KEY", "test-key");
                        (
                            Box::new(AnthropicMessagesModelClient::new(
                                ModelProviderRegistry::new(),
                                transport,
                            )),
                            "anthropic.default",
                        )
                    }
                    "responses" => {
                        std::env::set_var("OPENAI_API_KEY", "test-key");
                        (
                            Box::new(OpenAiResponsesModelClient::new(
                                ModelProviderRegistry::new(),
                                transport,
                            )),
                            "openai.default",
                        )
                    }
                    "compatible" => (
                        Box::new(OpenAiCompatibleModelClient::new(
                            ModelProviderRegistry::new(),
                            transport,
                        )),
                        "custom.openai_compatible",
                    ),
                    _ => panic!("unknown corpus protocol"),
                };
            let mut request = build_model_request(provider);
            request.session_config.max_retries = 1;
            request.session_config.retry_backoff_ms = 1;
            let mut events = Vec::new();
            let result = if streaming {
                client
                    .generate_stream(&request, &mut |event| events.push(event))
                    .await
            } else {
                client.generate(&request).await
            };
            if case["invalid"] == true || interrupted {
                assert!(
                    result.is_err(),
                    "{} streaming={streaming}: malformed reasoning accepted",
                    case["id"]
                );
                assert!(!events
                    .iter()
                    .any(|event| matches!(event, ModelClientStreamEvent::Done { .. })));
                continue;
            }
            let result = result
                .unwrap_or_else(|error| panic!("{} streaming={streaming}: {error:?}", case["id"]))
                .generate_result;
            assert_eq!(
                result.reasoning_content.as_deref(),
                case["expectedReasoning"].as_str(),
                "{} streaming={streaming}",
                case["id"]
            );
            assert_eq!(
                result.continuation_reasoning_content.as_deref(),
                if case["protocol"] == "compatible" {
                    case["expectedReasoning"].as_str()
                } else {
                    None
                }
            );
            assert_eq!(result.content, case["expectedText"].as_str().unwrap());
            assert_eq!(
                result.tool_calls.len(),
                case["expectedToolCount"].as_u64().unwrap() as usize
            );
            let visible: String = events
                .iter()
                .filter_map(|event| match event {
                    ModelClientStreamEvent::Token { content } => Some(content.as_str()),
                    _ => None,
                })
                .collect();
            if streaming {
                if let Some(prefix) = case["livePrefix"].as_str() {
                    assert!(events.iter().any(|event| matches!(event, ModelClientStreamEvent::Reasoning { text } if text == prefix)), "summary delta must be visible before terminal completion");
                }
                assert_eq!(visible, case["expectedText"].as_str().unwrap());
                let thinking = events
                    .iter()
                    .filter_map(|event| match event {
                        ModelClientStreamEvent::Reasoning { text } if !text.is_empty() => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .next_back();
                assert_eq!(
                    thinking,
                    case["expectedReasoning"].as_str(),
                    "{} live reasoning",
                    case["id"]
                );
            }
        }
    }
}

#[tokio::test]
async fn go_compatible_models_send_each_explicit_reasoning_effort() {
    for model in ["glm-5.3-flash", "deepseek-v4.1-flash"] {
        for effort in ["low", "high", "max"] {
            let transport = MockJsonHttpTransport::with_response(Ok(JsonHttpResponse {
                status_code: 200,
                headers: HashMap::new(),
                body_json: json!({"choices":[{"message":{"content":"ok"}}]}).to_string(),
            }));
            let client = OpenAiCompatibleModelClient::new(ModelProviderRegistry::new(), transport);
            let mut request = build_model_request("custom.openai_compatible");
            request.session_config.model = model.into();
            request.session_config.thinking_mode = Some(effort.into());
            client.generate(&request).await.unwrap();
            let requests = client.transport.take_requests();
            let body: Value = serde_json::from_str(&requests[0].body_json).unwrap();
            assert_eq!(body["reasoning_effort"], effort);
            assert_eq!(body["model"], model);
        }
    }
}
