use super::*;
use crate::session::turn_input::attachments::UserInputAttachment;

fn image_attachment() -> UserInputAttachment {
    UserInputAttachment {
        input_ref: "input-image".into(),
        display_name: "evidence.png".into(),
        content_type: "image/png".into(),
    }
}

struct ImageResolver;
impl crate::model::prepared_prompt::ModelInputImageResolverPort for ImageResolver {
    fn resolve(&self, input_ref: &str, content_type: &str) -> Result<Vec<u8>, String> {
        assert!(matches!(input_ref, "input-image" | "input-late-image"));
        assert_eq!(content_type, "image/png");
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 3)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        Ok(bytes.into_inner())
    }
}

#[tokio::test]
async fn query_loop_prepared_attachment_recovery_retains_initial_and_supplement_vision_once() {
    use super::recovery_tests::{AckOnceFailQueue, RecoveryJournal};
    let queue = Arc::new(InputQueue::default());
    let mut late = image_attachment();
    late.input_ref = "input-late-image".into();
    queue.add(
        TurnInputPayload::UserSupplement {
            supplement_id: "late-image".into(),
            message: "".into(),
            attachments: vec![late],
        },
        1,
    );
    let failing = Arc::new(AckOnceFailQueue {
        queue: queue.clone(),
        fail_next_ack: AtomicBool::new(true),
        ack_attempts: Mutex::new(vec![]),
    });
    let journal = Arc::new(RecoveryJournal::default());
    let store = AgentRuntimeTestStore::new();
    let model = AttachmentModel {
        requests: Mutex::new(vec![]),
        late_queue: None,
    };
    for (token, failure) in [("first-claim", true), ("recovered-claim", false)] {
        let materializer = journal.clone();
        let control = TurnControl::new_durable_inputs(
            failing.clone(),
            input_binding(token),
            Arc::new(move |turn, items| materializer.materialize(turn, items, None)),
        )
        .unwrap();
        let engine = AgentRuntime::new_for_test(
            store.clone(),
            AgentRuntimeConfig {
                enable_prompt_compaction: false,
                ..Default::default()
            },
        )
        .with_model_input_image_resolver(Arc::new(ImageResolver));
        let mut request = input_run("active-input");
        request.agent_run_identity = Some(input_identity());
        request.initial_input = AgentRunInitialInput::UserInput {
            input_id: "initial-image".into(),
            message: "".into(),
            attachments: vec![image_attachment()],
        };
        let result = engine
            .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                request,
                &model,
                &StaticModelSessionConfigStore {
                    config: Some(ModelSessionConfig::default()),
                },
                &mut |_| {},
                &|| Ok(None),
                &control,
                &mut |point| {
                    if let ToolSafePoint::ModelRequestStarted(started) = point {
                        journal.commit_main(&started, &store)?;
                    }
                    Ok(())
                },
            )
            .await;
        if failure {
            assert!(result.unwrap_err().contains("injected_input_ack_failure"));
            assert!(model.requests.lock().unwrap().is_empty());
        } else {
            assert_eq!(result.unwrap().stop, AgentRunStop::Finalized);
        }
    }
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let prepared = &requests[0].prepared_prompt;
    assert_eq!(prepared.input_images.len(), 2);
    for image in &prepared.input_images {
        assert!(!image.data_base64.is_empty());
        assert_eq!(
            prepared
                .messages
                .iter()
                .map(|message| message.content.matches(&image.placeholder).count())
                .sum::<usize>(),
            1
        );
    }
    for id in ["initial-image", "late-image"] {
        assert_eq!(
            journal
                .read_facts()
                .iter()
                .flat_map(input_ids)
                .filter(|item| item == id)
                .count(),
            1
        );
    }
    assert_eq!(queue.0.lock().unwrap().acknowledged, ["late-image"]);
}

struct AttachmentModel {
    requests: Mutex<Vec<ModelClientRequest>>,
    late_queue: Option<Arc<InputQueue>>,
}
impl ModelClient for AttachmentModel {
    fn generate<'a>(
        &'a self,
        request: &'a ModelClientRequest,
    ) -> ModelClientFuture<'a, ModelClientResponse> {
        Box::pin(async move {
            let first = self.requests.lock().unwrap().is_empty();
            self.requests.lock().unwrap().push(request.clone());
            if first {
                if let Some(queue) = &self.late_queue {
                    queue.add(
                        TurnInputPayload::UserSupplement {
                            supplement_id: "late-image".into(),
                            message: "".into(),
                            attachments: vec![image_attachment()],
                        },
                        1,
                    );
                }
            }
            Ok(ModelClientResponse {
                generate_result: GenerateResult {
                    content: "Image answer".into(),
                    tool_calls: vec![],
                    continuation_reasoning_content: None,
                    reasoning_content: None,
                    input_tokens: None,
                    total_tokens: None,
                    prompt_cache_hit_tokens: None,
                    prompt_cache_miss_tokens: None,
                },
                provider_request_id: None,
                provider_latency_ms: None,
                provider_attempts: 1,
            })
        })
    }
}

#[tokio::test]
async fn query_loop_initial_and_running_attachment_reach_actual_main_vision_payload_and_uptake() {
    for initial in [true, false] {
        let queue = Arc::new(InputQueue::default());
        let control = TurnControl::new_durable_inputs(
            queue.clone(),
            input_binding("attachment-claim"),
            Arc::new(|_, _| Ok(())),
        )
        .unwrap();
        let engine = AgentRuntime::new_for_test(
            AgentRuntimeTestStore::new(),
            AgentRuntimeConfig {
                enable_prompt_compaction: false,
                ..Default::default()
            },
        )
        .with_model_input_image_resolver(Arc::new(ImageResolver));
        let model = AttachmentModel {
            requests: Mutex::new(vec![]),
            late_queue: (!initial).then(|| queue.clone()),
        };
        let mut request = input_run("active-input");
        request.agent_run_identity = Some(input_identity());
        if initial {
            request.initial_input = AgentRunInitialInput::UserInput {
                input_id: "initial-image".into(),
                message: "".into(),
                attachments: vec![image_attachment()],
            };
        }
        let mut records = vec![];
        let result = engine
            .process_turn_loop_online_with_model_client_stream_controlled_and_tool_safe_point_async(
                request,
                &model,
                &StaticModelSessionConfigStore {
                    config: Some(ModelSessionConfig::default()),
                },
                &mut |_| {},
                &|| Ok(None),
                &control,
                &mut |point| {
                    if let ToolSafePoint::ModelRequestStarted(started) = point {
                        records.extend(canonical_model_request_started_records(
                            "input-run",
                            &started,
                            3,
                        )?);
                    }
                    Ok(())
                },
            )
            .await
            .unwrap();
        assert_eq!(result.stop, AgentRunStop::Finalized);
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), if initial { 1 } else { 2 });
        let actual = requests.last().unwrap();
        assert_eq!(actual.prepared_prompt.input_images.len(), 1);
        let image = &actual.prepared_prompt.input_images[0];
        assert_eq!(image.content_type, "image/png");
        assert!(!image.data_base64.is_empty());
        assert_eq!(
            actual
                .prepared_prompt
                .messages
                .iter()
                .map(|message| message.content.matches(&image.placeholder).count())
                .sum::<usize>(),
            1
        );
        assert_eq!(
            input_ids(records.last().unwrap()),
            if initial {
                vec!["initial-image"]
            } else {
                vec!["late-image"]
            }
        );
        if !initial {
            assert_eq!(queue.0.lock().unwrap().acknowledged, ["late-image"]);
        }
    }
}

#[test]
fn query_loop_supplement_attachment_wire_restores_actual_image_input_metadata() {
    let mut wire = serde_json::to_value(
        crate::session::turn_supplement_record(
            "attachment-session",
            "attachment-turn",
            "input-run",
            "file-input",
            "Describe this image",
            1,
        )
        .unwrap(),
    )
    .unwrap();
    wire["payload"]["attachments"] = json!([{
        "inputRef": "input-image", "displayName": "evidence.png", "contentType": "image/png",
    }]);
    let record = crate::session::parse_event(&wire)
        .expect("attached accepted supplement is a canonical record");
    let restored = crate::session::restore_runtime_snapshot_from_session_records(
        "attachment-session",
        &[record],
    )
    .unwrap();
    assert_eq!(restored.messages.len(), 1);
    let images: Vec<crate::model::prepared_prompt::ModelInputImageRefV1> = serde_json::from_str(
        restored.messages[0]
            .metadata
            .get(crate::runtime::keys::metadata::MODEL_INPUT_IMAGES)
            .expect("image metadata must survive durable replay"),
    )
    .unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].input_ref, "input-image");
    assert_eq!(
        restored.messages[0]
            .content
            .matches(&images[0].placeholder)
            .count(),
        1
    );
}

#[tokio::test]
async fn query_loop_file_attachment_limit_accepts_nine_and_fifty_without_host_specific_tools() {
    for count in [9, 50] {
        let attachments = (0..count)
            .map(|index| UserInputAttachment {
                input_ref: format!("file-{index:02}"),
                display_name: format!("evidence-{index}.txt"),
                content_type: "text/plain".into(),
            })
            .collect::<Vec<_>>();
        let engine = AgentRuntime::new_for_test(
            AgentRuntimeTestStore::new(),
            AgentRuntimeConfig {
                enable_prompt_compaction: false,
                ..Default::default()
            },
        );
        let model = AttachmentModel {
            requests: Mutex::new(vec![]),
            late_queue: None,
        };
        let mut request = input_run("active-input");
        request.agent_run_identity = Some(input_identity());
        request.initial_input = AgentRunInitialInput::UserInput {
            input_id: "files-only".into(),
            message: "".into(),
            attachments: attachments.clone(),
        };
        let result = engine
            .process_turn_loop_online_with_model_client_async(
                request,
                &model,
                &StaticModelSessionConfigStore {
                    config: Some(ModelSessionConfig::default()),
                },
            )
            .await
            .unwrap();
        assert_eq!(result.stop, AgentRunStop::Finalized);
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let context = requests[0]
            .prepared_prompt
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for item in attachments {
            assert_eq!(context.matches(&item.input_ref).count(), 1);
        }
        assert!(!context.contains("read_material"));
    }
}
