use super::*;
use ratatui::style::Color;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[test]
fn automatic_followup_attaches_only_to_the_visible_session() {
    let workspace = unique_test_dir("followup-workspace");
    let data_root = unique_test_dir("followup-data");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.active_session = Some(TuiSession {
        id: "visible".into(),
    });
    let update = |session: &str, run: &str| json!({"sessionId":session,"agentRunId":run,"payload":{"type":"runtime_event","event":{"id":format!("event:{run}"),"type":"ModelRequestStart","processState":"thinking","payload":{"purpose":"main","contextTokenEstimate":10,"initialContent":"follow-up"}}}});
    apply_session_update(&mut app, &update("other", "other-run"));
    assert!(app.assistant_buffer.is_empty());
    assert!(app.active_agent_run_id.is_none());
    apply_session_update(&mut app, &update("visible", "followup-run"));
    assert_eq!(app.active_agent_run_id.as_deref(), Some("followup-run"));
    assert_eq!(app.assistant_buffer, "follow-up");
    apply_session_update(&mut app, &update("other", "other-run-2"));
    assert!(!app
        .transcript
        .iter()
        .any(|line| matches!(line, TranscriptLine::Error(_))));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}
use centaeris_core::session::transcript::{
    TranscriptBlockBodyV1, TranscriptBlockStatusV1, TranscriptBlockV1, TranscriptOrderKeyV1,
    TranscriptPageV1, TranscriptPatchV1, TranscriptResumeCursorV1, TranscriptTextContentV1,
    TRANSCRIPT_PAGE_SCHEMA_V1, TRANSCRIPT_PATCH_SCHEMA_V1, TRANSCRIPT_PROJECTION_VERSION_V1,
};

fn transcript_test_block(
    block_id: &str,
    revision: u64,
    source_sequence: u64,
    body: TranscriptBlockBodyV1,
) -> TranscriptBlockV1 {
    TranscriptBlockV1 {
        block_id: block_id.to_string(),
        block_revision: revision.to_string(),
        presentation: None,
        order_key: TranscriptOrderKeyV1 {
            source_sequence: source_sequence.to_string(),
            ordinal: 0,
        },
        body,
    }
}

fn transcript_test_page(
    blocks: Vec<TranscriptBlockV1>,
    older_cursor: Option<&str>,
) -> TranscriptPageV1 {
    TranscriptPageV1 {
        schema: TRANSCRIPT_PAGE_SCHEMA_V1.to_string(),
        session_id: "session-paged".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-paged".to_string(),
        source_high_water: "12".to_string(),
        blocks,
        older_cursor: older_cursor.map(str::to_string),
        has_older: older_cursor.is_some(),
        resume_cursors: vec![TranscriptResumeCursorV1 {
            stream_id: "session-jsonl.v1".to_string(),
            cursor: "12".to_string(),
        }],
    }
}

#[test]
fn transcript_paging_materializes_tail_then_prepends_at_the_fixed_waterline() {
    let tail = transcript_test_page(
        vec![
            transcript_test_block(
                "assistant-11",
                1,
                11,
                TranscriptBlockBodyV1::AssistantText {
                    content: TranscriptTextContentV1::inline("tail".to_string()),
                    status: TranscriptBlockStatusV1::Completed,
                },
            ),
            transcript_test_block(
                "tool:call-12",
                1,
                12,
                TranscriptBlockBodyV1::Tool {
                    call_id: "call-12".to_string(),
                    tool_name: "read_file".to_string(),
                    status: TranscriptBlockStatusV1::Running,
                    summary: Some("src/main.rs".to_string()),
                    summary_ref: None,
                    output_ref: None,
                },
            ),
        ],
        Some("before-11"),
    );
    let mut state = TranscriptPagingState::open(tail).expect("open tail page");

    assert_eq!(state.source_high_water(), 12);
    assert_eq!(state.older_cursor(), Some("before-11"));
    assert_eq!(
        state.materialize_history(false),
        vec![
            TranscriptLine::Summary("tail".to_string()),
            TranscriptLine::Tool(crate::tool_projection::transcript_page_tool_line(
                "call-12",
                "read_file",
                TranscriptBlockStatusV1::Running,
                "src/main.rs".to_string(),
                None,
            )),
        ]
    );

    let older = transcript_test_page(
        vec![transcript_test_block(
            "user-2",
            1,
            2,
            TranscriptBlockBodyV1::UserText {
                content: TranscriptTextContentV1::inline("earlier".to_string()),
            },
        )],
        None,
    );
    state.apply_older_page(older).expect("prepend older page");

    assert_eq!(state.older_cursor(), None);
    assert_eq!(
        state.materialize_history(false).first(),
        Some(&TranscriptLine::User("earlier".to_string()))
    );
}

#[test]
fn transcript_paging_holds_new_committed_tail_behind_the_live_overlay() {
    let mut state = TranscriptPagingState::open(transcript_test_page(
        vec![transcript_test_block(
            "tool:call-8",
            1,
            8,
            TranscriptBlockBodyV1::Tool {
                call_id: "call-8".to_string(),
                tool_name: "exec_command".to_string(),
                status: TranscriptBlockStatusV1::Running,
                summary: Some("cargo test".to_string()),
                summary_ref: None,
                output_ref: None,
            },
        )],
        None,
    ))
    .expect("open tail page");
    state
        .apply_patch(TranscriptPatchV1 {
            schema: TRANSCRIPT_PATCH_SCHEMA_V1.to_string(),
            session_id: "session-paged".to_string(),
            projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
            projection_generation: "generation-paged".to_string(),
            source_high_water: "14".to_string(),
            stream_id: "session-jsonl.v1".to_string(),
            applied_cursor: "14".to_string(),
            upserts: vec![
                transcript_test_block(
                    "tool:call-8",
                    2,
                    8,
                    TranscriptBlockBodyV1::Tool {
                        call_id: "call-8".to_string(),
                        tool_name: "exec_command".to_string(),
                        status: TranscriptBlockStatusV1::Completed,
                        summary: Some("cargo test".to_string()),
                        summary_ref: None,
                        output_ref: None,
                    },
                ),
                transcript_test_block(
                    "assistant-14",
                    1,
                    14,
                    TranscriptBlockBodyV1::AssistantText {
                        content: TranscriptTextContentV1::inline("done".to_string()),
                        status: TranscriptBlockStatusV1::Completed,
                    },
                ),
            ],
            removals: Vec::new(),
        })
        .expect("apply committed patch");

    let while_live = state.materialize_history(true);
    assert_eq!(while_live.len(), 1);
    assert!(matches!(
        &while_live[0],
        TranscriptLine::Tool(tool) if !tool.running
    ));
    assert_eq!(
        state.materialize_history(false).last(),
        Some(&TranscriptLine::Summary("done".to_string()))
    );
}

#[test]
fn transcript_page_polling_freezes_generation_and_waterline_after_first_response() {
    let mut requests = Vec::new();
    let mut responses = vec![
        json!({
            "schema": "transcript.page.rpc.v1",
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation-paged",
            "projectedSourceHighWater": "4",
            "targetSourceHighWater": "12",
            "targetReached": false,
            "page": null
        }),
        json!({
            "schema": "transcript.page.rpc.v1",
            "projectionVersion": "transcript.projection.v1",
            "projectionGeneration": "generation-paged",
            "projectedSourceHighWater": "12",
            "targetSourceHighWater": "12",
            "targetReached": true,
            "page": transcript_test_page(Vec::new(), None)
        }),
    ]
    .into_iter();

    let page = paging::request_transcript_page_with(
        "session-paged",
        None,
        None,
        None,
        |request| {
            requests.push(request);
            responses
                .next()
                .ok_or_else(|| "unexpected request".to_string())
        },
        || {},
    )
    .expect("load projected tail page");

    assert_eq!(page.source_high_water, "12");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["request"]["projectionGeneration"], Value::Null);
    assert_eq!(requests[0]["request"]["sourceHighWater"], Value::Null);
    assert_eq!(
        requests[1]["request"]["projectionGeneration"],
        "generation-paged"
    );
    assert_eq!(requests[1]["request"]["sourceHighWater"], "12");
}

#[test]
fn transcript_patch_polling_applies_revisions_without_exposing_new_tail_during_live_run() {
    let mut state = TranscriptPagingState::open(transcript_test_page(
        vec![transcript_test_block(
            "tool:call-8",
            1,
            8,
            TranscriptBlockBodyV1::Tool {
                call_id: "call-8".to_string(),
                tool_name: "exec_command".to_string(),
                status: TranscriptBlockStatusV1::Running,
                summary: Some("cargo test".to_string()),
                summary_ref: None,
                output_ref: None,
            },
        )],
        None,
    ))
    .expect("open tail page");
    let patch = TranscriptPatchV1 {
        schema: TRANSCRIPT_PATCH_SCHEMA_V1.to_string(),
        session_id: "session-paged".to_string(),
        projection_version: TRANSCRIPT_PROJECTION_VERSION_V1.to_string(),
        projection_generation: "generation-paged".to_string(),
        source_high_water: "14".to_string(),
        stream_id: "session-jsonl.v1".to_string(),
        applied_cursor: "14".to_string(),
        upserts: vec![transcript_test_block(
            "tool:call-8",
            2,
            8,
            TranscriptBlockBodyV1::Tool {
                call_id: "call-8".to_string(),
                tool_name: "exec_command".to_string(),
                status: TranscriptBlockStatusV1::Completed,
                summary: Some("cargo test".to_string()),
                summary_ref: None,
                output_ref: None,
            },
        )],
        removals: Vec::new(),
    };
    let mut requests = Vec::new();

    let changed = paging::request_transcript_patches_with(
        &mut state,
        |request| {
            requests.push(request);
            Ok(json!({
                "schema": "transcript.patch.rpc.v1",
                "projectionVersion": "transcript.projection.v1",
                "projectionGeneration": "generation-paged",
                "projectedSourceHighWater": "14",
                "targetSourceHighWater": "14",
                "targetReached": true,
                "patches": [patch],
                "nextSourceHighWater": "14",
                "hasMore": false
            }))
        },
        || {},
    )
    .expect("load transcript patches");

    assert!(changed);
    assert_eq!(requests[0]["request"]["afterSourceHighWater"], "12");
    assert_eq!(state.current_source_high_water(), 14);
    {
        let lines = state.materialize_history(true);
        assert!(matches!(&lines[0], TranscriptLine::Tool(tool) if !tool.running));
    }
}

#[test]
fn tui_uses_current_directory_as_default_workspace() {
    let cwd = unique_test_dir("default-cwd-workspace");

    let config =
        resolve_config_with_default(None, cwd.clone()).expect("default workspace from cwd");

    let canonical = cwd.canonicalize().expect("canonical cwd");
    assert_eq!(config.session_cwd, display_path(canonical.as_path()));
    assert_eq!(config.workspace_root, canonical);
    std::fs::remove_dir_all(&cwd).expect("cleanup cwd");
}

#[test]
fn bare_home_workspace_sets_first_prompt_warning_only_for_implicit_launch() {
    let home = unique_test_dir("implicit-home-workspace");
    let implicit = resolve_config_with_default_and_home(None, home.clone(), Some(home.clone()))
        .expect("implicit home workspace");
    assert!(implicit.warn_home_on_first_prompt);

    let explicit =
        resolve_config_with_default_and_home(Some(home.clone()), home.clone(), Some(home.clone()))
            .expect("explicit home workspace");
    assert!(!explicit.warn_home_on_first_prompt);
    std::fs::remove_dir_all(home).expect("cleanup home");
}

#[test]
fn home_risk_waits_until_the_user_has_entered_a_prompt() {
    let workspace = PathBuf::from("D:/home");
    let mut app = test_app("", workspace.clone(), workspace);
    app.home_risk_pending = true;
    assert!(!should_open_home_risk_panel(&app, ""));
    assert!(should_open_home_risk_panel(&app, "inspect this project"));
    app.home_risk_pending = false;
    assert!(!should_open_home_risk_panel(&app, "inspect this project"));
}

#[test]
fn tui_requires_an_existing_explicit_workspace() {
    let cwd = unique_test_dir("explicit-workspace-cwd");

    let nonexistent = cwd.join(format!(
        "centaeris-tui-nonexistent-workspace-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    let error = resolve_config_with_default(Some(nonexistent.clone()), cwd.clone())
        .expect_err("TUI must not create a workspace");
    assert!(error.contains("workspace path is not readable"));
    assert!(!nonexistent.exists());

    let workspace = unique_test_dir("explicit-workspace");
    let config = resolve_config_with_default(Some(workspace.clone()), cwd.clone())
        .expect("existing workspace");
    assert_eq!(
        config.session_cwd,
        display_path(
            workspace
                .canonicalize()
                .expect("canonical workspace")
                .as_path()
        )
    );
    std::fs::remove_dir_all(workspace).expect("cleanup workspace");
    std::fs::remove_dir_all(cwd).expect("cleanup cwd");
}

#[test]
fn tui_refresh_model_display_reads_shared_runtime_config() {
    let app = test_app(
        "",
        unique_test_dir("model-config-workspace"),
        std::env::temp_dir(),
    );

    let response = json!({
        "modelProviderId": "openai",
        "model": "gpt-5.5",
        "modelThinkingMode": "xhigh",
        "selectableModels": []
    });
    let mut app = app;
    apply_model_display(&mut app, &response);
    assert_eq!(app.model_provider_id.as_deref(), Some("openai"));
    assert_eq!(app.model_display.as_deref(), Some("gpt-5.5"));
    assert_eq!(app.model_effort.as_deref(), Some("xhigh"));

    let response = json!({
        "model": "gpt-5.5",
        "selectableModels": []
    });
    apply_model_display(&mut app, &response);
    assert_eq!(app.model_display.as_deref(), Some("gpt-5.5"));

    let response = json!({ "selectableModels": [] });
    apply_model_display(&mut app, &response);
    assert_eq!(app.model_display, None);
}

#[test]
fn pending_model_request_keeps_input_responsive_until_config_arrives() {
    let workspace = unique_test_dir("pending-model-workspace");
    let data_root = unique_test_dir("pending-model-data");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    let (response_tx, response_rx) = std::sync::mpsc::channel();
    app.pending_model_request = Some(PendingModelRequest::LoadingConfig {
        response: RuntimeResponse::from_response_receiver(response_rx),
        command: ModelCommand::Refresh,
    });

    assert!(!drain_model_request(&mut app));
    assert_eq!(status_line(&app, 200).to_string(), "");
    handle_key(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &mut app,
    );
    assert_eq!(
        app.input, "x",
        "model config loading must not block keyboard input"
    );
    start_model_command(&mut app, ModelCommand::List)
        .expect("explicit /model must reuse the startup refresh");

    response_tx
        .send(Ok(json!({
            "modelProviderId": "openai",
            "model": "gpt-5.5",
            "modelProviders": [],
            "selectableModels": []
        })))
        .expect("send model config");
    assert!(drain_model_request(&mut app));

    assert_eq!(app.model_display.as_deref(), Some("gpt-5.5"));
    assert!(app.model_panel.is_some());
    assert!(app.message.is_none());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn paste_appends_text_to_input() {
    let workspace = unique_test_dir("workspace-paste");
    let data_root = unique_test_dir("data-paste");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    insert_text_at_cursor(&mut app, "before");
    handle_paste(&mut app, "\nline two\nline three".to_string());
    assert_eq!(app.input, "before\nline two\nline three");
    assert!(!app.show_help);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn terminal_text_of_an_absolute_image_path_creates_an_image_placeholder() {
    let workspace = unique_test_dir("workspace-pasted-image-path");
    let data_root = unique_test_dir("data-pasted-image-path");
    let source = data_root.join("source image.png");
    std::fs::write(source.as_path(), test_png_bytes()).expect("write image fixture");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    handle_paste(&mut app, format!("\"{}\"", source.display()));

    assert_eq!(app.input, "[Image #1] ");
    assert_eq!(app.draft_image_attachments.len(), 1);
    assert_ne!(app.draft_image_attachments[0].local_path, source);
    assert!(app.draft_image_attachments[0].local_path.is_file());
    clear_composer(&mut app);

    for character in source.display().to_string().replace(' ', "` ").chars() {
        handle_key(
            KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
            &mut app,
        );
    }
    assert_eq!(app.input, "[Image #1] ");
    assert_eq!(app.draft_image_attachments.len(), 1);
    clear_composer(&mut app);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn shift_enter_inserts_a_newline_and_expands_the_composer() {
    let workspace = unique_test_dir("workspace-shift-enter");
    let data_root = unique_test_dir("data-shift-enter");
    let mut app = test_app("first line", workspace.clone(), data_root.clone());

    handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT), &mut app);

    assert_eq!(app.input, "first line\n");
    assert_eq!(input_row_count(&app, 80), 2);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn paste_is_ignored_while_panels_are_open() {
    let workspace = unique_test_dir("workspace-paste-panel");
    let data_root = unique_test_dir("data-paste-panel");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    open_model_panel(&mut app, &model_picker_config()).unwrap();
    handle_paste(&mut app, "ignored".to_string());
    assert!(app.input.is_empty());
    close_model_panel(&mut app);
    app.home_risk_panel = Some(HomeRiskPanel {
        workspaces: Vec::new(),
        selected: 0,
        notice: None,
    });
    handle_paste(&mut app, "still ignored".to_string());
    assert!(app.input.is_empty());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

fn deliver_terminal_paste_keys(app: &mut App, text: &str, crlf: bool) {
    let mut input = input_events::InputEvents::default();
    let started = Instant::now();
    for character in text.chars() {
        let key = KeyEvent::new(
            if character == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(character)
            },
            KeyModifiers::NONE,
        );
        for event in input.push(Event::Key(key), started) {
            assert!(!handle_terminal_event(event, app));
        }
        if character == '\n' && crlf {
            for event in input.push(
                Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
                started,
            ) {
                assert!(!handle_terminal_event(event, app));
            }
        }
        for event in input.push(
            Event::Key(KeyEvent::new_with_kind(
                key.code,
                key.modifiers,
                KeyEventKind::Release,
            )),
            started,
        ) {
            assert!(!handle_terminal_event(event, app));
        }
    }
    for event in input.flush_due(started + Duration::from_millis(20)) {
        assert!(!handle_terminal_event(event, app));
    }
}

#[test]
fn native_multiline_paste_waits_for_one_manual_submit_in_idle_and_busy_sessions() {
    let text = "| 项目 | 状态 |\n| 中文 e\u{301} 👩‍💻 | 待验收 |\n";
    for busy in [false, true] {
        let path = PathBuf::from("D:/workspace");
        let mut app = test_app("", path.clone(), path);
        app.active_session = Some(TuiSession {
            id: "paste-session".into(),
        });
        if busy {
            app.active_agent_run_id = Some("paste-run".into());
            app.active_agent_run_ids.insert("paste-run".into());
        }
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let received = requests.clone();
        app.runtime = Some(RuntimeClient::from_test_request_handler(move |frame| {
            received.lock().unwrap().push(frame);
            json!({"accepted": true, "sessionId": "paste-session", "agentRunId": "paste-run"})
        }));

        deliver_terminal_paste_keys(&mut app, text, true);
        assert_eq!(app.input, text);
        assert!(
            requests.lock().unwrap().is_empty(),
            "paste must not send a prompt or supplement"
        );
        assert!(app.transcript.is_empty());
        assert!(app.message.is_none());

        assert!(!handle_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            &mut app
        ));
        assert!(app.input.is_empty(), "a paste must undo as one edit");
        assert!(!handle_terminal_event(
            Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL)),
            &mut app
        ));
        assert_eq!(app.input, text);

        let mut input = input_events::InputEvents::default();
        let now = Instant::now();
        assert!(input
            .push(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                now
            )
            .is_empty());
        for event in input.flush_due(now + Duration::from_millis(20)) {
            assert!(!handle_terminal_event(event, &mut app));
        }
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["params"]["request"]["message"], text.trim_end());
        assert!(app.input.is_empty());
        assert!(app.message.is_none());
    }
}

#[test]
fn native_multiline_paste_cannot_activate_modal_or_command_actions() {
    let path = PathBuf::from("D:/workspace");
    for panel in ["model", "home", "question", "commands"] {
        let mut app = test_app("draft", path.clone(), path.clone());
        match panel {
            "model" => open_model_panel(&mut app, &model_picker_config()).unwrap(),
            "home" => {
                app.home_risk_panel = Some(HomeRiskPanel {
                    workspaces: Vec::new(),
                    selected: 0,
                    notice: None,
                })
            }
            "question" => {
                app.pending_question = Some(PendingQuestion {
                    id: "paste-question".into(),
                    question: "Pick one".into(),
                    options: Vec::new(),
                    multi_select: false,
                    required: true,
                })
            }
            "commands" => {
                app.input = "/".into();
                app.input_cursor = 1;
                app.show_help = true;
            }
            _ => unreachable!(),
        }
        let draft = app.input.clone();
        deliver_terminal_paste_keys(&mut app, "/model\n/exit\n", false);
        assert_eq!(app.input, draft, "{panel}");
        assert!(app.message.is_none(), "{panel}");
        assert!(
            app.runtime.is_none(),
            "paste must not create a Runtime: {panel}"
        );
        assert!(app.pending_model_request.is_none(), "{panel}");
        match panel {
            "model" => assert!(matches!(
                app.model_panel.as_ref().unwrap().stage,
                model_picker::Stage::Models
            )),
            "home" => assert!(app.home_risk_panel.is_some()),
            "question" => assert_eq!(app.pending_question.as_ref().unwrap().id, "paste-question"),
            "commands" => assert!(app.command_panel_open()),
            _ => unreachable!(),
        }
    }
}

#[test]
fn explicit_terminal_paste_normalizes_crlf_without_creating_submit_actions() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    assert!(!handle_terminal_event(
        Event::Paste("中文\r\n👩‍💻\r\n".into()),
        &mut app
    ));
    assert_eq!(app.input, "中文\n👩‍💻\n");
    assert!(app.active_agent_run_id.is_none());
    assert!(app.runtime.is_none());
}

#[cfg(windows)]
#[test]
fn native_image_drop_preserves_text_cursor_and_one_undo_step() {
    let root = unique_test_dir("native-image-drop");
    let source = root.join("截图 with spaces.png");
    std::fs::write(&source, test_png_bytes()).unwrap();
    let dropped = format!("\"{}\"", source.display());
    for (draft, cursor) in [("", 0), ("图片", "图片".len()), ("前文后文", "前文".len())] {
        let mut app = test_app(draft, root.clone(), root.clone());
        app.input_cursor = cursor;
        deliver_terminal_paste_keys(&mut app, &dropped, false);

        let expected = format!("{}[Image #1] {}", &draft[..cursor], &draft[cursor..]);
        assert_eq!(app.input, expected);
        assert_eq!(app.input_cursor, cursor + "[Image #1] ".len());
        assert_eq!(app.draft_image_attachments.len(), 1);
        let image = &app.draft_image_attachments[0];
        assert_eq!(
            (image.start, image.end),
            (cursor, cursor + "[Image #1]".len())
        );
        let owned = image.local_path.clone();
        assert_ne!(owned, source);
        assert!(owned.is_file());
        assert!(app.runtime.is_none(), "dropping an image must not submit");

        let request = build_prompt_request(
            &TuiSession {
                id: "drop-session".into(),
            },
            &app.input,
            &app.draft_image_attachments,
            "drop-operation",
        );
        assert_eq!(request["message"], expected);
        assert_eq!(request["attachments"][0]["placeholder"], "[Image #1]");

        handle_key(
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
            &mut app,
        );
        assert_eq!(app.input, draft);
        assert_eq!(app.input_cursor, cursor);
        assert!(app.draft_image_attachments.is_empty());
        assert!(owned.is_file());
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            handle_key(
                KeyEvent::new(
                    KeyCode::Char(if modifiers.contains(KeyModifiers::SHIFT) {
                        'z'
                    } else {
                        'y'
                    }),
                    modifiers,
                ),
                &mut app,
            );
            assert_eq!(app.input, expected);
            assert_eq!(app.input_cursor, cursor + "[Image #1] ".len());
            assert_eq!(app.draft_image_attachments[0].local_path, owned);
            handle_key(
                KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
                &mut app,
            );
            assert_eq!(app.input, draft);
        }
        clear_composer(&mut app);
        assert!(!owned.exists());
        assert!(source.is_file());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn subagent_events_project_to_summary_lines() {
    let line = subagent_transcript_line(
        "SubagentResult",
        &json!({
            "subagentId": "agent-1",
            "title": "Researcher",
            "description": "Research release gates",
            "summary": "found 3 references",
            "parentTurnId": "turn-1"
        }),
    )
    .expect("subagent line");
    assert_eq!(line.title, "Research release gates");
    assert_eq!(line.summary, "found 3 references");
    assert_eq!(line.status, "result");

    let line = subagent_transcript_line(
        "SubagentFailed",
        &json!({
            "subagentId": "agent-2",
            "message": "timeout",
            "parentTurnId": "turn-1"
        }),
    )
    .expect("subagent line");
    assert_eq!(line.title, "agent-2");
    assert_eq!(line.summary, "timeout");
    assert_eq!(line.status, "failed");
}

#[test]
fn subagent_lines_render_with_prefix_and_error_color() {
    let items = vec![TranscriptLine::Subagent(SubagentTranscriptLine {
        title: "Researcher".to_string(),
        summary: "done".to_string(),
        status: "result".to_string(),
    })];
    let lines = transcript_to_lines(&items, 80);
    let rendered = lines[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(rendered, "  Researcher: done");
    assert_eq!(lines[0].spans[0].style.fg, Some(theme().muted));
}

#[test]
fn tui_status_line_only_shows_effort_and_model() {
    let workspace = unique_test_dir("status-cfg-workspace");
    let data_root = unique_test_dir("status-cfg-data");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.model_display = Some("openai/gpt-5.5".to_string());
    app.model_effort = Some("xhigh".to_string());
    assert!(!status_line(&app, 200).to_string().contains("context"));

    let status = status_line(&app, 200).to_string();
    assert_eq!(status.trim(), "openai/gpt-5.5 (xhigh)");
    assert!(!status.contains(app.workspace_root.as_str()));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tui_model_query_resolves_exact_then_fuzzy_with_ambiguity_loud_fail() {
    let response = json!({
        "selectableModels": [
            {"providerId": "openai", "model": "gpt-5.5", "displayName": "GPT-5.5", "modelThinkingMode": "high", "modelThinkingModes": ["low", "high"], "preferredThinkingMode": null},
            {"providerId": "openai", "model": "gpt-4.1"},
            {"providerId": "ollama", "model": "gpt-oss:20b"}
        ]
    });

    let (provider_id, model) =
        resolve_model_request(&response, "gpt-5.5").expect("exact model match");
    assert_eq!(provider_id, "openai");
    assert_eq!(model, "gpt-5.5");

    let (provider_id, model) =
        resolve_model_request(&response, "GPT-5.5").expect("exact display name match");
    assert_eq!(provider_id, "openai");
    assert_eq!(model, "gpt-5.5");

    let (provider_id, model) =
        resolve_model_request(&response, "gpt-4.1").expect("exact second model match");
    assert_eq!(provider_id, "openai");
    assert_eq!(model, "gpt-4.1");

    let error = resolve_model_request(&response, "nonexistent").expect_err("no match");
    assert!(error.contains("no selectable model matches: nonexistent"));

    let error = resolve_model_request(&response, "gpt").expect_err("ambiguous fuzzy match");
    assert!(error.contains("model query is ambiguous: gpt"));
    assert!(error.contains("openai/gpt-5.5"));
    assert!(error.contains("openai/gpt-4.1"));
    assert!(error.contains("ollama/gpt-oss:20b"));
}

#[test]
fn tui_model_panel_is_provider_first_and_hides_unconfigured_models() {
    let response = json!({
        "modelProviderId": "openai",
        "model": "gpt-5.5",
        "modelProviders": [
            {
                "providerId": "openai",
                "name": "OpenAI",
                "configured": true,
                "models": [
                    {"providerId": "openai", "model": "gpt-5.5", "displayName": "GPT-5.5", "modelThinkingMode": "high", "modelThinkingModes": ["low", "high"], "preferredThinkingMode": null}
                ]
            },
            {
                "providerId": "deepseek.default",
                "name": "DeepSeek",
                "configured": false,
                "models": [
                    {"providerId": "deepseek.default", "model": "banana", "displayName": "Banana"}
                ]
            }
        ]
    });
    let panel = model_panel_from_config(&response).expect("model panel");
    assert_eq!(panel.selected_provider, 0);
    assert_eq!(panel.selected_model, 0);
    assert_eq!(panel.active_provider_id.as_deref(), Some("openai"));
    assert_eq!(panel.active_model.as_deref(), Some("gpt-5.5"));
    assert_eq!(panel.providers[0].models[0].display_name, "GPT-5.5");
    assert!(panel.providers[1].models.is_empty());

    let empty = json!({ "model": null, "modelProviders": [] });
    let panel = model_panel_from_config(&empty).expect("empty model panel");
    assert!(panel.providers.is_empty());
    assert!(panel.active_provider_id.is_none());
    assert!(panel.active_model.is_none());
}

#[test]
fn model_panel_renders_cleanly_and_supports_mouse_provider_setup() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("", workspace.clone(), workspace);
    let response = json!({
        "modelProviderId": "openai",
        "model": "gpt-5.5",
        "modelThinkingMode": "high",
        "modelProviders": [
            {
                "providerId": "openai",
                "name": "OpenAI",
                "configured": true,
                "models": [
                    {"model": "gpt-5.5", "displayName": "GPT-5.5", "modelThinkingMode": "high", "modelThinkingModes": ["low", "high"], "preferredThinkingMode": null},
                    {"model": "gpt-4.1", "displayName": "GPT-4.1", "modelThinkingMode": null, "modelThinkingModes": [], "preferredThinkingMode": null}
                ]
            },
            {
                "providerId": "deepseek.default",
                "name": "DeepSeek",
                "configured": false,
                "models": []
            }
        ]
    });
    apply_model_display(&mut app, &response);
    open_model_panel(&mut app, &response).expect("open model panel");
    let view = build_transcript_view(&app, 100);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render model panel");
    let rendered = test_buffer_text(terminal.backend().buffer());
    assert!(rendered.contains("Models"));
    assert!(rendered.contains("Current  GPT-5.5 · openai/gpt-5.5 · high"));
    assert!(rendered.contains("DeepSeek (setup)"));
    assert!(rendered.contains("GPT-5.5 (current)"));
    assert!(!rendered.contains("/model openai/gpt-5.5"));
    assert!(
        app.input_area.is_none(),
        "model page must not render Composer"
    );

    handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut app);
    assert_eq!(
        app.model_panel
            .as_ref()
            .expect("model panel")
            .selected_provider,
        1
    );
    handle_key(
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        &mut app,
    );
    assert_eq!(
        app.model_panel
            .as_ref()
            .expect("model panel")
            .selected_provider,
        0
    );

    let tab = app.model_provider_hit_regions[1];
    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: tab.x,
            row: tab.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    assert_eq!(
        app.model_panel
            .as_ref()
            .expect("model panel")
            .selected_provider,
        1
    );
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render provider setup");
    let list = app.model_list_area.expect("model list area");
    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: list.x,
            row: list.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    assert!(app.model_panel.is_none());
    assert_eq!(
        app.model_credential_prompt
            .as_ref()
            .map(|prompt| prompt.provider_id.as_str()),
        Some("deepseek.default")
    );
}

#[test]
fn tui_model_provider_query_opens_credential_flow_only_when_unconfigured() {
    let response = json!({
        "modelProviders": [
            {
                "providerId": "deepseek.default",
                "name": "DeepSeek",
                "configured": false,
                "models": []
            },
            {
                "providerId": "kimi.default",
                "name": "Kimi",
                "configured": true,
                "models": []
            }
        ]
    });
    let (prompt, configured) = resolve_model_provider_request(&response, "DeepSeek")
        .expect("provider lookup")
        .expect("provider match");
    assert_eq!(prompt.provider_id, "deepseek.default");
    assert!(!configured);
    assert!(
        resolve_model_provider_request(&response, "kimi.default")
            .expect("provider lookup")
            .expect("provider match")
            .1
    );
}

#[test]
fn tui_masks_model_credentials_in_the_input_projection() {
    let workspace = unique_test_dir("credential-mask-workspace");
    let data_root = unique_test_dir("credential-mask-data");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.model_credential_prompt = Some(ModelCredentialPrompt {
        provider_id: "deepseek.default".to_string(),
        provider_name: "DeepSeek".to_string(),
    });
    assert_eq!(input_for_display(&app, "sk-secret"), "•••••••••");
    app.model_credential_prompt = None;
    assert_eq!(input_for_display(&app, "hello"), "hello");
    assert_eq!(
        model_api_key_input(" sk-example ").as_deref(),
        Ok("sk-example")
    );
    assert!(model_api_key_input("first\nsecond").is_err());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

fn test_tool_line(
    action_kind: ToolActionKind,
    subject: &str,
    operations: Vec<Value>,
    result_states: Vec<ToolResultState>,
) -> ToolTranscriptLine {
    ToolTranscriptLine {
        is_bash: false,
        exit_code: None,
        full_text: None,
        readable_range: None,
        duration_ms: None,
        key: "step-test".to_string(),
        action_kind,
        subject: subject.to_string(),
        operations: operations
            .into_iter()
            .map(|operation| ToolOperation {
                call_id: operation
                    .get("callId")
                    .and_then(Value::as_str)
                    .unwrap_or("call-test")
                    .to_string(),
                tool_name: operation
                    .get("toolName")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                kind: operation
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string(),
                title: operation
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| operation.get("kind").and_then(Value::as_str))
                    .unwrap_or("tool")
                    .to_string(),
                status: None,
                path: operation
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                start_line: operation
                    .get("startLine")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                end_line: operation
                    .get("endLine")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                total_lines: operation
                    .get("totalLines")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                next_offset: operation
                    .get("nextOffset")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok()),
                truncated_by: operation
                    .get("truncatedBy")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                command_preview: None,
                query: None,
                added: operation.get("added").and_then(Value::as_u64),
                removed: operation.get("removed").and_then(Value::as_u64),
                output_preview: None,
                error: None,
                text: None,
                diff_rows: None,
            })
            .collect(),
        result_blocks: Vec::new(),
        images: Vec::new(),
        result_states,
        interrupted: false,
        running: false,
        command: None,
        description_title: false,
    }
}

fn apply_tool_call(app: &mut App, call_id: &str, tool_name: &str) {
    apply_stream_payload(
        app,
        &json!({
            "type": "session_event",
            "event": {
                "id": format!("evt-call-{call_id}"),
                "type": "ToolCall",
                "status": "running",
                "toolName": tool_name,
                "payload": {"callId": call_id}
            }
        }),
    );
}

fn apply_described_bash_call(app: &mut App, call_id: &str) {
    apply_stream_payload(
        app,
        &json!({
            "type": "session_event",
            "event": {
                "id": format!("evt-call-{call_id}"),
                "type": "ToolCall",
                "status": "running",
                "toolName": "bash",
                "payload": {
                    "callId": call_id,
                    "displayTarget": "Run focused tests",
                    "command": "cargo test -p centaeris-tui",
                    "description": "Run focused tests"
                }
            }
        }),
    );
}

fn apply_tool_result(
    app: &mut App,
    call_id: &str,
    result_state: &str,
    status: &str,
    operations: Value,
) {
    let tool_name = app
        .tool_projection
        .open_tool_name(call_id)
        .unwrap_or("bash")
        .to_string();
    let mut operations = operations;
    if let Some(items) = operations.as_array_mut() {
        for item in items {
            let Some(object) = item.as_object_mut() else {
                continue;
            };
            object
                .entry("toolName".to_string())
                .or_insert_with(|| Value::String(tool_name.clone()));
            if tool_name == "bash" {
                object
                    .entry("kind".to_string())
                    .or_insert_with(|| Value::String("command".to_string()));
            } else {
                object.remove("kind");
            }
        }
    }
    apply_stream_payload(
        app,
        &json!({
            "type": "session_event",
            "event": {
                "id": format!("evt-result-{call_id}"),
                "type": "ToolResult",
                "status": status,
                "toolName": tool_name,
                "payload": {
                    "callId": call_id,
                    "resultState": result_state,
                    "operations": operations,
                    "modelInputImages": []
                }
            }
        }),
    );
}

fn rendered_line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn slash_command_requires_first_character() {
    assert_eq!(slash_command_name("/help"), Some("/help"));
    assert_eq!(slash_command_name("/model test/model-a"), Some("/model"));
    assert_eq!(slash_command_name("/"), Some("/"));
    assert_eq!(slash_command_name(" /help"), None);
    assert_eq!(slash_command_name("hello /help"), None);
}

#[test]
fn command_matching_filters_by_prefix() {
    let matches = matching_commands("/mo");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "/model");
    for input in [
        "/model ",
        "/model test/model-a",
        "/effort high",
        "/model test/model-a high",
    ] {
        assert!(
            matching_commands(input).is_empty(),
            "only command names are completed: {input}"
        );
    }
}

#[test]
fn command_table_contains_only_the_available_commands() {
    let names = SLASH_COMMANDS
        .iter()
        .map(|command| command.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["/model", "/effort", "/resume", "/new", "/clear", "/stop", "/help", "/exit"]
    );
}

fn resume_panel_text(app: &mut App) -> String {
    let view = build_transcript_view(app, 80);
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| render(frame, app, &view)).unwrap();
    test_buffer_text(terminal.backend().buffer())
}

fn wait_for_resume_text(app: &mut App, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        session_picker::poll(app);
        let text = resume_panel_text(app);
        if text.contains(expected) {
            return;
        }
        assert!(Instant::now() < deadline, "missing {expected} in {text}");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn resume_searches_later_catalog_pages_and_opens_the_selected_history() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("Draft 中文 ", path.clone(), path);
    let temporary = unique_test_dir("parked-draft");
    let image = temporary.join("draft.png");
    std::fs::write(&image, test_png_bytes()).unwrap();
    insert_draft_image(&mut app, image.clone());
    let draft = app.input.clone();
    app.active_session = Some(TuiSession {
        id: "before".into(),
    });
    app.transcript
        .push(TranscriptLine::Summary("Prior session".into()));
    app.transcript_scroll = 2;
    app.transcript_follow_bottom = false;
    app.active_agent_run_id = Some("run-before".into());
    app.runtime = Some(RuntimeClient::from_test_request_handler(|frame| {
        let request = &frame["params"]["request"];
        match frame["method"].as_str().unwrap() {
            "session/catalog" if request["mode"] == "pinned" => {
                json!({"items": [], "nextCursor": null, "reset": false})
            }
            "session/catalog" if request["cursor"].is_null() => {
                json!({"items": [{"id": "before", "title": "Recent work", "cwd": "D:/workspace", "isPinned": false}],
                    "nextCursor": "page-2", "reset": false})
            }
            "session/catalog" => {
                assert_eq!(request["cursor"], "page-2");
                json!({"items": [{"id": "session-paged", "title": "终端历史验收", "cwd": "D:/other", "isPinned": false}],
                    "nextCursor": null, "reset": false})
            }
            "_centaeris/session/agent-runs/attach" => {
                assert_eq!(request["sessionId"], "session-paged");
                json!({"transitionReason": "viewer_attached"})
            }
            "_centaeris/session/agent-runs" => json!({"agentRuns": []}),
            "transcript/page" => json!({
                "schema": "transcript.page.rpc.v1", "projectionVersion": TRANSCRIPT_PROJECTION_VERSION_V1,
                "projectionGeneration": "generation-paged", "projectedSourceHighWater": "12",
                "targetSourceHighWater": "12", "targetReached": true,
                "page": transcript_test_page(vec![transcript_test_block("answer", 1, 12,
                    TranscriptBlockBodyV1::AssistantText {
                        content: TranscriptTextContentV1::inline("Resumed history".into()),
                        status: TranscriptBlockStatusV1::Completed,
                    })], None),
            }),
            "_centaeris/session/activate" => {
                if request["sessionId"] == "before" {
                    json!({"id": "before", "title": "Recent work", "cwd": "D:/workspace", "isPinned": false})
                } else {
                    assert_eq!(request["sessionId"], "session-paged");
                    json!({"id": "session-paged", "title": "终端历史验收", "cwd": "D:/other", "isPinned": false})
                }
            }
            other => panic!("unexpected resume request: {other}"),
        }
    }));
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let sessions = KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL);
    let recent_activity = |app: &mut App| {
        resume_panel_text(app)
            .lines()
            .find_map(|row| row.split_once("Recent work"))
            .map(|(prefix, _)| prefix.to_owned())
            .expect("Recent work in the session list")
    };
    handle_key(sessions, &mut app);
    wait_for_resume_text(&mut app, "Recent work");
    assert!(recent_activity(&mut app).contains(['⋅', ':', '⸬', '⁙']));
    handle_terminal_event(Event::Paste("终端历史".into()), &mut app);
    wait_for_resume_text(&mut app, "D:/other");
    handle_key(enter, &mut app);
    assert!(app.session_picker.is_none());
    assert_eq!(app.active_session.as_ref().unwrap().id, "session-paged");
    assert_eq!(app.session_cwd, "D:/other");
    assert_eq!(
        app.transcript,
        vec![TranscriptLine::Summary("Resumed history".into())]
    );
    assert!(app.input.is_empty());
    let update = json!({
        "sessionId": "before",
        "agentRunId": "run-before",
        "payload": {"type": "runtime_event", "event": {
            "id": "background-delta",
            "type": "ModelTextDelta",
            "payload": {"delta": "Background response"}
        }}
    });
    assert!(!session_views::route_update(&mut app, &update));
    assert!(app.assistant_buffer.is_empty());

    handle_key(sessions, &mut app);
    wait_for_resume_text(&mut app, "Recent work");
    assert!(recent_activity(&mut app).contains(['⋅', ':', '⸬', '⁙']));
    let now = Instant::now();
    let last_draw = now - STATUS_REDRAW_INTERVAL;
    assert!(periodic_redraw_due(&app, last_draw, now));
    assert!(session_views::route_update(
        &mut app,
        &json!({
            "sessionId": "before",
            "agentRunId": "run-before",
            "payload": {"type": "runtime_event", "event": {
                "id": "background-waiting", "type": "ModelStatus",
                "processState": "waiting_user", "payload": {}
            }}
        })
    ));
    assert!(recent_activity(&mut app).contains('◆'));
    assert!(!periodic_redraw_due(&app, last_draw, now));
    assert!(session_views::route_update(
        &mut app,
        &json!({
            "sessionId": "before",
            "agentRunId": "run-before",
            "payload": {"type": "runtime_event", "event": {
                "id": "background-final", "type": "Final", "processState": "responding",
                "payload": {"content": "Background response"}
            }}
        })
    ));
    assert!(session_views::route_update(
        &mut app,
        &json!({
            "sessionId": "before",
            "agentRunId": "run-before",
            "payload": {"type": "session_event", "event": {
                "id": "background-completed",
                "type": "AgentRunCompleted"
            }}
        })
    ));
    let list = resume_panel_text(&mut app);
    assert!(recent_activity(&mut app).contains('◇'));
    assert!(!list.contains("Running"));
    assert!(!periodic_redraw_due(&app, last_draw, now));
    handle_key(enter, &mut app);
    assert_eq!(app.active_session.as_ref().unwrap().id, "before");
    assert_eq!(app.input, draft);
    assert_eq!(app.draft_image_attachments[0].local_path, image);
    assert!(image.exists());
    assert_eq!(app.transcript_scroll, 2);
    assert!(!app.transcript_follow_bottom);
    assert!(
        rendered_lines_text(&build_transcript_view(&app, 80).lines).contains("Background response")
    );
    drop(app);
    assert!(!image.exists());
    std::fs::remove_dir(temporary).unwrap();
}

#[test]
fn resume_renames_and_pins_by_selected_session_id() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/resume", path.clone(), path);
    let entry = std::sync::Mutex::new(
        json!({"id": "selected", "title": "Original", "cwd": "D:/workspace", "isPinned": false}),
    );
    app.runtime = Some(RuntimeClient::from_test_request_handler(move |frame| {
        let request = &frame["params"]["request"];
        let mut entry = entry.lock().unwrap();
        match frame["method"].as_str().unwrap() {
            "session/catalog" => {
                let pinned = entry["isPinned"].as_bool().unwrap();
                let mode = if pinned { "pinned" } else { "recent" };
                let items = if request["mode"] == mode {
                    vec![entry.clone()]
                } else {
                    vec![]
                };
                json!({"items": items, "nextCursor": null, "reset": false})
            }
            "_centaeris/session/update_metadata" => {
                assert_eq!(request["sessionId"], "selected");
                if let Some(title) = request.get("title") {
                    entry["title"] = title.clone();
                }
                if let Some(pinned) = request.get("isPinned") {
                    entry["isPinned"] = pinned.clone();
                }
                entry.clone()
            }
            other => panic!("unexpected picker request: {other}"),
        }
    }));
    let control = |key| KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL);
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(enter, &mut app);
    wait_for_resume_text(&mut app, "Original");
    handle_key(control('r'), &mut app);
    handle_terminal_event(Event::Paste("中文标题".into()), &mut app);
    handle_key(enter, &mut app);
    wait_for_resume_text(&mut app, "中 文 标 题");
    handle_key(control('t'), &mut app);
    wait_for_resume_text(&mut app, "* 中 文 标 题");
    // Finish the recent page too before toggling the same saved entry again.
    wait_for_resume_text(&mut app, "^T Unpin");
    handle_key(control('t'), &mut app);
    wait_for_resume_text(&mut app, "^T Pin");
    assert!(!resume_panel_text(&mut app).contains("* 中 文 标 题"));
}

#[test]
fn new_and_clear_leave_running_sessions_open() {
    for command in ["/new", "/clear"] {
        let path = PathBuf::from("D:/workspace");
        let mut app = test_app(command, path.clone(), path);
        app.active_session = Some(TuiSession {
            id: "original".into(),
        });
        app.active_agent_run_id = Some("working".into());
        app.transcript
            .push(TranscriptLine::User("Earlier prompt".into()));
        handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert!(app.active_session.is_none());
        assert!(!has_active_agent_run(&app));
        assert!(app.input.is_empty() && app.transcript.is_empty());
        session_views::restore(&mut app, "original");
        assert_eq!(app.active_agent_run_id.as_deref(), Some("working"));
        assert_eq!(
            app.transcript,
            vec![TranscriptLine::User("Earlier prompt".into())]
        );
    }
}

fn open_occupied_session(available: Arc<AtomicBool>, draft_image: Option<PathBuf>) -> App {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/resume", path.clone(), path);
    app.runtime = Some(
        RuntimeClient::from_test_request_handler_with_session_ownership(move |frame| {
            let request = &frame["params"]["request"];
            let id = request["sessionId"].as_str().unwrap_or_default();
            match frame["method"].as_str().unwrap() {
                "session/catalog" if request["mode"] == "pinned" => {
                    json!({"items": [], "nextCursor": null, "reset": false})
                }
                "session/catalog" => json!({
                    "items": [{"id": "session-paged", "title": "Original", "cwd": "D:/workspace", "isPinned": false}],
                    "nextCursor": null, "reset": false
                }),
                "_centaeris/session/claim-writer" => {
                    json!({"claimed": id == "session-fork" || available.load(Ordering::SeqCst)})
                }
                "_centaeris/session/fork-before-user-turn" => {
                    assert_eq!(id, "session-paged");
                    json!({
                        "id": "session-fork", "title": "Fork", "cwd": "D:/workspace",
                        "draft": "请检查 [Image #1]",
                        "attachments": [{
                            "placeholder": "[Image #1]",
                            "localPath": draft_image.as_ref().expect("draft image"),
                        }],
                    })
                }
                "_centaeris/session/agent-runs/attach" => {
                    assert!(id == "session-fork" || available.load(Ordering::SeqCst));
                    json!({"transitionReason": "viewer_attached"})
                }
                "_centaeris/session/agent-runs" => json!({"agentRuns": []}),
                "_centaeris/session/activate" => {
                    assert!(id == "session-fork" || available.load(Ordering::SeqCst));
                    json!({"id": id, "title": "Writable session", "cwd": "D:/workspace", "isPinned": false})
                }
                "transcript/page" => {
                    let last_answer = if id == "session-fork" {
                        "First answer"
                    } else {
                        "Committed original history"
                    };
                    let mut page = transcript_test_page(
                        vec![transcript_test_block(
                            "answer",
                            1,
                            12,
                            TranscriptBlockBodyV1::AssistantText {
                                content: TranscriptTextContentV1::inline(last_answer.into()),
                                status: TranscriptBlockStatusV1::Completed,
                            },
                        )],
                        None,
                    );
                    page.session_id = id.to_string();
                    json!({
                        "schema": "transcript.page.rpc.v1",
                        "projectionVersion": TRANSCRIPT_PROJECTION_VERSION_V1,
                        "projectionGeneration": "generation-paged",
                        "projectedSourceHighWater": "12",
                        "targetSourceHighWater": "12",
                        "targetReached": true,
                        "page": page,
                    })
                }
                other => panic!("unexpected read-only request: {other}"),
            }
        }),
    );
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(enter, &mut app);
    wait_for_resume_text(&mut app, "Original");
    handle_key(enter, &mut app);
    assert!(app.read_only_session.is_some());
    app
}

#[test]
fn occupied_session_opens_read_only_until_retry_can_claim_it() {
    let available = Arc::new(AtomicBool::new(false));
    let mut app = open_occupied_session(Arc::clone(&available), None);
    let surface = resume_panel_text(&mut app);
    assert!(surface.contains("Committed original history"));
    assert!(surface.contains("R Retry · F Fork"));
    assert!(app.input_area.is_none());
    handle_key(
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        &mut app,
    );
    handle_terminal_event(Event::Paste("ignored paste".into()), &mut app);
    assert!(app.input.is_empty());

    let retry = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE);
    handle_key(retry, &mut app);
    assert!(app.read_only_session.is_some());
    available.store(true, Ordering::SeqCst);
    handle_key(retry, &mut app);
    assert!(app.read_only_session.is_none());
    assert_eq!(app.active_session.as_ref().unwrap().id, "session-paged");
    assert!(!has_active_agent_run(&app));
    resume_panel_text(&mut app);
    assert!(app.input_area.is_some());
}

#[test]
fn occupied_session_fork_restores_editable_unsent_draft_and_image() {
    let dir = unique_test_dir("fork-draft-image");
    let image_path = dir.join("draft.png");
    image::RgbaImage::new(2, 2).save(&image_path).unwrap();
    let mut app = open_occupied_session(Arc::new(AtomicBool::new(false)), Some(image_path.clone()));
    handle_key(
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        &mut app,
    );

    assert!(app.read_only_session.is_none());
    assert_eq!(app.active_session.as_ref().unwrap().id, "session-fork");
    assert_eq!(app.input, "请检查 [Image #1]");
    assert_eq!(app.input_cursor, app.input.len());
    assert_eq!(app.draft_image_attachments.len(), 1);
    let image = &app.draft_image_attachments[0];
    assert_eq!(&app.input[image.start..image.end], "[Image #1]");
    assert_eq!(image.local_path, image_path);
    assert!(image_path.is_file());
    assert!(!has_active_agent_run(&app));
    let surface = resume_panel_text(&mut app);
    assert!(surface.contains("First answer"));
    assert!(!surface.contains("Committed original history"));
    let next_image_path = dir.join("next.png");
    image::RgbaImage::new(2, 2).save(&next_image_path).unwrap();
    insert_draft_image(&mut app, next_image_path.clone());
    let next_image = &app.draft_image_attachments[1];
    assert_eq!(&app.input[next_image.start..next_image.end], "[Image #2]");
    clear_composer(&mut app);
    assert!(!image_path.exists());
    assert!(!next_image_path.exists());
    std::fs::remove_dir(&dir).expect("remove empty fork test directory");
}

#[test]
fn resume_deletes_only_after_explicit_confirmation_and_a_matching_receipt() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/resume", path.clone(), path);
    app.active_session = Some(TuiSession {
        id: "disposable".into(),
    });
    app.transcript
        .push(TranscriptLine::Summary("Visible history".into()));
    let (delete_seen, receipt_request) = mpsc::channel();
    let (send_receipt, receipt) = mpsc::channel();
    let deleted = std::sync::atomic::AtomicBool::new(false);
    app.runtime = Some(RuntimeClient::from_test_request_handler(move |frame| {
        let request = &frame["params"]["request"];
        match frame["method"].as_str().unwrap() {
            "session/catalog" => {
                let items = if request["mode"] == "recent"
                    && !deleted.load(std::sync::atomic::Ordering::Relaxed)
                {
                    json!([{"id": "disposable", "title": "Disposable session", "cwd": "D:/workspace", "isPinned": false}])
                } else {
                    json!([])
                };
                json!({"items": items, "nextCursor": null, "reset": false})
            }
            "_centaeris/session/delete" => {
                delete_seen.send(request["sessionId"].clone()).unwrap();
                receipt.recv_timeout(Duration::from_secs(2)).unwrap();
                deleted.store(true, std::sync::atomic::Ordering::Relaxed);
                json!({"deletedSessionId": "disposable", "deletedSessionIds": ["disposable"]})
            }
            other => panic!("unexpected delete request: {other}"),
        }
    }));
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    handle_key(key(KeyCode::Enter), &mut app);
    wait_for_resume_text(&mut app, "Disposable session");
    handle_key(key(KeyCode::Char('d')), &mut app);
    assert!(resume_panel_text(&mut app).contains("y Delete"));
    handle_key(key(KeyCode::Enter), &mut app);
    handle_key(key(KeyCode::Char('n')), &mut app);
    assert!(receipt_request.try_recv().is_err());
    handle_key(key(KeyCode::Char('d')), &mut app);
    assert!(resume_panel_text(&mut app).contains("Delete Disposable session?"));
    handle_key(key(KeyCode::Char('y')), &mut app);
    assert_eq!(
        receipt_request
            .recv_timeout(Duration::from_secs(2))
            .unwrap(),
        "disposable"
    );
    assert!(resume_panel_text(&mut app).contains("Disposable session"));
    assert_eq!(app.active_session.as_ref().unwrap().id, "disposable");
    send_receipt.send(()).unwrap();
    wait_for_resume_text(&mut app, "No matching sessions");
    assert!(app.active_session.is_none());
    assert!(app.transcript.is_empty());
}

#[test]
fn resume_delete_requires_a_fresh_confirmation_after_target_change_or_resize() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/resume", path.clone(), path);
    app.runtime = Some(RuntimeClient::from_test_request_handler(|frame| {
        assert_eq!(frame["method"], "session/catalog");
        let items = if frame["params"]["request"]["mode"] == "pinned" {
            json!([])
        } else {
            json!([
                {"id": "first", "title": "First", "cwd": "D:/workspace", "isPinned": false},
                {"id": "second", "title": "Second", "cwd": "D:/workspace", "isPinned": false},
            ])
        };
        json!({"items": items, "nextCursor": null, "reset": false})
    }));
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    handle_key(key(KeyCode::Enter), &mut app);
    wait_for_resume_text(&mut app, "Second");
    handle_key(key(KeyCode::Char('d')), &mut app);
    let frame = resume_panel_text(&mut app);
    assert!(frame.contains("Delete First?"));
    let (x, y) = frame
        .lines()
        .enumerate()
        .find_map(|(row, line)| line.find("y Delete").map(|col| (col as u16, row as u16)))
        .unwrap();
    handle_key(key(KeyCode::Char('n')), &mut app);
    handle_key(key(KeyCode::Down), &mut app);
    handle_key(key(KeyCode::Char('d')), &mut app);
    handle_key(key(KeyCode::Char('y')), &mut app);
    send_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    assert!(resume_panel_text(&mut app).contains("Delete Second?"));

    handle_terminal_event(Event::Resize(60, 15), &mut app);
    handle_key(key(KeyCode::Char('y')), &mut app);
    send_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    assert!(resume_panel_text(&mut app).contains("Delete Second?"));
}

#[test]
fn retired_slash_commands_fail_without_changing_the_chat() {
    for command in [
        "/state",
        "/process",
        "/ssh",
        "/schedule",
        "/plugins",
        "/mcp",
        "/trim-history",
    ] {
        for suffix in ["", " argument"] {
            let input = format!("{command}{suffix}");
            let path = PathBuf::from("D:/workspace");
            let mut app = test_app(&input, path.clone(), path);
            app.active_session = Some(TuiSession {
                id: "active".into(),
            });
            app.active_agent_run_id = Some("running".into());
            app.active_agent_run_ids.insert("running".into());
            app.transcript
                .push(TranscriptLine::Summary("Keep this answer".into()));
            assert!(!handle_key(
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &mut app
            ));
            assert_eq!(
                app.message,
                Some(format!("Unknown slash command: {command}"))
            );
            assert_eq!(app.input, input);
            assert_eq!(app.active_session.as_ref().unwrap().id, "active");
            assert_eq!(app.active_agent_run_id.as_deref(), Some("running"));
            assert!(app.active_agent_run_ids.contains("running"));
            assert_eq!(
                app.transcript,
                vec![TranscriptLine::Summary("Keep this answer".into())]
            );
            assert!(app.runtime.is_none());
            assert!(app.pending_model_request.is_none());
        }
    }
}

#[test]
fn help_opens_the_available_command_menu() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/help", path.clone(), path);
    handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
    assert!(app.command_panel_open());
    assert_eq!(matching_commands(&app.input).len(), SLASH_COMMANDS.len());
    let view = build_transcript_view(&app, 80);
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .unwrap();
    let text = test_buffer_text(terminal.backend().buffer());
    for command in SLASH_COMMANDS {
        assert!(
            text.contains(command.description),
            "missing {} in {text}",
            command.name
        );
    }
    assert!(app.runtime.is_none());
}

#[test]
fn command_preview_descriptions_share_one_plain_text_column() {
    for command in SLASH_COMMANDS {
        let preview = command_preview_line(*command, false).to_string();
        assert_eq!(
            preview.find(command.description),
            Some(COMMAND_NAME_WIDTH),
            "preview: {preview}"
        );
        assert!(
            !['[', ']', '|', '<', '>']
                .into_iter()
                .any(|character| preview.contains(character)),
            "preview: {preview}"
        );
    }
}

#[test]
fn slash_menu_names_and_composer_share_a_column() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/", path.clone(), path);
    app.selected_command = 2;
    for width in [42, 88] {
        let view = build_transcript_view(&app, width);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, &mut app, &view))
            .unwrap();
        let input = app.input_area.unwrap();
        let menu = app.panel_area.unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(input.x, input.y)].symbol(), "/");
        for y in menu.y..menu.bottom() {
            assert_eq!(buffer[(input.x, y)].symbol(), "/", "menu row {y}");
        }
    }
}

#[test]
fn command_completion_ghost_suffix_has_no_gap() {
    assert_eq!(command_completion_suffix("/mo", 0).as_deref(), Some("del "));
    assert_eq!(command_completion_suffix("/model ", 0), None);
}

#[test]
fn home_risk_combines_registered_and_session_workspaces() {
    let project = unique_test_dir("home-risk-session-workspace");
    let project_root = display_path(&project.canonicalize().expect("canonical project directory"));
    let response = json!({
        "activeWorkspaceRoot": "D:/home",
        "workspaces": [
            {
                "root": "D:/home",
                "name": "Home",
                "activeSessionId": null,
                "sortOrder": 0,
                "updatedAt": 30
            },
            {
                "root": "D:/older",
                "name": "Older",
                "activeSessionId": null,
                "sortOrder": 1,
                "updatedAt": 10
            },
            {
                "root": "D:/newer",
                "name": "Newer",
                "activeSessionId": null,
                "sortOrder": 2,
                "updatedAt": 20
            }
        ],
        "cancelled": false
    });

    let session = |updated_at| {
        json!({
            "id": format!("session-{updated_at}"),
            "title": "Example session",
            "updatedAt": updated_at,
            "lastMessage": null,
            "cwd": &project_root,
            "sessionKind": "main",
            "parentSessionId": null,
            "runtimeJobId": null,
            "sortOrder": 0,
            "isPinned": false,
            "isUnread": false,
            "messageCount": 0,
            "activityState": "inactive"
        })
    };
    let sessions = json!([session(5), session(25)]);
    let workspaces = recent_workspace_choices(response, sessions, "D:/home");
    std::fs::remove_dir(&project).expect("remove empty test workspace");
    assert_eq!(
        workspaces
            .expect("workspace catalog")
            .iter()
            .map(|workspace| workspace.root.as_str())
            .collect::<Vec<_>>(),
        vec![project_root.as_str(), "D:/newer", "D:/older"]
    );
    assert!(recent_workspace_choices(
        json!({
            "activeWorkspaceRoot": null,
            "workspaces": [],
            "cancelled": false,
            "banana": true
        }),
        json!([]),
        "D:/home"
    )
    .is_err());
}

#[test]
fn home_risk_panel_opens_without_a_configured_workspace_and_preserves_the_draft() {
    let workspace = PathBuf::from("D:/home");
    let mut app = test_app("inspect this project", workspace.clone(), workspace);
    app.home_risk_pending = true;
    app.runtime = Some(RuntimeClient::from_test_request_handler(
        |frame| match frame["method"].as_str() {
            Some("workspace_get") => {
                json!({"activeWorkspaceRoot": null, "workspaces": [], "cancelled": true})
            }
            Some("session/list") => {
                assert_eq!(frame["params"], json!({"request": {}}));
                json!([])
            }
            method => panic!("unexpected request from home reminder: {method:?}"),
        },
    ));

    handle_enter(&mut app);
    assert!(app.message.is_none());
    assert!(app.active_session.is_none());
    let view = build_transcript_view(&app, 80);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render home risk panel");
    let buffer = terminal.backend().buffer();
    let rows = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let warning_row = rows
        .iter()
        .position(|row| row.contains("Home directory risk"))
        .expect("warning row");
    let composer_row = rows
        .iter()
        .position(|row| row.contains("inspect this project"))
        .expect("composer row");
    assert!(warning_row < composer_row);
    assert!(rows
        .iter()
        .any(|row| row.contains("Continue in the home directory")));

    handle_home_risk_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert!(app.home_risk_panel.is_none());
    assert_eq!(app.input, "inspect this project");
    assert!(app.home_risk_pending);

    handle_enter(&mut app);
    assert!(app.home_risk_panel.is_some());
}

#[test]
fn home_risk_selection_applies_workspace_activation_or_explicit_home_continue() {
    let home = PathBuf::from("D:/home");
    let mut app = test_app("inspect this project", home.clone(), home);
    let workspace = TuiWorkspaceChoice {
        root: "D:/Projects/example-workspace".to_string(),
        name: "Example Workspace".to_string(),
        active_session_id: None,
        sort_order: 0,
        updated_at: 1,
    };
    apply_workspace_activation(
        &mut app,
        workspace.root.as_str(),
        json!({
            "activeWorkspaceRoot": "D:/Projects/example-workspace",
            "workspaces": [{
                "root": "D:/Projects/example-workspace",
                "name": "Example Workspace",
                "activeSessionId": null,
                "sortOrder": 0,
                "updatedAt": 2
            }],
            "cancelled": false
        }),
    )
    .expect("apply workspace activation");
    assert_eq!(app.session_cwd, "D:/Projects/example-workspace");

    app.workspace_root = "D:/home".to_string();
    app.session_cwd = "D:/home".to_string();
    app.home_risk_pending = true;
    app.home_risk_panel = Some(HomeRiskPanel {
        workspaces: Vec::new(),
        selected: 0,
        notice: None,
    });
    let (_response_tx, response_rx) = std::sync::mpsc::channel();
    app.pending_model_request = Some(PendingModelRequest::LoadingConfig {
        response: RuntimeResponse::from_response_receiver(response_rx),
        command: ModelCommand::Refresh,
    });
    assert!(confirm_home_risk_selection(&mut app).is_err());
    assert!(!app.home_risk_pending);
    assert!(app.home_risk_panel.is_none());
    assert_eq!(app.input, "inspect this project");
}

#[test]
fn tab_completion_accepts_selected_command_with_trailing_space() {
    let workspace = unique_test_dir("workspace-complete");
    let data_root = unique_test_dir("data-complete");
    let mut app = test_app("/mo", workspace.clone(), data_root.clone());
    complete_selected_command(&mut app);
    assert_eq!(app.input, "/model ");
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn welcome_stays_while_the_command_panel_uses_fullscreen_space() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("", workspace.clone(), workspace);
    assert_eq!(input_row_count(&app, 80), 1);

    app.input = "/".to_string();

    assert_eq!(panel_height(&app, 20), SLASH_COMMANDS.len() as u16);
    assert!(app.transcript.is_empty());
}

#[test]
fn slash_command_panel_opens_above_the_composer() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("/", workspace.clone(), workspace);
    let view = build_transcript_view(&app, 80);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render command panel");

    let buffer = terminal.backend().buffer();
    let rows = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let panel_row = rows
        .iter()
        .position(|row| row.contains("Choose model and reasoning effort"))
        .expect("slash command panel row");
    let composer_row = rows
        .iter()
        .rposition(|row| row.contains('│') && row.contains("/model"))
        .expect("composer row");

    assert!(panel_row < composer_row);
}

#[test]
fn user_message_leaves_one_row_before_assistant_text() {
    let lines = transcript_to_lines(
        &[
            TranscriptLine::User("hello".to_string()),
            TranscriptLine::Summary("reply".to_string()),
        ],
        80,
    );

    assert_eq!(rendered_line_text(&lines[0]), "  ❯ hello");
    assert!(rendered_line_text(&lines[1]).is_empty());
    assert_eq!(rendered_line_text(&lines[2]), "  reply");
}

#[test]
fn welcome_uses_one_compact_brand_signature() {
    assert_eq!(
        welcome_line().to_string(),
        format!(
            "Centaeris v{} · Run /help for commands",
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[test]
fn transcript_view_starts_at_the_live_bottom() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("", workspace.clone(), workspace);

    assert!(app.transcript_follow_bottom);
    app.transcript
        .push(TranscriptLine::User("hello".to_string()));
    let view = build_transcript_view(&app, 80);
    assert!(view.total_rows > 0);
    assert!(view.source_rows.is_empty());
}

#[test]
fn transcript_golden_fixture_has_versioned_semantic_operations() {
    let bytes = include_bytes!("../../../core/tests/fixtures/transcript_projection_v1/golden.json");
    assert_eq!(
        format!("{:x}", Sha256::digest(bytes)),
        "904971875d4f105d0a8ae735278017f3f6aab301549083ac3db7ce2f1568c8f7"
    );
    let fixture: Value = serde_json::from_slice(bytes).expect("transcript golden fixture");

    assert_eq!(fixture["schema"], "transcript.golden.v1");
    assert_eq!(fixture["fixtureRevision"], "2026-09-13.1");
    assert_eq!(fixture["scenarioId"], "reasoning-tool-answer");
    assert_eq!(
        fixture["operations"]
            .as_array()
            .expect("operations")
            .iter()
            .filter_map(|operation| operation["kind"].as_str())
            .collect::<Vec<_>>(),
        vec![
            "userText",
            "reasoningReplace",
            "toolStart",
            "toolFinish",
            "assistantTextReplace",
            "terminal",
        ]
    );
}

#[test]
#[ignore = "opt-in rendering baseline; run through scripts/transcript-baseline.ps1"]
fn transcript_rendering_scale_baseline() {
    let workspace = PathBuf::from("D:/workspace");
    let samples = [100usize, 1_000, 10_000]
        .into_iter()
        .map(|size| {
            let mut app = test_app("", workspace.clone(), workspace.clone());
            app.transcript
                .extend((0..size).map(|index| TranscriptLine::Summary(format!("line {index}"))));
            let started_at = Instant::now();
            let view = build_transcript_view(&app, 80);
            let elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0;
            let unchanged_redraw_started_at = Instant::now();
            let unchanged_view = build_transcript_view(&app, 80);
            json!({
                "size": size,
                "elapsedMs": elapsed_ms,
                "unchangedRedrawElapsedMs": unchanged_redraw_started_at.elapsed().as_secs_f64() * 1000.0,
                "inputItems": app.transcript.len(),
                "renderedLines": view.lines.len(),
                "paragraphLineCountCalls": view.lines.len(),
                "unchangedRedrawParagraphLineCountCalls": unchanged_view.lines.len(),
                "reportedRows": view.total_rows,
            })
        })
        .collect::<Vec<_>>();

    let mut boundary_app = test_app("", workspace.clone(), workspace.clone());
    boundary_app
        .transcript
        .extend((0..70_000).map(|index| TranscriptLine::Summary(format!("row {index}"))));
    let boundary_view = build_transcript_view(&boundary_app, 80);

    let mut snapshot_app = test_app("", workspace.clone(), workspace);
    let before = snapshot_app.transcript.len();
    apply_session_event(
        &mut snapshot_app,
        &json!({
            "id": "runtime:model_snapshot:session:turn:1",
            "type": "ModelSnapshot",
            "visibility": "user",
            "payload": {
                "revision": 1,
                "text": "answer",
                "reasoning": {
                    "blockId": "reasoning:req-1",
                    "requestId": "req-1",
                    "text": "inspect"
                }
            }
        }),
        Some("agent-run-1"),
    );
    let snapshot_accepted = !snapshot_app.transcript[before..]
        .iter()
        .any(|line| matches!(line, TranscriptLine::Error(_)));

    println!(
        "{}",
        serde_json::to_string(&json!({
            "schema": "transcript.rendering-baseline.tui.v1",
            "samples": samples,
            "u16Boundary": {
                "inputItems": boundary_app.transcript.len(),
                "reportedRows": boundary_view.total_rows,
                "saturated": boundary_view.total_rows == u64::from(u16::MAX),
            },
            "modelSnapshot": {
                "accepted": snapshot_accepted,
                "targetAccepted": true,
            }
        }))
        .expect("serialize TUI transcript baseline")
    );
}

#[test]
fn session_response_maps_runtime_identity() {
    let response = json!({"id":"chat-1","title":"Shared","cwd":"D:/repo"});
    let session = tui_session_from_response(&response).expect("session response");
    assert_eq!(session.id, "chat-1");
    for invalid in [
        json!({}),
        json!({"id":""}),
        json!({"id":" "}),
        json!({"id":123}),
    ] {
        assert!(tui_session_from_response(&invalid).is_err());
    }
}

#[test]
fn active_agent_runs_parse_running_and_waiting_user() {
    let agent_runs = active_agent_runs_from_response(&json!({
        "agentRuns": [
            {
                "agentRunId": "agent-run-electron",
                "status": "running",
            },
            {
                "agentRunId": "agent-run-tui",
                "status": "waiting_user",
            },
            {
                "agentRunId": "agent-run-done",
                "status": "succeeded",
            }
        ]
    }))
    .expect("active AgentRuns");

    assert_eq!(
        agent_runs,
        vec![
            ActiveAgentRun {
                agent_run_id: "agent-run-electron".to_string(),
                status: "running".to_string(),
            },
            ActiveAgentRun {
                agent_run_id: "agent-run-tui".to_string(),
                status: "waiting_user".to_string(),
            },
        ]
    );
}

#[test]
fn restore_active_agent_runs_marks_waiting_user_as_running_task() {
    let workspace = unique_test_dir("workspace-active-runs");
    let data_root = unique_test_dir("data-active-runs");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    restore_active_agent_runs(
        &mut app,
        vec![ActiveAgentRun {
            agent_run_id: "agent-run-question".to_string(),
            status: "waiting_user".to_string(),
        }],
    );

    assert_eq!(
        app.active_agent_run_id.as_deref(),
        Some("agent-run-question")
    );
    assert!(app.active_agent_run_ids.contains("agent-run-question"));
    assert_eq!(app.process_state, RuntimeDisplayState::WaitingUser);
    assert!(app.agent_run_started_at.is_some());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn live_turn_supplement_renders_plain_supplement_line() {
    let workspace = unique_test_dir("workspace-supplement");
    let data_root = unique_test_dir("data-supplement");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    let payload = json!({
        "type": "session_event",
        "event": {
            "id": "evt-supplement",
            "type": "TurnSupplement",
            "visibility": "user",
            "payload": {"message": "再检查 tests"}
        }
    });

    apply_stream_payload(&mut app, &payload);

    assert_eq!(
        app.transcript,
        vec![TranscriptLine::Supplement("再检查 tests".to_string())]
    );
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn question_required_sets_pending_question_without_number_parsing() {
    let workspace = unique_test_dir("workspace-question-required");
    let data_root = unique_test_dir("data-question-required");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-question",
                "type": "QuestionRequired",
                "visibility": "user",
                "processState": "waiting_user",
                "payload": {
                    "questionRequest": {
                        "id": "q-1",
                        "question": "Which branch should I inspect?",
                        "options": ["main branch", "release branch"],
                        "multiSelect": true,
                        "required": true
                    }
                }
            }
        }),
    );

    assert_eq!(
        app.pending_question,
        Some(PendingQuestion {
            id: "q-1".to_string(),
            question: "Which branch should I inspect?".to_string(),
            options: vec!["main branch".to_string(), "release branch".to_string()],
            multi_select: true,
            required: true,
        })
    );
    assert_eq!(app.process_state, RuntimeDisplayState::WaitingUser);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn question_answer_request_uses_answer_text_verbatim() {
    assert_eq!(
        question_answer_request("chat-1", "q-1", "Use the release branch"),
        json!({
            "sessionId": "chat-1",
            "questionId": "q-1",
            "answerText": "Use the release branch",
            "answers": [],
        })
    );
}

#[test]
fn agent_run_terminal_preserves_pending_question() {
    let workspace = unique_test_dir("workspace-question-terminal");
    let data_root = unique_test_dir("data-question-terminal");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.active_agent_run_id = Some("agent-run-question".to_string());
    app.active_agent_run_ids
        .insert("agent-run-question".to_string());
    app.pending_question = Some(PendingQuestion {
        id: "q-1".to_string(),
        question: "Which branch?".to_string(),
        options: Vec::new(),
        multi_select: false,
        required: true,
    });
    app.process_state = RuntimeDisplayState::WaitingUser;

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "agentRunId": "agent-run-question",
            "taskId": "task-question",
            "event": {
                "id": "evt-question-completed",
                "type": "AgentRunCompleted",
                "taskId": "task-question",
                "visibility": "internal",
                "payload": {"doneReason": "finalized"}
            }
        }),
    );

    assert!(app.active_agent_run_id.is_none());
    assert!(app.active_agent_run_ids.is_empty());
    assert!(app.pending_question.is_some());
    assert_eq!(app.process_state, RuntimeDisplayState::WaitingUser);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn empty_question_answer_fails_without_clearing_pending_question() {
    let workspace = unique_test_dir("workspace-question-empty");
    let data_root = unique_test_dir("data-question-empty");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.pending_question = Some(PendingQuestion {
        id: "q-1".to_string(),
        question: "Which branch?".to_string(),
        options: Vec::new(),
        multi_select: false,
        required: true,
    });

    let error = submit_question_answer(&mut app, "  ".to_string()).expect_err("empty");

    assert_eq!(error, "question answer cannot be empty");
    assert!(app.pending_question.is_some());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_event_uses_first_operation_title_and_bounds_result_lines() {
    let workspace = unique_test_dir("workspace-tool");
    let data_root = unique_test_dir("data-tool");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-1", "bash");
    let payload = json!({
        "type": "session_event",
        "event": {
            "id": "evt-tool",
            "type": "ToolResult",
            "status": "done",
            "toolName": "bash",
            "payload": {
                "callId": "call-1",
                "resultState": "successWithOutput",
                "operations": [
                    {
                        "callId": "call-1",
                        "toolName": "bash",
                        "kind": "command",
                        "title": "bash",
                        "commandPreview": "{\"command\":\"rg session/prompt ui\"}",
                        "outputPreview": "one\ntwo\nthree\nfour\nfive\nsix"
                    }
                ],
                "modelInputImages": []
            }
        }
    });

    apply_stream_payload(&mut app, &payload);
    assert!(!app.tool_projection.has_open_calls());
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(_)]
    ));

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-narrative",
                "type": "ModelTextDelta",
                "payload": {"delta": "done"}
            }
        }),
    );
    assert!(!app.tool_projection.has_open_calls());
    assert_eq!(app.assistant_buffer, "done");

    match app.transcript.first().expect("tool line") {
        TranscriptLine::Tool(tool) => {
            assert_eq!(tool.subject, "rg session/prompt ui");
            assert_eq!(
                tool.result_blocks,
                vec![ToolResultBlock::Text {
                    lines: vec![
                        TextResultLine::Text("one".to_string()),
                        TextResultLine::Text("two".to_string()),
                        TextResultLine::Text("three".to_string()),
                        TextResultLine::Text("four".to_string()),
                        TextResultLine::Text("five".to_string()),
                        TextResultLine::Text("six".to_string()),
                    ],
                }]
            );
        }
        other => panic!("unexpected line: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_result_projects_strict_execution_image_facts() {
    let call = json!({
        "status": "running",
        "toolName": "read"
    });
    let call_payload = json!({"callId": "call-image"});
    let result = json!({
        "status": "done",
        "toolName": "read"
    });
    let result_payload = json!({
        "callId": "call-image",
        "resultState": "successWithOutput",
        "operations": [{
            "callId": "call-image",
            "toolName": "read",
            "path": "generated.png"
        }],
        "modelInputImages": [{
            "sourceKind": "executionFile",
            "image": {
                "path": "generated.png",
                "contentType": "image/png",
                "sha256": format!("sha256:{}", "0".repeat(64)),
                "byteLength": 1,
                "widthPx": 1,
                "heightPx": 1,
                "placeholder": "[Image observation: call-image]"
            }
        }]
    });
    let mut projection = ToolProjection::default();
    projection
        .apply_event("ToolCall", &call, &call_payload)
        .expect("image ToolCall");
    let update = projection
        .apply_event("ToolResult", &result, &result_payload)
        .expect("image ToolResult");
    let images = update.settled.expect("settled tool").images;
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].path, "generated.png");

    let mut invalid = result_payload;
    invalid["modelInputImages"][0]["image"]["banana"] = Value::Bool(true);
    let mut projection = ToolProjection::default();
    projection
        .apply_event("ToolCall", &call, &call_payload)
        .expect("invalid image ToolCall");
    assert!(projection
        .apply_event("ToolResult", &result, &invalid)
        .expect_err("unknown image field must fail")
        .contains("invalid payload.modelInputImages"));
}

#[test]
fn write_tool_diff_preview_uses_diff_block_with_line_numbers_and_cap() {
    let workspace = unique_test_dir("workspace-diff");
    let data_root = unique_test_dir("data-diff");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    let mut diff = String::from("@@ -1,30 +1,30 @@\n");
    for line in 1..=30 {
        diff.push_str(format!("-old {line}\n+new {line}\n").as_str());
    }
    apply_tool_call(&mut app, "call-edit", "edit");
    let payload = json!({
        "type": "session_event",
        "event": {
            "id": "evt-diff",
            "type": "ToolResult",
            "status": "done",
            "toolName": "edit",
            "payload": {
                "callId": "call-edit",
                "resultState": "successWithOutput",
                "operations": [
                    {
                        "callId": "call-edit",
                        "toolName": "edit",
                        "path": "packages/cli/src/main.rs",
                        "diffPreview": diff
                    }
                ],
                "modelInputImages": []
            }
        }
    });

    apply_stream_payload(&mut app, &payload);
    seal_active_tool_calls(&mut app);

    match app.transcript.first().expect("tool line") {
        TranscriptLine::Tool(tool) => match tool.result_blocks.as_slice() {
            [ToolResultBlock::Diff {
                rows, hidden_lines, ..
            }] => {
                assert!(rows.len() <= DIFF_PREVIEW_MAX_ROWS);
                assert!(*hidden_lines > 0);
                assert_eq!(rows.first().and_then(|row| row.line_number), Some(1));
                assert!(matches!(
                    rows.first().map(|row| &row.kind),
                    Some(DiffRowKind::Delete)
                ));
                assert!(rows
                    .iter()
                    .any(|row| matches!(row.kind, DiffRowKind::Hidden(hidden) if hidden > 0)));
            }
            other => panic!("unexpected blocks: {other:?}"),
        },
        other => panic!("unexpected line: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn replacement_diff_preview_projects_without_a_protocol_error() {
    let workspace = unique_test_dir("workspace-replacement-diff");
    let data_root = unique_test_dir("data-replacement-diff");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-edit", "edit");
    apply_tool_result(
        &mut app,
        "call-edit",
        "successWithOutput",
        "done",
        json!([{
            "callId": "call-edit",
            "kind": "edit",
            "path": "castle/index.html",
            "diffPreview": "--- castle/index.html\n+++ castle/index.html\n@@ replacement 1 @@\n-old height\n+new height"
        }]),
    );

    match app.transcript.as_slice() {
        [TranscriptLine::Tool(ToolTranscriptLine { result_blocks, .. })] => {
            assert!(matches!(
                result_blocks.as_slice(),
                [ToolResultBlock::Diff { path, rows, .. }]
                    if path == "castle/index.html"
                        && rows.iter().all(|row| row.line_number.is_none())
                        && matches!(rows[0].kind, DiffRowKind::Delete)
                        && rows[0].text == "old height"
                        && matches!(rows[1].kind, DiffRowKind::Insert)
                        && rows[1].text == "new height"
            ));
        }
        other => panic!("unexpected transcript: {other:?}"),
    }
    assert!(!app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn failed_edit_projects_its_error_without_requiring_a_diff() {
    let workspace = unique_test_dir("workspace-failed-edit");
    let data_root = unique_test_dir("data-failed-edit");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-edit", "edit");
    apply_tool_result(
        &mut app,
        "call-edit",
        "failed",
        "error",
        json!([{
            "callId": "call-edit",
            "kind": "edit",
            "error": "unknown field `path`, expected `oldText` or `newText`"
        }]),
    );

    match app.transcript.as_slice() {
        [TranscriptLine::Tool(tool)] => {
            assert_eq!(stable_tool_title(tool), "Failed to edit files");
            assert_eq!(tool.result_states, vec![ToolResultState::Failed]);
            assert_eq!(
                tool.result_blocks,
                vec![ToolResultBlock::Text {
                    lines: vec![TextResultLine::Text(
                        "unknown field `path`, expected `oldText` or `newText`".to_string()
                    )]
                }]
            );
        }
        other => panic!("unexpected transcript: {other:?}"),
    }
    assert!(!app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn diff_rows_keep_default_foreground_and_dim_with_background() {
    let row = DiffRow {
        line_number: Some(12),
        kind: DiffRowKind::Insert,
        text: "added line".to_string(),
    };

    let rendered = diff_row_line(&row, 2, 80, "    ");

    assert!(rendered.spans.iter().all(|span| span.style.fg.is_none()));
    assert!(rendered
        .spans
        .iter()
        .all(|span| span.style.bg == Some(theme().diff_add_bg)));
    assert!(rendered
        .spans
        .iter()
        .all(|span| span.style.add_modifier.contains(Modifier::DIM)));
}

#[test]
fn write_tool_without_diff_preview_reports_protocol_error() {
    let workspace = unique_test_dir("workspace-tool-write-without-diff");
    let data_root = unique_test_dir("data-tool-write-without-diff");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-1", "edit");
    apply_tool_result(
        &mut app,
        "call-1",
        "successWithOutput",
        "done",
        json!([{"callId": "call-1", "kind": "edit", "path": "src/main.rs"}]),
    );

    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(tool), TranscriptLine::Error(error)]
            if tool.running
                && error.contains("successful write operation missing diffPreview")
    ));
    assert!(app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn stable_tool_titles_use_past_tense_and_truthful_outcomes() {
    let no_output = test_tool_line(
        ToolActionKind::Command,
        "cargo fmt --check",
        vec![json!({"kind": "command"})],
        vec![ToolResultState::SuccessNoOutput],
    );
    assert_eq!(stable_tool_title(&no_output), "Ran cargo fmt --check");
    assert_eq!(
        empty_tool_result_detail(&no_output).as_deref(),
        Some("No output")
    );

    let failed = test_tool_line(
        ToolActionKind::Command,
        "cargo test",
        vec![json!({"kind": "command"})],
        vec![ToolResultState::Failed],
    );
    assert_eq!(stable_tool_title(&failed), "Failed to run cargo test");
    assert_eq!(
        empty_tool_result_detail(&failed).as_deref(),
        Some("No error output")
    );

    let edited = test_tool_line(
        ToolActionKind::Edit,
        "src/a.rs",
        vec![
            json!({"kind": "edit", "path": "src/a.rs", "added": 2, "removed": 1}),
            json!({"kind": "edit", "path": "src/b.rs", "added": 1, "removed": 0}),
        ],
        vec![
            ToolResultState::SuccessWithOutput,
            ToolResultState::SuccessWithOutput,
        ],
    );
    assert_eq!(stable_tool_title(&edited), "Edited 2 files (+3 -1)");
    assert_eq!(empty_tool_result_detail(&edited), None);
}

#[test]
fn bash_description_is_the_title_while_the_command_remains_visible() {
    let workspace = unique_test_dir("workspace-tool-description");
    let data_root = unique_test_dir("data-tool-description");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_described_bash_call(&mut app, "call-1");
    let TranscriptLine::Tool(running) = &app.transcript[0] else {
        panic!("expected running tool row");
    };
    assert_eq!(stable_tool_title(running), "Run focused tests");
    assert_eq!(
        running.command.as_deref(),
        Some("cargo test -p centaeris-tui")
    );

    apply_tool_result(
        &mut app,
        "call-1",
        "successNoOutput",
        "done",
        json!([{"callId": "call-1", "kind": "command"}]),
    );
    let TranscriptLine::Tool(settled) = &app.transcript[0] else {
        panic!("expected settled tool row");
    };
    assert_eq!(stable_tool_title(settled), "Run focused tests");
    let rendered = transcript_to_lines(&app.transcript, 100);
    assert!(rendered_lines_text(&rendered).contains("Ran cargo test -p centaeris-tui"));

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn external_tools_use_called_instead_of_ran() {
    let tool = test_tool_line(
        ToolActionKind::Tool,
        "mcp.github.fetch_run",
        vec![json!({"kind": "tool"})],
        vec![ToolResultState::SuccessNoOutput],
    );
    assert_eq!(stable_tool_title(&tool), "Called mcp.github.fetch_run");
}

#[test]
fn partial_read_shows_its_range_and_coverage_not_a_generic_truncation_label() {
    let workspace = unique_test_dir("workspace-partial-read");
    let data_root = unique_test_dir("data-partial-read");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-read", "read");
    apply_tool_result(
        &mut app,
        "call-read",
        "successWithOutput",
        "done",
        json!([{
            "callId": "call-read",
            "kind": "read",
            "path": "castle/index.html",
            "startLine": 1,
            "endLine": 50,
            "totalLines": 1642,
            "nextOffset": 50,
            "truncatedBy": "lines"
        }]),
    );
    let read = match app.transcript.first().expect("read line") {
        TranscriptLine::Tool(tool) => tool.clone(),
        other => panic!("unexpected line: {other:?}"),
    };

    assert_eq!(
        stable_tool_title(&read),
        "Read castle/index.html · lines 1–50"
    );
    assert_eq!(
        empty_tool_result_detail(&read).as_deref(),
        Some("Partial read · 50 of 1642 lines")
    );
    let rendered = transcript_to_lines(&[TranscriptLine::Tool(read)], 100);
    let text = rendered
        .iter()
        .map(rendered_line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Read castle/index.html · lines 1–50"));
    assert!(text.contains("Partial read · 50 of 1642 lines"));
    assert!(!text.contains("Output truncated"));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_output_preview_stays_within_six_physical_rows() {
    let block = ToolResultBlock::Text {
        lines: vec![
            TextResultLine::Text("第一条很长的工具输出必须在终端中按显示宽度裁切".to_string()),
            TextResultLine::Hidden(8),
            TextResultLine::Text("最后一条很长的工具输出必须在终端中按显示宽度裁切".to_string()),
            TextResultLine::Text("final output line".to_string()),
        ],
    };
    let mut lines = Vec::new();
    push_tool_result_block(&mut lines, &block, 24, false);

    assert!(paragraph_line_count(&lines, 24) <= 6);
    assert!(rendered_line_text(&lines[0]).contains("第一"));
    assert!(rendered_line_text(&lines[1]).contains("8 lines hidden"));
    assert!(rendered_line_text(&lines[3]).contains("final output line"));
}

#[test]
fn result_hierarchy_aligns_all_rows_under_the_tool_header() {
    let mut lines = Vec::new();
    push_text_result_block(
        &mut lines,
        &["first output".to_string(), "second output".to_string()],
    );

    assert_eq!(
        lines.iter().map(rendered_line_text).collect::<Vec<_>>(),
        vec!["  │ first output", "  │ second output"]
    );
}

#[test]
fn tool_call_missing_call_id_reports_protocol_error() {
    let workspace = unique_test_dir("workspace-tool-protocol-error");
    let data_root = unique_test_dir("data-tool-protocol-error");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-malformed-tool",
                "type": "ToolCall",
                "status": "running",
                "payload": {}
            }
        }),
    );

    assert_eq!(
        app.transcript,
        vec![TranscriptLine::Error(
            "Protocol error: ToolCall evt-malformed-tool missing payload.callId".to_string()
        )]
    );
    assert!(!app.tool_projection.has_open_calls());
    assert!(app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_result_without_call_reports_protocol_error() {
    let workspace = unique_test_dir("workspace-tool-orphan-result");
    let data_root = unique_test_dir("data-tool-orphan-result");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_tool_result(
        &mut app,
        "call-1",
        "successNoOutput",
        "done",
        json!([{"callId": "call-1", "kind": "command", "commandPreview": "cargo build"}]),
    );

    assert_eq!(
        app.transcript,
        vec![TranscriptLine::Error(
            "Protocol error: ToolResult evt-result-call-1 ToolResult without ToolCall: call-1"
                .to_string()
        )]
    );
    assert!(app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_result_unknown_call_id_loud_fails() {
    let workspace = unique_test_dir("workspace-tool-unknown-call");
    let data_root = unique_test_dir("data-tool-unknown-call");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-1", "bash");

    apply_tool_result(
        &mut app,
        "call-unknown",
        "successNoOutput",
        "done",
        json!([{"callId": "call-unknown", "kind": "command", "commandPreview": "ls"}]),
    );

    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(tool), TranscriptLine::Error(error)]
            if tool.running
                && error.contains("ToolResult without ToolCall: call-unknown")
    ));
    assert!(app.tool_projection.has_open_calls());
    assert!(app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_call_row_settles_in_place() {
    let workspace = unique_test_dir("workspace-tool-transition");
    let data_root = unique_test_dir("data-tool-transition");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_tool_call(&mut app, "call-1", "bash");
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(tool)] if tool.running
    ));
    assert!(app.tool_projection.has_open_calls());

    apply_tool_result(
        &mut app,
        "call-1",
        "successNoOutput",
        "done",
        json!([{"callId": "call-1", "kind": "command", "commandPreview": "cargo fmt --check"}]),
    );
    assert!(!app.tool_projection.has_open_calls());
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(tool)]
            if tool.subject == "cargo fmt --check"
                && tool.result_states == vec![ToolResultState::SuccessNoOutput]
                && !tool.interrupted
    ));

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-narrative",
                "type": "ModelTextDelta",
                "payload": {"delta": "formatting checked"}
            }
        }),
    );

    assert!(!app.tool_projection.has_open_calls());
    assert_eq!(app.assistant_buffer, "formatting checked");
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(tool)]
            if tool.subject == "cargo fmt --check"
                && tool.result_states == vec![ToolResultState::SuccessNoOutput]
                && !tool.interrupted
    ));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn bash_runtime_summary_does_not_reopen_committed_tool_row() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    let call_id = "bash-probe";
    let command = "printf 'probe'";
    let description = "Run Bash probe";
    let call = json!({
        "type": "session_event",
        "event": {
            "id": "committed-call",
            "type": "ToolCall",
            "status": "running",
            "toolName": "bash",
            "payload": {
                "callId": call_id,
                "displayTarget": description,
                "normalizedInput": {"command": command, "description": description}
            }
        }
    });
    let result = json!({
        "type": "session_event",
        "event": {
            "id": "shared-result",
            "type": "ToolResult",
            "status": "done",
            "toolName": "bash",
            "payload": {
                "callId": call_id,
                "resultState": "successWithOutput",
                "modelContent": "probe",
                "modelInputImages": [],
                "operations": [{"kind": "command", "callId": call_id, "toolName": "bash", "exitCode": 0}]
            }
        }
    });
    let summary_call = json!({
        "type": "runtime_event",
        "event": {
            "id": "summary-call",
            "type": "ToolCall",
            "status": "running",
            "toolName": "bash",
            "payload": {"callId": call_id, "command": command, "description": description}
        }
    });
    let mut summary_result = result.clone();
    summary_result["type"] = json!("runtime_event");
    let next = json!({"type":"runtime_event","event":{
        "id":"answer-start", "type":"ModelTextDelta", "payload":{"delta":"Done"}
    }});
    for item in [call, result, summary_call, summary_result, next] {
        apply_stream_payload_for_agent_run(&mut app, &item, Some("run"));
    }

    assert_eq!(app.assistant_buffer, "Done");
    assert!(!app.tool_protocol_error);
    assert!(!app.tool_projection.has_open_calls());
    assert_eq!(app.transcript.len(), 1);
    assert!(matches!(
        app.transcript[0].content(),
        TranscriptLine::Tool(tool)
            if !tool.running && !tool.interrupted && tool.command.as_deref() == Some(command)
                && tool.exit_code == Some(0) && tool.full_text.as_deref() == Some("probe")
    ));
}

#[test]
fn repeated_stream_event_ids_are_applied_once() {
    let workspace = unique_test_dir("workspace-tool-event-dedup");
    let data_root = unique_test_dir("data-tool-event-dedup");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_tool_call(&mut app, "call-1", "read");
    apply_tool_call(&mut app, "call-1", "read");
    apply_tool_result(
        &mut app,
        "call-1",
        "successWithOutput",
        "done",
        json!([{
            "callId": "call-1",
            "path": "castle/index.html",
            "startLine": 1,
            "endLine": 20,
            "totalLines": 20
        }]),
    );
    apply_tool_result(
        &mut app,
        "call-1",
        "successWithOutput",
        "done",
        json!([{
            "callId": "call-1",
            "path": "castle/index.html",
            "startLine": 1,
            "endLine": 20,
            "totalLines": 20
        }]),
    );

    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(tool)] if !tool.running && tool.subject == "castle/index.html"
    ));
    assert!(!app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_activity_events_render_the_current_step_until_result() {
    let workspace = unique_test_dir("workspace-tool-activity");
    let data_root = unique_test_dir("data-tool-activity");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    for (event_type, tool_name, expected) in [
        ("ToolCallPreparing", "read", "Reading files"),
        ("ToolCallReady", "bash", "Running command"),
        ("ToolProgress", "bash", "Running command"),
    ] {
        apply_stream_payload(
            &mut app,
            &json!({
                "type": "session_event",
                "event": {
                    "id": format!("evt-{event_type}"),
                    "type": event_type,
                    "toolName": tool_name,
                    "payload": {"callId": "call-1"}
                }
            }),
        );
        assert_eq!(status_inline(&app).as_deref(), Some(expected));
    }

    apply_tool_call(&mut app, "call-1", "bash");
    apply_tool_result(
        &mut app,
        "call-1",
        "successNoOutput",
        "done",
        json!([{"callId": "call-1", "kind": "command", "commandPreview": "cargo check"}]),
    );

    assert!(!app.tool_projection.has_open_calls());
    assert!(status_inline(&app).is_none());
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(_)]
    ));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_activity_missing_tool_name_loud_fails() {
    let workspace = unique_test_dir("workspace-tool-activity-error");
    let data_root = unique_test_dir("data-tool-activity-error");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-ready-missing-name",
                "type": "ToolCallReady",
                "payload": {"callId": "call-1"}
            }
        }),
    );

    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Error(error)]
            if error.contains("ToolCallReady evt-ready-missing-name missing event.toolName")
    ));
    assert!(app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn parallel_tool_calls_keep_individual_rows_in_call_order() {
    let workspace = unique_test_dir("workspace-tool-multi");
    let data_root = unique_test_dir("data-tool-multi");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    for call_id in ["call-1", "call-2"] {
        apply_tool_call(&mut app, call_id, "read");
    }
    assert!(app.tool_projection.has_open_calls());

    for (index, call_id) in ["call-2", "call-1"].into_iter().enumerate() {
        apply_tool_result(
            &mut app,
            call_id,
            "successWithOutput",
            "done",
            json!([{
                "callId": call_id,
                "kind": "read",
                "path": format!("src/{call_id}.rs"),
                "text": format!("Read {call_id}")
            }]),
        );
        if index == 0 {
            assert!(app.tool_projection.has_open_calls());
            assert!(matches!(
                app.transcript.as_slice(),
                [TranscriptLine::Tool(first), TranscriptLine::Tool(second)]
                    if first.running && !second.running
            ));
        }
    }

    seal_active_tool_calls(&mut app);
    assert!(!app.tool_projection.has_open_calls());
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(first), TranscriptLine::Tool(second)]
            if first.operations[0].call_id == "call-1"
                && second.operations[0].call_id == "call-2"
                && !first.running
                && !second.running
    ));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn next_tool_call_keeps_its_own_row() {
    let workspace = unique_test_dir("workspace-tool-batches");
    let data_root = unique_test_dir("data-tool-batches");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_tool_call(&mut app, "call-1", "bash");
    apply_tool_result(
        &mut app,
        "call-1",
        "successNoOutput",
        "done",
        json!([{"callId": "call-1", "kind": "command", "commandPreview": "cargo build"}]),
    );

    apply_tool_call(&mut app, "call-2", "edit");
    assert!(app.tool_projection.has_open_calls());
    assert!(matches!(
        app.tool_projection.open_call_ids().as_slice(),
        [call_id] if *call_id == "call-2"
    ));
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(first), TranscriptLine::Tool(second)]
            if first.operations[0].call_id == "call-1"
                && !first.interrupted
                && second.running
    ));

    apply_tool_result(
        &mut app,
        "call-2",
        "successWithOutput",
        "done",
        json!([{
            "callId": "call-2",
            "kind": "edit",
            "path": "src/a.rs",
            "diffPreview": "@@ -1 +1 @@\n-old\n+new"
        }]),
    );
    seal_active_tool_calls(&mut app);
    assert!(matches!(
        app.transcript.as_slice(),
        [
            TranscriptLine::Tool(first),
            TranscriptLine::Tool(second),
        ] if first.operations[0].call_id == "call-1"
            && second.operations[0].call_id == "call-2"
    ));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn interrupted_calls_settle_individually_without_fabricating_result_state() {
    let workspace = unique_test_dir("workspace-tool-interrupted");
    let data_root = unique_test_dir("data-tool-interrupted");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_tool_call(&mut app, "call-1", "bash");
    apply_tool_call(&mut app, "call-2", "bash");

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-stop",
                "type": "Error",
                "payload": {"message": "task cancelled"}
            }
        }),
    );

    assert!(matches!(
        app.transcript.as_slice(),
        [
            TranscriptLine::Tool(first),
            TranscriptLine::Tool(second),
            TranscriptLine::Error(_),
        ] if first.interrupted
            && second.interrupted
            && first.result_states.is_empty()
            && second.result_states.is_empty()
            && first.operations.is_empty()
            && second.operations.is_empty()
    ));
    assert!(!app.tool_projection.has_open_calls());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn agent_run_terminal_seals_calls_then_commits_buffer() {
    let workspace = unique_test_dir("workspace-tool-terminal");
    let data_root = unique_test_dir("data-tool-terminal");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_tool_call(&mut app, "call-1", "bash");
    apply_tool_result(
        &mut app,
        "call-1",
        "successNoOutput",
        "done",
        json!([{"callId": "call-1", "kind": "command", "commandPreview": "cargo build"}]),
    );
    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-final",
                "type": "Final",
                "payload": {"content": "built"}
            }
        }),
    );
    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "agentRunId": "agent-run-1",
            "taskId": "task-1",
            "event": {
                "id": "evt-task-completed",
                "type": "AgentRunCompleted",
                "taskId": "task-1",
                "visibility": "internal",
                "payload": {"doneReason": "finalized"}
            }
        }),
    );

    assert!(matches!(
        app.transcript.iter().filter(|line| !matches!(line, TranscriptLine::RunBoundary { .. })).map(TranscriptLine::content).collect::<Vec<_>>().as_slice(),
        [
            TranscriptLine::Tool(tool),
            TranscriptLine::LiveAssistant { markdown, .. },
        ] if !tool.interrupted
            && markdown == "built"
    ));
    assert!(!app.tool_projection.has_open_calls());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn streaming_text_commits_before_each_tool_call() {
    let workspace = unique_test_dir("workspace-tool-text-order");
    let data_root = unique_test_dir("data-tool-text-order");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-text-a",
                "type": "ModelTextDelta",
                "payload": {"delta": "checking "}
            }
        }),
    );
    apply_tool_call(&mut app, "call-1", "bash");
    apply_tool_result(
        &mut app,
        "call-1",
        "successNoOutput",
        "done",
        json!([{"callId": "call-1", "kind": "command", "commandPreview": "cargo build"}]),
    );
    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-text-b",
                "type": "ModelTextDelta",
                "payload": {"delta": "then editing "}
            }
        }),
    );
    apply_tool_call(&mut app, "call-2", "edit");
    apply_tool_result(
        &mut app,
        "call-2",
        "successWithOutput",
        "done",
        json!([{
            "callId": "call-2",
            "kind": "edit",
            "path": "src/a.rs",
            "diffPreview": "@@ -1 +1 @@\n-old\n+new"
        }]),
    );
    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-final",
                "type": "Final",
                "payload": {"content": "done"}
            }
        }),
    );
    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "agentRunId": "agent-run-1",
            "taskId": "task-1",
            "event": {
                "id": "evt-task-completed",
                "type": "AgentRunCompleted",
                "taskId": "task-1",
                "visibility": "internal",
                "payload": {"doneReason": "finalized"}
            }
        }),
    );

    assert!(matches!(
        app.transcript.iter().filter(|line| !matches!(line, TranscriptLine::RunBoundary { .. })).map(TranscriptLine::content).collect::<Vec<_>>().as_slice(),
        [
            TranscriptLine::LiveAssistant { markdown: first, .. },
            TranscriptLine::Tool(build),
            TranscriptLine::LiveAssistant { markdown: middle, .. },
            TranscriptLine::Tool(edit),
            TranscriptLine::LiveAssistant { markdown: last, .. },
        ] if first == "checking "
            && middle == "then editing "
            && last == "done"
            && build.operations[0].call_id == "call-1"
            && edit.operations[0].call_id == "call-2"
    ));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn subagent_lines_land_after_their_tool_call_settles() {
    let workspace = unique_test_dir("workspace-tool-subagent");
    let data_root = unique_test_dir("data-tool-subagent");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_tool_call(&mut app, "call-agent", "agent");
    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-subagent",
                "type": "SubagentSpawned",
                "payload": {"subagentId": "agent-1", "title": "Helper"}
            }
        }),
    );
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(tool)] if tool.running
    ));
    apply_tool_result(
        &mut app,
        "call-agent",
        "successWithOutput",
        "done",
        json!([{"callId": "call-agent", "kind": "tool", "toolName": "agent"}]),
    );
    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-narrative",
                "type": "ModelTextDelta",
                "payload": {"delta": "helped"}
            }
        }),
    );

    assert!(matches!(
        app.transcript.as_slice(),
        [
            TranscriptLine::Tool(tool),
            TranscriptLine::Subagent(subagent),
        ] if !tool.interrupted
            && subagent.title == "Helper"
    ));
    assert!(!app.tool_projection.has_open_calls());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_result_operations_must_match_call_id() {
    let workspace = unique_test_dir("workspace-tool-op-mismatch");
    let data_root = unique_test_dir("data-tool-op-mismatch");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-1", "bash");

    apply_tool_result(
        &mut app,
        "call-1",
        "successNoOutput",
        "done",
        json!([{"callId": "call-other", "kind": "command", "commandPreview": "ls"}]),
    );

    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(tool), TranscriptLine::Error(error)]
            if tool.running
                && error.contains("call-1 has operation with mismatched callId")
    ));
    assert!(app.tool_projection.has_open_calls());
    assert!(app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tool_result_empty_operations_reports_protocol_error() {
    let workspace = unique_test_dir("workspace-tool-empty-ops");
    let data_root = unique_test_dir("data-tool-empty-ops");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-1", "bash");

    apply_tool_result(&mut app, "call-1", "successNoOutput", "done", json!([]));

    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Tool(tool), TranscriptLine::Error(error)]
            if tool.running && error.contains("payload.operations is empty")
    ));
    assert!(app.tool_protocol_error);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn status_summary_replaces_streaming_buffer_without_duplication() {
    let workspace = unique_test_dir("workspace-status-dedup");
    let data_root = unique_test_dir("data-status-dedup");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-delta",
                "type": "ModelTextDelta",
                "payload": {"delta": "Let me also看看"}
            }
        }),
    );
    assert_eq!(app.assistant_buffer, "Let me also看看");

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-status",
                "type": "Status",
                "payload": {"stage": "model_process_summary", "message": "Let me also看看"}
            }
        }),
    );

    assert_eq!(app.assistant_buffer, "");
    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::Summary(text)] if text == "Let me also看看"
    ));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn streaming_code_fence_renders_unclosed_block_in_live_area() {
    let workspace = unique_test_dir("workspace-fence");
    let data_root = unique_test_dir("data-fence");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    app.assistant_buffer = "```rust\nfn main() {}".to_string();
    let lines = build_assistant_live_lines(&app, 80);
    assert_eq!(lines.len(), 2);
    assert!(lines[1]
        .spans
        .iter()
        .skip(1)
        .all(|span| span.style.bg == Some(theme().code_bg)));

    replace_assistant_buffer(&mut app, "```rust\nfn main() {}\n```".to_string());
    let lines = build_assistant_live_lines(&app, 80);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].spans.iter().all(|span| span.style.bg.is_none()));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn streaming_assistant_commits_closed_lines_and_keeps_only_tail_live() {
    let workspace = unique_test_dir("workspace-live-lines");
    let data_root = unique_test_dir("data-live-lines");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    let closed = (0..60)
        .map(|index| format!("line {index}\n"))
        .collect::<String>();
    app.assistant_buffer = format!("{closed}line tail");
    materialize_assistant_prefix(&mut app);

    assert_eq!(app.assistant_emitted_bytes, closed.len());
    let static_markdown = match app.transcript.as_slice() {
        [TranscriptLine::LiveAssistant { markdown, .. }] => markdown,
        other => panic!("expected one materialized stream, got {other:?}"),
    };
    assert!(static_markdown.contains("line 0") && static_markdown.contains("line 59"));
    let tail = build_assistant_live_lines(&app, 80);
    assert_eq!(rendered_lines_text(&tail), "  line tail");

    commit_assistant_buffer(&mut app);
    assert!(app.assistant_buffer.is_empty());
    assert!(transcript_to_lines(&app.transcript, 80)
        .iter()
        .any(|line| rendered_line_text(line).contains("line tail")));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn source_backed_assistant_reflows_to_current_terminal_width() {
    let workspace = unique_test_dir("workspace-live-reflow");
    let data_root = unique_test_dir("data-live-reflow");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    app.assistant_buffer = "a deliberately long streamed sentence\n".to_string();
    materialize_assistant_prefix(&mut app);

    assert!(matches!(
        app.transcript.as_slice(),
        [TranscriptLine::LiveAssistant { markdown, separator: false }]
            if markdown == "a deliberately long streamed sentence\n"
    ));
    let wide = transcript_to_lines(&app.transcript, 80);
    let narrow = transcript_to_lines(&app.transcript, 12);
    assert!(paragraph_line_count(&narrow, 12) > paragraph_line_count(&wide, 80));

    app.assistant_buffer.push_str("next source line\n");
    materialize_assistant_prefix(&mut app);
    assert!(!rendered_lines_text(&transcript_to_lines_from(&app.transcript, 1, 80)).contains("• "));

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn running_tool_group_updates_in_the_owned_transcript() {
    let workspace = unique_test_dir("workspace-running-tool-live");
    let data_root = unique_test_dir("data-running-tool-live");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    apply_tool_call(&mut app, "call-1", "bash");
    let running = build_transcript_view(&app, 80);
    assert!(rendered_lines_text(&running.lines).contains("Running"));
    assert_eq!(running.source_rows.len(), 1);

    apply_tool_result(
        &mut app,
        "call-1",
        "successNoOutput",
        "done",
        json!([{"callId": "call-1", "kind": "command"}]),
    );
    let settled = build_transcript_view(&app, 80);
    assert!(rendered_lines_text(&settled.lines).contains("Ran"));
    assert!(!rendered_lines_text(&settled.lines).contains("Running"));

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn transcript_scroll_disables_and_restores_bottom_following() {
    let workspace = unique_test_dir("workspace-owned-scroll");
    let data_root = unique_test_dir("data-owned-scroll");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.transcript_max_scroll = 20;
    app.transcript_scroll = 20;

    scroll_transcript(&mut app, -3);
    assert_eq!(app.transcript_scroll, 17);
    assert!(!app.transcript_follow_bottom);
    scroll_transcript(&mut app, 3);
    assert_eq!(app.transcript_scroll, 20);
    assert!(app.transcript_follow_bottom);

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn transcript_view_keeps_global_rows_beyond_u16_and_slices_the_viewport() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("", workspace.clone(), workspace);
    app.transcript
        .extend((0..40_000).map(|index| TranscriptLine::Summary(format!("row {index}"))));

    let view = build_transcript_view(&app, 80);
    let scroll = u64::from(u16::MAX) + 100;
    let window = transcript_render_window(&view, scroll, 20);

    assert!(view.total_rows > u64::from(u16::MAX));
    assert!(window.lines.len() < view.lines.len());
    assert!(scroll.saturating_sub(u64::from(window.local_scroll)) <= scroll);
    assert!(u64::from(window.local_scroll) < scroll);
}

#[test]
fn composer_input_redraw_reuses_the_transcript_layout() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("", workspace.clone(), workspace);
    app.transcript
        .push(TranscriptLine::Summary("stable history".to_string()));

    let first = build_cached_transcript_view(&mut app, 80);
    app.input.push('x');
    let second = build_cached_transcript_view(&mut app, 80);

    assert_eq!(first.lines, second.lines);
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    assert_eq!(
        app.transcript_layout_cache
            .as_ref()
            .expect("layout cache")
            .rebuilds,
        1
    );
}

#[test]
fn prepending_history_keeps_the_previous_top_row_anchored() {
    assert_eq!(prepend_anchor_scroll(0, 80, 132), 52);
    assert_eq!(prepend_anchor_scroll(17, 80, 132), 69);
    assert_eq!(prepend_anchor_scroll(17, 132, 80), 17);
}

#[test]
fn model_text_replace_discards_materialized_attempt_before_redraw() {
    let workspace = unique_test_dir("workspace-live-replace");
    let data_root = unique_test_dir("data-live-replace");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    app.assistant_buffer = "old attempt\n".to_string();
    materialize_assistant_prefix(&mut app);

    apply_stream_payload(
        &mut app,
        &json!({
            "type": "session_event",
            "event": {
                "id": "evt-replace",
                "type": "ModelTextReplace",
                "payload": {"content": "replacement"}
            }
        }),
    );

    assert!(app.transcript.is_empty());
    assert_eq!(app.assistant_buffer, "replacement");
    assert_eq!(app.assistant_emitted_bytes, 0);
    assert_eq!(
        rendered_lines_text(&build_assistant_live_lines(&app, 80)),
        "  replacement"
    );
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn status_line_omits_session_id() {
    let workspace = unique_test_dir("workspace-status");
    let data_root = unique_test_dir("data-status");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.active_session = Some(TuiSession {
        id: "chat-1800000000000-42".to_string(),
    });

    let status = status_line(&app, 200).to_string();

    assert!(!status.contains("chat-1800000000000-42"));
    assert!(!status.contains("Idle"));
    assert!(!status.contains("Working"));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn status_indicator_header_maps_process_state_and_protocol_error() {
    let workspace = unique_test_dir("workspace-indicator");
    let data_root = unique_test_dir("data-indicator");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    app.process_state = RuntimeDisplayState::PendingPhase;
    assert_eq!(status_header(&app), "");
    app.process_state = RuntimeDisplayState::Thinking;
    assert_eq!(status_header(&app), "Thinking...");
    app.process_state = RuntimeDisplayState::ToolRunning;
    assert_eq!(status_header(&app), "Running tools...");
    app.process_state = RuntimeDisplayState::WaitingUser;
    assert_eq!(status_header(&app), "Waiting for input");

    app.tool_protocol_error = true;
    assert_eq!(status_header(&app), "Protocol error");
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn status_indicator_projects_deterministic_easter_eggs() {
    let workspace = unique_test_dir("workspace-easter-eggs");
    let data_root = unique_test_dir("data-easter-eggs");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.active_agent_run_id = Some("tachikoma-0".to_string());
    app.process_state = RuntimeDisplayState::PendingPhase;
    for (index, subagent_id) in ["one", "two", "three"].into_iter().enumerate() {
        apply_session_event(
            &mut app,
            &json!({
                "id": format!("subagent-spawned-{index}"),
                "type": "SubagentSpawned",
                "payload": {"subagentId": subagent_id, "title": subagent_id}
            }),
            Some("tachikoma-0"),
        );
    }
    assert_eq!(status_header(&app), "Tachikoma ×3");
    for (index, subagent_id) in ["three", "two"].into_iter().enumerate() {
        apply_session_event(
            &mut app,
            &json!({
                "id": format!("subagent-result-{index}"),
                "type": "SubagentResult",
                "processState": "reviewing",
                "payload": {"subagentId": subagent_id, "summary": "done"}
            }),
            Some("tachikoma-0"),
        );
    }
    assert_eq!(status_header(&app), "Tachikoma ×1");
    apply_session_event(
        &mut app,
        &json!({
            "id": "subagent-result-last",
            "type": "SubagentResult",
            "payload": {"subagentId": "one", "summary": "done"}
        }),
        Some("tachikoma-0"),
    );
    assert_eq!(status_header(&app), "");
    run_event(
        &mut app,
        "tachikoma-0",
        "spawn-again",
        "SubagentSpawned",
        json!({"subagentId": "four", "title": "four"}),
    );
    assert_eq!(status_header(&app), "Running tools...");

    for (run, phase, egg, normal) in [
        (
            "runtime-181",
            "thinking",
            "A faint signal crossed the Wired...",
            "Thinking...",
        ),
        (
            "runtime-0",
            "responding",
            "Emerging from the long tunnel...",
            "Responding...",
        ),
        (
            "runtime-13",
            "compaction",
            "That Keepsake Clover...",
            "Compacting...",
        ),
    ] {
        let mut app = test_app("", workspace.clone(), data_root.clone());
        app.active_agent_run_id = Some(run.into());
        let phase_event = match phase {
            "thinking" => json!({"type":"ModelStatus","processState":"thinking"}),
            "responding" => json!({"type":"ModelTextDelta","payload":{"delta":"Answer"}}),
            _ => json!({"type":"ModelRequestStart","payload":{
                "purpose":"compaction","contextTokenEstimate":10,"initialContent":""
            }}),
        };
        let mut first = phase_event.clone();
        first["id"] = json!("first-phase");
        apply_session_event(&mut app, &first, Some(run));
        assert_eq!(status_header(&app), egg);

        if phase == "compaction" {
            run_event(&mut app, run, "compacted", "PromptCompaction", json!({}));
            assert_eq!(status_header(&app), "");
            assert!(app.transcript.is_empty());
        } else {
            apply_session_event(
                &mut app,
                &json!({
                    "id":"wait", "type":"ModelStatus", "processState":"provider_waiting"
                }),
                Some(run),
            );
            assert_eq!(status_header(&app), "Waiting for model...");
        }

        let mut second = phase_event;
        second["id"] = json!("second-phase");
        apply_session_event(&mut app, &second, Some(run));
        assert_eq!(status_header(&app), normal);
    }
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn model_request_start_and_status_update_live_projection_without_errors() {
    let workspace = unique_test_dir("workspace-model-events");
    let data_root = unique_test_dir("data-model-events");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    let config =
        json!({"modelProviderId":"custom.test","model":"test-a","modelContextTokens":128_000});
    apply_model_display(&mut app, &config);
    assert_eq!(
        status_usage::context_label(&app).as_deref(),
        Some("– / 128k")
    );

    apply_session_event(
        &mut app,
        &json!({
            "id": "runtime:model-start",
            "type": "ModelRequestStart",
            "processState": "thinking",
            "payload": {
                "purpose": "main",
                "contextTokenEstimate": 1234,
                "initialContent": "partial answer"
            }
        }),
        Some("agent-run-1"),
    );
    assert_eq!(app.process_state, RuntimeDisplayState::Thinking);
    assert_eq!(app.assistant_buffer, "partial answer");
    assert!(app.transcript.is_empty());
    assert_eq!(
        status_usage::context_label(&app).as_deref(),
        Some("1.2k / 128k")
    );

    run_event(
        &mut app,
        "agent-run-1",
        "delta",
        "ModelTextDelta",
        json!({"delta":" continues"}),
    );
    assert_eq!(app.process_state, RuntimeDisplayState::Responding);
    assert_eq!(app.assistant_buffer, "partial answer continues");

    apply_session_event(
        &mut app,
        &json!({
            "id": "runtime:model-status",
            "type": "ModelStatus",
            "processState": "provider_waiting",
            "payload": {"message": "waiting"}
        }),
        Some("agent-run-1"),
    );
    assert_eq!(
        app.process_state,
        RuntimeDisplayState::Waiting(WaitReason::Model)
    );
    assert_eq!(app.assistant_buffer, "partial answer continues");
    assert!(app.transcript.is_empty());

    apply_model_display(
        &mut app,
        &json!({"modelProviderId":"custom.test","model":"test-b","modelContextTokens":32_000}),
    );
    assert_eq!(
        status_usage::context_label(&app).as_deref(),
        Some("– / 32k")
    );

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn idle_surface_does_not_request_periodic_redraws() {
    let workspace = unique_test_dir("workspace-redraw");
    let data_root = unique_test_dir("data-redraw");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    let now = Instant::now();
    let last_draw_at = now
        .checked_sub(STATUS_REDRAW_INTERVAL)
        .expect("earlier instant");

    assert!(!periodic_redraw_due(&app, last_draw_at, now));
    app.active_agent_run_id = Some("agent-run-1".to_string());
    assert!(periodic_redraw_due(&app, last_draw_at, now));

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn fit_middle_preserves_requested_width() {
    assert_eq!(fit_middle("abcdef", 6), "abcdef");
    assert_eq!(
        fit_middle("abcdefghijklmnopqrstuvwxyz", 10).chars().count(),
        10
    );
}

#[test]
fn cursor_position_tracks_wrap_boundaries() {
    assert_eq!(cursor_position_in_wrapped("", 10), (0, 0));
    assert_eq!(cursor_position_in_wrapped("abc", 10), (0, 3));
    assert_eq!(
        cursor_position_in_wrapped("abcdefghijk", 10),
        (1, 1),
        "eleventh character wraps to row 1"
    );
    assert_eq!(
        cursor_position_in_wrapped("abcdefghij", 10),
        (0, 10),
        "exactly full segment stays on the first row"
    );
    assert_eq!(
        cursor_position_in_wrapped("a\nbc", 10),
        (1, 2),
        "newline advances the row"
    );
    assert_eq!(
        cursor_position_in_wrapped("abc\n", 10),
        (1, 0),
        "trailing newline puts the cursor at the start of the next row"
    );
}

#[test]
fn cursor_position_counts_full_width_characters_as_two_columns() {
    assert_eq!(cursor_position_in_wrapped("你好", 10), (0, 4));
    assert_eq!(
        cursor_position_in_wrapped("你好abcdef", 10),
        (0, 10),
        "two full-width plus six half-width exactly fill the segment"
    );
    assert_eq!(
        cursor_position_in_wrapped("你好abcdefghi", 10),
        (1, 3),
        "full-width character that no longer fits wraps first"
    );
}

#[test]
fn mouse_position_maps_to_wrapped_unicode_input_boundaries() {
    assert_eq!(
        input_byte_at_position("你好ab", 4, TextPoint { row: 0, column: 2 },),
        "你".len()
    );
    assert_eq!(
        input_byte_at_position("你好ab", 4, TextPoint { row: 1, column: 1 },),
        "你好a".len()
    );
}

#[test]
fn selected_composer_text_is_replaced_in_the_editor() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("hello world", workspace.clone(), workspace);
    app.input_selection_anchor = Some(6);
    app.input_cursor = 11;

    insert_text_at_cursor(&mut app, "Rust");

    assert_eq!(app.input, "hello Rust");
    assert_eq!(app.input_cursor, app.input.len());
    assert!(app.input_selection_anchor.is_none());
}

#[test]
fn composer_mouse_drag_copies_once_and_keeps_the_selection() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("abcdef", workspace.clone(), workspace);
    let copied = record_clipboard(&mut app);
    let view = build_transcript_view(&app, 80);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render composer");
    let area = app.input_area.expect("composer area");
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        let column = if matches!(kind, MouseEventKind::Down(_)) {
            area.x + 1
        } else {
            area.x + 4
        };
        handle_mouse(
            MouseEvent {
                kind,
                column,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
            &mut app,
        );
    }

    assert_eq!(input_selection_range(&app), Some(1..4));
    assert!(app.transcript_selection.is_none());
    assert!(app.message.is_none());
    assert_eq!(&*copied.borrow(), &["bcd".to_string()]);
}

#[test]
fn clicking_image_placeholder_opens_pixel_preview_on_supported_protocols() {
    check_pixel_image_preview(ProtocolType::Kitty, "\x1b_G");
    check_pixel_image_preview(ProtocolType::Iterm2, "\x1b]1337;File=");
}

fn check_pixel_image_preview(protocol: ProtocolType, image_escape: &str) {
    fn pixels(app: &App) -> &PixelImagePreview {
        let Some(ImagePreview::Pixels(preview)) = app.image_preview.as_ref() else {
            panic!("expected pixel preview");
        };
        preview
    }

    let workspace = unique_test_dir("workspace-image-preview");
    let data_root = unique_test_dir("data-image-preview");
    let path = data_root.join("clipboard.png");
    std::fs::write(path.as_path(), test_png_bytes()).expect("write image fixture");
    let mut app = test_app("[Image #1]", workspace.clone(), data_root.clone());
    app.image_picker.set_protocol_type(protocol);
    app.draft_image_attachments.push(DraftImageAttachment {
        start: 0,
        end: app.input.len(),
        local_path: path.clone(),
    });
    let backend = ratatui::backend::TestBackend::new(180, 60);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    let view = build_transcript_view(&app, 180);
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render composer");
    let input = app.input_area.expect("composer area");

    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: input.x + 2,
            row: input.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("request image preview");
    assert!(test_buffer_text(terminal.backend().buffer()).contains("Loading preview…"));
    for _ in 0..1_000 {
        drain_image_preview(&mut app);
        if pixels(&app).protocol.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render image preview");
    let rendered = test_buffer_text(terminal.backend().buffer());
    assert!(rendered.contains("Fit · Path:"), "{rendered}");
    assert!(rendered.contains("Path:"));
    assert!(rendered.contains("clipboard.png"), "{rendered}");
    assert!(!rendered.contains("Image preview"));
    assert!(!rendered.contains("click outside"));
    assert!(rendered.contains("[Image #1]"));
    assert!(rendered.contains(image_escape));
    let card = app.image_preview_area.expect("preview card");
    assert!(card.bottom() <= input.y);
    assert!(card.height <= 2, "small source image must not be upscaled");

    let image_area = pixels(&app).image_area;
    let image_position = Position::new(image_area.x, image_area.y);
    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: image_position.x,
            row: image_position.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render zoom label");
    let rendered = test_buffer_text(terminal.backend().buffer());
    assert!(rendered.contains("1.5× · Path:"), "{rendered}");
    let generation = pixels(&app).generation;
    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: image_position.x,
            row: image_position.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: image_position.x.saturating_add(1),
            row: image_position.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    assert_eq!(
        pixels(&app).generation,
        generation,
        "dragging must not prepare every intermediate frame"
    );
    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: image_position.x.saturating_add(1),
            row: image_position.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    assert_eq!(
        pixels(&app).generation,
        generation + 1,
        "mouse-up must prepare exactly one panned frame"
    );

    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: card.x.saturating_sub(1),
            row: card.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    assert!(app.image_preview.is_none());

    clear_composer(&mut app);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn clicking_image_placeholder_shows_details_without_graphics() {
    let root = unique_test_dir("draft-image-details");
    let path = root.join("clipboard.png");
    let bytes = test_png_bytes();
    std::fs::write(&path, &bytes).unwrap();
    let draft = "before [Image #1] after";
    let mut app = test_app(draft, root.clone(), root.clone());
    app.draft_image_attachments.push(DraftImageAttachment {
        start: "before ".len(),
        end: "before [Image #1]".len(),
        local_path: path.clone(),
    });
    let backend = ratatui::backend::TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let view = build_transcript_view(&app, 100);
    for protocol in [ProtocolType::Halfblocks, ProtocolType::Sixel] {
        app.image_picker.set_protocol_type(protocol);
        terminal
            .draw(|frame| render(frame, &mut app, &view))
            .unwrap();
        let input = app.input_area.unwrap();
        send_mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            input.x + 8,
            input.y,
        );
        terminal
            .draw(|frame| render(frame, &mut app, &view))
            .unwrap();
        let rendered = test_buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("2 × 2 · image/png"));
        assert!(rendered.contains(&format!("Size: {} bytes", bytes.len())));
        assert!(rendered.contains("clipboard.png"));
        assert!(rendered.contains(draft));
        assert!(!rendered.contains("Fit · Path"));
        assert!(!rendered.contains("\x1b"));

        send_mouse(&mut app, MouseEventKind::ScrollUp, input.x, input.y);
        handle_key(
            KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE),
            &mut app,
        );
        handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(app.image_preview.is_none());
        assert_eq!(app.input, draft);
        assert_eq!(app.draft_image_attachments[0].local_path, path);
    }
    clear_composer(&mut app);
    std::fs::remove_dir(&root).unwrap();
}

#[test]
fn tool_result_execution_image_renders_inline_from_verified_workspace_bytes() {
    let workspace = unique_test_dir("workspace-inline-tool-image");
    let data_root = unique_test_dir("data-inline-tool-image");
    let bytes = test_png_bytes();
    let image = ToolImage {
        key: "tool_image:call-image:0".to_string(),
        path: "generated.png".to_string(),
        content_type: "image/png".to_string(),
        sha256: format!("sha256:{:x}", Sha256::digest(bytes.as_slice())),
        byte_length: bytes.len() as u64,
        width_px: 2,
        height_px: 2,
    };
    let decoded = decode_workspace_tool_image(
        json!({
            "root": display_path(workspace.as_path()),
            "path": "generated.png",
            "name": "generated.png",
            "content": "",
            "byteLen": bytes.len(),
            "encoding": "base64",
            "contentKind": "image",
            "mimeType": "image/png",
            "dataUrl": format!(
                "data:image/png;base64,{}",
                general_purpose::STANDARD.encode(bytes.as_slice())
            )
        }),
        display_path(workspace.as_path()).as_str(),
        &image,
    )
    .expect("verified workspace image");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    let mut tool = test_tool_line(
        ToolActionKind::Read,
        "generated.png",
        vec![],
        vec![ToolResultState::SuccessWithOutput],
    );
    tool.images.push(image.clone());
    app.transcript.push(TranscriptLine::Tool(tool));
    app.inline_images.insert(
        image.key.clone(),
        app.image_picker.new_resize_protocol(decoded),
    );
    let view = build_transcript_view(&app, 80);
    assert_eq!(view.images.len(), 1);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render inline image");
    let rendered = test_buffer_text(terminal.backend().buffer());
    assert!(rendered.contains("Path: generated.png"));
    assert!(
        test_buffer_has_rgb_halfblock(terminal.backend().buffer()),
        "image anchor {}, scroll {}, transcript {:?}\n{rendered}",
        view.images[0].row,
        app.transcript_scroll,
        app.transcript_area
    );
    assert!(
        app.text_layout
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .all(|row| !row.to_string().contains('▀')),
        "image cells must not leak into copied transcript text"
    );

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn execution_images_remain_visible_when_completed_tools_move_to_history() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    let mut blocks = Vec::new();
    for (id, tool_name, sequence) in [("capture", "bash", 1), ("image-read", "read", 2)] {
        let mut tool = crate::tool_projection::transcript_page_tool_line(
            id,
            tool_name,
            TranscriptBlockStatusV1::Completed,
            tool_name.into(),
            None,
        );
        tool.images.push(ToolImage {
            key: format!("tool_image:{id}:0"),
            path: format!("{id}.png"),
            content_type: "image/png".into(),
            sha256: format!("sha256:{:x}", Sha256::digest(test_png_bytes())),
            byte_length: test_png_bytes().len() as u64,
            width_px: 2,
            height_px: 2,
        });
        app.transcript
            .push(TranscriptLine::Tool(tool).for_run(Some("run"), false));
        let mut block = run_block(
            &format!("tool:{id}"),
            sequence,
            "run",
            "tool_result",
            TranscriptBlockBodyV1::Tool {
                call_id: id.into(),
                tool_name: tool_name.into(),
                status: TranscriptBlockStatusV1::Completed,
                summary: Some(tool_name.into()),
                summary_ref: None,
                output_ref: None,
            },
        );
        if tool_name == "bash" {
            block.presentation.as_mut().unwrap().operation =
                Some(json!({"commandPreview": "command kept folded"}));
        }
        blocks.push(block);
    }
    blocks.extend(
        history_run_page(Some("agent_run_completed"))
            .blocks
            .into_iter()
            .skip(1),
    );
    app.transcript_paging =
        Some(TranscriptPagingState::open(transcript_test_page(blocks, None)).unwrap());
    for _ in 0..2 {
        sync_materialized_transcript_history(&mut app);
        let view = build_transcript_view(&app, 80);
        assert_eq!(view.images.len(), 2);
        let text = rendered_lines_text(&view.lines);
        assert!(text.contains("Path: capture.png") && text.contains("Path: image-read.png"));
        assert!(text.contains("Useful result"));
        assert!(!text.contains("command kept folded"));
    }
}

#[test]
fn tool_result_execution_image_rejects_decoded_content_over_budget() {
    let workspace = unique_test_dir("workspace-oversized-tool-image");
    let bytes = test_png_bytes();
    let image = ToolImage {
        key: "tool_image:oversized:0".to_string(),
        path: "oversized.png".to_string(),
        content_type: "image/png".to_string(),
        sha256: format!("sha256:{:x}", Sha256::digest(bytes.as_slice())),
        byte_length: bytes.len() as u64,
        width_px: TOOL_IMAGE_MAX_DECODED_PIXELS as u32 + 1,
        height_px: 1,
    };
    let error = decode_workspace_tool_image(
        json!({
            "root": display_path(workspace.as_path()),
            "path": "oversized.png",
            "name": "oversized.png",
            "content": "",
            "byteLen": bytes.len(),
            "encoding": "base64",
            "contentKind": "image",
            "mimeType": "image/png",
            "dataUrl": format!(
                "data:image/png;base64,{}",
                general_purpose::STANDARD.encode(bytes.as_slice())
            )
        }),
        display_path(workspace.as_path()).as_str(),
        &image,
    )
    .expect_err("oversized decoded image must be rejected before decoding");
    assert!(error.contains("decoded image budget"));
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn command_panel_rows_accept_mouse_clicks() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("/he", workspace.clone(), workspace);
    let view = build_transcript_view(&app, 80);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render command panel");
    let area = app.panel_area.expect("command panel area");

    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 2,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );

    assert_eq!(app.input, "/help ");
    assert_eq!(app.input_cursor, app.input.len());
}

#[test]
fn user_message_and_composer_align_across_resizes_and_copy_cleanly() {
    let path = PathBuf::from("D:/workspace");
    let message = format!("你👩‍💻a\n好{}", "x".repeat(44));
    let mut app = test_app(&message, path.clone(), path);
    app.transcript.extend([
        TranscriptLine::Summary("Earlier reply".into()),
        TranscriptLine::User(message.clone()),
        TranscriptLine::Summary("Later reply".into()),
    ]);
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(88, 24)).unwrap();

    for (width, height) in [(88, 24), (42, 24), (42, 16)] {
        app.transcript_selection = None;
        terminal.backend_mut().resize(width, height);
        let view = build_transcript_view(&app, width);
        terminal
            .draw(|frame| render(frame, &mut app, &view))
            .unwrap();

        let history = app.transcript_area.unwrap();
        let input = app.input_area.unwrap();
        let buffer = terminal.backend().buffer();
        let message_start = view.layout.text.find(&message).unwrap();
        let (text_row, text_column) = view.layout.position(message_start);
        let first = history.y + text_row as u16;
        let (last_row, last_column) = view.layout.position(message_start + message.len());
        let last = history.y + last_row as u16;
        let last_composer_row = input.y + input.height - 1;
        let bg = theme().user_prompt().bg.unwrap();
        assert_eq!(history.x, 0);
        assert_eq!(buffer[(history.x, input.y)].symbol(), "│");
        assert_eq!(buffer[(width - 1, input.y)].symbol(), "│");
        for row in first..=last {
            assert_eq!(buffer[(history.x, row)].bg, bg);
            assert_eq!(buffer[(width - 2, row)].bg, bg);
        }
        for gap in [first - 1, last + 1] {
            assert_ne!(buffer[(history.x, gap)].bg, bg);
            assert_ne!(buffer[(width - 2, gap)].bg, bg);
            assert_eq!(buffer[(input.x, gap)].symbol(), " ");
        }
        assert_eq!(buffer[(history.x + 2, first - 2)].symbol(), "E");
        assert_eq!(buffer[(history.x + 2, last + 2)].symbol(), "L");
        assert_ne!(buffer[(width - 1, first)].bg, bg);
        assert_eq!(input.x, history.x + text_column as u16);
        for row in [first, input.y] {
            assert_eq!(buffer[(input.x - 3, row)].symbol(), " ");
            assert_eq!(buffer[(input.x - 2, row)].symbol(), "❯");
            assert_eq!(buffer[(input.x - 1, row)].symbol(), " ");
            assert_eq!(buffer[(input.x, row)].symbol(), "你");
            assert_eq!(buffer[(input.x, row + 1)].symbol(), "好");
        }
        assert_eq!(
            buffer[(input.x, last_composer_row)].symbol(),
            buffer[(input.x, last)].symbol()
        );
        let shortcuts_row = height - 1;
        let shortcut: String = (history.x..history.x + 10)
            .map(|column| buffer[(column, shortcuts_row)].symbol())
            .collect();
        assert_eq!(shortcut, "Enter send");
        assert_eq!(history.y, 1);
        assert_eq!(shortcuts_row - last_composer_row, 2);
        app.transcript_selection = Some(TextSelection {
            anchor: view.layout.offset_at(text_row, text_column),
            head: view.layout.offset_at(last_row, last_column),
        });
        assert_eq!(
            selected_transcript_text(&app).as_deref(),
            Some(message.as_str())
        );
    }
}

#[test]
fn input_height_uses_the_same_display_width_as_the_cursor() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("abc", workspace.clone(), workspace);
    assert_eq!(
        input_row_count(&app, 8),
        2,
        "the caret after a full row stays visible"
    );

    app.input = "你好".to_string();
    assert_eq!(input_row_count(&app, 8), 2, "wide text wraps by columns");
}

#[test]
fn composer_rendering_uses_the_same_hard_wrap_as_mouse_mapping() {
    let lines = hard_wrap_input_lines(vec![Line::from("hello world")], 8);
    assert_eq!(lines.len(), 2);
    assert_eq!(rendered_lines_text(&lines), "hello world");
    assert_eq!(
        lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
        "hello wo"
    );
    assert_eq!(cursor_position_in_wrapped("hello world", 8), (1, 3));
}

#[test]
fn composer_model_label_preserves_the_frame_after_resize() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("first line\nsecond line", workspace.clone(), workspace);
    app.model_effort = Some("low".to_string());
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 4)).unwrap();

    for (width, model) in [
        (80, Some("deepseek-flash")),
        (32, Some("deepseek-flash-with-a-long-model-name")),
        (32, None),
    ] {
        app.model_display = model.map(str::to_owned);
        terminal.backend_mut().resize(width, 4);
        terminal
            .draw(|frame| render_input(frame, frame.area(), &mut app))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let symbol = |column, row| buffer.cell((column, row)).unwrap().symbol();
        assert_eq!(symbol(0, 0), "╭");
        assert_eq!(symbol(width - 1, 0), "╮");
        for row in 1..3 {
            assert_eq!(symbol(0, row), "│");
            assert_eq!(symbol(width - 1, row), "│");
        }
        assert_eq!(symbol(0, 3), "╰");
        assert_eq!(symbol(width - 2, 3), "─");
        assert_eq!(symbol(width - 1, 3), "╯");
        if model.is_some() {
            let bottom: String = (1..width - 2).map(|column| symbol(column, 3)).collect();
            assert!(bottom.contains("(low)"), "{bottom}");
        } else {
            assert!((1..width - 1).all(|column| symbol(column, 3) == "─"));
        }
    }
}

#[test]
fn display_path_strips_windows_verbatim_prefix() {
    assert_eq!(
        display_path(std::path::Path::new(r"\\?\D:\Projects\Centaeris")),
        r"D:\Projects\Centaeris"
    );
    assert_eq!(
        display_path(std::path::Path::new(r"\\?\UNC\server\share")),
        r"\\server\share"
    );
}

#[test]
fn ctrl_c_closes_overlay_then_clears_input_then_exits_idle() {
    let workspace = unique_test_dir("workspace-ctrl-c");
    let data_root = unique_test_dir("data-ctrl-c");
    let mut app = test_app("draft", workspace.clone(), data_root.clone());
    app.show_help = true;

    assert!(!handle_ctrl_c(&mut app));
    assert!(!app.show_help);
    assert_eq!(app.input, "draft");

    assert!(!handle_ctrl_c(&mut app));
    assert!(app.input.is_empty());

    assert!(handle_ctrl_c(&mut app));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn stop_command_is_silent_when_idle() {
    let workspace = unique_test_dir("workspace-stop-idle");
    let data_root = unique_test_dir("data-stop-idle");
    let mut app = test_app("/stop", workspace.clone(), data_root.clone());

    assert!(!handle_enter(&mut app));
    assert!(app.input.is_empty());
    assert!(app.message.is_none());
    assert!(app.runtime.is_none());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn prepare_exit_does_not_locally_cancel_without_runtime() {
    let workspace = unique_test_dir("workspace-exit-running");
    let data_root = unique_test_dir("data-exit-running");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.active_agent_run_id = Some("agent-run-running".to_string());
    app.active_agent_run_ids
        .insert("agent-run-running".to_string());

    assert!(prepare_exit(&mut app));
    assert_eq!(
        app.active_agent_run_id.as_deref(),
        Some("agent-run-running")
    );
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn disconnected_runtime_allows_exit_without_waiting_for_a_response() {
    let workspace = unique_test_dir("workspace-exit-disconnected");
    let data_root = unique_test_dir("data-exit-disconnected");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    let (_event_tx, event_rx) = std::sync::mpsc::channel();
    app.runtime = Some(RuntimeClient::from_test_event_receiver(event_rx, false));

    assert!(prepare_exit(&mut app));

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn accepted_stop_keeps_observing_until_the_terminal_event() {
    let workspace = unique_test_dir("workspace-stop-accepted");
    let mut app = test_app("", workspace.clone(), workspace.clone());
    app.active_session = Some(TuiSession {
        id: "session-running".into(),
    });
    app.active_agent_run_id = Some("run-running".into());
    app.active_agent_run_ids.insert("run-running".into());
    app.process_state = RuntimeDisplayState::PendingPhase;
    app.runtime = Some(RuntimeClient::from_test_request_handler(|frame| {
        assert_eq!(frame["method"], "_centaeris/session/agent-runs/cancel");
        json!({"cancelAccepted": true, "agentRun": {"agentRunId": "run-running", "status": "running"}})
    }));
    stop_active_agent_runs(&mut app, "tui_user_stop").expect("stop accepted");
    assert_eq!(app.active_agent_run_id.as_deref(), Some("run-running"));
    assert!(app.active_agent_run_ids.contains("run-running"));
    assert_ne!(app.process_state, RuntimeDisplayState::Idle);
    run_event(
        &mut app,
        "run-running",
        "terminal",
        "AgentRunInterrupted",
        json!({"reasonType": "cancelled"}),
    );
    assert!(app.active_agent_run_id.is_none());
    assert!(app.active_agent_run_ids.is_empty());
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn terminal_stop_response_recovers_final_text_from_history_without_a_notification() {
    let workspace = unique_test_dir("workspace-stop-terminal-history");
    let mut app = test_app("", workspace.clone(), workspace.clone());
    app.active_session = Some(TuiSession {
        id: "session-paged".into(),
    });
    app.active_agent_run_id = Some("run-running".into());
    app.active_agent_run_ids.insert("run-running".into());
    app.process_state = RuntimeDisplayState::PendingPhase;
    app.transcript
        .push(TranscriptLine::Summary("Incomplete live text".into()));
    app.pending_question = Some(PendingQuestion {
        id: "old-question".into(),
        question: "Stale question".into(),
        options: Vec::new(),
        multi_select: false,
        required: true,
    });
    app.runtime = Some(RuntimeClient::from_test_request_handler(
        |frame| match frame["method"].as_str().unwrap() {
            "_centaeris/session/agent-runs/cancel" => json!({"cancelAccepted": false,
                "agentRun": {"agentRunId": "run-running", "status": "succeeded"}}),
            "_centaeris/session/agent-runs/attach" => {
                json!({"transitionReason": "viewer_attached"})
            }
            "_centaeris/session/agent-runs" => json!({"agentRuns": []}),
            "transcript/page" => json!({
                "schema": "transcript.page.rpc.v1", "projectionVersion": TRANSCRIPT_PROJECTION_VERSION_V1,
                "projectionGeneration": "generation-paged", "projectedSourceHighWater": "12",
                "targetSourceHighWater": "12", "targetReached": true,
                "page": transcript_test_page(vec![transcript_test_block("answer", 1, 12,
                    TranscriptBlockBodyV1::AssistantText {
                        content: TranscriptTextContentV1::inline("Final answer from committed history".into()),
                        status: TranscriptBlockStatusV1::Completed,
                    })], None),
            }),
            other => panic!("unexpected stop reconciliation request {other}"),
        },
    ));
    stop_active_agent_runs(&mut app, "tui_user_stop").expect("natural completion won");
    assert_eq!(
        app.transcript,
        vec![TranscriptLine::Summary(
            "Final answer from committed history".into()
        )]
    );
    assert!(app.active_agent_run_id.is_none());
    assert!(app.active_agent_run_ids.is_empty());
    assert!(app.pending_question.is_none());
    assert_eq!(app.process_state, RuntimeDisplayState::Idle);
    assert!(
        app.completed_agent_run_ids.is_empty(),
        "history refresh must not fabricate cancellation events"
    );
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn cancel_receipt_rejects_the_unsupported_cancelled_field() {
    let workspace = unique_test_dir("workspace-stop-accepted");
    let mut app = test_app("", workspace.clone(), workspace.clone());
    app.active_session = Some(TuiSession {
        id: "session-running".into(),
    });
    app.active_agent_run_id = Some("run-running".into());
    app.active_agent_run_ids.insert("run-running".into());
    app.process_state = RuntimeDisplayState::PendingPhase;
    app.runtime = Some(RuntimeClient::from_test_request_handler(|frame| {
        assert_eq!(frame["method"], "_centaeris/session/agent-runs/cancel");
        json!({"cancelled": true, "agentRun": {"agentRunId": "run-running", "status": "running"}})
    }));
    let error =
        stop_active_agent_runs(&mut app, "tui_user_stop").expect_err("old receipt must fail");
    assert!(error.contains("cancelAccepted"), "{error}");
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn failed_observation_refresh_preserves_unknown_state_and_can_retry() {
    let workspace = unique_test_dir("workspace-reconnect-refresh");
    let mut app = test_app("", workspace.clone(), workspace.clone());
    app.active_session = Some(TuiSession {
        id: "session-paged".into(),
    });
    app.active_agent_run_id = Some("run-running".into());
    app.active_agent_run_ids.insert("run-running".into());
    app.process_state = RuntimeDisplayState::ConnectionLost;
    app.runtime = Some(RuntimeClient::from_test_request_handler(|_| json!({})));
    assert!(refresh_current_session_observation(&mut app).is_err());
    assert!(
        app.runtime.is_none(),
        "failed refresh must permit a fresh connection"
    );
    assert_eq!(app.process_state, RuntimeDisplayState::ConnectionLost);
    assert_eq!(app.active_agent_run_id.as_deref(), Some("run-running"));
    app.runtime = Some(RuntimeClient::from_test_request_handler(
        |frame| match frame["method"].as_str().unwrap() {
            "_centaeris/session/agent-runs/attach" => {
                json!({"transitionReason": "viewer_attached"})
            }
            "_centaeris/session/agent-runs" => json!({"agentRuns": []}),
            "transcript/page" => json!({
                "schema": "transcript.page.rpc.v1", "projectionVersion": TRANSCRIPT_PROJECTION_VERSION_V1,
                "projectionGeneration": "generation-paged", "projectedSourceHighWater": "12",
                "targetSourceHighWater": "12", "targetReached": true,
                "page": transcript_test_page(Vec::new(), None),
            }),
            other => panic!("unexpected refresh request {other}"),
        },
    ));
    refresh_current_session_observation(&mut app).expect("authoritative refresh");
    assert!(app.runtime.is_some());
    assert_eq!(app.process_state, RuntimeDisplayState::Idle);
    assert!(app.active_agent_run_id.is_none());
    assert!(app.active_agent_run_ids.is_empty());
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn runtime_disconnect_keeps_run_identity_and_marks_observation_unknown() {
    let workspace = unique_test_dir("workspace-runtime-disconnect");
    let data_root = unique_test_dir("data-runtime-disconnect");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    app.runtime = Some(RuntimeClient::from_test_event_receiver(event_rx, true));
    app.active_agent_run_id = Some("agent-run-disconnected".to_string());
    app.active_agent_run_ids
        .insert("agent-run-disconnected".to_string());
    app.agent_run_started_at = Some(Instant::now());
    app.process_state = RuntimeDisplayState::Thinking;
    event_tx
        .send(RuntimeEvent::Error(
            "Runtime Server connection closed".to_string(),
        ))
        .expect("disconnect event");

    assert!(drain_runtime_events(&mut app));
    assert!(app.runtime.is_none(), "next command must reconnect");
    assert_eq!(
        app.active_agent_run_id.as_deref(),
        Some("agent-run-disconnected")
    );
    assert!(app.active_agent_run_ids.contains("agent-run-disconnected"));
    assert!(app.agent_run_started_at.is_some());
    assert_ne!(app.process_state, RuntimeDisplayState::Idle);
    assert!(matches!(
        app.transcript.last(),
        Some(TranscriptLine::Error(message)) if message == "Runtime Server connection closed"
    ));

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn esc_sets_confirmation_while_running_and_any_key_cancels_it() {
    let workspace = unique_test_dir("workspace-esc-confirm");
    let data_root = unique_test_dir("data-esc-confirm");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.active_agent_run_id = Some("agent-run-running".to_string());
    app.active_agent_run_ids
        .insert("agent-run-running".to_string());

    handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert!(app.pending_esc_stop, "first Esc must arm the confirmation");

    handle_key(
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
        &mut app,
    );
    assert!(!app.pending_esc_stop, "any other key must disarm it");
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn esc_twice_while_running_stops_without_exiting() {
    let workspace = unique_test_dir("workspace-esc-stop");
    let data_root = unique_test_dir("data-esc-stop");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.active_agent_run_id = Some("agent-run-running".to_string());
    app.active_agent_run_ids
        .insert("agent-run-running".to_string());

    handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert!(app.pending_esc_stop);
    handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert!(
        !app.pending_esc_stop,
        "second Esc must consume the confirmation"
    );
    assert!(
        app.message
            .as_deref()
            .is_some_and(|message| message.contains("session")),
        "stop failure without runtime must surface a message"
    );
    assert_eq!(
        app.active_agent_run_id.as_deref(),
        Some("agent-run-running")
    );
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn esc_while_idle_is_a_no_op() {
    let workspace = unique_test_dir("workspace-esc-idle");
    let data_root = unique_test_dir("data-esc-idle");
    let mut app = test_app("", workspace.clone(), data_root.clone());

    handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert!(!app.pending_esc_stop);
    assert!(app.message.is_none());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn transcript_to_lines_renders_closed_turn_for_owned_viewport() {
    let items = vec![
        TranscriptLine::User("build".to_string()),
        TranscriptLine::Summary("searched files".to_string()),
        TranscriptLine::Supplement("再检查 tests".to_string()),
        TranscriptLine::Error("boom".to_string()),
    ];
    let lines = transcript_to_lines(&items, 80);
    let rendered = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "  ❯ build".to_string(),
            String::new(),
            "  searched files".to_string(),
            String::new(),
            "  └ 再检查 tests".to_string(),
            "  Error: boom".to_string(),
        ]
    );
}

#[test]
fn markdown_renders_fences_headings_lists_bold_and_inline_code() {
    let text =
        "# Title\n\n- item one\n\n```rust\nfn main() {}\n```\n\nbefore **bold** and `code` after";
    let (lines, still_code) = render_markdown_lines(text, 80, false);
    assert!(!still_code);
    let rendered = lines
        .iter()
        .map(|line| {
            (
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
                line.spans
                    .iter()
                    .any(|span| span.style.add_modifier == Modifier::BOLD),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(rendered[0], ("Title".to_string(), true));
    assert_eq!(rendered[1], ("".to_string(), false));
    assert_eq!(rendered[2], ("• item one".to_string(), false));
    assert_eq!(rendered[3], ("".to_string(), false));
    assert_eq!(rendered[4], ("  rust".to_string(), false));
    assert_eq!(rendered[5], ("  fn main() {}".to_string(), false));
    assert_eq!(rendered[6], ("".to_string(), false));
    assert_eq!(
        rendered[7],
        ("before bold and code after".to_string(), true)
    );
    assert!(lines[7]
        .spans
        .iter()
        .any(|span| span.style.fg == Some(theme().inline_code_fg)));
}

#[test]
fn markdown_keeps_unclosed_fence_state_for_streaming() {
    let (lines, still_code) = render_markdown_lines("```rust\nfn main", 80, false);
    assert!(still_code);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].spans[0].content.as_ref(), "  rust");
    assert_eq!(lines[1].spans[0].content.as_ref(), "  fn main");
    let (more, done) = render_markdown_lines("() {}\n```", 80, true);
    assert!(!done);
    assert_eq!(more.len(), 1);
    assert_eq!(more[0].spans[0].content.as_ref(), "  () {}");
}

#[test]
fn markdown_table_aligns_columns_and_retains_copyable_cells_when_narrow() {
    let source =
        "| 项目 | Status |\n| --- | ---: |\n| 自动复制 | **通过** |\n| path \\| code | `👩‍💻` |";
    let wide = render_summary_lines(source, 60);
    let rendered = rendered_lines_text(&wide);
    assert!(rendered.contains("┼"));
    assert!(rendered.contains("path | code"));
    assert!(rendered.contains("自动复制") && rendered.contains("👩‍💻"));
    let narrow_source = render_summary_lines(source, 14);
    let narrow = text_layout::TextLayout::new(narrow_source, 13, true);
    assert!(narrow.text.contains("path | code │ 👩‍💻"));
    assert!(narrow.lines.iter().all(|line| line.width() <= 13));
}

#[test]
fn streaming_table_holds_its_header_until_rows_can_be_laid_out_together() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    app.assistant_buffer = "| Header | State |\n".into();
    assert!(!materialize_assistant_prefix(&mut app));
    app.assistant_buffer
        .push_str("| --- | --- |\n| Editor | Ready |\n");
    assert!(!materialize_assistant_prefix(&mut app));
    assert!(rendered_lines_text(&build_assistant_live_lines(&app, 80)).contains("┼"));
    app.assistant_buffer.push('\n');
    assert!(materialize_assistant_prefix(&mut app));
    let settled = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert_eq!(settled.matches("Header").count(), 1);
    assert!(settled.contains("Editor") && settled.contains("┼"));
}

#[test]
fn assistant_text_is_the_tool_group_boundary() {
    let workspace = unique_test_dir("workspace-tool-boundary");
    let data_root = unique_test_dir("data-tool-boundary");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    let mut first = test_tool_line(
        ToolActionKind::Command,
        "first",
        Vec::new(),
        vec![ToolResultState::SuccessNoOutput],
    );
    first.key = "tool_call:first".to_string();
    let mut second = test_tool_line(
        ToolActionKind::Read,
        "second",
        Vec::new(),
        vec![ToolResultState::SuccessNoOutput],
    );
    second.key = "tool_call:second".to_string();
    let mut third = test_tool_line(
        ToolActionKind::Edit,
        "third",
        Vec::new(),
        vec![ToolResultState::SuccessNoOutput],
    );
    third.key = "tool_call:third".to_string();
    app.transcript = vec![
        TranscriptLine::Tool(first),
        TranscriptLine::Subagent(SubagentTranscriptLine {
            title: "research".to_string(),
            summary: "done".to_string(),
            status: "result".to_string(),
        }),
        TranscriptLine::Tool(second),
        TranscriptLine::Summary("Next step".to_string()),
        TranscriptLine::Tool(third),
    ];

    let view = build_transcript_view(&app, 80);
    assert_eq!(view.source_rows.len(), 3);
    assert_eq!(view.source_rows[0].0, "tool_call:first");
    assert_eq!(view.source_rows[2].0, "tool_call:third");
    let rendered = rendered_lines_text(&view.lines);
    assert!(rendered.contains("Next step"));
    assert!(rendered.contains("research: done"));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tui_consumes_core_committed_turn_projection() {
    for (suffix, terminal_type, final_status) in [
        ("success", "AgentRunCompleted", "done"),
        ("failure", "AgentRunFailed", "error"),
        ("interruption", "AgentRunInterrupted", "error"),
    ] {
        let workspace = unique_test_dir(&format!("core-session-projection-{suffix}"));
        let data_root = unique_test_dir(&format!("core-session-projection-data-{suffix}"));
        let mut app = test_app("", workspace.clone(), data_root.clone());
        let turn_id = format!("turn-{suffix}");
        let agent_run_id = format!("agent-run-{suffix}");
        app.active_agent_run_id = Some(agent_run_id.clone());
        app.active_agent_run_ids.insert(agent_run_id.clone());
        for projection in [
            json!({
                "type": "session_event",
                "agentRunId": agent_run_id,
                "cursor": "2",
                "event": {
                    "id": format!("evt:assistant:{suffix}"),
                    "version": "v1",
                    "type": "Final",
                    "at": 2,
                    "sessionId": "chat-1",
                    "turnId": turn_id,
                    "taskId": format!("task-root-{suffix}"),
                    "parentTaskId": turn_id,
                    "status": final_status,
                    "visibility": "user",
                    "processState": "reviewing",
                    "payload": {"content": "answer", "artifactRefs": []},
                    "meta": {"source": "core.session_log", "durable": true}
                }
            }),
            json!({
                "type": "session_event",
                "agentRunId": agent_run_id,
                "cursor": "3",
                "event": {
                    "id": format!("evt:terminal:{suffix}"),
                    "version": "v1",
                    "type": terminal_type,
                    "at": 3,
                    "sessionId": "chat-1",
                    "turnId": turn_id,
                    "taskId": format!("task-root-{suffix}"),
                    "parentTaskId": turn_id,
                    "status": final_status,
                    "visibility": "internal",
                    "payload": if terminal_type == "AgentRunInterrupted" {
                        json!({"reasonType": "cancelled"})
                    } else {
                        json!({"doneReason": "finalized"})
                    },
                    "meta": {"source": "core.session_log", "durable": true}
                }
            }),
        ] {
            apply_stream_payload(&mut app, &projection);
        }
        assert!(app.transcript.iter().any(|line| matches!(
            line.content(),
            TranscriptLine::LiveAssistant { markdown, .. } if markdown == "answer"
        )));
        assert!(app.active_agent_run_id.is_none());
        let _ = std::fs::remove_dir_all(workspace);
        let _ = std::fs::remove_dir_all(data_root);
    }
}

#[test]
fn inline_image_placeholder_is_atomic_and_serializes_in_body_order() {
    let workspace = unique_test_dir("inline-image");
    let data_root = unique_test_dir("inline-image-data");
    let mut app = test_app(
        "a[Image #2]b[Image #1]c",
        workspace.clone(),
        data_root.clone(),
    );
    let first_image_path = data_root.join("first.png");
    let second_image_path = data_root.join("second.png");
    std::fs::write(first_image_path.as_path(), b"first").expect("first image fixture");
    std::fs::write(second_image_path.as_path(), b"second").expect("second image fixture");
    app.draft_image_attachments.push(DraftImageAttachment {
        start: 12,
        end: 22,
        local_path: second_image_path,
    });
    app.draft_image_attachments.push(DraftImageAttachment {
        start: 1,
        end: 11,
        local_path: first_image_path,
    });
    app.input_cursor = 11;

    move_cursor_left(&mut app);
    assert_eq!(app.input_cursor, 1);
    move_cursor_right(&mut app);
    assert_eq!(app.input_cursor, 11);

    let request = build_prompt_request(
        &TuiSession {
            id: "session-image".to_string(),
        },
        app.input.as_str(),
        app.draft_image_attachments.as_slice(),
        "session-prompt:image-action",
    );
    assert_eq!(request["message"], "a[Image #2]b[Image #1]c");
    assert_eq!(request["attachments"][0]["placeholder"], "[Image #2]");
    assert_eq!(request["attachments"][1]["placeholder"], "[Image #1]");

    backspace_at_cursor(&mut app);
    assert_eq!(app.input, "ab[Image #1]c");
    assert_eq!(app.draft_image_attachments.len(), 1);
    clear_composer(&mut app);
    assert_eq!(app.next_image_number, 1);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn command_requests_keep_the_user_actions_operation_identity() {
    let first = new_runtime_operation_id().expect("first operation identity");
    let second = new_runtime_operation_id().expect("second operation identity");
    assert_eq!(first.len(), 32);
    assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(first, second);

    let session_request =
        build_session_create_request("Session title", "D:/Workspace", "session-new:action-1");
    assert_eq!(session_request["operationId"], "session-new:action-1");

    let prompt_request = build_prompt_request(
        &TuiSession {
            id: "session-operation".to_string(),
        },
        "Prompt",
        &[],
        "session-prompt:action-1",
    );
    assert_eq!(prompt_request["operationId"], "session-prompt:action-1");
    assert_eq!(
        prompt_request.clone()["operationId"],
        prompt_request["operationId"]
    );
}

fn test_app(input: &str, workspace_root: PathBuf, _data_root: PathBuf) -> App {
    App {
        workspace_root: display_path(workspace_root.as_path()),
        session_cwd: display_path(workspace_root.as_path()),
        model_provider_id: None,
        model_display: None,
        model_effort: None,
        runtime: None,
        pending_model_request: None,
        runtime_config_refresh_pending: false,
        model_credential_prompt: None,
        model_panel: None,
        model_catalog: Vec::new(),
        model_saved_composer: None,
        model_saved_panel: None,
        model_provider_hit_regions: Vec::new(),
        model_list_area: None,
        model_list_offset: 0,
        session_picker: None,
        home_risk_pending: false,
        home_risk_panel: None,
        input: input.to_string(),
        input_cursor: input.len(),
        input_selection_anchor: None,
        input_area: None,
        input_layout: None,
        input_scroll: 0,
        input_scroll_override: false,
        edit_history: editing::History::default(),
        retired_image_paths: HashSet::new(),
        mouse_state: mouse::State::default(),
        clipboard: Box::<TestClipboard>::default(),
        panel_area: None,
        draft_image_attachments: Vec::new(),
        next_image_number: 1,
        image_picker: halfblock_image_picker(),
        output_preview: HashMap::new(),
        expanded_bash_tools: HashSet::new(),
        image_preview: None,
        image_preview_area: None,
        inline_images: HashMap::new(),
        inline_image_cache_order: VecDeque::new(),
        inline_image_cache_bytes: 0,
        inline_image_errors: HashMap::new(),
        pending_esc_stop: false,
        message: None,
        show_help: false,
        selected_command: 0,
        active_session: None,
        parked_sessions: HashMap::new(),
        read_only_session: None,
        transcript: Vec::new(),
        transcript_paging: None,
        transcript_history_len: 0,
        transcript_revision: 0,
        transcript_layout_cache: None,
        transcript_scroll: 0,
        transcript_max_scroll: 0,
        transcript_follow_bottom: true,
        snapshot_revisions: HashMap::new(),
        transcript_area: None,
        text_layout: None,
        text_scroll: 0,
        transcript_viewport: None,
        transcript_selection: None,
        mouse_drag: None,
        tool_projection: ToolProjection::default(),
        pending_subagent_lines: Vec::new(),
        assistant_buffer: String::new(),
        assistant_emitted_bytes: 0,
        assistant_tail_in_code_block: false,
        assistant_stream_started: false,
        assistant_stream_start: None,
        presentation_run_id: None,
        assistant_run_id: None,
        assistant_is_final: false,
        run_activity: HashMap::new(),
        render_width: 80,
        active_tool_label: None,
        active_agent_run_id: None,
        active_agent_run_ids: HashSet::new(),
        completed_agent_run_ids: HashSet::new(),
        seen_subagent_ids: HashSet::new(),
        live_subagent_ids: HashSet::new(),
        seen_stream_event_ids: HashSet::new(),
        agent_run_started_at: None,
        process_state: RuntimeDisplayState::Idle,
        status_usage: status_usage::StatusUsage::default(),
        status_easter_egg: StatusEasterEgg::Available,
        pending_question: None,
        tool_protocol_error: false,
    }
}

fn rendered_lines_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect()
}

fn test_png_bytes() -> Vec<u8> {
    let image = image::RgbaImage::from_fn(2, 2, |_, y| {
        if y == 0 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 0, 255, 255])
        }
    });
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .expect("encode PNG fixture");
    encoded.into_inner()
}

fn test_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn test_buffer_has_rgb_halfblock(buffer: &ratatui::buffer::Buffer) -> bool {
    buffer.content.iter().any(|cell| {
        cell.symbol() == "▀"
            && (matches!(cell.fg, ratatui::style::Color::Rgb(_, _, _))
                || matches!(cell.bg, ratatui::style::Color::Rgb(_, _, _)))
    })
}

fn unique_test_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "centaeris-tui-main-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos()
    ));
    std::fs::create_dir_all(dir.as_path()).expect("create temp dir");
    dir
}

#[test]
fn reasoning_stays_hidden_across_final_and_updates() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let reasoning = |id: &str, text: &str| {
        json!({
            "id": id, "type": "Reasoning", "visibility": "user",
            "payload": {"blockId":"reasoning:req", "requestId":"req", "text":text, "status":"streaming"}
        })
    };
    apply_session_event(
        &mut app,
        &reasoning("r1", "private process text"),
        Some("run"),
    );
    let first = build_transcript_view(&app, 80);
    assert!(!rendered_lines_text(&first.lines).contains("Thoughts"));
    assert!(first.source_rows.is_empty());
    apply_session_event(
        &mut app,
        &json!({"id":"f", "type":"Final", "payload":{"content":"Answer"}}),
        Some("run"),
    );
    assert!(!rendered_lines_text(&build_transcript_view(&app, 80).lines)
        .contains("private process text"));
    apply_session_event(
        &mut app,
        &reasoning("r2", "updated process text"),
        Some("run"),
    );
    assert!(!rendered_lines_text(&build_transcript_view(&app, 80).lines)
        .contains("updated process text"));
    assert!(app.transcript.iter().any(|line| matches!(line, TranscriptLine::Reasoning { text, .. } if text == "updated process text")));
    assert!(!app
        .transcript
        .iter()
        .any(|line| matches!(line, TranscriptLine::Error(_))));
}

#[test]
fn model_snapshot_restores_reasoning_and_answer_without_an_unsupported_event() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    apply_session_event(
        &mut app,
        &json!({"id":"snapshot", "type":"ModelSnapshot", "turnId":"turn", "payload":{
            "revision":1,"text":"restored answer", "reasoning":{"blockId":"reasoning:req","requestId":"req","text":"restored thought"}
        }}),
        Some("run"),
    );
    assert_eq!(app.assistant_buffer, "restored answer");
    assert!(
        !rendered_lines_text(&build_transcript_view(&app, 80).lines).contains("restored thought")
    );
    assert!(!app
        .transcript
        .iter()
        .any(|line| matches!(line, TranscriptLine::Error(_))));
}

#[test]
fn historical_reasoning_has_no_rows_or_focus_after_projection_refresh() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let page = transcript_test_page(
        vec![
            transcript_test_block(
                "reasoning:req",
                1,
                1,
                TranscriptBlockBodyV1::Reasoning {
                    request_id: "req".to_string(),
                    content: TranscriptTextContentV1::inline("history thought".to_string()),
                    status: TranscriptBlockStatusV1::Completed,
                },
            ),
            transcript_test_block(
                "answer",
                1,
                2,
                TranscriptBlockBodyV1::AssistantText {
                    content: TranscriptTextContentV1::inline("final answer".to_string()),
                    status: TranscriptBlockStatusV1::Completed,
                },
            ),
        ],
        None,
    );
    app.transcript_paging = Some(TranscriptPagingState::open(page).unwrap());
    sync_materialized_transcript_history(&mut app);
    let view = build_transcript_view(&app, 80);
    assert!(!rendered_lines_text(&view.lines).contains("history thought"));
    assert!(rendered_lines_text(&view.lines).contains("final answer"));
    assert!(view.source_rows.is_empty());
    sync_materialized_transcript_history(&mut app);
    assert!(
        !rendered_lines_text(&build_transcript_view(&app, 80).lines).contains("history thought")
    );
}

#[test]
fn reasoning_cannot_receive_tool_keyboard_focus() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    apply_session_event(
        &mut app,
        &json!({"id":"r","type":"Reasoning","payload":{
            "blockId":"reasoning:req","requestId":"req","text":"reading thought","status":"streaming"
        }}),
        Some("run"),
    );
    let view = build_transcript_view(&app, 80);
    assert!(view.source_rows.is_empty());
    apply_session_event(
        &mut app,
        &json!({"id":"final","type":"Final","payload":{"content":"answer"}}),
        Some("run"),
    );
    assert!(
        !rendered_lines_text(&build_transcript_view(&app, 80).lines).contains("reading thought")
    );
}

#[test]
fn stale_model_snapshot_cannot_overwrite_newer_text_and_unknown_fields_fail() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    for (revision, text) in [(2, "new"), (1, "old")] {
        let event = centaeris_core::runtime::projection::project_live_model_snapshot(
            "session".into(),
            "turn".into(),
            revision,
            text.into(),
            None,
        )
        .unwrap();
        apply_stream_payload_for_agent_run(&mut app, &event, Some("run"));
    }
    assert_eq!(app.assistant_buffer, "new");
    apply_session_event(
        &mut app,
        &json!({"id":"bad","type":"Reasoning","payload":{
            "blockId":"reasoning:req","requestId":"req","text":"bad","status":"streaming","unexpected":true
        }}),
        Some("run"),
    );
    assert!(matches!(
        app.transcript.last(),
        Some(TranscriptLine::Error(_))
    ));
}

#[test]
fn hidden_reasoning_does_not_split_tools_or_leave_spacing() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let make = |id: &str| {
        TranscriptLine::Tool(crate::tool_projection::transcript_page_tool_line(
            id,
            "read",
            TranscriptBlockStatusV1::Completed,
            "file.rs".into(),
            None,
        ))
    };
    app.transcript = vec![
        make("a"),
        TranscriptLine::Reasoning {
            key: "r".into(),
            text: "hidden".into(),
            final_started: false,
        },
        make("b"),
    ];
    let with = build_transcript_view(&app, 80);
    app.transcript.remove(1);
    let without = build_transcript_view(&app, 80);
    assert_eq!(with.lines, without.lines);
    assert_eq!(with.source_rows, without.source_rows);
    assert_eq!(with.source_rows.len(), 2);
}

#[test]
fn historical_tool_title_does_not_repeat_read_verb() {
    let tool = crate::tool_projection::transcript_page_tool_line(
        "a",
        "read",
        TranscriptBlockStatusV1::Completed,
        "Read src/main.rs".into(),
        Some("30"),
    );
    assert_eq!(stable_tool_title(&tool), "Read src/main.rs");
    let directory = crate::tool_projection::transcript_page_tool_line(
        "d",
        "read",
        TranscriptBlockStatusV1::Completed,
        "Listed packages · 8 entries".into(),
        Some("30"),
    );
    assert_eq!(stable_tool_title(&directory), "Listed packages · 8 entries");
}

#[test]
fn automatic_tools_preserve_follow_mode_through_layout_and_new_output() {
    for following in [true, false] {
        let path = PathBuf::from("D:/presentation-test");
        let mut app = test_app("", path.clone(), path);
        app.transcript
            .push(TranscriptLine::Summary("previous output\n".repeat(40)));
        let tool = test_tool_line(
            ToolActionKind::Read,
            "file.rs",
            vec![],
            vec![ToolResultState::SuccessNoOutput],
        );
        app.transcript.push(TranscriptLine::Tool(tool));
        app.transcript_follow_bottom = following;
        app.transcript_scroll = 3;
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 10)).unwrap();
        for _ in 0..2 {
            assert_eq!(app.transcript_follow_bottom, following);
            app.transcript
                .push(TranscriptLine::Summary("new output\n".repeat(4)));
            terminal
                .draw(|frame| {
                    let view = build_transcript_view(&app, 80);
                    render_transcript(frame, frame.area(), &mut app, &view);
                })
                .unwrap();
            assert_eq!(app.transcript_follow_bottom, following);
            if following {
                assert_eq!(app.transcript_scroll, app.transcript_max_scroll);
            } else {
                assert_eq!(app.transcript_scroll, 3);
            }
        }
    }
}

#[test]
fn automatic_preview_resize_preserves_detached_reading_anchor() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let mut tool = test_tool_line(
        ToolActionKind::Read,
        "main.rs",
        vec![],
        vec![ToolResultState::SuccessWithOutput],
    );
    tool.full_text = Some("中".repeat(30000));
    app.transcript.push(TranscriptLine::Tool(tool));
    app.transcript
        .push(TranscriptLine::Summary("reading anchor\n".repeat(4)));
    app.transcript_follow_bottom = false;
    let old = build_cached_transcript_view(&mut app, 80);
    let at = old
        .lines
        .iter()
        .position(|l| rendered_lines_text(std::slice::from_ref(l)).contains("reading anchor"))
        .unwrap();
    app.transcript_scroll = old.lines[..at]
        .iter()
        .map(|l| u64::from(paragraph_line_count(std::slice::from_ref(l), 80)))
        .sum();
    invalidate_transcript_layout(&mut app);
    let next = build_cached_transcript_view(&mut app, 80);
    assert!(
        rendered_lines_text(&transcript_render_window(&next, app.transcript_scroll, 1).lines)
            .contains("reading anchor")
    );
    let resized = build_cached_transcript_view(&mut app, 45);
    assert!(rendered_lines_text(
        &transcript_render_window(&resized, app.transcript_scroll, 1).lines
    )
    .contains("reading anchor"));
    assert!(!app.transcript_follow_bottom);
}

#[test]
fn automatic_preview_poll_preserves_reading() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let mut tool = test_tool_line(
        ToolActionKind::Read,
        "main.rs",
        vec![],
        vec![ToolResultState::SuccessWithOutput],
    );
    tool.full_text = Some((0..4000).map(|i| format!("中文 output {i:04}\n")).collect());
    app.transcript.push(TranscriptLine::Tool(tool));
    app.transcript_follow_bottom = false;
    invalidate_transcript_layout(&mut app);
    let old = build_cached_transcript_view(&mut app, 80);
    app.transcript_scroll = 0;
    let old_scroll = app.transcript_scroll;
    assert!(!output_preview::poll(&mut app));
    let next = build_cached_transcript_view(&mut app, 80);
    assert_eq!(next.total_rows, old.total_rows);
    assert!(app.transcript_scroll.abs_diff(old_scroll) <= 2);
}

#[test]
fn automatic_preview_hides_machine_framing() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let mut tool = test_tool_line(
        ToolActionKind::Read,
        "main.rs",
        vec![],
        vec![ToolResultState::SuccessWithOutput],
    );
    tool.full_text = Some("machine header\n正文\nContinuation: complete".into());
    tool.readable_range = Some((15, 6));
    tool.duration_ms = Some(250);
    app.transcript.push(TranscriptLine::Tool(tool));
    invalidate_transcript_layout(&mut app);
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
    let draw = |terminal: &mut Terminal<ratatui::backend::TestBackend>, app: &mut App| {
        terminal
            .draw(|frame| {
                let view = build_transcript_view(app, 100);
                render(frame, app, &view);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    let screen = draw(&mut terminal, &mut app);
    assert!(screen.contains("正") && screen.contains("文"), "{screen}");
    assert!(!screen.contains("Finished") && !screen.contains("0.25s"));
    let preview = output_preview::preview_lines(
        &app,
        match app.transcript[0].content() {
            TranscriptLine::Tool(tool) => tool,
            _ => panic!(),
        },
        100,
    );
    assert_eq!(
        preview.len(),
        1,
        "short output must not reserve ten blank rows"
    );
    assert!(!screen.contains("machine header"));
    assert!(!screen.contains("Continuation:"));
}

#[test]
fn referenced_tool_renders_only_its_title_without_preview_hint_row() {
    let tool = crate::tool_projection::transcript_page_tool_line(
        "read-file",
        "read",
        TranscriptBlockStatusV1::Completed,
        "Read README.md".into(),
        Some("4230"),
    );
    let lines = transcript_to_lines(&[TranscriptLine::Tool(tool)], 100);
    let visible: Vec<_> = lines
        .iter()
        .filter(|line| {
            !rendered_lines_text(std::slice::from_ref(line))
                .trim()
                .is_empty()
        })
        .collect();
    assert_eq!(visible.len(), 1);
    let text = rendered_lines_text(&lines);
    assert!(text.contains("Read README.md"));
    assert!(!text.contains("Enter to preview") && !text.contains("bytes"));
}

#[test]
fn tool_visual_hierarchy_marks_outcome_and_emphasizes_action() {
    for (state, color) in [
        (ToolResultState::SuccessWithOutput, theme().muted),
        (ToolResultState::Failed, ratatui::style::Color::Red),
        (ToolResultState::Denied, ratatui::style::Color::Red),
    ] {
        let tool = test_tool_line(ToolActionKind::Read, "src/main.rs", vec![], vec![state]);
        let lines = transcript_to_lines(&[TranscriptLine::Tool(tool)], 100);
        assert_eq!(lines[0].spans[1].style.fg, Some(color));
        assert!(lines[0]
            .spans
            .iter()
            .any(|span| span.style.add_modifier.contains(Modifier::BOLD)));
    }
}

#[test]
fn agent_run_completion_folds_process_but_keeps_final_answer() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    app.active_agent_run_id = Some("run-a".into());
    for (id, kind, payload) in [
        (
            "stage",
            "Status",
            json!({"stage":"model_process_summary","message":"Inspecting source"}),
        ),
        ("answer", "Final", json!({"content":"The result is ready"})),
    ] {
        apply_session_event(
            &mut app,
            &json!({"id":id,"type":kind,"payload":payload}),
            Some("run-a"),
        );
    }
    let running = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert!(running.contains("Inspecting source"));
    apply_session_event(
        &mut app,
        &json!({"id":"complete","type":"AgentRunCompleted","payload":{}}),
        Some("run-a"),
    );
    let completed = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert!(completed.contains("The result is ready"));
    assert!(completed.contains("Inspecting source"), "{completed}");
    assert!(!completed.contains('─'), "{completed}");
}

fn run_event(app: &mut App, run: &str, id: &str, kind: &str, payload: Value) {
    apply_session_event(
        app,
        &json!({"id":id,"type":kind,"payload":payload}),
        Some(run),
    );
}

#[test]
fn run_terminal_states_keep_results_and_do_not_require_final() {
    for (kind, reason_type, reason, expected) in [
        ("AgentRunCompleted", "", "", None),
        (
            "AgentRunFailed",
            "provider_unavailable",
            "provider_unavailable",
            Some("The model service is temporarily unavailable."),
        ),
        (
            "AgentRunInterrupted",
            "shutdown",
            "runtime_server_shutdown",
            Some("The work session was interrupted. Retry the request."),
        ),
        ("AgentRunInterrupted", "cancelled", "tui_user_stop", None),
    ] {
        for final_first in [true, false] {
            let path = PathBuf::from("D:/presentation-test");
            let mut app = test_app("", path.clone(), path);
            app.active_agent_run_id = Some("run".into());
            run_event(
                &mut app,
                "run",
                "stage",
                "Status",
                json!({"stage":"model_process_summary","message":"Inspecting source"}),
            );
            if final_first {
                run_event(
                    &mut app,
                    "run",
                    "final",
                    "Final",
                    json!({"content":"Useful result"}),
                );
            }
            let payload = json!({"message":reason,"reasonType":reason_type});
            run_event(&mut app, "run", "terminal", kind, payload.clone());
            let before = rendered_lines_text(&build_transcript_view(&app, 80).lines);
            if let Some(expected) = expected {
                assert!(before.contains(expected), "{before}");
            }
            assert!(!has_active_agent_run(&app));
            assert!(!before.contains("└─"), "{before}");
            assert!(!before.contains("Working"), "{before}");
            assert!(before.contains("Inspecting source"));
            assert_eq!(before.contains("Useful result"), final_first);
            if !final_first {
                run_event(
                    &mut app,
                    "run",
                    "final",
                    "Final",
                    json!({"content":"Useful result"}),
                );
            }
            run_event(&mut app, "run", "terminal", kind, payload);
            let after = rendered_lines_text(&build_transcript_view(&app, 80).lines);
            assert_eq!(after.matches("Useful result").count(), 1, "{after}");
            assert_eq!(
                after.matches("! ").count(),
                usize::from(expected.is_some()),
                "{after}"
            );
        }
    }
}

#[test]
fn child_completion_does_not_fold_parent_and_waiting_is_not_completion() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    app.active_agent_run_id = Some("parent".into());
    app.active_agent_run_ids = HashSet::from(["parent".into(), "child".into()]);
    for run in ["parent", "child"] {
        run_event(
            &mut app,
            run,
            &format!("stage-{run}"),
            "Status",
            json!({"stage":"model_process_summary","message":format!("Process of {run}")}),
        );
    }
    run_event(
        &mut app,
        "child",
        "done-child",
        "AgentRunCompleted",
        json!({}),
    );
    apply_session_event(
        &mut app,
        &json!({"id":"waiting-parent","type":"ModelStatus","processState":"waiting_user","payload":{}}),
        Some("parent"),
    );
    let text = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert_eq!(status_header(&app), "Waiting for input");
    assert!(text.contains("Process of parent"));
    assert!(text.contains("Process of child"));
    assert!(!text.contains('─'));
}

fn run_block(
    id: &str,
    sequence: u64,
    run: &str,
    source: &str,
    body: TranscriptBlockBodyV1,
) -> TranscriptBlockV1 {
    let mut block = transcript_test_block(id, 1, sequence, body);
    block.presentation = Some(
        centaeris_core::session::transcript::TranscriptPresentationV1 {
            agent_run_id: Some(run.into()),
            source_type: source.into(),
            observed_at_ms: sequence as i64,
            display_target: None,
            duration_ms: None,
            operation: None,
            reason_type: None,
        },
    );
    block
}

fn history_run_page(terminal: Option<&str>) -> TranscriptPageV1 {
    let mut blocks = vec![
        run_block(
            "stage",
            2,
            "run",
            "phase_event",
            TranscriptBlockBodyV1::Notice {
                notice_type: "model_process_summary".into(),
                content: TranscriptTextContentV1::inline("Inspecting source".into()),
                status: TranscriptBlockStatusV1::Running,
            },
        ),
        run_block(
            "result",
            3,
            "run",
            "assistant_message",
            TranscriptBlockBodyV1::AssistantText {
                content: TranscriptTextContentV1::inline("Useful result".into()),
                status: TranscriptBlockStatusV1::Completed,
            },
        ),
    ];
    if let Some(source) = terminal {
        blocks.push(run_block(
            "end",
            4,
            "run",
            source,
            TranscriptBlockBodyV1::Notice {
                notice_type: "run_boundary".into(),
                content: TranscriptTextContentV1::inline(
                    if source == "agent_run_failed" {
                        "provider_unavailable"
                    } else {
                        ""
                    }
                    .into(),
                ),
                status: TranscriptBlockStatusV1::Completed,
            },
        ));
    }
    transcript_test_page(blocks, None)
}

#[test]
fn historical_runs_use_authoritative_terminal_state_and_preserve_user_disclosure() {
    for terminal in [None, Some("agent_run_completed"), Some("agent_run_failed")] {
        let path = PathBuf::from("D:/presentation-test");
        let mut app = test_app("", path.clone(), path);
        app.transcript_paging =
            Some(TranscriptPagingState::open(history_run_page(terminal)).unwrap());
        sync_materialized_transcript_history(&mut app);
        let text = rendered_lines_text(&build_transcript_view(&app, 80).lines);
        assert!(text.contains("Useful result"));
        assert!(!text.contains("Working"));
        assert!(text.contains("Inspecting source"));
        match terminal {
            None => assert!(!text.contains('─')),
            Some("agent_run_failed") => {
                assert!(text.contains("! The model service is temporarily unavailable."))
            }
            _ => {
                assert!(!text.contains('─'));
                for _ in 0..2 {
                    sync_materialized_transcript_history(&mut app);
                }
                assert!(rendered_lines_text(&build_transcript_view(&app, 80).lines)
                    .contains("Inspecting source"));
            }
        }
    }
}

#[test]
fn run_failure_display_matches_web_for_history_and_repeated_sync() {
    let vectors: Vec<Value> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/run-error-display.json"
    ))
    .unwrap();
    for vector in vectors {
        let reason = vector["reason"].as_str().unwrap();
        let expected = vector["expected"].as_str().unwrap();
        let path = PathBuf::from("D:/presentation-test");
        let mut app = test_app("", path.clone(), path);
        let mut page = history_run_page(Some("agent_run_failed"));
        if let TranscriptBlockBodyV1::Notice { content, .. } =
            &mut page.blocks.last_mut().unwrap().body
        {
            *content = TranscriptTextContentV1::inline(reason.into());
        }
        app.transcript_paging = Some(TranscriptPagingState::open(page).unwrap());
        for _ in 0..2 {
            sync_materialized_transcript_history(&mut app);
            let text = rendered_lines_text(&build_transcript_view(&app, 240).lines);
            assert!(text.contains(&format!("! {expected}")), "{reason}: {text}");
            assert_eq!(text.matches("! ").count(), 1, "{reason}: {text}");
            assert!(text.contains("Useful result"));
        }
    }
}

#[test]
fn run_terminal_notices_match_in_live_and_restored_history() {
    for (event_type, source, reason_type, message, expected) in [
        (
            "AgentRunFailed",
            "agent_run_failed",
            "provider_timeout",
            "provider_timeout",
            Some("The model took too long to respond. Retry the request."),
        ),
        (
            "AgentRunInterrupted",
            "agent_run_interrupted",
            "provider_interrupted",
            "provider_interrupted",
            Some("The model response was interrupted. Retry the request."),
        ),
        (
            "AgentRunInterrupted",
            "agent_run_interrupted",
            "cancelled",
            "tui_user_stop",
            None,
        ),
    ] {
        let path = PathBuf::from("D:/presentation-test");
        let mut app = test_app("", path.clone(), path);
        app.active_agent_run_id = Some("run".into());
        app.transcript_paging =
            Some(TranscriptPagingState::open(transcript_test_page(vec![], None)).unwrap());
        run_event(
            &mut app,
            "run",
            "runtime-error",
            "RuntimeError",
            json!({"message":"provider_timeout"}),
        );
        assert!(
            has_active_agent_run(&app),
            "runtime error must await the durable terminal event"
        );
        run_event(
            &mut app,
            "run",
            "partial",
            "ModelTextDelta",
            json!({"delta":"Useful result"}),
        );
        let payload = json!({"reasonType":reason_type,"message":message});
        for _ in 0..2 {
            run_event(&mut app, "run", "terminal", event_type, payload.clone());
        }
        let before = rendered_lines_text(&build_transcript_view(&app, 240).lines);
        assert!(before.contains("Useful result"));
        assert_eq!(
            before.matches("! ").count(),
            usize::from(expected.is_some())
        );
        if let Some(expected) = expected {
            assert!(before.contains(expected));
        }
        run_event(
            &mut app,
            "run",
            "late-runtime-error",
            "RuntimeError",
            json!({"message":"wrong late reason"}),
        );
        assert_eq!(
            rendered_lines_text(&build_transcript_view(&app, 240).lines),
            before
        );
        let mut history = history_run_page(Some(source));
        let boundary = history.blocks.last_mut().unwrap();
        boundary.presentation.as_mut().unwrap().reason_type = Some(reason_type.into());
        let TranscriptBlockBodyV1::Notice { content, .. } = &mut boundary.body else {
            panic!("expected a run boundary")
        };
        *content = TranscriptTextContentV1::inline(if reason_type == "cancelled" {
            String::new()
        } else {
            message.into()
        });
        app.transcript_paging = Some(TranscriptPagingState::open(history).unwrap());
        sync_materialized_transcript_history(&mut app);
        let after = rendered_lines_text(&build_transcript_view(&app, 240).lines);
        assert!(after.contains("Useful result"));
        assert_eq!(after.matches("! ").count(), usize::from(expected.is_some()));
        if let Some(expected) = expected {
            assert!(after.contains(expected));
        }
    }
}

#[test]
fn run_failure_wrap_preserves_plain_text_and_continuation_indent() {
    let reason = "<b>broken</b> 中文 ".repeat(20);
    let item = TranscriptLine::RunFailure {
        key: "run:r".into(),
        reason: reason.clone(),
    };
    for width in [12, 40, 100] {
        let lines = text_layout::TextLayout::new(
            transcript_to_lines(std::slice::from_ref(&item), width),
            width,
            true,
        )
        .lines;
        assert!(rendered_line_text(&lines[0]).starts_with("! "));
        assert!(lines
            .iter()
            .skip(1)
            .all(|line| rendered_line_text(line).starts_with("  ")));
        assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
        let restored: String = lines
            .iter()
            .map(|line| rendered_line_text(line)[2..].to_owned())
            .collect();
        assert_eq!(restored, reason);
    }
}

#[test]
fn referenced_run_failure_reads_all_pages_and_rejects_stale_or_invalid_content() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let reason = format!("{} final error detail", "中".repeat(6000));
    let mut page = history_run_page(Some("agent_run_failed"));
    if let TranscriptBlockBodyV1::Notice { content, .. } = &mut page.blocks.last_mut().unwrap().body
    {
        *content = TranscriptTextContentV1 {
            inline_content: None,
            source_ref: Some(
                centaeris_core::session::transcript::TranscriptContentRefV1 {
                    ref_id: "error-ref".into(),
                    revision: "1".into(),
                    byte_length: reason.len().to_string(),
                },
            ),
        };
    }
    let invalid = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let invalid_response = invalid.clone();
    let body = reason.clone();
    app.runtime = Some(RuntimeClient::from_test_request_handler(move |frame| {
        assert_eq!(frame["method"], "transcript/content-range");
        let request = &frame["params"]["request"];
        let start: usize = request["offset"].as_str().unwrap().parse().unwrap();
        let mut end = (start + request["maxBytes"].as_u64().unwrap() as usize).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        json!({"schema":"transcript.content.range.v1", "sessionId":request["sessionId"], "projectionVersion":request["projectionVersion"],
            "projectionGeneration":request["projectionGeneration"], "refId":if invalid_response.load(std::sync::atomic::Ordering::SeqCst) { "wrong" } else { "error-ref" },
            "revision":"1","byteLength":body.len().to_string(),"startOffset":start.to_string(),"endOffset":end.to_string(),
            "content":&body[start..end],"hasMore":end<body.len()})
    }));
    app.transcript_paging = Some(TranscriptPagingState::open(page.clone()).unwrap());
    sync_materialized_transcript_history(&mut app);
    let initial = build_transcript_view(&app, 80);
    assert!(rendered_lines_text(&initial.lines).contains("Loading error details"));
    assert!(!rendered_lines_text(&initial.lines).contains("referenced content"));
    output_preview::prepare_visible(&mut app, &initial, 100);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        output_preview::poll(&mut app);
        if output_preview::run_error_text(&app, "run:run").as_deref() == Some(reason.as_str()) {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let rendered = build_transcript_view(&app, 80);
    let error_start = rendered
        .source_rows
        .iter()
        .find(|(key, _)| key == "run:run")
        .unwrap()
        .1 as usize;
    let restored: String = rendered.lines[error_start..]
        .iter()
        .map(|line| rendered_line_text(line)[2..].to_owned())
        .collect();
    assert_eq!(restored, reason);
    page.projection_generation = "replacement-generation".into();
    app.transcript_paging = Some(TranscriptPagingState::open(page).unwrap());
    sync_materialized_transcript_history(&mut app);
    assert_eq!(output_preview::run_error_text(&app, "run:run"), None);
    assert!(output_preview::poll(&mut app));
    invalid.store(true, std::sync::atomic::Ordering::SeqCst);
    let next = build_transcript_view(&app, 80);
    output_preview::prepare_visible(&mut app, &next, 100);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        output_preview::poll(&mut app);
        if output_preview::run_error_text(&app, "run:run")
            .is_some_and(|text| text.starts_with("Unable to load error details:"))
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn final_promotes_already_materialized_stream_without_duplicate_or_hidden_answer() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    app.active_agent_run_id = Some("run".into());
    run_event(
        &mut app,
        "run",
        "delta",
        "ModelTextDelta",
        json!({"delta":"answer line\n"}),
    );
    materialize_assistant_prefix(&mut app);
    run_event(
        &mut app,
        "run",
        "final",
        "Final",
        json!({"content":"answer line\n"}),
    );
    run_event(&mut app, "run", "done", "AgentRunCompleted", json!({}));
    let text = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert_eq!(text.matches("answer line").count(), 1, "{text}");
}

#[test]
fn completed_run_structure_survives_older_page() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let mut tail = history_run_page(Some("agent_run_completed"));
    tail.older_cursor = Some("older".into());
    tail.has_older = true;
    app.transcript_paging = Some(TranscriptPagingState::open(tail).unwrap());
    sync_materialized_transcript_history(&mut app);
    app.transcript_paging
        .as_mut()
        .unwrap()
        .apply_older_page(transcript_test_page(
            vec![run_block(
                "earlier",
                1,
                "run",
                "phase_event",
                TranscriptBlockBodyV1::Notice {
                    notice_type: "model_process_summary".into(),
                    content: TranscriptTextContentV1::inline("Earlier stage".into()),
                    status: TranscriptBlockStatusV1::Running,
                },
            )],
            None,
        ))
        .unwrap();
    sync_materialized_transcript_history(&mut app);
    let view = build_transcript_view(&app, 80);
    let text = rendered_lines_text(&view.lines);
    assert!(text.contains("Earlier stage"));
    assert!(text.contains("Inspecting source"));
    assert_eq!(text.matches("Useful result").count(), 1);
    assert!(!text.contains("└─"));
}

#[test]
fn prepending_history_preserves_live_stream_replacement_boundary() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    app.transcript_paging =
        Some(TranscriptPagingState::open(transcript_test_page(vec![], Some("older"))).unwrap());
    app.active_agent_run_id = Some("live".into());
    run_event(
        &mut app,
        "live",
        "delta",
        "ModelTextDelta",
        json!({"delta":"draft\n"}),
    );
    materialize_assistant_prefix(&mut app);
    app.transcript_paging
        .as_mut()
        .unwrap()
        .apply_older_page(transcript_test_page(
            vec![transcript_test_block(
                "user-old",
                1,
                1,
                TranscriptBlockBodyV1::UserText {
                    content: TranscriptTextContentV1::inline("Historical question".into()),
                },
            )],
            None,
        ))
        .unwrap();
    sync_materialized_transcript_history(&mut app);
    run_event(
        &mut app,
        "live",
        "final",
        "Final",
        json!({"content":"replacement answer"}),
    );
    let text = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert!(text.contains("Historical question"), "{text}");
    assert!(text.contains("replacement answer"));
    assert!(!text.contains("draft"));
}

#[test]
fn runtime_wait_and_resume_keep_run_open_with_truthful_status() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    app.active_agent_run_id = Some("run".into());
    for (id, status, expected) in [("wait", "waiting", "Waiting..."), ("resume", "resumed", "")] {
        apply_session_event(
            &mut app,
            &json!({"id":id,"type":"RuntimeWaitChanged","visibility":"user","processState":"waiting","payload":{"status":status}}),
            Some("run"),
        );
        assert_eq!(status_header(&app), expected);
        assert!(has_active_agent_run(&app));
    }
}

#[test]
fn child_wait_does_not_change_parent_run_status() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    app.active_agent_run_id = Some("parent".into());
    app.active_agent_run_ids = HashSet::from(["parent".into(), "child".into()]);
    app.process_state = RuntimeDisplayState::PendingPhase;
    apply_session_event(
        &mut app,
        &json!({"id":"wait-child","type":"RuntimeWaitChanged","processState":"waiting","payload":{"status":"waiting"}}),
        Some("child"),
    );
    let view = build_transcript_view(&app, 80);
    assert_eq!(status_header(&app), "");
    let child_row = view
        .source_rows
        .iter()
        .find(|(id, _)| id == "status:child")
        .unwrap()
        .1;
    assert!(rendered_line_text(&view.lines[child_row as usize]).contains("Waiting"));
}

#[test]
fn active_tools_show_six_line_preview_without_an_empty_completion_rule() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    app.active_agent_run_id = Some("run".into());
    let mut tool = test_tool_line(
        ToolActionKind::Read,
        "README.md",
        vec![],
        vec![ToolResultState::SuccessWithOutput],
    );
    tool.full_text = Some((0..20).map(|i| format!("body {i}\n")).collect());
    app.transcript = vec![
        TranscriptLine::Summary("Checking the project".into()).for_run(Some("run"), false),
        TranscriptLine::Tool(tool).for_run(Some("run"), false),
    ];
    let running = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert!(running.contains("body 0"), "{running}");
    assert!(running.contains("body 5"));
    assert!(!running.contains("body 6"));
    assert!(running.contains("  │ body 0") && running.contains('…'));
    run_event(
        &mut app,
        "run",
        "answer",
        "Final",
        json!({"content":"The result"}),
    );
    run_event(&mut app, "run", "end", "AgentRunCompleted", json!({}));
    let closed = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert!(closed.contains("Checking the project") && closed.contains("The result"));
    assert!(!closed.contains("README.md") && !closed.contains("body 0"));
    assert!(!closed.contains('─'));
    assert!(!closed.contains("Worked") && !closed.contains('⌄'));
}

#[test]
fn automatic_referenced_previews_only_load_visible_tools_without_collapsing_previous_rows() {
    use centaeris_core::session::transcript::TranscriptContentRefV1;
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let blocks = (1..=12)
        .map(|seq| {
            transcript_test_block(
                &format!("tool:call-{seq}"),
                1,
                seq,
                TranscriptBlockBodyV1::Tool {
                    call_id: format!("call-{seq}"),
                    tool_name: "read".into(),
                    status: TranscriptBlockStatusV1::Completed,
                    summary: Some(format!("Read file-{seq}")),
                    summary_ref: None,
                    output_ref: Some(TranscriptContentRefV1 {
                        ref_id: format!("ref-{seq}"),
                        revision: "1".into(),
                        byte_length: "100".into(),
                    }),
                },
            )
        })
        .collect();
    app.transcript_paging =
        Some(TranscriptPagingState::open(transcript_test_page(blocks, None)).unwrap());
    sync_materialized_transcript_history(&mut app);
    app.transcript_follow_bottom = false;
    let view = build_transcript_view(&app, 80);
    let (first, row) = view.source_rows[0].clone();
    app.transcript_scroll = row;
    output_preview::prepare_visible(&mut app, &view, 1);
    assert_eq!(app.output_preview.len(), 1);
    assert!(app.output_preview.contains_key(&first));
    let (last, row) = view.source_rows.last().unwrap().clone();
    app.transcript_scroll = row;
    output_preview::prepare_visible(&mut app, &view, 1);
    assert_eq!(app.output_preview.len(), 2);
    assert!(app.output_preview.contains_key(&last));
    assert!(app.output_preview.contains_key(&first));
    app.transcript_paging = None;
    assert!(output_preview::poll(&mut app));
    assert!(app.output_preview.is_empty());
}

#[test]
fn retired_tool_shortcuts_do_not_change_the_transcript_or_insert_control_characters() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let mut tool = test_tool_line(
        ToolActionKind::Read,
        "README.md",
        vec![],
        vec![ToolResultState::SuccessWithOutput],
    );
    tool.full_text = Some("visible body".into());
    app.transcript.push(TranscriptLine::Tool(tool));
    let before = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    for key in [
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
    ] {
        handle_key(key, &mut app);
    }
    assert!(app.input.is_empty());
    assert_eq!(
        rendered_lines_text(&build_transcript_view(&app, 80).lines),
        before
    );
    for ch in "npvq".chars() {
        handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            &mut app,
        );
    }
    assert_eq!(app.input, "npvq");
}

#[test]
fn transcript_visual_contract_marks_messages_once_and_colors_code() {
    let items = vec![
        TranscriptLine::User("inspect".into()),
        TranscriptLine::Summary("Checking `Cargo.toml`".into()),
        TranscriptLine::LiveAssistant {
            markdown: "First line\n".into(),
            separator: false,
        },
        TranscriptLine::LiveAssistant {
            markdown: "Second line".into(),
            separator: true,
        },
        TranscriptLine::Summary("Finished".into()),
    ];
    let lines = transcript_to_lines(&items, 80);
    let text = lines
        .iter()
        .map(rendered_line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.starts_with("  ❯ inspect"));
    assert!(text.contains("  Checking Cargo.toml"));
    assert!(text.contains("  First line\n  Second line"));
    assert!(text.contains("  Finished"));
    let code = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "Cargo.toml")
        .unwrap();
    assert_eq!(code.style.fg, Some(ratatui::style::Color::Green));
    assert!(code.style.bg.is_none());
}

#[test]
fn command_visual_contract_uses_command_not_description_and_single_connector() {
    let mut tool = test_tool_line(
        ToolActionKind::Command,
        "Friendly description",
        vec![],
        vec![ToolResultState::SuccessWithOutput],
    );
    tool.command = Some("Get-ChildItem -LiteralPath 'D:\\Projects' | Select-Object Name".into());
    tool.description_title = true;
    tool.full_text = Some("first\nsecond".into());
    tool.result_blocks = vec![ToolResultBlock::Text {
        lines: vec![
            TextResultLine::Text("first".into()),
            TextResultLine::Text("second".into()),
        ],
    }];
    let lines = transcript_to_lines(&[TranscriptLine::Tool(tool.clone())], 100);
    let text = lines
        .iter()
        .map(rendered_line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.starts_with("  Ran Get-ChildItem"));
    assert!(!text.contains("Friendly description"));
    assert!(text.contains("  │ first\n  │ second"));
    assert!(lines[0]
        .spans
        .iter()
        .any(|s| s.style.add_modifier.contains(Modifier::BOLD)));
    assert!(lines[0]
        .spans
        .iter()
        .any(|s| s.content.contains("-LiteralPath") && s.style.fg.is_some()));
    let root = unique_test_dir("visual-contract");
    let mut app = test_app("", root.clone(), root.clone());
    app.transcript.push(TranscriptLine::Tool(tool));
    let view = build_transcript_view(&app, 100);
    let actual = view
        .lines
        .iter()
        .map(rendered_line_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(actual.starts_with("  Ran Get-ChildItem"));
    assert!(actual.contains("  │ first\n  │ second"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn assistant_visual_contract_wraps_with_hanging_indent() {
    let lines =
        text_layout::TextLayout::new(render_summary_lines("abcdefghijklmnop", 8), 8, true).lines;
    let text: Vec<_> = lines.iter().map(rendered_line_text).collect();
    assert_eq!(text, ["  abcdef", "  ghijkl", "  mnop"]);
}

#[test]
fn tool_visual_contract_wraps_command_with_hanging_indent() {
    let mut tool = test_tool_line(
        ToolActionKind::Command,
        "abcdef ghijklmnop",
        vec![],
        vec![ToolResultState::SuccessNoOutput],
    );
    tool.command = Some("abcdef ghijklmnop".into());
    let (header, indent) = presentation::tool_header(&tool);
    let layout = text_layout::TextLayout::transcript(vec![header], 12, &[(0, indent)], &[], &[]);
    let texts: Vec<_> = layout.lines.iter().map(rendered_line_text).collect();
    assert!(texts[0].starts_with("  Ran "));
    assert!(texts[1].starts_with("      "));
    assert!(texts.iter().all(|text| text.chars().count() <= 12));
    assert_eq!(layout.text, "  Ran abcdef ghijklmnop");
}

#[test]
fn visualization_tui_preserves_information_in_summary_and_stream() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../core/tests/fixtures/visualizations.json"
    ))
    .unwrap();
    let source = cases[0]["source"].as_str().unwrap();
    let fence = "\u{60}".repeat(3);
    let markdown = format!("Before\n\n{fence}centaeris_visual\n{source}\n{fence}\n\nAfter");
    let text = rendered_lines_text(&render_summary_lines(&markdown, 80));
    assert!(text.contains("Run A") && text.contains("Run B") && text.contains("After"));
    assert!(!text.contains("elements"));
    let workspace = unique_test_dir("visualization-workspace");
    let data_root = unique_test_dir("visualization-data");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.assistant_buffer = format!("Before\n\n{fence}centaeris_visual\n{source}");
    materialize_assistant_prefix(&mut app);
    let pending = rendered_lines_text(&build_assistant_live_lines(&app, 80));
    assert!(!pending.contains("elements"));
    app.assistant_buffer.push_str(&format!("\n{fence}\n"));
    materialize_assistant_prefix(&mut app);
    let complete = rendered_lines_text(&transcript_to_lines(&app.transcript, 80));
    assert!(complete.contains("Run A") && complete.contains("Run B"));
    assert!(!complete.contains("elements"));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn visualization_referenced_tui_answer_loads_all_pages_and_restores_reading_order() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../core/tests/fixtures/visualizations.json"
    ))
    .unwrap();
    let fence = "\u{60}".repeat(3);
    let body = format!(
        "{}\n\n{fence}centaeris_visual\n{}\n{fence}\nAfter",
        "Preface ".repeat(2050),
        cases[0]["source"].as_str().unwrap()
    );
    let mut page = history_run_page(None);
    if let TranscriptBlockBodyV1::AssistantText { content, .. } = &mut page.blocks[1].body {
        *content = TranscriptTextContentV1 {
            inline_content: None,
            source_ref: Some(
                centaeris_core::session::transcript::TranscriptContentRefV1 {
                    ref_id: "answer-ref".into(),
                    revision: "1".into(),
                    byte_length: body.len().to_string(),
                },
            ),
        };
    }
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requests = calls.clone();
    app.runtime = Some(RuntimeClient::from_test_request_handler(move |frame| {
        assert_eq!(frame["method"], "transcript/content-range");
        let request = &frame["params"]["request"];
        requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let start: usize = request["offset"].as_str().unwrap().parse().unwrap();
        let mut end = (start + request["maxBytes"].as_u64().unwrap() as usize).min(body.len());
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        json!({"schema":"transcript.content.range.v1","sessionId":request["sessionId"],"projectionVersion":request["projectionVersion"],
            "projectionGeneration":request["projectionGeneration"],"refId":"answer-ref","revision":"1","byteLength":body.len().to_string(),
            "startOffset":start.to_string(),"endOffset":end.to_string(),"content":&body[start..end],"hasMore":end<body.len()})
    }));
    app.transcript_paging = Some(TranscriptPagingState::open(page).unwrap());
    sync_materialized_transcript_history(&mut app);
    let initial = build_transcript_view(&app, 100);
    assert!(rendered_lines_text(&initial.lines).contains("Loading answer"));
    output_preview::prepare_visible(&mut app, &initial, 100);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        output_preview::poll(&mut app);
        let text = rendered_lines_text(&build_transcript_view(&app, 100).lines);
        if text.contains("Run A") && text.contains("Run B") && text.contains("After") {
            assert!(!text.contains("elements"));
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(calls.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}

#[test]
fn visualization_referenced_answer_rejects_oversized_body_before_reading() {
    let path = PathBuf::from("D:/presentation-test");
    let mut app = test_app("", path.clone(), path);
    let mut page = history_run_page(None);
    if let TranscriptBlockBodyV1::AssistantText { content, .. } = &mut page.blocks[1].body {
        *content = TranscriptTextContentV1 {
            inline_content: None,
            source_ref: Some(
                centaeris_core::session::transcript::TranscriptContentRefV1 {
                    ref_id: "oversized-answer".into(),
                    revision: "1".into(),
                    byte_length: (centaeris_core::session::visualization::MAX_ANSWER_BYTES + 1)
                        .to_string(),
                },
            ),
        };
    }
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requests = calls.clone();
    app.runtime = Some(RuntimeClient::from_test_request_handler(move |_| {
        requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        json!(null)
    }));
    app.transcript_paging = Some(TranscriptPagingState::open(page).unwrap());
    sync_materialized_transcript_history(&mut app);
    let initial = build_transcript_view(&app, 80);
    output_preview::prepare_visible(&mut app, &initial, 100);
    let text = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert!(text.contains("display byte limit"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[derive(Default)]
struct TestClipboard(std::rc::Rc<std::cell::RefCell<Vec<String>>>);
impl crate::clipboard::TextClipboard for TestClipboard {
    fn store(&mut self, text: &str) -> Result<(), String> {
        self.0.borrow_mut().push(text.into());
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct TextPoint {
    row: u16,
    column: u16,
}

fn input_byte_at_position(input: &str, usable: usize, target: TextPoint) -> usize {
    text_layout::TextLayout::plain(input, usable.min(u16::MAX as usize) as u16)
        .offset_at(target.row as usize, target.column as usize)
}

fn hard_wrap_input_lines(lines: Vec<Line<'static>>, usable: usize) -> Vec<Line<'static>> {
    text_layout::TextLayout::new(lines, usable.min(u16::MAX as usize) as u16, false).lines
}

fn cursor_position_in_wrapped(before: &str, usable: usize) -> (usize, usize) {
    text_layout::TextLayout::plain(before, usable.min(u16::MAX as usize) as u16)
        .position(before.len())
}

fn record_clipboard(app: &mut App) -> std::rc::Rc<std::cell::RefCell<Vec<String>>> {
    let clipboard = TestClipboard::default();
    let result = clipboard.0.clone();
    app.clipboard = Box::new(clipboard);
    result
}
fn draw_for_mouse(app: &mut App, width: u16, height: u16) {
    let view = build_transcript_view(app, width);
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app, &view)).unwrap();
}
fn send_mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    handle_mouse(
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
        app,
    );
}

#[test]
fn unicode_editing_and_undo_keep_whole_graphemes() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    handle_paste(&mut app, "Ae\u{301}👩‍💻B".into());
    handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &mut app);
    handle_key(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        &mut app,
    );
    assert_eq!(app.input, "Ae\u{301}B");
    handle_key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &mut app,
    );
    assert_eq!(app.input, "Ae\u{301}👩‍💻B");
    handle_key(
        KeyEvent::new(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        &mut app,
    );
    assert_eq!(app.input, "Ae\u{301}B");
    handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT), &mut app);
    assert_eq!(&app.input[input_selection_range(&app).unwrap()], "e\u{301}");
}

#[cfg(windows)]
#[test]
fn altgr_text_does_not_trigger_control_shortcuts() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    for ch in ['v', 'c', 'g', 'z'] {
        handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL | KeyModifiers::ALT),
            &mut app,
        );
    }
    assert_eq!(app.input, "vcgz");
}

#[test]
fn dragging_outside_a_long_draft_scrolls_until_keyboard_input_ends_capture() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    let text = (0..30)
        .map(|row| format!("draft line {row}"))
        .collect::<Vec<_>>()
        .join("\n");
    handle_paste(&mut app, text);
    app.input_cursor = 0;
    draw_for_mouse(&mut app, 60, 30);
    let area = app.input_area.unwrap();
    send_mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x,
        area.y,
    );
    send_mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        area.x + 5,
        area.bottom(),
    );
    assert!(mouse::drag_scroll_tick(&mut app));
    draw_for_mouse(&mut app, 60, 30);
    assert_eq!(app.input_scroll, 1);
    assert!(app.input_cursor > "draft line 0\n".len());
    handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &mut app);
    let scroll = app.input_scroll;
    assert!(!mouse::drag_scroll_tick(&mut app));
    assert_eq!(app.input_scroll, scroll);
}

#[test]
fn image_undo_keeps_owned_bytes_until_the_draft_is_cleared() {
    let root = unique_test_dir("editor-image-undo");
    let source = root.join("source.png");
    std::fs::write(&source, test_png_bytes()).unwrap();
    let mut app = test_app("", root.clone(), root.clone());
    handle_paste(&mut app, source.to_string_lossy().into_owned());
    let owned = app.draft_image_attachments[0].local_path.clone();
    handle_key(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        &mut app,
    );
    assert_eq!(app.input, "[Image #1]");
    assert_eq!(app.draft_image_attachments.len(), 1);
    handle_key(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
        &mut app,
    );
    assert!(app.draft_image_attachments.is_empty());
    assert!(owned.exists());
    handle_key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &mut app,
    );
    assert_eq!(app.draft_image_attachments[0].local_path, owned);
    assert_eq!(app.input, "[Image #1]");
    clear_composer(&mut app);
    assert!(!owned.exists());
    assert!(source.exists());
    handle_key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &mut app,
    );
    assert!(app.input.is_empty());
    handle_paste(&mut app, source.to_string_lossy().into_owned());
    let undone = app.draft_image_attachments[0].local_path.clone();
    handle_key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &mut app,
    );
    assert!(app.draft_image_attachments.is_empty());
    assert!(undone.exists());
    handle_key(
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
        &mut app,
    );
    assert_eq!(app.draft_image_attachments[0].local_path, undone);
    handle_key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &mut app,
    );
    clear_composer(&mut app);
    assert!(!undone.exists());
    assert!(source.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn credential_input_stays_in_place_and_selection_never_copies_or_survives_cancel() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("draft", path.clone(), path);
    let copied = record_clipboard(&mut app);
    copied.borrow_mut().push("clipboard baseline".to_string());
    handle_key(
        KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        &mut app,
    );
    open_model_panel(&mut app, &model_picker_config()).unwrap();
    select_model_provider(&mut app, 1);
    activate_selected_model(&mut app).unwrap();
    draw_for_mouse(&mut app, 80, 24);
    let area = app.input_area.unwrap();

    for ch in "/test-token".chars() {
        handle_key(
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE),
            &mut app,
        );
        draw_for_mouse(&mut app, 80, 24);
        assert_eq!(app.input_area, Some(area));
        assert!(!app.command_panel_open());
    }
    send_mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x,
        area.y,
    );
    send_mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        area.x + 6,
        area.y,
    );
    send_mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        area.x + 6,
        area.y,
    );
    assert_eq!(input_selection_range(&app), Some(0..6));
    assert_eq!(&*copied.borrow(), &["clipboard baseline"]);
    assert_eq!(app.input, "/test-token");
    handle_key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &mut app,
    );
    assert_eq!(app.input, "/test-token");
    handle_key(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &mut app,
    );
    assert!(app.model_credential_prompt.is_none());
    assert!(app.model_panel.is_some());
    assert_eq!(app.input, "draft!");
    close_model_panel(&mut app);
    handle_key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &mut app,
    );
    assert_eq!(app.input, "draft");
}

#[test]
fn transcript_triple_click_copies_a_logical_line_and_survives_resize() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    let text = "alpha 中文👩‍💻 beta and a long sentence across terminal rows";
    app.transcript.push(TranscriptLine::User(text.into()));
    let copied = record_clipboard(&mut app);
    draw_for_mouse(&mut app, 40, 24);
    let area = app.transcript_area.unwrap();
    let (row_in_alpha, column_in_alpha) = app.text_layout.as_ref().unwrap().position(1);
    for _ in 0..3 {
        send_mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            area.x + column_in_alpha as u16,
            area.y + row_in_alpha as u16,
        );
        send_mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            area.x + column_in_alpha as u16,
            area.y + row_in_alpha as u16,
        );
    }
    assert_eq!(&*copied.borrow(), &["alpha".to_string(), text.to_string()]);
    let selected = selected_transcript_text(&app);
    draw_for_mouse(&mut app, 70, 24);
    assert_eq!(selected_transcript_text(&app), selected);
}

#[test]
fn bash_fold_stays_on_the_same_call_through_result_and_run_completion() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    let clipboard = record_clipboard(&mut app);
    app.active_agent_run_id = Some("run".into());
    apply_session_event(
        &mut app,
        &json!({"id":"start","type":"ToolCall","toolName":"bash","status":"running","payload":{
            "callId":"bash-probe","displayTarget":"Run focused checks","description":"Run focused checks",
            "command":"printf 'probe'"
        }}),
        Some("run"),
    );
    let collapsed = build_transcript_view(&app, 80);
    assert!(rendered_lines_text(&collapsed.lines).contains("Running focused checks"));
    assert!(!rendered_lines_text(&collapsed.lines).contains("printf"));

    let click = |app: &mut App, column, row| {
        send_mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
        send_mouse(app, MouseEventKind::Up(MouseButton::Left), column, row);
    };
    draw_for_mouse(&mut app, 80, 24);
    let area = app.transcript_area.unwrap();
    click(&mut app, area.right() - 1, area.y);
    assert!(app.expanded_bash_tools.is_empty());
    click(&mut app, area.x + 5, area.y);
    let expanded = build_transcript_view(&app, 80);
    assert!(rendered_lines_text(&expanded.lines).contains("$ printf 'probe'"));
    assert!(clipboard.borrow().is_empty());

    let output: String = (1..=9).map(|i| format!("probe line {i}\n")).collect();
    apply_session_event(
        &mut app,
        &json!({"id":"result","type":"ToolResult","toolName":"bash","status":"error","payload":{
            "callId":"bash-probe","resultState":"failed","modelContent":output,"modelInputImages":[],
            "operations":[{"kind":"command","toolName":"bash","callId":"bash-probe","status":"error","exitCode":7}]
        }}),
        Some("run"),
    );
    run_event(
        &mut app,
        "run",
        "answer",
        "Final",
        json!({"content":"Final answer"}),
    );
    run_event(&mut app, "run", "complete", "AgentRunCompleted", json!({}));
    let settled = rendered_lines_text(&build_transcript_view(&app, 80).lines);
    assert_eq!(
        settled.matches("Failed focused checks (exit 7)").count(),
        1,
        "{settled}"
    );
    assert!(
        settled.contains("probe line 9") && settled.contains("Final answer"),
        "{settled}"
    );

    draw_for_mouse(&mut app, 60, 24);
    let area = app.transcript_area.unwrap();
    let output_row = area.y + 2;
    send_mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 4,
        output_row,
    );
    send_mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        area.x + 9,
        output_row,
    );
    send_mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        area.x + 9,
        output_row,
    );
    assert_eq!(clipboard.borrow().last().map(String::as_str), Some("probe"));

    click(&mut app, area.x + 5, output_row);
    assert!(!app.expanded_bash_tools.is_empty());
    click(&mut app, area.right() - 1, output_row + 1);
    assert_eq!(clipboard.borrow().len(), 1);
    let collapsed_view = build_transcript_view(&app, 22);
    let collapsed = rendered_lines_text(&collapsed_view.lines);
    assert!(
        rendered_line_text(&collapsed_view.lines[0]).ends_with("(exit 7)"),
        "{collapsed}"
    );
    assert!(
        !collapsed.contains("probe line") && collapsed.contains("Final answer"),
        "{collapsed}"
    );
}

#[test]
fn transcript_spacing_stacks_collapsed_tools_and_separates_the_reply() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    for key in ["first check", "second check"] {
        let mut tool = test_tool_line(
            ToolActionKind::Command,
            key,
            vec![],
            vec![ToolResultState::SuccessWithOutput],
        );
        tool.key = key.into();
        tool.is_bash = true;
        tool.command = Some(format!("printf '{key}'"));
        app.transcript.push(TranscriptLine::Tool(tool));
    }
    app.assistant_buffer = "Final answer".into();
    let collapsed = build_transcript_view(&app, 80);
    assert_eq!(collapsed.source_rows[1].1, collapsed.source_rows[0].1 + 1);
    let reply_row = collapsed.source_rows[1].1 as usize + 2;
    assert!(rendered_line_text(&collapsed.lines[reply_row - 1]).is_empty());
    assert_eq!(
        rendered_line_text(&collapsed.lines[reply_row]),
        "  Final answer"
    );
    assert!(collapsed
        .layout
        .text
        .contains("second check\n  Final answer"));

    app.expanded_bash_tools.insert("second check".into());
    draw_for_mouse(&mut app, 80, 24);
    let expanded = build_transcript_view(&app, 80);
    let area = app.transcript_area.unwrap();
    let expanded_row = expanded.source_rows[1].1 as usize;
    assert_eq!(expanded.source_rows[1].1, expanded.source_rows[0].1 + 2);
    let reply_row = expanded
        .lines
        .iter()
        .position(|line| rendered_line_text(line) == "  Final answer")
        .unwrap();
    for gap in [expanded_row - 1, reply_row - 1] {
        assert!(rendered_line_text(&expanded.lines[gap]).is_empty());
        assert!(!rendered_line_text(&expanded.lines[gap - 1]).is_empty());
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            send_mouse(&mut app, kind, area.x, area.y + gap as u16);
        }
    }
    assert_eq!(app.expanded_bash_tools.len(), 1);
    assert!(app.expanded_bash_tools.contains("second check"));
}

#[test]
fn panel_hover_and_wheel_are_isolated_from_the_transcript() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/", path.clone(), path);
    app.transcript
        .extend((0..80).map(|row| TranscriptLine::User(format!("row {row}"))));
    app.transcript_follow_bottom = false;
    app.transcript_scroll = 5;
    draw_for_mouse(&mut app, 80, 24);
    let area = app.panel_area.unwrap();
    send_mouse(&mut app, MouseEventKind::Moved, area.x + 3, area.y + 1);
    assert_eq!(app.selected_command, 0);
    assert_eq!(app.input, "/");
    send_mouse(&mut app, MouseEventKind::ScrollDown, area.x + 3, area.y + 1);
    assert_eq!(app.selected_command, 1);
    assert_eq!(app.transcript_scroll, 5);
    app.home_risk_panel = Some(HomeRiskPanel {
        workspaces: Vec::new(),
        selected: 0,
        notice: None,
    });
    draw_for_mouse(&mut app, 80, 24);
    let area = app.panel_area.unwrap();
    send_mouse(&mut app, MouseEventKind::ScrollDown, area.x + 3, area.y + 2);
    assert!(app.home_risk_panel.is_some());
    assert_eq!(app.transcript_scroll, 5);
    assert!(app.runtime.is_none());
}

#[test]
fn command_list_distinguishes_hover_from_the_current_selection() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/", path.clone(), path);
    let selected = SLASH_COMMANDS
        .iter()
        .position(|command| command.name == "/effort")
        .unwrap();
    let hovered = SLASH_COMMANDS
        .iter()
        .position(|command| command.name == "/help")
        .unwrap();
    app.selected_command = selected;
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    let view = build_transcript_view(&app, 80);
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .unwrap();
    let area = mouse::embedded_area(&app).unwrap();
    let x = area.x;
    let selected_y = area.y + selected as u16;
    let hover_y = area.y + hovered as u16;
    send_mouse(&mut app, MouseEventKind::Moved, x + 3, hover_y);
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let current = buffer.cell((x, selected_y)).unwrap();
    let hover = buffer.cell((x, hover_y)).unwrap();
    assert_eq!(current.symbol(), "❯");
    assert!(current
        .modifier
        .contains(Modifier::REVERSED | Modifier::BOLD));
    assert!(!hover
        .modifier
        .intersects(Modifier::REVERSED | Modifier::BOLD | Modifier::UNDERLINED));
    assert_ne!(hover.bg, current.bg);
    assert_eq!(
        app.selected_command, selected,
        "hover does not move the keyboard selection"
    );
    let selected_style = current.style();
    send_mouse(&mut app, MouseEventKind::Moved, x + 3, selected_y);
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .unwrap();
    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((x, selected_y))
            .unwrap()
            .style(),
        selected_style,
        "selection wins when hover overlaps it"
    );
    assert_eq!(
        terminal.backend().buffer().cell((x, hover_y)).unwrap().bg,
        Color::Reset,
        "the previous hover band is cleared"
    );
    send_mouse(&mut app, MouseEventKind::Moved, x + 3, hover_y);
    handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut app);
    assert_eq!(
        app.input, "/effort ",
        "confirmation still uses the selected item"
    );
}

#[test]
fn model_and_effort_pages_keep_one_selection_while_hovering_another_row() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("draft", path.clone(), path);
    let config = model_picker_config();
    apply_model_display(&mut app, &config);
    open_model_panel(&mut app, &config).unwrap();
    handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut app);
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
    let view = build_transcript_view(&app, 90);
    for effort in [false, true] {
        if effort {
            handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        }
        terminal
            .draw(|frame| render(frame, &mut app, &view))
            .unwrap();
        let area = app.model_list_area.unwrap();
        let (selected, hovered) = if effort { (0, 1) } else { (1, 0) };
        send_mouse(
            &mut app,
            MouseEventKind::Moved,
            area.x + 3,
            area.y + hovered,
        );
        terminal
            .draw(|frame| render(frame, &mut app, &view))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let current = buffer.cell((area.x, area.y + selected)).unwrap();
        let hover = buffer.cell((area.x, area.y + hovered)).unwrap();
        assert_eq!(current.symbol(), "❯");
        assert!(current
            .modifier
            .contains(Modifier::REVERSED | Modifier::BOLD));
        assert!(!hover
            .modifier
            .intersects(Modifier::REVERSED | Modifier::UNDERLINED));
        assert_ne!(current.bg, hover.bg);
        assert_eq!(model_picker::selection(&app), selected as usize);
    }
    assert_eq!(app.model_display.as_deref(), Some("model-a"));
    assert_eq!(app.input, "draft");
}

#[test]
fn scrollbar_drag_has_the_same_bounds_as_the_viewport_and_resize_ends_capture() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    app.transcript
        .extend((0..100).map(|row| TranscriptLine::User(format!("row {row}"))));
    app.transcript_follow_bottom = false;
    draw_for_mouse(&mut app, 80, 24);
    let area = app.transcript_viewport.unwrap();
    let x = area.right() - 1;
    send_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, area.y);
    draw_for_mouse(&mut app, 80, 24);
    send_mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        x,
        area.bottom() - 1,
    );
    assert_eq!(app.transcript_scroll, app.transcript_max_scroll);
    mouse::cancel_capture(&mut app);
    let current = app.transcript_scroll;
    send_mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, area.y);
    assert_eq!(app.transcript_scroll, current);
}

#[test]
fn clipboard_failure_keeps_the_draft_and_reports_the_error() {
    struct Failed;
    impl crate::clipboard::TextClipboard for Failed {
        fn store(&mut self, _: &str) -> Result<(), String> {
            Err("clipboard unavailable".into())
        }
    }
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("abcdef", path.clone(), path);
    app.clipboard = Box::new(Failed);
    draw_for_mouse(&mut app, 80, 24);
    let area = app.input_area.unwrap();
    send_mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x,
        area.y,
    );
    send_mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        area.x + 3,
        area.y,
    );
    send_mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        area.x + 3,
        area.y,
    );
    assert_eq!(app.input, "abcdef");
    assert_eq!(app.message.as_deref(), Some("clipboard unavailable"));
}

fn model_picker_config() -> Value {
    let models = json!([
        {"model":"model-a","displayName":"Alpha","modelThinkingMode":"medium","modelThinkingModes":["low","medium","high"],"preferredThinkingMode":"low"},
        {"model":"model-b","displayName":"Beta","modelThinkingMode":"medium","modelThinkingModes":["low","medium","high"],"preferredThinkingMode":"low"},
        {"model":"fixed","displayName":"Fixed","modelThinkingMode":null,"modelThinkingModes":[],"preferredThinkingMode":null}
    ]);
    let selectable = models
        .as_array()
        .unwrap()
        .iter()
        .map(|model| {
            let mut model = model.clone();
            model["providerId"] = json!("test");
            model
        })
        .collect::<Vec<_>>();
    json!({
        "modelProviderId":"test","model":"model-a","modelThinkingMode":"high",
        "modelProviders":[
            {"providerId":"test","name":"Test","configured":true,"models":models},
            {"providerId":"setup","name":"Setup","configured":false,"models":[]}
        ],
        "selectableModels":selectable
    })
}

fn attach_model_runtime(
    app: &mut App,
    config: Value,
) -> std::sync::Arc<std::sync::Mutex<Vec<Value>>> {
    let writes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = writes.clone();
    let config = std::sync::Mutex::new(config);
    app.runtime = Some(RuntimeClient::from_test_request_handler(move |frame| {
        let mut config = config.lock().unwrap();
        match frame["method"].as_str().unwrap() {
            "agent_runtime_config_get" => config.clone(),
            "agent_runtime_config_set" => {
                let request = frame["params"]["request"].clone();
                recorded.lock().unwrap().push(request.clone());
                config["modelProviderId"] = request["modelProviderId"].clone();
                config["model"] = request["model"].clone();
                if let Some(mode) = request.get("modelThinkingMode") {
                    config["modelThinkingMode"] = mode.clone();
                }
                config.clone()
            }
            other => panic!("unexpected model request: {other}"),
        }
    }));
    writes
}

fn finish_model_requests(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.pending_model_request.is_some() {
        drain_model_request(app);
        assert!(Instant::now() < deadline, "model request did not settle");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn model_picker_chains_to_effort_and_escape_preserves_the_draft_without_writing() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("draft 👩‍💻", path.clone(), path);
    let writes = attach_model_runtime(&mut app, model_picker_config());
    handle_key(
        KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        &mut app,
    );
    finish_model_requests(&mut app);
    handle_key(
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE),
        &mut app,
    );
    assert_eq!(model_picker::rows(&app).len(), 1);
    handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
    assert_eq!(
        app.model_panel.as_ref().unwrap().stage,
        model_picker::Stage::ModelEffort(0)
    );
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
    let view = build_transcript_view(&app, 90);
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .unwrap();
    let rendered = test_buffer_text(terminal.backend().buffer());
    assert!(rendered.contains("Pick reasoning effort · Beta"));
    assert!(rendered.contains("medium (default)"));
    assert!(writes.lock().unwrap().is_empty());
    handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert_eq!(
        app.model_panel.as_ref().unwrap().stage,
        model_picker::Stage::Models
    );
    handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert!(app.model_panel.is_none());
    assert_eq!(app.input, "draft 👩‍💻");
    assert!(writes.lock().unwrap().is_empty());
}

#[test]
fn model_picker_mouse_and_enter_apply_model_and_effort_in_one_request() {
    for mouse in [false, true] {
        let path = PathBuf::from("D:/workspace");
        let mut app = test_app("unsent draft", path.clone(), path);
        let config = model_picker_config();
        apply_model_display(&mut app, &config);
        open_model_panel(&mut app, &config).unwrap();
        let writes = attach_model_runtime(&mut app, config);
        if mouse {
            draw_for_mouse(&mut app, 90, 24);
            let area = app.model_list_area.unwrap();
            send_mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                area.x,
                area.y + 1,
            );
        } else {
            handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut app);
            handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        }
        assert!(writes.lock().unwrap().is_empty());
        assert_eq!(
            app.model_panel.as_ref().unwrap().stage,
            model_picker::Stage::ModelEffort(0)
        );
        if mouse {
            draw_for_mouse(&mut app, 90, 24);
            let area = app.model_list_area.unwrap();
            send_mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                area.x,
                area.y + 2,
            );
        } else {
            handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut app);
            handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut app);
            handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        }
        finish_model_requests(&mut app);
        assert_eq!(
            *writes.lock().unwrap(),
            vec![json!({"modelProviderId":"test","model":"model-b","modelThinkingMode":"high"})]
        );
        assert!(app.model_panel.is_none());
        assert_eq!(app.model_display.as_deref(), Some("model-b"));
        assert_eq!(app.model_effort.as_deref(), Some("high"));
        assert_eq!(app.input, "unsent draft");
    }
}

#[test]
fn clicking_model_commands_opens_the_picker_without_applying_a_setting() {
    for (input, command, stage) in [
        ("/", "/model", model_picker::Stage::Models),
        ("/mo", "/model", model_picker::Stage::Models),
        ("/model", "/model", model_picker::Stage::Models),
        ("/", "/effort", model_picker::Stage::CurrentEffort(2)),
        ("/ef", "/effort", model_picker::Stage::CurrentEffort(2)),
        ("/effort", "/effort", model_picker::Stage::CurrentEffort(2)),
    ] {
        let path = PathBuf::from("D:/workspace");
        let mut app = test_app(input, path.clone(), path);
        let writes = attach_model_runtime(&mut app, model_picker_config());
        draw_for_mouse(&mut app, 80, 24);
        let area = app.panel_area.unwrap();
        let row = matching_commands(input)
            .iter()
            .position(|item| item.name == command)
            .unwrap();
        send_mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            area.x + 3,
            area.y + row as u16,
        );
        finish_model_requests(&mut app);
        assert_eq!(app.model_panel.as_ref().unwrap().stage, stage);
        assert!(app.input.is_empty());
        assert!(!app.command_panel_open());
        assert!(writes.lock().unwrap().is_empty());
        draw_for_mouse(&mut app, 80, 24);
        send_mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            area.x + 3,
            area.y + row as u16,
        );
        assert!(app.pending_model_request.is_none());
        assert!(writes.lock().unwrap().is_empty());
        handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(app.model_panel.is_none());
        assert!(writes.lock().unwrap().is_empty());
        handle_key(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut app,
        );
        assert_eq!(app.input, "x");
        handle_key(
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
            &mut app,
        );
        assert!(
            app.input.is_empty(),
            "undo must not restore a consumed command"
        );
    }
}

#[test]
fn model_and_effort_columns_stay_aligned_as_the_selection_moves() {
    let mut config = model_picker_config();
    config["model"] = json!("@a");
    for (index, (id, name)) in [
        ("@a", "中文 é 👩‍💻"),
        ("@b", "DeepSeek V4 Pro"),
        (
            "@c",
            "Fixed model with a very long display name that needs shortening",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        config["modelProviders"][0]["models"][index]["model"] = json!(id);
        config["modelProviders"][0]["models"][index]["displayName"] = json!(name);
    }
    for width in [44, 64, 100] {
        let path = PathBuf::from("D:/workspace");
        let mut app = test_app("draft", path.clone(), path);
        apply_model_display(&mut app, &config);
        open_model_panel(&mut app, &config).unwrap();
        let view = build_transcript_view(&app, width);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
        for effort in [false, true] {
            if effort {
                model_picker::open_effort(&mut app, &config).unwrap();
            }
            let names = if effort {
                ["l", "m", "h"]
            } else {
                ["中", "D", "F"]
            };
            let descriptions = if effort {
                ["F", "B", "M"]
            } else {
                ["@", "@", "@"]
            };
            let mut previous_columns = None;
            for _ in 0..3 {
                terminal
                    .draw(|frame| render(frame, &mut app, &view))
                    .unwrap();
                let area = app.model_list_area.unwrap();
                let buffer = terminal.backend().buffer();
                let find_column = |row: u16, symbol: &str| {
                    (area.x..area.right())
                        .find(|&x| buffer.cell((x, area.y + row)).unwrap().symbol() == symbol)
                        .unwrap_or_else(|| panic!("missing {symbol:?} at width {width}, row {row}"))
                };
                let name_column = find_column(0, names[0]);
                let description_column = find_column(0, descriptions[0]);
                for row in 0..3 {
                    assert_eq!(find_column(row, names[row as usize]), name_column);
                    assert_eq!(
                        find_column(row, descriptions[row as usize]),
                        description_column
                    );
                    let text = (area.x..area.right())
                        .map(|x| buffer.cell((x, area.y + row)).unwrap().symbol())
                        .collect::<String>();
                    let current = row == if effort { 2 } else { 0 };
                    assert_eq!(text.matches("(current)").count(), usize::from(current));
                    assert_eq!(
                        text.starts_with("❯ "),
                        model_picker::rows(&app)[row as usize].index
                            == model_picker::selection(&app)
                    );
                }
                let columns = (name_column, description_column);
                if let Some(previous) = previous_columns {
                    assert_eq!(columns, previous, "selection must not shift either column");
                }
                previous_columns = Some(columns);
                handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut app);
            }
        }
        assert_eq!(app.input, "draft");
    }
}

#[test]
fn command_list_enter_opens_the_highlighted_model_or_effort_picker() {
    for (input, command, stage) in [
        ("/", "/model", model_picker::Stage::Models),
        ("/mo", "/model", model_picker::Stage::Models),
        ("", "/model", model_picker::Stage::Models),
        ("/", "/effort", model_picker::Stage::CurrentEffort(2)),
        ("/ef", "/effort", model_picker::Stage::CurrentEffort(2)),
        ("", "/effort", model_picker::Stage::CurrentEffort(2)),
    ] {
        let path = PathBuf::from("D:/workspace");
        let mut app = test_app(input, path.clone(), path);
        app.show_help = input.is_empty();
        let writes = attach_model_runtime(&mut app, model_picker_config());
        let row = matching_commands(input)
            .iter()
            .position(|item| item.name == command)
            .unwrap();
        for _ in 0..row {
            handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut app);
        }
        assert_eq!(app.selected_command, row);
        handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        finish_model_requests(&mut app);
        assert_eq!(app.model_panel.as_ref().unwrap().stage, stage);
        assert!(app.input.is_empty());
        assert!(!app.command_panel_open());
        assert!(writes.lock().unwrap().is_empty());
        handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(app.model_panel.is_none());
        assert!(writes.lock().unwrap().is_empty());
    }
}

#[test]
fn command_list_enter_confirms_commands_and_preserves_modified_newlines() {
    for input in ["/", "/st", ""] {
        let path = PathBuf::from("D:/workspace");
        let mut app = test_app(input, path.clone(), path);
        app.show_help = input.is_empty();
        app.selected_command = matching_commands(input)
            .iter()
            .position(|item| item.name == "/stop")
            .unwrap();
        handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert!(app.input.is_empty());
        assert!(!app.command_panel_open());
        assert!(app.runtime.is_none());
    }
    for modifiers in [KeyModifiers::SHIFT, KeyModifiers::ALT] {
        let path = PathBuf::from("D:/workspace");
        let mut app = test_app("/", path.clone(), path);
        app.selected_command = matching_commands("/")
            .iter()
            .position(|item| item.name == "/model")
            .unwrap();
        handle_key(KeyEvent::new(KeyCode::Enter, modifiers), &mut app);
        assert_eq!(app.input, "/\n");
        assert!(app.pending_model_request.is_none());
        assert!(app.model_panel.is_none());
        assert!(app.runtime.is_none());
    }
}

#[test]
fn model_commands_complete_only_the_name_then_open_the_picker() {
    for (prefix, command, stage) in [
        ("/mo", "/model ", model_picker::Stage::Models),
        ("/ef", "/effort ", model_picker::Stage::CurrentEffort(2)),
    ] {
        let path = PathBuf::from("D:/workspace");
        let mut app = test_app(prefix, path.clone(), path);
        let config = model_picker_config();
        apply_model_display(&mut app, &config);
        open_model_panel(&mut app, &config).unwrap();
        close_model_panel(&mut app);
        let writes = attach_model_runtime(&mut app, config);
        assert!(app.command_panel_open());
        handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut app);
        assert_eq!(app.input, command);
        assert!(!app.command_panel_open());
        handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut app);
        assert_eq!(
            app.input, command,
            "Tab must not expand model or effort arguments"
        );
        assert!(writes.lock().unwrap().is_empty());
        handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        finish_model_requests(&mut app);
        assert_eq!(app.model_panel.as_ref().unwrap().stage, stage);
        assert!(
            app.input.is_empty(),
            "picker navigation must not construct a command line"
        );
        assert!(writes.lock().unwrap().is_empty());
    }
}

#[test]
fn model_command_keeps_manually_entered_model_and_effort_arguments() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/model test/model-b low", path.clone(), path);
    let writes = attach_model_runtime(&mut app, model_picker_config());
    assert!(!app.command_panel_open());
    handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut app);
    assert_eq!(app.input, "/model test/model-b low");
    handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
    finish_model_requests(&mut app);
    assert_eq!(
        *writes.lock().unwrap(),
        vec![json!({"modelProviderId":"test","model":"model-b","modelThinkingMode":"low"})]
    );
}

#[test]
fn effort_command_keeps_both_picker_and_direct_level_forms() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("", path.clone(), path);
    let writes = attach_model_runtime(&mut app, model_picker_config());
    handle_effort_command(&mut app, "").unwrap();
    finish_model_requests(&mut app);
    assert_eq!(
        app.model_panel.as_ref().unwrap().stage,
        model_picker::Stage::CurrentEffort(2)
    );
    handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &mut app);
    handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
    finish_model_requests(&mut app);
    handle_effort_command(&mut app, "low").unwrap();
    finish_model_requests(&mut app);
    let writes = writes.lock().unwrap();
    assert_eq!(writes.len(), 2);
    assert_eq!(
        writes[0],
        json!({"modelProviderId":"test","model":"model-a","modelThinkingMode":"medium"})
    );
    assert_eq!(
        writes[1],
        json!({"modelProviderId":"test","model":"model-a","modelThinkingMode":"low"})
    );
}

#[test]
fn model_without_selectable_effort_skips_the_step_and_never_invents_a_level() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("/effort ", path.clone(), path);
    let mut config = model_picker_config();
    config["model"] = json!("fixed");
    config["modelThinkingMode"] = Value::Null;
    let writes = attach_model_runtime(&mut app, config.clone());
    apply_model_display(&mut app, &config);
    app.model_catalog = model_panel_from_config(&config).unwrap().providers;
    handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut app);
    finish_model_requests(&mut app);
    assert!(app.model_panel.is_none());
    assert!(writes.lock().unwrap().is_empty());
    assert!(app
        .message
        .as_deref()
        .unwrap()
        .contains("no selectable reasoning levels"));
    open_model_panel(&mut app, &config).unwrap();
    activate_selected_model(&mut app).unwrap();
    finish_model_requests(&mut app);
    assert_eq!(
        *writes.lock().unwrap(),
        vec![json!({"modelProviderId":"test","model":"fixed"})]
    );
}

#[test]
fn model_credential_cancel_restores_draft_images_and_undo_without_the_secret() {
    let root = unique_test_dir("model-credential-draft");
    let source = root.join("source.png");
    std::fs::write(&source, test_png_bytes()).unwrap();
    let mut app = test_app("draft ", root.clone(), root.clone());
    handle_paste(&mut app, source.to_string_lossy().into_owned());
    let text = app.input.clone();
    let owned = app.draft_image_attachments[0].local_path.clone();
    open_model_panel(&mut app, &model_picker_config()).unwrap();
    select_model_provider(&mut app, 1);
    activate_selected_model(&mut app).unwrap();
    assert!(owned.exists());
    handle_paste(&mut app, "secret-key".into());
    handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert_eq!(app.input, text);
    assert_eq!(app.draft_image_attachments[0].local_path, owned);
    assert!(owned.exists());
    close_model_panel(&mut app);
    handle_key(
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        &mut app,
    );
    assert_eq!(app.input, "draft ");
    clear_composer(&mut app);
    assert!(!owned.exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn model_apply_error_keeps_the_selected_effort_and_does_not_claim_success() {
    let path = PathBuf::from("D:/workspace");
    let mut app = test_app("draft", path.clone(), path);
    let config = model_picker_config();
    apply_model_display(&mut app, &config);
    open_model_panel(&mut app, &config).unwrap();
    activate_selected_model(&mut app).unwrap();
    let (tx, rx) = mpsc::channel();
    app.pending_model_request = Some(PendingModelRequest::Setting {
        response: RuntimeResponse::from_response_receiver(rx),
        mutation: ModelMutation::Selection,
    });
    drop(tx);
    assert!(drain_model_request(&mut app));
    assert_eq!(
        app.model_panel.as_ref().unwrap().stage,
        model_picker::Stage::ModelEffort(2)
    );
    assert_eq!(app.model_effort.as_deref(), Some("high"));
    assert_eq!(app.input, "draft");
    assert!(app.message.as_deref().unwrap().contains("channel closed"));
}
