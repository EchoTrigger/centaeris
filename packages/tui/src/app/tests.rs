use super::*;

#[test]
fn tui_uses_current_directory_as_default_workspace() {
    let cwd = unique_test_dir("default-cwd-workspace");

    let config = parse_args_with_default(Vec::<String>::new(), cwd.clone())
        .expect("default workspace from cwd");

    let canonical = cwd.canonicalize().expect("canonical cwd");
    assert_eq!(config.session_cwd, display_path(canonical.as_path()));
    assert_eq!(config.workspace_root, canonical);
    std::fs::remove_dir_all(&cwd).expect("cleanup cwd");
}

#[test]
fn bare_home_workspace_sets_first_prompt_warning_only_for_implicit_launch() {
    let home = unique_test_dir("implicit-home-workspace");
    let implicit =
        parse_args_with_default_and_home(Vec::<String>::new(), home.clone(), Some(home.clone()))
            .expect("implicit home workspace");
    assert!(implicit.warn_home_on_first_prompt);

    let explicit = parse_args_with_default_and_home(
        vec![
            "--workspace".to_string(),
            home.to_string_lossy().to_string(),
        ],
        home.clone(),
        Some(home.clone()),
    )
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
fn help_and_version_are_local_cli_fast_paths() {
    assert_eq!(
        local_cli_output(&["--help".to_string()]).as_deref(),
        Some(CLI_USAGE)
    );
    assert_eq!(
        local_cli_output(&["-V".to_string()]).as_deref(),
        Some(concat!("centa ", env!("CARGO_PKG_VERSION")))
    );
    assert!(local_cli_output(&["--workspace".to_string()]).is_none());
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
    let error = parse_args_with_default(
        vec![
            "--workspace".to_string(),
            nonexistent.to_string_lossy().to_string(),
        ],
        cwd.clone(),
    )
    .expect_err("TUI must not create a workspace");
    assert!(error.contains("workspace path is not readable"));
    assert!(!nonexistent.exists());

    let workspace = unique_test_dir("explicit-workspace");
    let config = parse_args_with_default(
        vec![
            "--workspace".to_string(),
            workspace.to_string_lossy().to_string(),
        ],
        cwd.clone(),
    )
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

    assert_eq!(app.input, "[Image #1]");
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
    assert_eq!(app.input, "[Image #1]");
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
    app.session_picker_open = true;
    handle_paste(&mut app, "ignored".to_string());
    assert!(app.input.is_empty());
    app.session_picker_open = false;
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

#[test]
fn relative_time_formats_common_units() {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_millis() as i64;
    assert_eq!(format_relative_time(now_ms), "just now");
    assert_eq!(format_relative_time(now_ms - 5 * 60_000), "5m ago");
    assert_eq!(format_relative_time(now_ms - 3 * 3_600_000), "3h ago");
    assert_eq!(format_relative_time(now_ms - 4 * 86_400_000), "4d ago");
    assert_eq!(format_relative_time(now_ms - 60 * 86_400_000), "2mo ago");
    assert_eq!(format_relative_time(now_ms - 700 * 86_400_000), "1y ago");
}

#[test]
fn delete_pending_confirms_with_y_and_cancels_with_n() {
    let workspace = unique_test_dir("workspace-delete");
    let data_root = unique_test_dir("data-delete");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.sessions = vec![TuiSession {
        id: "chat-1".to_string(),
        title: "Session".to_string(),
        updated_at: 1,
        last_message: None,
        cwd: display_path(workspace.as_path()),
        session_kind: TuiSessionKind::Main,
        activity_state: TuiSessionActivityState::Inactive,
        is_unread: false,
        is_pinned: false,
    }];
    app.session_picker_open = true;

    begin_delete_selected_session(&mut app);
    assert_eq!(app.pending_delete.as_deref(), Some("chat-1"));

    assert!(!handle_delete_key(
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        &mut app,
    ));
    assert_eq!(app.pending_delete, None);

    begin_delete_selected_session(&mut app);
    assert!(!handle_delete_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut app,
    ));
    assert_eq!(app.pending_delete, None);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn deleted_active_session_clears_running_projection() {
    let workspace = unique_test_dir("workspace-delete-active");
    let data_root = unique_test_dir("data-delete-active");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.active_session = Some(TuiSession {
        id: "chat-1".to_string(),
        title: "Running Session".to_string(),
        updated_at: 1,
        last_message: None,
        cwd: display_path(workspace.as_path()),
        session_kind: TuiSessionKind::Main,
        activity_state: TuiSessionActivityState::Idle,
        is_unread: false,
        is_pinned: false,
    });
    app.active_agent_run_id = Some("agent-run-1".to_string());
    app.active_agent_run_ids.insert("agent-run-1".to_string());
    app.process_state = RuntimeDisplayState::Working;

    clear_deleted_active_session(&mut app, "chat-1");

    assert!(app.active_session.is_none());
    assert!(app.active_agent_run_id.is_none());
    assert!(app.active_agent_run_ids.is_empty());
    assert_eq!(app.process_state, RuntimeDisplayState::Idle);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn rename_begins_with_current_title_and_esc_cancels() {
    let workspace = unique_test_dir("workspace-rename");
    let data_root = unique_test_dir("data-rename");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.sessions = vec![TuiSession {
        id: "chat-1".to_string(),
        title: "Old Title".to_string(),
        updated_at: 1,
        last_message: None,
        cwd: display_path(workspace.as_path()),
        session_kind: TuiSessionKind::Main,
        activity_state: TuiSessionActivityState::Inactive,
        is_unread: false,
        is_pinned: false,
    }];
    app.session_picker_open = true;

    begin_rename_selected_session(&mut app);
    assert_eq!(app.rename_session_id.as_deref(), Some("chat-1"));
    assert_eq!(app.input, "Old Title");

    assert!(!handle_rename_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut app,
    ));
    assert_eq!(app.rename_session_id, None);
    assert!(app.input.is_empty());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
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
    assert_eq!(rendered, "  ↳ Researcher: done");
    assert_eq!(lines[0].spans[0].style.fg, Some(theme().muted));
}

#[test]
fn tui_context_usage_parses_shared_runtime_usage() {
    let app = test_app(
        "",
        unique_test_dir("context-usage-workspace"),
        std::env::temp_dir(),
    );
    let mut app = app;

    let response = json!({
        "sessionId": "chat-1",
        "usedTokens": 50000,
        "maxContextTokens": 200000,
        "usedPercentage": 25,
        "updatedAt": 123
    });
    apply_context_usage(&mut app, &response);
    assert_eq!(
        app.context_usage,
        Some(ContextUsage {
            used_tokens: Some(50000),
            max_context_tokens: Some(200000),
            used_percentage: Some(25),
        })
    );

    let response = json!({ "sessionId": "chat-1" });
    apply_context_usage(&mut app, &response);
    assert_eq!(app.context_usage, None);
}

#[test]
fn tui_status_line_only_shows_effort_and_model() {
    let workspace = unique_test_dir("status-cfg-workspace");
    let data_root = unique_test_dir("status-cfg-data");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    app.model_display = Some("openai/gpt-5.5".to_string());
    app.model_effort = Some("xhigh".to_string());
    assert!(!status_line(&app, 200).to_string().contains("context"));

    app.context_usage = Some(ContextUsage {
        used_tokens: None,
        max_context_tokens: None,
        used_percentage: Some(63),
    });
    let status = status_line(&app, 200).to_string();
    assert_eq!(status, "xhigh · openai/gpt-5.5");
    assert!(!status.contains(app.workspace_root.as_str()));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn tui_model_query_resolves_exact_then_fuzzy_with_ambiguity_loud_fail() {
    let response = json!({
        "selectableModels": [
            {"providerId": "openai", "model": "gpt-5.5", "displayName": "GPT-5.5"},
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
                    {"providerId": "openai", "model": "gpt-5.5", "displayName": "GPT-5.5"}
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
                    {"model": "gpt-5.5", "displayName": "GPT-5.5"},
                    {"model": "gpt-4.1", "displayName": "GPT-4.1"}
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
    assert!(rendered.contains("● GPT-5.5"));
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
    assert_eq!(slash_command_name("/resume session-1"), Some("/resume"));
    assert_eq!(slash_command_name("/"), Some("/"));
    assert_eq!(slash_command_name(" /help"), None);
    assert_eq!(slash_command_name("hello /help"), None);
}

#[test]
fn command_matching_filters_by_prefix() {
    let matches = matching_commands("/res");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "/resume");
}

#[test]
fn command_table_has_no_aliases() {
    let names = SLASH_COMMANDS
        .iter()
        .map(|command| command.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "/new", "/resume", "/model", "/effort", "/state", "/stop", "/plugins", "/mcp",
            "/clear", "/help", "/exit"
        ]
    );
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
fn native_mcp_catalog_is_strict_and_masks_credential_input() {
    let catalog = json!({
        "schema": "native.mcp.catalog.v1",
        "servers": [{
            "pluginName": "demo-plugin",
            "pluginDisplayName": "Demo",
            "serverId": "demo-server",
            "pluginEnabled": true,
            "status": "needsConfiguration",
            "configurable": true,
            "configured": false,
            "transport": "streamableHttp",
            "endpoint": "https://example.com/mcp",
            "toolNames": ["demo_search"]
        }]
    });
    let catalog = parse_mcp_catalog(catalog).expect("Native MCP catalog");
    assert_eq!(catalog.servers.len(), 1);

    let workspace = unique_test_dir("mcp-mask-workspace");
    let data_root = unique_test_dir("mcp-mask-data");
    let mut app = test_app("secret", workspace.clone(), data_root.clone());
    app.mcp_panel = Some(TuiMcpPanel {
        servers: catalog.servers,
        selected: 0,
        configuring: Some(0),
        notice: None,
    });
    assert_eq!(input_for_display(&app, app.input.as_str()), "••••••");

    let invalid = json!({
        "schema": "native.mcp.catalog.v1",
        "servers": [],
        "banana": true
    });
    assert!(parse_mcp_catalog(invalid).is_err());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn command_completion_ghost_suffix_has_no_gap() {
    assert_eq!(
        command_completion_suffix("/r", 0).as_deref(),
        Some("esume ")
    );
    assert_eq!(command_completion_suffix("/resume ", 0), None);
}

#[test]
fn home_risk_uses_shared_recent_workspace_catalog() {
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

    let workspaces = recent_workspace_choices(response, "D:/home").expect("workspace catalog");
    assert_eq!(
        workspaces
            .iter()
            .map(|workspace| workspace.root.as_str())
            .collect::<Vec<_>>(),
        vec!["D:/newer", "D:/older"]
    );
    assert!(recent_workspace_choices(
        json!({
            "activeWorkspaceRoot": null,
            "workspaces": [],
            "cancelled": false,
            "banana": true
        }),
        "D:/home"
    )
    .is_err());
}

#[test]
fn home_risk_panel_appears_above_and_preserves_the_draft() {
    let workspace = PathBuf::from("D:/home");
    let mut app = test_app("inspect this project", workspace.clone(), workspace);
    app.home_risk_pending = true;
    app.home_risk_panel = Some(HomeRiskPanel {
        workspaces: vec![TuiWorkspaceChoice {
            root: "D:/Projects/example-workspace".to_string(),
            name: "Example Workspace".to_string(),
            active_session_id: None,
            sort_order: 0,
            updated_at: 1,
        }],
        selected: 0,
        notice: None,
    });
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

    handle_home_risk_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert!(app.home_risk_panel.is_none());
    assert_eq!(app.input, "inspect this project");
    assert!(app.home_risk_pending);
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
    let mut app = test_app("/r", workspace.clone(), data_root.clone());
    complete_selected_command(&mut app);
    assert_eq!(app.input, "/resume ");
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn new_command_returns_to_welcome_without_creating_session() {
    let workspace = unique_test_dir("workspace-new");
    let data_root = unique_test_dir("data-new");
    let mut app = test_app("/new", workspace.clone(), data_root.clone());

    assert!(!handle_enter(&mut app));

    assert!(app.active_session.is_none());
    assert!(app.transcript.is_empty());
    assert!(app.runtime.is_none());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn state_command_opens_an_inline_diagnostic_page() {
    let workspace = unique_test_dir("workspace-state");
    let data_root = unique_test_dir("data-state");
    let mut app = test_app("/state", workspace.clone(), data_root.clone());
    app.model_provider_id = Some("openai".to_string());
    app.model_display = Some("gpt-5.5".to_string());
    app.model_effort = Some("high".to_string());

    assert!(!handle_enter(&mut app));

    assert!(app.show_state);
    let state = rendered_lines_text(&state_lines(&app, 120));
    assert!(state.contains("MODEL       gpt-5.5"), "state: {state}");
    assert!(state.contains("PROVIDER    openai"), "state: {state}");
    assert!(
        state.contains(app.workspace_root.as_str()),
        "state: {state}"
    );
    handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
    assert!(!app.show_state);
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
        .position(|row| row.contains("Start a new session"))
        .expect("slash command panel row");
    let composer_row = rows
        .iter()
        .rposition(|row| row.contains('│') && row.contains("/new"))
        .expect("composer row");

    assert!(panel_row < composer_row);
}

#[test]
fn resume_session_picker_owns_fullscreen_and_switches_workspaces() {
    let workspace = PathBuf::from("D:/workspace");
    let other_workspace = "D:/other";
    let mut app = test_app("/resume", workspace.clone(), workspace);
    app.session_catalog = vec![
        TuiSession {
            id: "session-1234567890abcdef".to_string(),
            title: "Resume target".to_string(),
            updated_at: 1,
            last_message: Some("previous message".to_string()),
            cwd: "D:/workspace".to_string(),
            session_kind: TuiSessionKind::Main,
            activity_state: TuiSessionActivityState::Idle,
            is_unread: false,
            is_pinned: false,
        },
        TuiSession {
            id: "session-fedcba0987654321".to_string(),
            title: "Older target".to_string(),
            updated_at: 0,
            last_message: None,
            cwd: "D:/workspace".to_string(),
            session_kind: TuiSessionKind::Main,
            activity_state: TuiSessionActivityState::Inactive,
            is_unread: false,
            is_pinned: false,
        },
        TuiSession {
            id: "session-0000000000000001".to_string(),
            title: "Other workspace".to_string(),
            updated_at: 2,
            last_message: None,
            cwd: other_workspace.to_string(),
            session_kind: TuiSessionKind::Main,
            activity_state: TuiSessionActivityState::Inactive,
            is_unread: false,
            is_pinned: false,
        },
    ];
    app.session_workspaces = session_workspace_choices(
        json!({
            "activeWorkspaceRoot": "D:/workspace",
            "workspaces": [
                {
                    "root": "D:/workspace",
                    "name": "workspace",
                    "activeSessionId": null,
                    "sortOrder": 0,
                    "updatedAt": 2
                },
                {
                    "root": "D:/other",
                    "name": "other",
                    "activeSessionId": null,
                    "sortOrder": 1,
                    "updatedAt": 1
                }
            ],
            "cancelled": false
        }),
        "D:/workspace",
        app.session_catalog.as_slice(),
    )
    .expect("session workspaces");
    app.session_picker_open = true;
    refresh_visible_sessions(&mut app);
    let view = build_transcript_view(&app, 80);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render session picker");

    let buffer = terminal.backend().buffer();
    let rows = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let picker_row = rows
        .iter()
        .position(|row| row.contains("Resume target"))
        .expect("session picker row");
    assert!(picker_row > 0);
    assert!(rows.iter().any(|row| row.contains("Idle")));
    assert!(rows.iter().any(|row| row.contains("Inactive")));
    assert!(!rows
        .iter()
        .any(|row| row.contains('│') && row.contains("/resume")));

    let other_tab = app.session_workspace_hit_regions[1];
    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: other_tab.x,
            row: other_tab.y,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    assert_eq!(app.selected_session_workspace, 1);
    assert_eq!(app.sessions[0].title, "Other workspace");
    handle_key(
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        &mut app,
    );
    assert_eq!(app.selected_session_workspace, 0);
    handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut app);
    assert_eq!(app.selected_session_workspace, 1);
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

    assert_eq!(rendered_line_text(&lines[0]), "│ hello");
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
    assert!(view.tool_group_rows.is_empty());
}

#[test]
fn resume_query_matches_id_or_title_prefix() {
    let session = TuiSession {
        id: "chat-12345-1".to_string(),
        title: "Build TUI".to_string(),
        updated_at: 1,
        last_message: None,
        cwd: "D:/workspace".to_string(),
        session_kind: TuiSessionKind::Main,
        activity_state: TuiSessionActivityState::Inactive,
        is_unread: false,
        is_pinned: false,
    };

    assert!(session_matches_query(&session, "chat-123"));
    assert!(session_matches_query(&session, "build"));
    assert!(!session_matches_query(&session, "missing"));
}

#[test]
fn session_response_maps_runtime_fields() {
    let response = json!({
        "id": "chat-1",
        "title": "Shared",
        "updatedAt": 1,
        "lastMessage": null,
        "cwd": "D:/repo",
        "sessionKind": "main",
        "activityState": "inactive",
        "isUnread": false,
        "isPinned": false
    });
    let session = tui_session_from_response(&response).expect("session response");
    assert_eq!(session.id, "chat-1");
    assert_eq!(session.activity_state, TuiSessionActivityState::Inactive);

    let mut invalid = response;
    invalid["activityState"] = json!("banana");
    assert!(tui_session_from_response(&invalid)
        .expect_err("unknown activityState must loud-fail")
        .contains("unsupported session activityState"));
}

#[test]
fn session_restore_rebuilds_tool_pairs_and_separates_assistant_lines() {
    let load_response = json!({
        "id": "chat-1",
        "messages": [
            {"role": "user", "content": "build", "turnId": "turn-1", "agentRunId": "agent-run-1"},
            {"role": "assistant", "content": "done", "turnId": "turn-1", "agentRunId": "agent-run-1"}
        ]
    });
    let projection_response = json!({
        "activeAgentRunId": null,
        "session": {"id": "chat-1"},
        "agentRunReplays": [{
            "sessionId": "chat-1",
            "turnId": "turn-1",
            "agentRunId": "agent-run-1",
            "status": "succeeded",
            "items": [
                {
                    "type": "session_event",
                    "event": {
                        "type": "ToolCall",
                        "status": "running",
                        "toolName": "bash",
                        "visibility": "user",
                        "payload": {"callId": "call-1"}
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "ToolResult",
                        "status": "done",
                        "toolName": "bash",
                        "visibility": "user",
                        "payload": {
                            "callId": "call-1",
                            "resultState": "successWithOutput",
                            "operations": [{
                                "callId": "call-1",
                                "toolName": "bash",
                                "kind": "command",
                                "outputPreview": "raw output"
                            }],
                            "modelInputImages": []
                        }
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "Status",
                        "visibility": "user",
                        "payload": {"stage": "model_process_summary", "message": "searched files"}
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "ToolCall",
                        "status": "running",
                        "toolName": "read",
                        "visibility": "user",
                        "payload": {"callId": "call-2"}
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "ToolResult",
                        "status": "done",
                        "toolName": "read",
                        "visibility": "user",
                        "payload": {
                            "callId": "call-2",
                            "resultState": "successWithOutput",
                            "operations": [{
                                "callId": "call-2",
                                "toolName": "read",
                                "path": "src/lib.rs",
                                "startLine": 1,
                                "endLine": 5,
                                "totalLines": 5,
                                "outputPreview": "source"
                            }],
                            "modelInputImages": []
                        }
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "TurnSupplement",
                        "visibility": "user",
                        "payload": {"message": "再检查 tests"}
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "Final",
                        "visibility": "user",
                        "payload": {"content": "done"}
                    }
                }
            ]
        }]
    });

    let transcript =
        transcript_from_session_restore_response("chat-1", &load_response, &projection_response)
            .expect("restore");

    assert!(matches!(
        transcript.as_slice(),
        [
            TranscriptLine::User(user),
            TranscriptLine::Tool(first),
            TranscriptLine::Summary(summary),
            TranscriptLine::Tool(second),
            TranscriptLine::Supplement(supplement),
            TranscriptLine::Summary(final_text),
        ] if user == "build"
            && first.key == "tool_call:call-1"
            && summary == "searched files"
            && second.key == "tool_call:call-2"
            && supplement == "再检查 tests"
            && final_text == "done"
    ));
}

#[test]
fn session_restore_skips_active_project_replay_for_live_replay_path() {
    let load_response = json!({
        "id": "chat-1",
        "messages": [
            {"role": "user", "content": "build", "turnId": "turn-1", "agentRunId": "agent-run-1"},
            {"role": "assistant", "content": "", "turnId": "turn-1", "agentRunId": "agent-run-1", "status": "running"}
        ]
    });
    let projection_response = json!({
        "activeAgentRunId": "agent-run-1",
        "session": {"id": "chat-1"},
        "agentRunReplays": [{
            "sessionId": "chat-1",
            "turnId": "turn-1",
            "agentRunId": "agent-run-1",
            "status": "running",
            "items": [{
                "type": "session_event",
                "event": {
                    "type": "Status",
                    "visibility": "user",
                    "payload": {"stage": "model_process_summary", "message": "active summary"}
                }
            }]
        }]
    });

    let transcript =
        transcript_from_session_restore_response("chat-1", &load_response, &projection_response)
            .expect("restore active session bash");

    assert_eq!(transcript, vec![TranscriptLine::User("build".to_string())]);
}

#[test]
fn restore_to_transcript_rebuilds_closed_tool_pairs_and_renders_markdown_final() {
    let load_response = json!({
        "id": "chat-1",
        "messages": [
            {"role": "user", "content": "build", "turnId": "turn-1", "agentRunId": "agent-run-1"},
            {"role": "assistant", "content": "## Done\n\n- item **one**", "turnId": "turn-1", "agentRunId": "agent-run-1"}
        ]
    });
    let projection_response = json!({
        "activeAgentRunId": null,
        "session": {"id": "chat-1"},
        "agentRunReplays": [{
            "sessionId": "chat-1",
            "turnId": "turn-1",
            "agentRunId": "agent-run-1",
            "status": "succeeded",
            "items": [
                {
                    "type": "session_event",
                    "event": {
                        "type": "ToolCall",
                        "status": "running",
                        "toolName": "bash",
                        "visibility": "user",
                        "payload": {"callId": "call-1"}
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "ToolResult",
                        "status": "done",
                        "toolName": "bash",
                        "visibility": "user",
                        "payload": {
                            "callId": "call-1",
                            "resultState": "successWithOutput",
                            "operations": [{
                                "callId": "call-1",
                                "toolName": "bash",
                                "kind": "command",
                                "outputPreview": "output"
                            }],
                            "modelInputImages": []
                        }
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "Status",
                        "visibility": "user",
                        "payload": {"stage": "model_process_summary", "message": "ran build"}
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "SubagentSpawned",
                        "visibility": "user",
                        "payload": {"subagentId": "agent-1", "title": "Helper", "parentTurnId": "turn-1"}
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "SubagentResult",
                        "visibility": "user",
                        "payload": {"subagentId": "agent-1", "title": "Helper", "summary": "checked", "parentTurnId": "turn-1"}
                    }
                },
                {
                    "type": "session_event",
                    "event": {
                        "type": "Final",
                        "visibility": "user",
                        "payload": {"content": "## Done\n\n- item **one**"}
                    }
                }
            ]
        }]
    });

    let transcript =
        transcript_from_session_restore_response("chat-1", &load_response, &projection_response)
            .expect("restore");
    let mut app = test_app("", PathBuf::from("D:/workspace"), PathBuf::from("D:/data"));
    app.transcript = transcript;
    let lines = build_transcript_view(&app, 80).lines;
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
            "│ build".to_string(),
            String::new(),
            "  Ran command ›".to_string(),
            String::new(),
            "  ran build".to_string(),
            String::new(),
            "  ↳ Helper".to_string(),
            "  ↳ Helper: checked".to_string(),
            "  ## Done".to_string(),
            String::new(),
            "  • item one".to_string(),
            String::new(),
        ]
    );
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
            "request": {
                "sessionId": "chat-1",
                "questionId": "q-1",
                "answerText": "Use the release branch",
                "answers": [],
            }
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
fn session_restore_rejects_unknown_replay_event() {
    let load_response = json!({
        "id": "chat-1",
        "messages": []
    });
    let projection_response = json!({
        "activeAgentRunId": null,
        "session": {"id": "chat-1"},
        "agentRunReplays": [{
            "sessionId": "chat-1",
            "turnId": "turn-1",
            "agentRunId": "agent-run-1",
            "status": "succeeded",
            "items": [{
                "type": "session_event",
                "event": {
                    "type": "banana",
                    "visibility": "user",
                    "payload": {}
                }
            }]
        }]
    });

    let error =
        transcript_from_session_restore_response("chat-1", &load_response, &projection_response)
            .expect_err("unknown event must fail");

    assert!(error.contains("unsupported session_event type"));
}

#[test]
fn short_session_id_keeps_tail() {
    assert_eq!(short_session_id("chat-1"), "chat-1");
    assert_eq!(short_session_id("chat-1800000000000-42"), "...000000000-42");
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
                        "path": "packages/tui/src/main.rs",
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
    assert!(rendered_lines_text(&rendered).contains("└─ cargo test -p centaeris-tui"));

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
fn result_hierarchy_uses_one_connector_per_text_block() {
    let mut lines = Vec::new();
    push_text_result_block(
        &mut lines,
        &["first output".to_string(), "second output".to_string()],
    );

    assert_eq!(
        lines.iter().map(rendered_line_text).collect::<Vec<_>>(),
        vec!["  └─ first output", "      second output"]
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
        app.transcript.as_slice(),
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
        app.transcript.as_slice(),
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
    assert_eq!(running.tool_group_rows.len(), 1);

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
        title: "Session".to_string(),
        updated_at: 1,
        last_message: None,
        cwd: display_path(workspace.as_path()),
        session_kind: TuiSessionKind::Main,
        activity_state: TuiSessionActivityState::Idle,
        is_unread: false,
        is_pinned: false,
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

    app.process_state = RuntimeDisplayState::Working;
    assert_eq!(status_header(&app), "Working");
    app.process_state = RuntimeDisplayState::Thinking;
    assert_eq!(status_header(&app), "Thinking");
    app.process_state = RuntimeDisplayState::ToolRunning;
    assert_eq!(status_header(&app), "Running tools");
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
    assert_eq!(status_header(&app), "Tachikoma ×3 · whispering…");
    for (index, subagent_id) in ["three", "two"].into_iter().enumerate() {
        apply_session_event(
            &mut app,
            &json!({
                "id": format!("subagent-result-{index}"),
                "type": "SubagentResult",
                "payload": {"subagentId": subagent_id, "summary": "done"}
            }),
            Some("tachikoma-0"),
        );
    }
    assert_eq!(status_header(&app), "Tachikoma ×1 · awaiting result…");
    apply_session_event(
        &mut app,
        &json!({
            "id": "subagent-result-last",
            "type": "SubagentResult",
            "payload": {"subagentId": "one", "summary": "done"}
        }),
        Some("tachikoma-0"),
    );
    assert_eq!(status_header(&app), "Idle");

    app.active_agent_run_id = Some("runtime-0".to_string());
    update_runtime_easter_egg(&mut app, Some("runtime-0"), "thinking");
    assert_eq!(status_header(&app), "a faint signal crossed the Wired…");
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn model_request_start_and_status_update_live_projection_without_errors() {
    let workspace = unique_test_dir("workspace-model-events");
    let data_root = unique_test_dir("data-model-events");
    let mut app = test_app("", workspace.clone(), data_root.clone());

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
    assert_eq!(app.process_state, RuntimeDisplayState::ProviderWaiting);
    assert_eq!(app.assistant_buffer, "partial answer");
    assert!(app.transcript.is_empty());

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
fn selected_composer_text_is_replaced_without_clipboard_copy() {
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
fn composer_mouse_drag_keeps_selection_local_to_the_editor() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("abcdef", workspace.clone(), workspace);
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
}

#[test]
fn clicking_image_placeholder_opens_colored_preview_and_mask_closes_it() {
    let workspace = unique_test_dir("workspace-image-preview");
    let data_root = unique_test_dir("data-image-preview");
    let path = data_root.join("clipboard.png");
    std::fs::write(path.as_path(), test_png_bytes()).expect("write image fixture");
    let mut app = test_app("[Image #1]", workspace.clone(), data_root.clone());
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
    for _ in 0..1_000 {
        drain_image_preview(&mut app);
        if app
            .image_preview
            .as_ref()
            .and_then(|preview| preview.protocol.as_ref())
            .is_some()
        {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        app.image_preview
            .as_ref()
            .expect("image preview")
            .protocol
            .as_ref()
            .expect("prepared image preview")
            .protocol_type(),
        ratatui_image::protocol::StatefulProtocolType::Halfblocks(_)
    ));
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
    assert!(test_buffer_has_rgb_halfblock(terminal.backend().buffer()));
    let card = app.image_preview_area.expect("preview card");
    assert!(card.bottom() <= input.y);
    assert!(card.height <= 2, "small source image must not be upscaled");

    let image_area = app
        .image_preview
        .as_ref()
        .expect("image preview")
        .image_area;
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
    let generation = app
        .image_preview
        .as_ref()
        .expect("image preview")
        .generation;
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
        app.image_preview
            .as_ref()
            .expect("image preview")
            .generation,
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
        app.image_preview
            .as_ref()
            .expect("image preview")
            .generation,
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
    assert!(test_buffer_has_rgb_halfblock(terminal.backend().buffer()));
    assert!(
        app.transcript_rows
            .iter()
            .all(|row| !row.text.contains('▀')),
        "image cells must not leak into copied transcript text"
    );

    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
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
fn rendered_transcript_selection_preserves_wide_characters() {
    let area = Rect::new(0, 0, 8, 2);
    let mut buffer = ratatui::buffer::Buffer::empty(area);
    buffer.set_string(0, 0, "你a", Style::default());
    buffer.set_string(0, 1, "tool", Style::default());
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("", workspace.clone(), workspace);
    app.transcript_rows = rendered_text_rows(&buffer, area);
    app.transcript_selection = Some(TextSelection {
        anchor: TextPoint { row: 0, column: 0 },
        head: TextPoint { row: 1, column: 4 },
    });

    assert_eq!(selected_transcript_text(&app).as_deref(), Some("你a\ntool"));
}

#[test]
fn input_height_uses_the_same_display_width_as_the_cursor() {
    let workspace = PathBuf::from("D:/workspace");
    let mut app = test_app("abc", workspace.clone(), workspace);
    assert_eq!(input_row_count(&app, 5), 1, "an exact-width row fits");

    app.input = "你好".to_string();
    assert_eq!(input_row_count(&app, 5), 2, "wide text wraps by columns");
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
    let mut app = test_app("/resume", workspace.clone(), data_root.clone());
    app.show_help = true;

    assert!(!handle_ctrl_c(&mut app));
    assert!(!app.show_help);
    assert_eq!(app.input, "/resume");

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
fn plugins_list_output_includes_state_and_errors() {
    let output = format_plugin_list(&json!([
        {
            "id": "demo",
            "name": "Demo",
            "enabled": true,
            "errors": ["bad manifest"]
        }
    ]))
    .expect("plugin list output");

    assert!(output.contains("demo [enabled] Demo (1 error)"));
}

#[test]
fn plugin_snapshot_output_requires_snapshot_fields() {
    let output = format_plugin_snapshot(&json!({
        "enabledPlugins": ["core.document"],
        "disabledPlugins": ["disabled.plugin"]
    }))
    .expect("snapshot output");

    assert!(output.contains("enabled=1"));
    assert!(output.contains("disabled=1"));

    let error =
        format_plugin_snapshot(&json!({})).expect_err("missing snapshot fields must fail loudly");
    assert!(error.contains("plugin snapshot missing enabledPlugins"));
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
fn runtime_disconnect_clears_activity_and_drops_client_for_reconnect() {
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
    assert!(app.active_agent_run_id.is_none());
    assert!(app.active_agent_run_ids.is_empty());
    assert!(app.agent_run_started_at.is_none());
    assert_eq!(app.process_state, RuntimeDisplayState::Idle);
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
            "│ build".to_string(),
            String::new(),
            "  searched files".to_string(),
            String::new(),
            "     └─ 再检查 tests".to_string(),
            "! boom".to_string(),
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
    assert_eq!(rendered[4], ("```rust".to_string(), false));
    assert_eq!(rendered[5], ("fn main() {}".to_string(), false));
    assert_eq!(rendered[6], ("".to_string(), false));
    assert_eq!(
        rendered[7],
        ("before bold and code after".to_string(), true)
    );
    assert!(lines[7].spans.iter().any(|span| span.style.bg.is_some()));
}

#[test]
fn markdown_keeps_unclosed_fence_state_for_streaming() {
    let (lines, still_code) = render_markdown_lines("```rust\nfn main", 80, false);
    assert!(still_code);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].spans[0].content.as_ref(), "```rust");
    assert_eq!(lines[1].spans[0].content.as_ref(), "fn main");
    let (more, done) = render_markdown_lines("() {}\n```", 80, true);
    assert!(!done);
    assert_eq!(more.len(), 1);
    assert_eq!(more[0].spans[0].content.as_ref(), "() {}");
}

#[test]
fn tool_groups_expand_without_rewriting_transcript_data() {
    let workspace = unique_test_dir("workspace-tool-accordion");
    let data_root = unique_test_dir("data-tool-accordion");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-1", "bash");
    apply_tool_result(
        &mut app,
        "call-1",
        "successWithOutput",
        "done",
        json!([{
            "callId": "call-1",
            "kind": "command",
            "outputPreview": "accordion-output-one\naccordion-output-two"
        }]),
    );
    let key = match &app.transcript[0] {
        TranscriptLine::Tool(tool) => tool.key.clone(),
        other => panic!("unexpected line: {other:?}"),
    };
    let collapsed = build_transcript_view(&app, 80);
    assert!(!rendered_lines_text(&collapsed.lines).contains("accordion-output-one"));

    toggle_tool_group(&mut app, key);
    let expanded = build_transcript_view(&app, 80);
    assert!(rendered_lines_text(&expanded.lines).contains("accordion-output-one"));
    assert!(rendered_lines_text(&expanded.lines).contains("accordion-output-two"));
    assert_eq!(app.transcript.len(), 1);
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
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
    assert_eq!(view.tool_group_rows.len(), 2);
    assert_eq!(view.tool_group_rows[0].0, "tool_call:first");
    assert_eq!(view.tool_group_rows[1].0, "tool_call:third");
    let rendered = rendered_lines_text(&view.lines);
    assert!(rendered.contains("Next step"));
    assert!(rendered.contains("research: done"));
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(data_root);
}

#[test]
fn rendered_tool_group_header_is_mouse_expandable() {
    let workspace = unique_test_dir("workspace-tool-mouse");
    let data_root = unique_test_dir("data-tool-mouse");
    let mut app = test_app("", workspace.clone(), data_root.clone());
    apply_tool_call(&mut app, "call-mouse", "bash");
    let view = build_transcript_view(&app, 80);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| render(frame, &mut app, &view))
        .expect("render transcript");
    let region = app
        .tool_group_hit_regions
        .first()
        .cloned()
        .expect("visible tool group");

    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: region.row,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 4,
            row: region.row,
            modifiers: KeyModifiers::NONE,
        },
        &mut app,
    );
    assert!(app.expanded_tool_groups.contains(&region.key));
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
            line,
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
            title: String::new(),
            updated_at: 0,
            last_message: None,
            cwd: display_path(workspace.as_path()),
            session_kind: TuiSessionKind::Main,
            activity_state: TuiSessionActivityState::Inactive,
            is_unread: false,
            is_pinned: false,
        },
        app.input.as_str(),
        app.draft_image_attachments.as_slice(),
        "session-prompt:image-action",
    );
    assert_eq!(request["request"]["message"], "a[Image #2]b[Image #1]c");
    assert_eq!(
        request["request"]["attachments"][0]["placeholder"],
        "[Image #2]"
    );
    assert_eq!(
        request["request"]["attachments"][1]["placeholder"],
        "[Image #1]"
    );

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
    assert_eq!(
        session_request["request"]["operationId"],
        "session-new:action-1"
    );

    let prompt_request = build_prompt_request(
        &TuiSession {
            id: "session-operation".to_string(),
            title: String::new(),
            updated_at: 0,
            last_message: None,
            cwd: "D:/Workspace".to_string(),
            session_kind: TuiSessionKind::Main,
            activity_state: TuiSessionActivityState::Inactive,
            is_unread: false,
            is_pinned: false,
        },
        "Prompt",
        &[],
        "session-prompt:action-1",
    );
    assert_eq!(
        prompt_request["request"]["operationId"],
        "session-prompt:action-1"
    );
    assert_eq!(
        prompt_request.clone()["request"]["operationId"],
        prompt_request["request"]["operationId"]
    );
}

fn test_app(input: &str, workspace_root: PathBuf, _data_root: PathBuf) -> App {
    App {
        workspace_root: display_path(workspace_root.as_path()),
        session_cwd: display_path(workspace_root.as_path()),
        model_provider_id: None,
        model_display: None,
        model_effort: None,
        context_usage: None,
        runtime: None,
        pending_model_request: None,
        runtime_config_refresh_pending: false,
        model_credential_prompt: None,
        model_panel: None,
        model_provider_hit_regions: Vec::new(),
        model_list_area: None,
        model_list_offset: 0,
        mcp_panel: None,
        home_risk_pending: false,
        home_risk_panel: None,
        input: input.to_string(),
        input_cursor: input.len(),
        input_selection_anchor: None,
        input_area: None,
        panel_area: None,
        draft_image_attachments: Vec::new(),
        next_image_number: 1,
        image_picker: halfblock_image_picker(),
        image_preview: None,
        image_preview_area: None,
        inline_images: HashMap::new(),
        inline_image_errors: HashMap::new(),
        pending_esc_stop: false,
        message: None,
        show_help: false,
        show_state: false,
        selected_command: 0,
        session_catalog: Vec::new(),
        sessions: Vec::new(),
        session_workspaces: Vec::new(),
        selected_session_workspace: 0,
        session_workspace_hit_regions: Vec::new(),
        session_list_area: None,
        session_list_offset: 0,
        session_action_area: None,
        selected_session: 0,
        session_picker_open: false,
        active_session: None,
        transcript: Vec::new(),
        transcript_scroll: 0,
        transcript_max_scroll: 0,
        transcript_follow_bottom: true,
        expanded_tool_groups: HashSet::new(),
        focused_tool_group: None,
        tool_group_hit_regions: Vec::new(),
        transcript_area: None,
        transcript_rows: Vec::new(),
        transcript_selection: None,
        mouse_drag: None,
        tool_projection: ToolProjection::default(),
        pending_subagent_lines: Vec::new(),
        assistant_buffer: String::new(),
        assistant_emitted_bytes: 0,
        assistant_tail_in_code_block: false,
        assistant_stream_started: false,
        assistant_stream_start: None,
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
        runtime_easter_egg: None,
        pending_question: None,
        pending_delete: None,
        rename_session_id: None,
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
