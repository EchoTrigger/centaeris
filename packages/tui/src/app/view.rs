use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};

use super::*;

pub(super) const STATE_PAGE_HEIGHT: u16 = 9;
pub(super) const COMMAND_NAME_WIDTH: usize = 14;

pub(super) struct TranscriptView {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) tool_group_rows: Vec<(String, u16)>,
    pub(super) images: Vec<TranscriptImagePlacement>,
    pub(super) total_rows: u16,
}

pub(super) struct TranscriptImagePlacement {
    key: String,
    row: u16,
}

pub(super) fn render(frame: &mut Frame, app: &mut App, transcript_view: &TranscriptView) {
    let area = frame.area();
    app.input_area = None;
    app.panel_area = None;
    app.transcript_area = None;
    app.image_preview_area = None;
    app.transcript_rows.clear();
    app.model_provider_hit_regions.clear();
    app.model_list_area = None;
    app.model_list_offset = 0;
    app.session_workspace_hit_regions.clear();
    app.session_list_area = None;
    app.session_list_offset = 0;
    app.session_action_area = None;
    if area.width < 32 || area.height < 10 {
        app.tool_group_hit_regions.clear();
        let paragraph =
            Paragraph::new("Terminal too small.").style(Style::default().fg(theme().error));
        frame.render_widget(paragraph, area);
        render_image_preview(frame, app, area);
        return;
    }

    if app.session_picker_open {
        app.tool_group_hit_regions.clear();
        render_session_picker(frame, area, app);
        return;
    }

    if app.model_panel.is_some() {
        app.tool_group_hit_regions.clear();
        render_model_panel(frame, area, app);
        return;
    }

    if app.show_state {
        app.tool_group_hit_regions.clear();
        let input_rows = input_row_count(app, area.width);
        let panel_rows = panel_height(
            app,
            area.height
                .saturating_sub(STATE_PAGE_HEIGHT + input_rows + 1),
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(STATE_PAGE_HEIGHT),
                Constraint::Length(input_rows),
                Constraint::Length(panel_rows),
                Constraint::Length(1),
            ])
            .split(area);
        render_state_page(frame, chunks[0], app);
        render_input(frame, chunks[1], app);
        if panel_rows > 0 {
            app.panel_area = Some(chunks[2]);
            render_panel(frame, chunks[2], app);
        }
        render_status(frame, chunks[3], app);
        render_image_preview(frame, app, chunks[0]);
        return;
    }

    let has_timeline = !app.transcript.is_empty()
        || app.tool_projection.has_open_calls()
        || app.active_tool_label.is_some()
        || !app.assistant_buffer.trim().is_empty();
    let status_rows = u16::from(has_active_agent_run(app));
    let input_rows = input_row_count(app, area.width);
    let panel_rows = panel_height(
        app,
        area.height.saturating_sub(status_rows + input_rows + 1),
    );
    let panel_above_input =
        app.command_panel_open() || app.home_risk_panel.is_some() || app.mcp_panel.is_some();
    let constraints = if panel_above_input {
        [
            Constraint::Min(1),
            Constraint::Length(status_rows),
            Constraint::Length(panel_rows),
            Constraint::Length(input_rows),
            Constraint::Length(1),
        ]
    } else {
        [
            Constraint::Min(1),
            Constraint::Length(status_rows),
            Constraint::Length(input_rows),
            Constraint::Length(panel_rows),
            Constraint::Length(1),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    if has_timeline {
        render_transcript(frame, chunks[0], app, transcript_view);
    } else {
        render_welcome(frame, chunks[0]);
        app.transcript_scroll = 0;
        app.transcript_max_scroll = 0;
        app.tool_group_hit_regions.clear();
        app.transcript_selection = None;
    }
    if status_rows > 0 {
        render_status_indicator(frame, chunks[1], app);
    }
    let (panel_area, input_area) = if panel_above_input {
        (chunks[2], chunks[3])
    } else {
        (chunks[3], chunks[2])
    };
    if panel_rows > 0 {
        app.panel_area = Some(panel_area);
        render_panel(frame, panel_area, app);
    }
    render_input(frame, input_area, app);
    render_status(frame, chunks[4], app);
    render_image_preview(frame, app, chunks[0]);
}

fn render_image_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.image_preview.is_none() {
        return;
    }
    frame.render_widget(Clear, area);
    let available = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let image_bounds = Rect::new(
        available.x,
        available.y,
        available.width,
        available.height.saturating_sub(1),
    );
    let preview = app.image_preview.as_mut().expect("preview was checked");
    if preview.requested_bounds != image_bounds {
        preview.requested_bounds = image_bounds;
        request_image_preview_render(preview);
    }
    let image_size = Rect::new(
        0,
        0,
        preview.render_size.width.min(image_bounds.width),
        preview.render_size.height.min(image_bounds.height),
    );
    let content_width = image_size
        .width
        .max(72.min(available.width))
        .min(available.width);
    let content_height = image_size.height.saturating_add(1).min(available.height);
    let content = Rect::new(
        available
            .x
            .saturating_add(available.width.saturating_sub(content_width) / 2),
        available
            .y
            .saturating_add(available.height.saturating_sub(content_height) / 2),
        content_width,
        content_height,
    );
    app.image_preview_area = Some(content);
    let image_area = Rect::new(
        content
            .x
            .saturating_add(content.width.saturating_sub(image_size.width) / 2),
        content.y,
        image_size.width,
        image_size.height,
    );
    preview.image_area = image_area;
    if image_area.width > 0 && image_area.height > 0 {
        if let Some(protocol) = preview.protocol.as_mut() {
            protocol.render(image_area, frame.buffer_mut());
        }
    }
    let path_area = Rect::new(
        content.x,
        content.y.saturating_add(image_size.height),
        content.width,
        1,
    );
    let zoom = if preview.view.zoom_steps == 0 {
        "Fit".to_string()
    } else {
        format!(
            "{:.1}×",
            IMAGE_PREVIEW_ZOOM_FACTOR.powi(i32::from(preview.view.zoom_steps))
        )
    };
    let prefix = format!("{zoom} · Path: ");
    let prefix_width = prefix.chars().map(character_width).sum::<usize>();
    frame.render_widget(
        Paragraph::new(format!(
            "{prefix}{}",
            fit_middle_columns(
                preview.path.to_string_lossy().as_ref(),
                (path_area.width as usize).saturating_sub(prefix_width)
            )
        ))
        .style(Style::default().fg(theme().muted)),
        path_area,
    );
}

pub(super) fn input_row_count(app: &App, width: u16) -> u16 {
    let usable = (width.saturating_sub(2)).max(1) as usize;
    let (last_row, _) = cursor_position_in_wrapped(app.input.as_str(), usable);
    u16::try_from(last_row.saturating_add(1)).unwrap_or(u16::MAX)
}

pub(super) fn panel_height(app: &App, max_height: u16) -> u16 {
    if let Some(panel) = app.home_risk_panel.as_ref() {
        return (panel.workspaces.len() as u16 + 4).min(max_height).max(1);
    }
    if app.pending_question.is_some() {
        let rows = app
            .pending_question
            .as_ref()
            .map(|question| question.options.len() as u16)
            .unwrap_or(0);
        return (rows + 4).min(max_height);
    }
    if let Some(panel) = app.mcp_panel.as_ref() {
        let notice = u16::from(panel.notice.is_some());
        let rows = if panel.configuring.is_some() {
            4 + notice
        } else {
            panel.servers.len() as u16 + 1 + notice
        };
        return rows.min(max_height).max(1);
    }
    if app.command_panel_open() {
        let rows = matching_commands(app.input.as_str()).len() as u16;
        return rows.min(max_height).max(1);
    }
    if app.message.is_some() {
        let rows = app
            .message
            .as_deref()
            .map(|message| message.lines().count() as u16)
            .unwrap_or(0);
        return rows.min(max_height).max(1);
    }
    0
}

pub(super) fn render_transcript(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    transcript_view: &TranscriptView,
) {
    app.transcript_max_scroll = transcript_view.total_rows.saturating_sub(area.height);
    if app.transcript_follow_bottom {
        app.transcript_scroll = app.transcript_max_scroll;
    } else {
        app.transcript_scroll = app.transcript_scroll.min(app.transcript_max_scroll);
    }
    frame.render_widget(
        Paragraph::new(transcript_view.lines.clone())
            .scroll((app.transcript_scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
    let mut selectable_area = area;
    if !app.transcript_follow_bottom && app.transcript_max_scroll > 0 {
        let hint_area = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
        frame.render_widget(
            Paragraph::new(Line::styled(
                "↓ Jump to bottom (ctrl+End) ",
                Style::default().fg(theme().muted),
            ))
            .alignment(Alignment::Right),
            hint_area,
        );
        selectable_area.height = selectable_area.height.saturating_sub(1);
    }
    app.transcript_area = (selectable_area.height > 0).then_some(selectable_area);
    app.transcript_rows = rendered_text_rows(frame.buffer_mut(), selectable_area);
    render_transcript_selection(
        frame.buffer_mut(),
        selectable_area,
        app.transcript_selection,
    );
    render_inline_images(frame, selectable_area, app, transcript_view);
    app.tool_group_hit_regions = transcript_view
        .tool_group_rows
        .iter()
        .filter_map(|(key, row)| {
            let visible_row = row.checked_sub(app.transcript_scroll)?;
            (visible_row < area.height).then(|| ToolGroupHitRegion {
                key: key.clone(),
                row: area.y.saturating_add(visible_row),
            })
        })
        .collect();
}

fn render_inline_images(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    transcript_view: &TranscriptView,
) {
    // ponytail: resize in the frame loop; use ThreadProtocol only if measured redraw latency
    // from large decoded images becomes visible.
    for image in &transcript_view.images {
        let Some(row) = image.row.checked_sub(app.transcript_scroll) else {
            continue;
        };
        if row.saturating_add(INLINE_IMAGE_ROWS) > area.height {
            continue;
        }
        let image_area = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(row),
            area.width.saturating_sub(4).min(64),
            INLINE_IMAGE_ROWS,
        );
        if let Some(error) = app.inline_image_errors.get(image.key.as_str()) {
            frame.render_widget(
                Paragraph::new(format!("Image preview unavailable: {error}"))
                    .style(Style::default().fg(theme().warning))
                    .wrap(Wrap { trim: false }),
                image_area,
            );
        } else if let Some(protocol) = app.inline_images.get_mut(image.key.as_str()) {
            let resize = Resize::Scale(Some(FilterType::Triangle));
            let image_size = protocol.size_for(resize.clone(), image_area);
            let fitted_area = Rect::new(
                image_area.x,
                image_area.y,
                image_size.width,
                image_size.height,
            );
            frame.render_stateful_widget(
                StatefulImage::default().resize(resize),
                fitted_area,
                protocol,
            );
        } else {
            frame.render_widget(
                Paragraph::new("Image preview unavailable")
                    .style(Style::default().fg(theme().warning)),
                image_area,
            );
        }
    }
}

pub(super) fn rendered_text_rows(buffer: &Buffer, area: Rect) -> Vec<RenderedTextRow> {
    use unicode_width::UnicodeWidthStr;

    (0..area.height)
        .map(|row| {
            let mut text = String::new();
            let mut byte_at_column = vec![0; area.width as usize + 1];
            let mut column = 0usize;
            while column < area.width as usize {
                byte_at_column[column] = text.len();
                let symbol = buffer
                    .cell(Position::new(
                        area.x.saturating_add(column as u16),
                        area.y.saturating_add(row),
                    ))
                    .map(|cell| cell.symbol())
                    .unwrap_or(" ");
                let before = text.len();
                text.push_str(symbol);
                let after = text.len();
                let width = UnicodeWidthStr::width(symbol)
                    .max(1)
                    .min(area.width as usize - column);
                for offset in 1..=width {
                    byte_at_column[column + offset] =
                        if offset * 2 < width { before } else { after };
                }
                column += width;
            }
            let content_len = text.trim_end().len();
            text.truncate(content_len);
            for byte in &mut byte_at_column {
                *byte = (*byte).min(content_len);
            }
            RenderedTextRow {
                text,
                byte_at_column,
            }
        })
        .collect()
}

fn render_transcript_selection(buffer: &mut Buffer, area: Rect, selection: Option<TextSelection>) {
    let Some(selection) = selection else {
        return;
    };
    let (start, end) = if selection.anchor <= selection.head {
        (selection.anchor, selection.head)
    } else {
        (selection.head, selection.anchor)
    };
    for row in start.row..=end.row.min(area.height.saturating_sub(1)) {
        let from = if row == start.row { start.column } else { 0 };
        let to = if row == end.row {
            end.column
        } else {
            area.width
        };
        for column in from.min(area.width)..to.min(area.width) {
            if let Some(cell) = buffer.cell_mut(Position::new(
                area.x.saturating_add(column),
                area.y.saturating_add(row),
            )) {
                cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

/// Summary 行渲染：折叠前导空行，并以空白缩进与 user/tool 行区分。
pub(super) fn render_summary_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let mut markdown = render_markdown_lines(text.trim_start(), width.saturating_sub(2), false).0;
    indent_assistant_lines(&mut markdown);
    markdown
}

pub(super) fn indent_assistant_lines(markdown: &mut [Line<'static>]) {
    for line in markdown.iter_mut().filter(|line| {
        line.spans
            .iter()
            .any(|span| !span.content.as_ref().is_empty())
    }) {
        line.spans.insert(0, Span::raw("  "));
    }
}

/// 将 newline 闭合的输出从可变尾块推进 owned transcript source。
///
/// 这些行仍是当前 provider attempt 的视觉投影，不写入 session history；
/// `ModelTextReplace` 会撤销它们并从同一来源重绘。
pub(super) fn materialize_assistant_prefix(app: &mut App) {
    let Some(end) = app
        .assistant_buffer
        .rfind('\n')
        .map(|index| index.saturating_add(1))
    else {
        return;
    };
    if end <= app.assistant_emitted_bytes {
        return;
    }
    let source = app.assistant_buffer[app.assistant_emitted_bytes..end].to_string();
    let (_, in_code_block) = render_markdown_lines(
        source.as_str(),
        app.render_width,
        app.assistant_tail_in_code_block,
    );
    app.assistant_stream_started = true;
    app.assistant_emitted_bytes = end;
    app.assistant_tail_in_code_block = in_code_block;
    append_live_assistant_markdown(app, source, false);
}

pub(super) fn materialize_assistant_tail(app: &mut App, separator: bool) {
    let source = app.assistant_buffer[app.assistant_emitted_bytes..].to_string();
    if source.is_empty() && (!separator || !app.assistant_stream_started) {
        return;
    }
    let (_, in_code_block) = render_markdown_lines(
        source.as_str(),
        app.render_width,
        app.assistant_tail_in_code_block,
    );
    if !source.is_empty() {
        app.assistant_stream_started = true;
        app.assistant_emitted_bytes = app.assistant_buffer.len();
        app.assistant_tail_in_code_block = in_code_block;
    }
    append_live_assistant_markdown(app, source, separator);
}

pub(super) fn append_live_assistant_markdown(app: &mut App, markdown: String, separator: bool) {
    if markdown.is_empty() && !separator {
        return;
    }
    app.assistant_stream_start
        .get_or_insert(app.transcript.len());
    app.transcript.push(TranscriptLine::LiveAssistant {
        markdown,
        separator,
    });
}

/// 只渲染当前未闭合尾块；已闭合行已经进入 owned transcript source。
pub(super) fn build_assistant_live_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let assistant = &app.assistant_buffer[app.assistant_emitted_bytes..];
    if assistant.trim().is_empty() {
        return Vec::new();
    }
    let (mut lines, _) = render_markdown_lines(
        assistant,
        width.saturating_sub(2),
        app.assistant_tail_in_code_block,
    );
    indent_assistant_lines(&mut lines);
    lines
}

pub(super) fn build_transcript_view(app: &App, width: u16) -> TranscriptView {
    let mut lines = Vec::new();
    let mut tool_group_line_indices = Vec::new();
    let mut image_line_indices = Vec::new();
    let mut start = 0;
    while start < app.transcript.len() {
        if is_assistant_boundary(&app.transcript[start]) {
            let end = app.transcript[start..]
                .iter()
                .position(|item| !is_assistant_boundary(item))
                .map(|offset| start + offset)
                .unwrap_or(app.transcript.len());
            lines.extend(transcript_to_lines(&app.transcript[start..end], width));
            start = end;
            continue;
        }

        let end = app.transcript[start..]
            .iter()
            .position(is_assistant_boundary)
            .map(|offset| start + offset)
            .unwrap_or(app.transcript.len());
        let tools = app.transcript[start..end]
            .iter()
            .filter_map(|item| match item {
                TranscriptLine::Tool(tool) => Some(tool),
                _ => None,
            })
            .collect::<Vec<_>>();
        if tools.is_empty() {
            lines.extend(transcript_to_lines(&app.transcript[start..end], width));
            start = end;
            continue;
        }
        let key = tools[0].key.clone();
        let expanded = app.expanded_tool_groups.contains(&key);
        let focused = app.focused_tool_group.as_deref() == Some(key.as_str());
        let mut item = start;
        let mut header_rendered = false;
        while item < end {
            if let TranscriptLine::Tool(tool) = &app.transcript[item] {
                if !header_rendered {
                    tool_group_line_indices.push((key.clone(), lines.len()));
                    lines.push(tool_group_header(&tools, expanded, focused));
                    header_rendered = true;
                }
                if expanded {
                    lines.extend(transcript_to_lines(&app.transcript[item..item + 1], width));
                }
                for image in &tool.images {
                    image_line_indices.push((image.key.clone(), lines.len()));
                    push_inline_image_lines(&mut lines, image.path.as_str(), width);
                }
            } else {
                lines.extend(transcript_to_lines(&app.transcript[item..item + 1], width));
            }
            item += 1;
        }
        if !expanded {
            lines.push(Line::from(""));
        }
        start = end;
    }
    lines.extend(build_assistant_live_lines(app, width));

    let mut tool_group_rows = Vec::with_capacity(tool_group_line_indices.len());
    let mut images = Vec::with_capacity(image_line_indices.len());
    let mut total_rows = 0u16;
    let mut next_group = 0;
    let mut next_image = 0;
    for (line_index, line) in lines.iter().enumerate() {
        while tool_group_line_indices
            .get(next_group)
            .is_some_and(|(_, index)| *index == line_index)
        {
            tool_group_rows.push((tool_group_line_indices[next_group].0.clone(), total_rows));
            next_group += 1;
        }
        while image_line_indices
            .get(next_image)
            .is_some_and(|(_, index)| *index == line_index)
        {
            images.push(TranscriptImagePlacement {
                key: image_line_indices[next_image].0.clone(),
                row: total_rows,
            });
            next_image += 1;
        }
        total_rows =
            total_rows.saturating_add(paragraph_line_count(std::slice::from_ref(line), width));
    }
    TranscriptView {
        lines,
        tool_group_rows,
        images,
        total_rows,
    }
}

fn push_inline_image_lines(lines: &mut Vec<Line<'static>>, path: &str, width: u16) {
    lines.extend((0..INLINE_IMAGE_ROWS).map(|_| Line::from("")));
    lines.push(Line::from(vec![
        Span::styled("  Path: ", Style::default().fg(theme().muted)),
        Span::styled(
            fit_middle_columns(path, width.saturating_sub(8) as usize),
            Style::default().fg(theme().muted),
        ),
    ]));
    lines.push(Line::from(""));
}

fn is_assistant_boundary(item: &TranscriptLine) -> bool {
    matches!(
        item,
        TranscriptLine::User(_)
            | TranscriptLine::Summary(_)
            | TranscriptLine::LiveAssistant { .. }
            | TranscriptLine::Supplement(_)
    )
}

fn tool_group_header(
    tools: &[&ToolTranscriptLine],
    expanded: bool,
    focused: bool,
) -> Line<'static> {
    let title = tool_group_title(tools);
    let failed = tools.iter().any(|tool| {
        !tool.running && matches!(tool_outcome(&tool.result_states), ToolOutcome::Failed)
    });
    let style = if failed {
        Style::default().fg(theme().error)
    } else if focused {
        Style::default()
            .fg(theme().accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme().muted)
    };
    Line::from(vec![
        Span::styled(if focused { "› " } else { "  " }, style),
        Span::styled(title, style),
        Span::styled(if expanded { " ⌄" } else { " ›" }, style),
    ])
}

fn tool_group_title(tools: &[&ToolTranscriptLine]) -> String {
    if tools.len() == 1 {
        return stable_tool_title(tools[0]);
    }
    let running = tools.iter().any(|tool| tool.running);
    let mut parts = Vec::new();
    for kind in [
        ToolActionKind::Command,
        ToolActionKind::Read,
        ToolActionKind::Search,
        ToolActionKind::Edit,
        ToolActionKind::Browser,
        ToolActionKind::Plugin,
        ToolActionKind::Host,
        ToolActionKind::Tool,
    ] {
        let count = tools.iter().filter(|tool| tool.action_kind == kind).count();
        if count > 0 {
            parts.push(tool_group_part(kind, count, running));
        }
    }
    let mut title = parts.join(", ");
    if running {
        title.push_str(" · Running…");
    }
    title
}

fn tool_group_part(kind: ToolActionKind, count: usize, running: bool) -> String {
    let plural = |one: &'static str, many: &'static str| {
        if count == 1 {
            one
        } else {
            many
        }
    };
    let noun = match kind {
        ToolActionKind::Command => plural("command", "commands"),
        ToolActionKind::Read => plural("file", "files"),
        ToolActionKind::Search => plural("search", "searches"),
        ToolActionKind::Edit => plural("file", "files"),
        ToolActionKind::Browser => plural("browser action", "browser actions"),
        ToolActionKind::Plugin => plural("plugin", "plugins"),
        ToolActionKind::Host => plural("host operation", "host operations"),
        ToolActionKind::Tool => plural("tool", "tools"),
    };
    if running {
        return format!("{count} {noun}");
    }
    let verb = match kind {
        ToolActionKind::Command => "Ran",
        ToolActionKind::Read => "Read",
        ToolActionKind::Search => "Searched",
        ToolActionKind::Edit => "Edited",
        ToolActionKind::Browser => "Used",
        _ => "Called",
    };
    format!("{verb} {count} {noun}")
}

/// 行级 markdown 渲染：代码围栏、标题、列表、粗体、行内代码。
///
/// `in_code_block` 表示传入文本之前已处于未闭包代码围栏内；返回 `(lines, still_in_code_block)`。
/// 流式场景（assistant 增量文本）必须沿用上次的围栏状态。
pub(super) fn render_markdown_lines(
    text: &str,
    width: u16,
    in_code_block: bool,
) -> (Vec<Line<'static>>, bool) {
    let mut lines = Vec::new();
    let mut code = in_code_block;
    for raw_line in text.lines() {
        let trimmed = raw_line.trim_end();
        if let Some(fence) = code_fence(trimmed) {
            code = !code;
            if let Some(info) = fence {
                lines.push(Line::from(Span::styled(
                    format!("```{info}"),
                    Style::default().fg(theme().muted),
                )));
            }
            continue;
        }
        if code {
            lines.push(Line::from(Span::styled(
                trimmed.to_string(),
                Style::default().bg(theme().code_bg),
            )));
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("# ") {
            lines.push(Line::from(vec![Span::styled(
                heading.to_string(),
                Style::default()
                    .fg(theme().heading)
                    .add_modifier(Modifier::BOLD),
            )]));
            continue;
        }
        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let mut inline = render_inline_markdown(item, width);
            inline.insert(0, Span::styled("• ", Style::default().fg(theme().accent)));
            lines.push(Line::from(inline));
            continue;
        }
        lines.push(Line::from(render_inline_markdown(trimmed, width)));
    }
    (lines, code)
}

pub(super) fn code_fence(raw: &str) -> Option<Option<&str>> {
    if !raw.starts_with("```") {
        return None;
    }
    let rest = raw[3..].trim();
    Some(if rest.is_empty() { None } else { Some(rest) })
}

/// 行内渲染：`code` 带背景色，**粗体** 加 BOLD。
pub(super) fn render_inline_markdown(text: &str, width: u16) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    let mut plain = String::new();
    let max_width = width.saturating_sub(2) as usize;
    while !rest.is_empty() {
        if let Some(bold_end) = rest.find("**") {
            let (before, after_open) = rest.split_at(bold_end);
            let after_open = &after_open[2..];
            if let Some(bold_close) = after_open.find("**") {
                let (bold, tail) = after_open.split_at(bold_close);
                if !before.is_empty() {
                    plain.push_str(before);
                }
                flush_plain_span(&mut spans, &mut plain);
                let bold_text = fit_inline(bold, max_width);
                if !bold_text.is_empty() {
                    spans.push(Span::styled(
                        bold_text.to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                }
                rest = &tail[2..];
                continue;
            }
        }
        if let Some(code_end) = rest.find('`') {
            let (before, after_open) = rest.split_at(code_end);
            let after_open = &after_open[1..];
            if let Some(code_close) = after_open.find('`') {
                let (code, tail) = after_open.split_at(code_close);
                if !before.is_empty() {
                    plain.push_str(before);
                }
                flush_plain_span(&mut spans, &mut plain);
                let code_text = fit_inline(code, max_width);
                if !code_text.is_empty() {
                    spans.push(Span::styled(
                        code_text.to_string(),
                        Style::default().bg(theme().inline_code_bg),
                    ));
                }
                rest = &tail[1..];
                continue;
            }
        }
        plain.push_str(rest);
        break;
    }
    flush_plain_span(&mut spans, &mut plain);
    if spans.is_empty() {
        spans.push(Span::raw(text.to_string()));
    }
    spans
}

pub(super) fn flush_plain_span(spans: &mut Vec<Span<'static>>, plain: &mut String) {
    if plain.is_empty() {
        return;
    }
    spans.push(Span::raw(std::mem::take(plain)));
}

pub(super) fn fit_inline(text: &str, max_width: usize) -> &str {
    if text.chars().count() <= max_width {
        return text;
    }
    let cut = text
        .char_indices()
        .nth(max_width.saturating_sub(1))
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    &text[..cut]
}

pub(super) fn render_welcome(frame: &mut Frame, area: Rect) {
    render_welcome_to_buffer(frame.buffer_mut(), area);
}

fn render_welcome_to_buffer(buf: &mut Buffer, area: Rect) {
    Paragraph::new(welcome_line()).render(area, buf);
}

pub(super) fn welcome_line() -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "Centaeris",
            Style::default().add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Span::styled(
            format!(" v{} · Run ", env!("CARGO_PKG_VERSION")),
            Style::default().fg(theme().muted),
        ),
        Span::styled("/help", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(" for commands", Style::default().fg(theme().muted)),
    ])
}

pub(super) fn render_state_page(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(Paragraph::new(state_lines(app, area.width as usize)), area);
}

pub(super) fn state_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let value_width = width.saturating_sub(13).max(1);
    let runtime = if app.runtime.is_some() {
        "connected"
    } else if matches!(
        app.pending_model_request.as_ref(),
        Some(PendingModelRequest::Connecting { .. })
    ) {
        "connecting"
    } else {
        "not connected"
    };
    let model = app.model_display.as_deref().unwrap_or("<not configured>");
    let effort = app.model_effort.as_deref().unwrap_or("—");
    let provider = app.model_provider_id.as_deref().unwrap_or("—");
    let session = app
        .active_session
        .as_ref()
        .map(|session| format!("{} · {}", session.title, session.id))
        .unwrap_or_else(|| "none".to_string());
    let context = app
        .context_usage
        .as_ref()
        .map(
            |usage| match (usage.used_tokens, usage.max_context_tokens) {
                (Some(used), Some(max)) => match usage.used_percentage {
                    Some(percentage) => format!("{used} / {max} tokens · {percentage}%"),
                    None => format!("{used} / {max} tokens"),
                },
                _ => usage
                    .used_percentage
                    .map(|percentage| format!("{percentage}%"))
                    .unwrap_or_else(|| "unavailable".to_string()),
            },
        )
        .unwrap_or_else(|| "unavailable".to_string());

    vec![
        Line::from(vec![
            Span::styled("/state", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(" · current runtime", Style::default().fg(theme().muted)),
        ]),
        Line::from(""),
        state_row("RUNTIME", runtime),
        state_row("MODEL", fit_middle(model, value_width)),
        state_row("EFFORT", effort),
        state_row("PROVIDER", fit_middle(provider, value_width)),
        state_row("SESSION", fit_middle(session.as_str(), value_width)),
        state_row("CONTEXT", fit_middle(context.as_str(), value_width)),
        state_row(
            "WORKSPACE",
            fit_middle(app.workspace_root.as_str(), value_width),
        ),
    ]
}

fn state_row(label: &'static str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<11} "), Style::default().fg(theme().muted)),
        Span::raw(value.into()),
    ])
}

pub(super) fn render_input(frame: &mut Frame, area: Rect, app: &mut App) {
    let placeholder = if let Some(prompt) = app.model_credential_prompt.as_ref() {
        Some(format!("API key for {}", prompt.provider_name))
    } else if let Some(server) = app
        .mcp_panel
        .as_ref()
        .and_then(|panel| panel.configuring.and_then(|index| panel.servers.get(index)))
    {
        Some(format!("API key for {}", server.server_id))
    } else if app.pending_esc_stop {
        Some("Press Esc again to stop the AgentRun.".to_string())
    } else if app.pending_question.is_some() {
        Some("Answer the pending question".to_string())
    } else {
        None
    };
    let content_area = Rect::new(
        area.x.saturating_add(2),
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );
    let selection = input_selection_range(app);
    let body = if app.input.is_empty() {
        vec![Line::from(
            placeholder
                .map(|placeholder| Span::styled(placeholder, Style::default().fg(theme().muted)))
                .unwrap_or_else(|| Span::raw("")),
        )]
    } else {
        let display = input_for_display(app, app.input.as_str());
        let display_selection = selection.as_ref().map(|range| {
            if app.secret_input_active() {
                let bullet_bytes = '•'.len_utf8();
                let start = app.input[..range.start].chars().count() * bullet_bytes;
                let end = app.input[..range.end].chars().count() * bullet_bytes;
                start..end
            } else {
                range.clone()
            }
        });
        let ghost = if app.pending_question.is_some() || app.secret_input_active() {
            None
        } else {
            command_completion_suffix(app.input.as_str(), app.selected_command)
        };
        let mut input_lines = input_lines_with_selection(display.as_str(), display_selection);
        if let Some(ghost) = ghost.filter(|_| input_lines.len() == 1 && selection.is_none()) {
            if let Some(last) = input_lines.last_mut() {
                last.spans.push(Span::styled(
                    ghost,
                    Style::default()
                        .fg(theme().ghost)
                        .add_modifier(Modifier::DIM),
                ));
            }
        }
        hard_wrap_input_lines(input_lines, content_area.width.max(1) as usize)
    };
    app.input_area = Some(content_area);
    frame.render_widget(
        Paragraph::new(vec![Line::from("│"); area.height as usize]).style(
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        ),
        Rect::new(area.x, area.y, 1, area.height),
    );
    frame.render_widget(Paragraph::new(body), content_area);

    let cursor = clamped_cursor(app);
    let usable = content_area.width.max(1) as usize;
    let masked_before;
    let before = if app.secret_input_active() {
        masked_before = input_for_display(app, &app.input[..cursor]);
        masked_before.as_str()
    } else {
        &app.input[..cursor]
    };
    let (row, column) = cursor_position_in_wrapped(before, usable);
    let cursor_x = content_area.x.saturating_add(column as u16);
    let cursor_y = content_area.y.saturating_add(row as u16);
    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
}

pub(super) fn input_lines_with_selection(
    input: &str,
    selection: Option<Range<usize>>,
) -> Vec<Line<'static>> {
    let mut line_start = 0usize;
    input
        .split('\n')
        .map(|line| {
            let line_end = line_start + line.len();
            let spans = if let Some(selection) = selection.as_ref() {
                let start = selection.start.clamp(line_start, line_end) - line_start;
                let end = selection.end.clamp(line_start, line_end) - line_start;
                let mut spans = Vec::new();
                if start > 0 {
                    spans.push(Span::raw(line[..start].to_string()));
                }
                if start < end {
                    spans.push(Span::styled(
                        line[start..end].to_string(),
                        Style::default().add_modifier(Modifier::REVERSED),
                    ));
                }
                if end < line.len() {
                    spans.push(Span::raw(line[end..].to_string()));
                }
                spans
            } else {
                vec![Span::raw(line.to_string())]
            };
            line_start = line_end.saturating_add(1);
            Line::from(spans)
        })
        .collect()
}

pub(super) fn hard_wrap_input_lines(
    lines: Vec<Line<'static>>,
    usable: usize,
) -> Vec<Line<'static>> {
    let usable = usable.max(1);
    let mut wrapped = Vec::new();
    for line in lines {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut used = 0usize;
        for span in line.spans {
            for character in span.content.chars() {
                let width = character_width(character);
                if used + width > usable {
                    wrapped.push(Line::from(std::mem::take(&mut spans)));
                    used = 0;
                }
                if let Some(last) = spans.last_mut().filter(|last| last.style == span.style) {
                    last.content.to_mut().push(character);
                } else {
                    spans.push(Span::styled(character.to_string(), span.style));
                }
                used += width;
            }
        }
        wrapped.push(Line::from(spans));
    }
    wrapped
}

pub(super) fn input_for_display(app: &App, input: &str) -> String {
    if app.secret_input_active() {
        "•".repeat(input.chars().count())
    } else {
        input.to_string()
    }
}

pub(super) fn cursor_position_in_wrapped(before: &str, usable: usize) -> (usize, usize) {
    let usable = usable.max(1);
    let mut row = 0usize;
    let mut column = 0usize;
    let lines: Vec<&str> = before.split('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        let mut used = 0usize;
        for character in line.chars() {
            let width = character_width(character);
            if used + width > usable {
                row += 1;
                used = 0;
            }
            used += width;
        }
        if index + 1 == lines.len() {
            column = used;
        } else {
            row += 1;
        }
    }
    (row, column)
}

pub(super) fn character_width(character: char) -> usize {
    use unicode_width::UnicodeWidthChar;
    character.width().unwrap_or(0)
}

pub(super) fn render_panel(frame: &mut Frame, area: Rect, app: &App) {
    if app.home_risk_panel.is_some() {
        render_home_risk_panel(frame, area, app);
        return;
    }
    if app.pending_question.is_some() {
        render_question_panel(frame, area, app);
        return;
    }

    if app.mcp_panel.is_some() {
        render_mcp_panel(frame, area, app);
        return;
    }

    if app.command_panel_open() {
        render_command_panel(frame, area, app);
        return;
    }

    if let Some(message) = app.message.as_deref() {
        frame.render_widget(
            Paragraph::new(message.to_string())
                .style(Style::default().fg(theme().warning))
                .wrap(Wrap { trim: true }),
            area,
        );
    }
}

pub(super) fn render_home_risk_panel(frame: &mut Frame, area: Rect, app: &App) {
    let Some(panel) = app.home_risk_panel.as_ref() else {
        return;
    };
    let panel_area = Rect {
        x: area.x.saturating_add(2),
        y: area.y,
        width: area.width.saturating_sub(2).min(94),
        height: area.height,
    };
    let selected = panel.selected.min(panel.workspaces.len());
    let mut items = vec![
        ListItem::new(Line::from(Span::styled(
            "Home directory risk",
            Style::default()
                .fg(theme().warning)
                .add_modifier(Modifier::BOLD),
        ))),
        ListItem::new(Line::from(Span::styled(
            "This prompt would give the task the whole home directory as its workspace.",
            Style::default().fg(theme().muted),
        ))),
    ];
    for (index, workspace) in panel.workspaces.iter().enumerate() {
        items.push(
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<24}", fit_middle(workspace.name.as_str(), 24)),
                    Style::default().fg(theme().accent),
                ),
                Span::styled(
                    fit_middle(
                        workspace.root.as_str(),
                        panel_area.width.saturating_sub(26) as usize,
                    ),
                    Style::default().fg(theme().timestamp),
                ),
            ]))
            .style(if index == selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            }),
        );
    }
    items.push(
        ListItem::new(Line::from("Continue in the home directory")).style(
            if selected == panel.workspaces.len() {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default().fg(theme().warning)
            },
        ),
    );
    if let Some(notice) = panel.notice.as_deref() {
        items.push(ListItem::new(Line::from(Span::styled(
            fit_middle(notice, panel_area.width as usize),
            Style::default().fg(theme().error),
        ))));
    } else {
        items.push(ListItem::new(Line::from(Span::styled(
            "↑↓ Select · Enter continue · Esc return to the draft",
            Style::default().fg(theme().ghost),
        ))));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(List::new(items), panel_area);
}

pub(super) fn render_mcp_panel(frame: &mut Frame, area: Rect, app: &App) {
    let Some(panel) = app.mcp_panel.as_ref() else {
        return;
    };
    let panel_area = Rect {
        x: area.x.saturating_add(2),
        y: area.y,
        width: area.width.saturating_sub(2).min(94),
        height: area.height,
    };
    let mut items = Vec::new();
    if let Some(index) = panel.configuring {
        let Some(server) = panel.servers.get(index) else {
            return;
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled("← MCP  ", Style::default().fg(theme().muted)),
            Span::styled(
                server.server_id.clone(),
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ])));
        items.push(ListItem::new(Line::from(Span::styled(
            fit_middle(server.endpoint.as_deref().unwrap_or("fixed endpoint"), 88),
            Style::default().fg(theme().timestamp),
        ))));
        items.push(ListItem::new(Line::from(Span::styled(
            "Enter API key below; input is masked.",
            Style::default().fg(theme().muted),
        ))));
        items.push(ListItem::new(Line::from(Span::styled(
            "Enter Save & test · Esc Back",
            Style::default().fg(theme().ghost),
        ))));
    } else {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                "MCP",
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {} servers · ↑↓ Enter · Esc", panel.servers.len()),
                Style::default().fg(theme().muted),
            ),
        ])));
        for (index, server) in panel.servers.iter().enumerate() {
            let selected = index == panel.selected.min(panel.servers.len().saturating_sub(1));
            let actionable = server.configurable
                && !matches!(
                    server.status,
                    TuiMcpServerStatus::Disabled | TuiMcpServerStatus::Unsupported
                );
            let status = match server.status {
                TuiMcpServerStatus::Ready if server.configurable && server.configured => {
                    "configured"
                }
                TuiMcpServerStatus::Ready => "managed",
                TuiMcpServerStatus::NeedsConfiguration => "configure",
                TuiMcpServerStatus::Disabled => "plugin disabled",
                TuiMcpServerStatus::Unsupported => "unsupported",
            };
            let transport = match server.transport {
                TuiMcpTransport::Stdio => "stdio",
                TuiMcpTransport::StreamableHttp => "http",
            };
            let lock = if actionable { "" } else { "lock · " };
            let plugin = if server.plugin_enabled {
                server.plugin_display_name.as_str()
            } else {
                server.plugin_name.as_str()
            };
            let line = Line::from(vec![
                Span::styled(
                    fit_middle(server.server_id.as_str(), 34),
                    if actionable {
                        Style::default().fg(theme().accent)
                    } else {
                        Style::default().fg(theme().muted)
                    },
                ),
                Span::styled(
                    format!(
                        "  {plugin} · {} tools · {transport}  {lock}{status}",
                        server.tool_names.len()
                    ),
                    Style::default().fg(theme().timestamp),
                ),
            ]);
            items.push(ListItem::new(line).style(if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            }));
        }
    }
    if let Some(notice) = panel.notice.as_deref() {
        items.push(ListItem::new(Line::from(Span::styled(
            notice.to_string(),
            Style::default().fg(theme().warning),
        ))));
    }
    frame.render_widget(Clear, area);
    frame.render_widget(List::new(items), panel_area);
}

pub(super) fn render_command_panel(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 1 {
        return;
    }

    let commands = matching_commands(app.input.as_str());
    let height = (commands.len() as u16).min(area.height).max(1);
    let panel_area = Rect {
        x: area.x.saturating_add(2),
        y: area.y,
        width: area.width.saturating_sub(2).min(76),
        height,
    };
    let items = if commands.is_empty() {
        vec![ListItem::new(Line::from(vec![
            Span::styled("unknown", Style::default().fg(theme().error)),
            Span::raw("  Press Enter to fail loudly."),
        ]))]
    } else {
        let selected = app.selected_command.min(commands.len() - 1);
        commands
            .into_iter()
            .enumerate()
            .map(|(index, command)| {
                ListItem::new(command_preview_line(command, index == selected)).style(
                    if index == selected {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    },
                )
            })
            .collect()
    };
    let visible = panel_area.height as usize;
    let selected = app.selected_command.min(items.len().saturating_sub(1));
    let offset = selected.saturating_sub(visible.saturating_sub(1));
    let mut state = ListState::default()
        .with_offset(offset)
        .with_selected(Some(selected));

    frame.render_widget(Clear, area);
    frame.render_stateful_widget(List::new(items), panel_area, &mut state);
}

pub(super) fn command_preview_line(
    command: commands::SlashCommand,
    selected: bool,
) -> Line<'static> {
    let name_style = if selected {
        Style::default()
    } else {
        Style::default().fg(theme().accent)
    };
    Line::from(vec![
        Span::styled(
            format!("{:<width$}", command.name, width = COMMAND_NAME_WIDTH),
            name_style,
        ),
        Span::raw(command.description),
    ])
}

pub(super) fn render_question_panel(frame: &mut Frame, area: Rect, app: &App) {
    let Some(question) = app.pending_question.as_ref() else {
        return;
    };
    if area.height < 3 {
        return;
    }
    let option_rows = question.options.len() as u16;
    let height = (option_rows + 4).min(area.height).max(3);
    let panel_area = Rect {
        x: area.x.saturating_add(2),
        y: area.y,
        width: area.width.saturating_sub(2).min(94),
        height,
    };
    let mut items = vec![
        ListItem::new(Line::from(vec![
            Span::styled("问题：", Style::default().fg(theme().muted)),
            Span::styled(
                fit_middle(
                    question.question.as_str(),
                    (panel_area.width as usize).saturating_sub(10),
                ),
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        ListItem::new(Line::from(vec![Span::styled(
            "直接输入你的回答，Enter 继续。",
            Style::default().fg(theme().muted),
        )])),
    ];
    if !question.options.is_empty() {
        for option in &question.options {
            items.push(ListItem::new(Line::from(vec![Span::styled(
                fit_middle(
                    option.as_str(),
                    (panel_area.width as usize).saturating_sub(6),
                ),
                Style::default().fg(theme().muted),
            )])));
        }
    }
    items.push(ListItem::new(Line::from(vec![Span::styled(
        format!(
            "required={} · multiSelect={} · question {}",
            question.required, question.multi_select, question.id
        ),
        Style::default().fg(theme().muted),
    )])));

    frame.render_widget(Clear, panel_area);
    frame.render_widget(List::new(items), panel_area);
}

pub(super) fn render_model_panel(frame: &mut Frame, area: Rect, app: &mut App) {
    frame.render_widget(Clear, area);
    app.panel_area = Some(area);
    let content = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let notice_rows = u16::from(app.message.is_some());
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(notice_rows),
            Constraint::Length(1),
        ])
        .split(content);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Models",
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        Rect::new(chunks[0].x, chunks[0].y, chunks[0].width, 1),
    );

    let panel = app.model_panel.as_ref().expect("model panel was checked");
    let current = match (
        panel.active_provider_id.as_deref(),
        panel.active_model.as_deref(),
    ) {
        (Some(provider_id), Some(model_id)) => {
            let display_name = panel
                .providers
                .iter()
                .find(|provider| provider.provider_id == provider_id)
                .and_then(|provider| provider.models.iter().find(|model| model.model == model_id))
                .map(|model| model.display_name.as_str())
                .unwrap_or(model_id);
            let effort = app
                .model_effort
                .as_deref()
                .map(|value| format!(" · {value}"))
                .unwrap_or_default();
            format!("Current  {display_name} · {provider_id}/{model_id}{effort}")
        }
        _ => "Current  No active model".to_string(),
    };
    frame.render_widget(
        Paragraph::new(fit_middle_columns(
            current.as_str(),
            chunks[0].width as usize,
        ))
        .style(Style::default().fg(theme().timestamp)),
        Rect::new(
            chunks[0].x,
            chunks[0].y.saturating_add(1),
            chunks[0].width,
            1,
        ),
    );

    let selected_provider = panel
        .selected_provider
        .min(panel.providers.len().saturating_sub(1));
    let tab_labels = panel
        .providers
        .iter()
        .map(|provider| {
            if provider.configured {
                provider.name.clone()
            } else {
                format!("{} (setup)", provider.name)
            }
        })
        .collect::<Vec<_>>();
    let tab_widths = tab_labels
        .iter()
        .map(|label| label.chars().map(character_width).sum::<usize>())
        .collect::<Vec<_>>();
    let available_width = chunks[0].width as usize;
    let mut first_tab = 0usize;
    while first_tab < selected_provider
        && tab_widths[first_tab..=selected_provider]
            .iter()
            .sum::<usize>()
            + selected_provider.saturating_sub(first_tab) * 3
            > available_width
    {
        first_tab += 1;
    }
    app.model_provider_hit_regions = vec![Rect::default(); tab_labels.len()];
    let mut tabs = Vec::new();
    let mut used_width = 0usize;
    for index in first_tab..tab_labels.len() {
        let separator_width = usize::from(index > first_tab) * 3;
        if used_width + separator_width + tab_widths[index] > available_width {
            break;
        }
        if separator_width > 0 {
            tabs.push(Span::styled(" · ", Style::default().fg(theme().ghost)));
            used_width += separator_width;
        }
        let provider = &panel.providers[index];
        let style = if index == selected_provider {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if provider.configured {
            Style::default()
        } else {
            Style::default().fg(theme().muted)
        };
        app.model_provider_hit_regions[index] = Rect::new(
            chunks[0].x.saturating_add(used_width as u16),
            chunks[0].y.saturating_add(3),
            tab_widths[index] as u16,
            1,
        );
        tabs.push(Span::styled(tab_labels[index].clone(), style));
        used_width += tab_widths[index];
    }
    frame.render_widget(
        Paragraph::new(Line::from(tabs)),
        Rect::new(
            chunks[0].x,
            chunks[0].y.saturating_add(3),
            chunks[0].width,
            1,
        ),
    );

    let Some(provider) = panel.providers.get(selected_provider) else {
        frame.render_widget(
            Paragraph::new("No model providers available.")
                .style(Style::default().fg(theme().muted)),
            chunks[2],
        );
        frame.render_widget(
            Paragraph::new("Tab provider · ↑↓ select · Enter choose · Esc back")
                .style(Style::default().fg(theme().muted)),
            chunks[4],
        );
        return;
    };
    let provider_status = if provider.configured {
        format!(
            "{} · configured · {} models",
            provider.name,
            provider.models.len()
        )
    } else {
        format!("{} · API key required", provider.name)
    };
    frame.render_widget(
        Paragraph::new(provider_status).style(Style::default().fg(theme().muted)),
        chunks[1],
    );

    let selected_model = panel
        .selected_model
        .min(provider.models.len().saturating_sub(1));
    let items = if provider.configured {
        provider
            .models
            .iter()
            .enumerate()
            .map(|(index, model)| {
                let active = panel.active_provider_id.as_deref()
                    == Some(provider.provider_id.as_str())
                    && panel.active_model.as_deref() == Some(model.model.as_str());
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if active { "● " } else { "  " },
                        if active {
                            Style::default()
                        } else {
                            Style::default().fg(theme().ghost)
                        },
                    ),
                    Span::styled(
                        format!(
                            "{:<32}",
                            fit_middle_columns(model.display_name.as_str(), 32)
                        ),
                        Style::default(),
                    ),
                    Span::styled(
                        format!("  {}", model.model),
                        Style::default().fg(theme().timestamp),
                    ),
                ]))
                .style(if index == selected_model {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                })
            })
            .collect::<Vec<_>>()
    } else {
        vec![ListItem::new(Line::from(vec![
            Span::styled("Configure API key", Style::default()),
            Span::styled(
                "  Credentials stay in the shared Runtime config.",
                Style::default().fg(theme().muted),
            ),
        ]))
        .style(Style::default().add_modifier(Modifier::REVERSED))]
    };
    app.model_list_area = Some(chunks[2]);
    let visible = chunks[2].height as usize;
    let offset = selected_model.saturating_sub(visible.saturating_sub(1));
    app.model_list_offset = offset;
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No selectable models.").style(Style::default().fg(theme().muted)),
            chunks[2],
        );
    } else {
        let mut state = ListState::default()
            .with_offset(offset)
            .with_selected(Some(selected_model));
        frame.render_stateful_widget(List::new(items), chunks[2], &mut state);
    }

    if let Some(message) = app.message.as_deref() {
        let style = if message == "Switching model…" {
            Style::default().fg(theme().muted)
        } else {
            Style::default().fg(theme().warning)
        };
        frame.render_widget(Paragraph::new(message).style(style), chunks[3]);
    }
    frame.render_widget(
        Paragraph::new("Tab provider · ↑↓ select · Enter choose · Esc back")
            .style(Style::default().fg(theme().muted)),
        chunks[4],
    );
}

pub(super) fn render_session_picker(frame: &mut Frame, area: Rect, app: &mut App) {
    frame.render_widget(Clear, area);
    app.panel_area = Some(area);
    let content = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let action_rows = if app.rename_session_id.is_some() {
        input_row_count(app, content.width)
    } else {
        u16::from(app.pending_delete.is_some() || app.message.is_some())
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(action_rows),
            Constraint::Length(1),
        ])
        .split(content);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Sessions",
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        Rect::new(chunks[0].x, chunks[0].y, chunks[0].width, 1),
    );

    let selected_workspace = app
        .selected_session_workspace
        .min(app.session_workspaces.len().saturating_sub(1));
    let tab_labels = app
        .session_workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| {
            let name = fit_middle_columns(workspace.name.as_str(), 18);
            if index == selected_workspace {
                format!("[{name}]")
            } else {
                format!(" {name} ")
            }
        })
        .collect::<Vec<_>>();
    let tab_widths = tab_labels
        .iter()
        .map(|label| label.chars().map(character_width).sum::<usize>())
        .collect::<Vec<_>>();
    let available_width = chunks[0].width as usize;
    let mut first_tab = 0usize;
    while first_tab < selected_workspace
        && tab_widths[first_tab..=selected_workspace]
            .iter()
            .sum::<usize>()
            + selected_workspace.saturating_sub(first_tab)
            > available_width
    {
        first_tab += 1;
    }
    app.session_workspace_hit_regions = vec![Rect::default(); tab_labels.len()];
    let mut tabs = Vec::new();
    let mut used_width = 0usize;
    for index in first_tab..tab_labels.len() {
        let separator_width = usize::from(index > first_tab);
        if used_width + separator_width + tab_widths[index] > available_width {
            break;
        }
        if separator_width > 0 {
            tabs.push(Span::raw(" "));
            used_width += 1;
        }
        let style = if index == selected_workspace {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(theme().muted)
        };
        app.session_workspace_hit_regions[index] = Rect::new(
            chunks[0].x.saturating_add(used_width as u16),
            chunks[0].y.saturating_add(1),
            tab_widths[index] as u16,
            1,
        );
        tabs.push(Span::styled(tab_labels[index].clone(), style));
        used_width += tab_widths[index];
    }
    frame.render_widget(
        Paragraph::new(Line::from(tabs)),
        Rect::new(
            chunks[0].x,
            chunks[0].y.saturating_add(1),
            chunks[0].width,
            1,
        ),
    );
    let workspace_root = app
        .session_workspaces
        .get(selected_workspace)
        .map(|workspace| workspace.root.as_str())
        .unwrap_or("No workspace");
    frame.render_widget(
        Paragraph::new(fit_middle_columns(workspace_root, chunks[0].width as usize))
            .style(Style::default().fg(theme().timestamp)),
        Rect::new(
            chunks[0].x,
            chunks[0].y.saturating_add(2),
            chunks[0].width,
            1,
        ),
    );

    let selected = app
        .selected_session
        .min(app.sessions.len().saturating_sub(1));
    let title_width = ((chunks[1].width as usize) / 3).clamp(12, 32);
    let items = app
        .sessions
        .iter()
        .enumerate()
        .map(|(index, session)| {
            let pinned = if session.is_pinned { "[P] " } else { "    " };
            let unread = if session.is_unread { "* " } else { "  " };
            let preview = session
                .last_message
                .as_deref()
                .filter(|message| !message.trim().is_empty())
                .unwrap_or("no messages yet");
            let title = fit_middle_columns(session.title.as_str(), title_width);
            let title_padding =
                title_width.saturating_sub(title.chars().map(character_width).sum::<usize>());
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<8}  ", session.activity_state.label()),
                    if session.activity_state == TuiSessionActivityState::Idle {
                        Style::default()
                    } else {
                        Style::default().fg(theme().muted)
                    },
                ),
                Span::styled(
                    format!("{title}{}", " ".repeat(title_padding)),
                    Style::default(),
                ),
                Span::styled(
                    format!(
                        "  {pinned}{unread}{} · {} · {}",
                        short_session_id(session.id.as_str()),
                        format_relative_time(session.updated_at),
                        preview
                    ),
                    Style::default().fg(theme().timestamp),
                ),
            ]))
            .style(if index == selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            })
        })
        .collect::<Vec<_>>();
    app.session_list_area = Some(chunks[1]);
    let visible = chunks[1].height as usize;
    let offset = selected.saturating_sub(visible.saturating_sub(1));
    app.session_list_offset = offset;
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("No sessions in this workspace.")
                .style(Style::default().fg(theme().muted)),
            chunks[1],
        );
    } else {
        let mut state = ListState::default()
            .with_offset(offset)
            .with_selected(Some(selected));
        frame.render_stateful_widget(List::new(items), chunks[1], &mut state);
    }

    if action_rows > 0 {
        app.session_action_area = Some(chunks[2]);
        if app.rename_session_id.is_some() {
            render_input(frame, chunks[2], app);
        } else if let Some(session_id) = app.pending_delete.as_deref() {
            let title = app
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .map(|session| session.title.as_str())
                .unwrap_or(session_id);
            let half = chunks[2].width.saturating_div(2) as usize;
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!(
                            "{:<half$}",
                            fit_middle_columns(format!("[Delete] {title}").as_str(), half)
                        ),
                        Style::default().fg(theme().warning),
                    ),
                    Span::styled("[Cancel]", Style::default().fg(theme().muted)),
                ])),
                chunks[2],
            );
        } else if let Some(message) = app.message.as_deref() {
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(theme().warning)),
                chunks[2],
            );
        }
    }

    let help = if app.rename_session_id.is_some() {
        "Enter save · Esc cancel"
    } else if app.pending_delete.is_some() {
        "Enter delete · Esc cancel"
    } else {
        "Tab workspace · ↑↓ select · Enter resume · P pin · R rename · D delete · Esc back"
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(theme().muted)),
        chunks[3],
    );
}

pub(super) fn format_relative_time(updated_at: i64) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let delta_ms = now_ms.saturating_sub(updated_at).max(0);
    let minutes = delta_ms / 60_000;
    if minutes < 1 {
        return "just now".to_string();
    }
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{days}d ago");
    }
    let months = days / 30;
    if months < 12 {
        return format!("{months}mo ago");
    }
    format!("{}y ago", months / 12)
}

pub(super) fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let line = status_line(app, area.width as usize);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

/// 唯一动线：composer 上方的任务状态行（稳定指示 + 当前活动 + 计时）。
pub(super) fn render_status_indicator(frame: &mut Frame, area: Rect, app: &App) {
    let started_at = app.agent_run_started_at;
    let elapsed = started_at
        .map(|started_at| format_elapsed(started_at.elapsed()))
        .unwrap_or_else(|| "0m 00s".to_string());
    let header = status_header(app);
    let mut spans = vec![
        Span::styled(
            "•",
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            header,
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({elapsed} • esc 停止)"),
            Style::default()
                .fg(theme().muted)
                .add_modifier(Modifier::DIM),
        ),
    ];
    if let Some(inline) = status_inline(app) {
        spans.push(Span::styled(
            format!(" · {inline}"),
            Style::default()
                .fg(theme().muted)
                .add_modifier(Modifier::DIM),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(super) fn status_header(app: &App) -> String {
    if app.tool_protocol_error {
        return "Protocol error".to_string();
    }
    if let Some(count) = tachikoma_easter_egg_count(app) {
        return if count == 1 {
            "Tachikoma ×1 · awaiting result…".to_string()
        } else {
            format!("Tachikoma ×{count} · whispering…")
        };
    }
    if let Some(text) = app.runtime_easter_egg {
        return text.to_string();
    }
    match app.process_state {
        RuntimeDisplayState::Idle => "Idle",
        RuntimeDisplayState::Thinking => "Thinking",
        RuntimeDisplayState::ToolRunning => "Running tools",
        RuntimeDisplayState::ProviderWaiting => "Waiting for model",
        RuntimeDisplayState::WaitingUser => "Waiting for input",
        RuntimeDisplayState::Working => "Working",
    }
    .to_string()
}

pub(super) fn status_inline(app: &App) -> Option<String> {
    app.active_tool_label
        .clone()
        .or_else(|| app.tool_projection.active_label())
}

pub(super) fn status_line(app: &App, _width: usize) -> Line<'static> {
    let mut values = Vec::with_capacity(2);
    if let Some(effort) = app.model_effort.as_deref() {
        values.push(effort);
    }
    if let Some(model) = app.model_display.as_deref() {
        values.push(model);
    }
    Line::styled(values.join(" · "), Style::default().fg(theme().muted))
}

pub(super) fn paragraph_line_count(lines: &[Line<'static>], width: u16) -> u16 {
    let count = Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width);
    u16::try_from(count).unwrap_or(u16::MAX)
}

pub(super) fn transcript_to_lines(items: &[TranscriptLine], width: u16) -> Vec<Line<'static>> {
    transcript_to_lines_from(items, 0, width)
}

pub(super) fn transcript_to_lines_from(
    items: &[TranscriptLine],
    start: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut live_in_code_block = false;
    for (index, item) in items.iter().enumerate() {
        let emit = index >= start;
        match item {
            TranscriptLine::User(text) => {
                live_in_code_block = false;
                if emit {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "│ ",
                            Style::default()
                                .fg(theme().accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(text.clone()),
                    ]));
                    lines.push(Line::from(""));
                }
            }
            TranscriptLine::Summary(text) => {
                live_in_code_block = false;
                if emit {
                    lines.extend(render_summary_lines(text.as_str(), width));
                    lines.push(Line::from(""));
                }
            }
            TranscriptLine::LiveAssistant {
                markdown,
                separator,
            } => {
                let (mut rendered, in_code_block) = render_markdown_lines(
                    markdown.as_str(),
                    width.saturating_sub(2),
                    live_in_code_block,
                );
                indent_assistant_lines(&mut rendered);
                live_in_code_block = in_code_block;
                if emit {
                    lines.extend(rendered);
                    if *separator {
                        lines.push(Line::from(""));
                    }
                }
                if *separator {
                    live_in_code_block = false;
                }
            }
            TranscriptLine::Subagent(subagent) => {
                live_in_code_block = false;
                if !emit {
                    continue;
                }
                let style = match subagent.status.as_str() {
                    "failed" | "cancelled" => Style::default().fg(theme().error),
                    _ => Style::default().fg(theme().muted),
                };
                let summary = if subagent.summary.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", subagent.summary)
                };
                lines.push(Line::from(vec![
                    Span::styled("  ↳ ", style),
                    Span::styled(
                        fit_middle(
                            &format!("{}{}", subagent.title, summary),
                            width.saturating_sub(4) as usize,
                        ),
                        style,
                    ),
                ]));
            }
            TranscriptLine::Supplement(text) => {
                live_in_code_block = false;
                if emit {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "     └─ ",
                            Style::default()
                                .fg(theme().muted)
                                .add_modifier(Modifier::DIM),
                        ),
                        Span::styled(
                            text.clone(),
                            Style::default()
                                .fg(theme().muted)
                                .add_modifier(Modifier::DIM),
                        ),
                    ]));
                }
            }
            TranscriptLine::Tool(tool) => {
                live_in_code_block = false;
                if !emit {
                    continue;
                }
                let outcome = tool_outcome(&tool.result_states);
                let indicator_style = if tool.running {
                    Style::default().fg(theme().muted)
                } else if tool.interrupted {
                    Style::default().fg(theme().warning)
                } else {
                    match outcome {
                        ToolOutcome::Succeeded => Style::default().fg(theme().success),
                        ToolOutcome::Failed => Style::default().fg(theme().error),
                        ToolOutcome::Denied | ToolOutcome::Aborted => {
                            Style::default().fg(theme().warning)
                        }
                    }
                };
                lines.push(Line::from(vec![
                    Span::styled("  • ", indicator_style),
                    Span::styled(
                        stable_tool_title(tool),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]));
                if tool.action_kind == ToolActionKind::Command && tool.description_title {
                    push_text_result_block(
                        &mut lines,
                        &[tool.command.as_deref().unwrap_or_default().to_string()],
                    );
                }
                let show_diff_path = tool_operation_paths(&tool.operations).len() > 1;
                let result_detail = empty_tool_result_detail(tool);
                let detail_replaces_blocks =
                    matches!(result_detail.as_deref(), Some("No output" | "No matches"));
                if !detail_replaces_blocks {
                    for block in &tool.result_blocks {
                        push_tool_result_block(&mut lines, block, width, show_diff_path);
                    }
                }
                if let Some(detail) = result_detail {
                    push_text_result_block(&mut lines, &[detail.to_string()]);
                }
                lines.push(Line::from(""));
            }
            TranscriptLine::Error(text) => {
                live_in_code_block = false;
                if emit {
                    lines.push(Line::from(vec![
                        Span::styled("! ", Style::default().fg(theme().error)),
                        Span::styled(text.clone(), Style::default().fg(theme().error)),
                    ]));
                }
            }
        }
    }
    lines
}

pub(super) fn push_tool_result_block(
    lines: &mut Vec<Line<'static>>,
    block: &ToolResultBlock,
    width: u16,
    show_diff_path: bool,
) {
    match block {
        ToolResultBlock::Text {
            lines: result_lines,
        } => {
            let rendered = result_lines
                .iter()
                .map(|result| match result {
                    TextResultLine::Text(text) => {
                        fit_middle_columns(text, (width as usize).saturating_sub(6).max(1))
                    }
                    TextResultLine::Hidden(hidden) => format!("… {hidden} lines hidden"),
                })
                .collect::<Vec<_>>();
            push_text_result_block(lines, &rendered);
        }
        ToolResultBlock::Diff { path, rows, .. } => {
            let prefix = if show_diff_path {
                lines.push(Line::from(vec![
                    Span::styled("  └─ ", tool_result_style()),
                    Span::styled(path.clone(), tool_result_style()),
                ]));
                "      "
            } else {
                "    "
            };
            let line_number_width = diff_line_number_width(rows);
            for row in rows {
                lines.push(diff_row_line(
                    row,
                    line_number_width,
                    width as usize,
                    prefix,
                ));
            }
        }
    }
}

pub(super) fn push_text_result_block(lines: &mut Vec<Line<'static>>, result_lines: &[String]) {
    for (index, text) in result_lines.iter().enumerate() {
        let prefix = if index == 0 { "  └─ " } else { "      " };
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), tool_result_style()),
            Span::styled(text.clone(), tool_result_style()),
        ]));
    }
}

pub(super) fn diff_row_line(
    row: &DiffRow,
    line_number_width: usize,
    width: usize,
    prefix: &str,
) -> Line<'static> {
    let style = diff_result_style(&row.kind);
    let body = match row.kind {
        DiffRowKind::Hidden(hidden) if hidden > 0 => {
            format!(
                "{:width$}  ⋮ hidden {hidden} lines",
                "",
                width = line_number_width
            )
        }
        DiffRowKind::Hidden(_) => format!("{:width$}  ⋮", "", width = line_number_width),
        DiffRowKind::Context | DiffRowKind::Insert | DiffRowKind::Delete => {
            let marker = match row.kind {
                DiffRowKind::Context => ' ',
                DiffRowKind::Insert => '+',
                DiffRowKind::Delete => '-',
                DiffRowKind::Hidden(_) => unreachable!(),
            };
            let line_number = row
                .line_number
                .map(|value| value.to_string())
                .unwrap_or_default();
            let fixed_width = prefix.chars().count() + line_number_width + 3;
            let content =
                fit_middle_columns(row.text.as_str(), width.saturating_sub(fixed_width).max(1));
            format!("{line_number:>line_number_width$} {marker} {content}")
        }
    };
    Line::from(vec![
        Span::styled(prefix.to_string(), style),
        Span::styled(body, style),
    ])
}

pub(super) fn diff_line_number_width(rows: &[DiffRow]) -> usize {
    rows.iter()
        .filter_map(|row| row.line_number)
        .max()
        .map(|line_number| line_number.to_string().len())
        .unwrap_or(1)
}

pub(super) fn tool_result_style() -> Style {
    Style::default()
        .fg(theme().muted)
        .add_modifier(Modifier::DIM)
}

pub(super) fn diff_result_style(kind: &DiffRowKind) -> Style {
    let style = Style::default().add_modifier(Modifier::DIM);
    match kind {
        DiffRowKind::Insert => style.bg(theme().diff_add_bg),
        DiffRowKind::Delete => style.bg(theme().diff_delete_bg),
        DiffRowKind::Context | DiffRowKind::Hidden(_) => style,
    }
}

pub(super) fn format_elapsed(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes}m {seconds:02}s")
}

pub(super) fn session_summary(session: &TuiSession) -> String {
    format!(
        "{} ({})",
        session.title,
        short_session_id(session.id.as_str())
    )
}

pub(super) fn short_session_id(id: &str) -> String {
    let chars = id.chars().collect::<Vec<_>>();
    if chars.len() <= 18 {
        return id.to_string();
    }
    let tail = chars
        .iter()
        .skip(chars.len().saturating_sub(12))
        .collect::<String>();
    format!("...{tail}")
}

pub(super) fn fit_middle(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let left_len = (max_chars - 3) / 2;
    let right_len = max_chars - 3 - left_len;
    let left: String = chars.iter().take(left_len).collect();
    let right: String = chars.iter().skip(chars.len() - right_len).collect();
    format!("{left}...{right}")
}

pub(super) fn fit_middle_columns(value: &str, max_columns: usize) -> String {
    let value_width = value.chars().map(character_width).sum::<usize>();
    if value_width <= max_columns {
        return value.to_string();
    }
    if max_columns <= 3 {
        return ".".repeat(max_columns);
    }

    let content_columns = max_columns - 3;
    let left_columns = content_columns / 2;
    let right_columns = content_columns - left_columns;
    let mut left = String::new();
    let mut used_left = 0usize;
    for character in value.chars() {
        let width = character_width(character);
        if used_left + width > left_columns {
            break;
        }
        left.push(character);
        used_left += width;
    }
    let mut right = Vec::new();
    let mut used_right = 0usize;
    for character in value.chars().rev() {
        let width = character_width(character);
        if used_right + width > right_columns {
            break;
        }
        right.push(character);
        used_right += width;
    }
    right.reverse();
    format!("{left}...{}", right.into_iter().collect::<String>())
}
