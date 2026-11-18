use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::widgets::{Block, BorderType, Borders};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

use super::*;

pub(super) const COMMAND_NAME_WIDTH: usize = 16;
const SHORT_TERMINAL_ROWS: u16 = 18;
const USER_PROMPT_PREFIX: &str = "  ❯ ";
const USER_PROMPT_CONTINUATION: &str = "    ";
const PROMPT_TEXT_COLUMN: u16 = USER_PROMPT_CONTINUATION.len() as u16;

pub(super) struct TranscriptView {
    pub(super) lines: Vec<Line<'static>>,
    line_start_rows: Vec<u64>,
    pub(super) source_rows: Vec<(String, u64)>,
    bash_block_rows: Vec<(String, Range<u64>)>,
    pub(super) images: Vec<TranscriptImagePlacement>,
    pub(super) total_rows: u64,
    pub(super) layout: Arc<text_layout::TextLayout>,
}

pub(super) struct TranscriptImagePlacement {
    pub(super) key: String,
    pub(super) row: u64,
}

pub(super) struct TranscriptRenderWindow {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) local_scroll: u16,
}

pub(super) struct TranscriptLayoutCache {
    width: u16,
    revision: u64,
    transcript_len: usize,
    view: Arc<TranscriptView>,
    pub(super) rebuilds: u64,
}

pub(super) fn build_cached_transcript_view(app: &mut App, width: u16) -> Arc<TranscriptView> {
    if let Some(cache) = app.transcript_layout_cache.as_ref() {
        if cache.width == width
            && cache.revision == app.transcript_revision
            && cache.transcript_len == app.transcript.len()
        {
            return Arc::clone(&cache.view);
        }
    }
    let view = Arc::new(build_transcript_view(app, width));
    if !app.transcript_follow_bottom {
        if let Some(cache) = app.transcript_layout_cache.as_ref() {
            app.transcript_scroll = restore_reading_row(
                &cache.view,
                &view,
                app.transcript_scroll,
                cache.width,
                width,
            );
        }
    }
    let rebuilds = app
        .transcript_layout_cache
        .as_ref()
        .map(|cache| cache.rebuilds.saturating_add(1))
        .unwrap_or(1);
    app.transcript_layout_cache = Some(TranscriptLayoutCache {
        width,
        revision: app.transcript_revision,
        transcript_len: app.transcript.len(),
        view: Arc::clone(&view),
        rebuilds,
    });
    view
}

// Keep the visible source line when preview loading or wrapping changes rows above it.
// Prefer the same tool span so identical output from different tools does not alias.
fn restore_reading_row(
    old: &TranscriptView,
    new: &TranscriptView,
    scroll: u64,
    old_width: u16,
    width: u16,
) -> u64 {
    if old.layout.text == new.layout.text {
        let offset = old.layout.offset_at(scroll as usize, 0);
        let preceding_rows = (old.layout.position(offset).0 as u64).saturating_sub(scroll);
        return (new.layout.position(offset).0 as u64).saturating_sub(preceding_rows);
    }
    let index = old
        .line_start_rows
        .partition_point(|row| *row <= scroll)
        .saturating_sub(1);
    let Some(line) = old.lines.get(index) else {
        return scroll;
    };
    let text = |line: &Line<'_>| {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    };
    let target = text(line);
    let scope = old.source_rows.iter().rev().find(|(_, row)| *row <= scroll);
    let new_scope = scope.and_then(|(key, _)| {
        new.source_rows
            .iter()
            .find(|(candidate, _)| candidate == key)
    });
    let old_start = scope.map_or(0, |(_, row)| {
        old.line_start_rows.partition_point(|r| r < row)
    });
    let new_start = new_scope.map_or(0, |(_, row)| {
        new.line_start_rows.partition_point(|r| r < row)
    });
    let occurrence = old.lines[old_start..index]
        .iter()
        .filter(|line| text(line) == target)
        .count();
    if !target.trim().is_empty() {
        if let Some((found, _)) = new
            .lines
            .iter()
            .enumerate()
            .skip(new_start)
            .filter(|(_, line)| text(line) == target)
            .nth(occurrence)
        {
            let offset = scroll.saturating_sub(old.line_start_rows[index])
                * u64::from(old_width.max(1))
                / u64::from(width.max(1));
            let height = u64::from(paragraph_line_count(
                std::slice::from_ref(&new.lines[found]),
                width,
            ));
            return new.line_start_rows[found] + offset.min(height.saturating_sub(1));
        }
    }
    // A collapsed/evicted section no longer has the source line: retain its header.
    if let Some((_, row)) = new_scope {
        return *row;
    }
    scroll
}

pub(super) fn render(frame: &mut Frame, app: &mut App, transcript_view: &TranscriptView) {
    mouse::prepare_frame(app);
    render_content(frame, app, transcript_view);
    mouse::decorate(frame, app);
}

fn render_content(frame: &mut Frame, app: &mut App, transcript_view: &TranscriptView) {
    let area = frame.area();
    app.input_area = None;
    app.panel_area = None;
    app.transcript_area = None;
    app.image_preview_area = None;
    app.transcript_viewport = None;
    app.model_provider_hit_regions.clear();
    app.model_list_area = None;
    app.model_list_offset = 0;
    if app.session_picker.is_some() {
        session_picker::render(frame, area, app);
        return;
    }
    if area.width < 32 || area.height < 10 {
        let paragraph =
            Paragraph::new("Terminal too small.").style(Style::default().fg(theme().muted));
        frame.render_widget(paragraph, area);
        render_image_preview(frame, app, area);
        return;
    }

    if app.model_panel.is_some() {
        render_model_panel(frame, area, app);
        return;
    }
    if app.read_only_session.is_some() {
        render_read_only_session(frame, area, app, transcript_view);
        return;
    }

    let has_timeline = !app.transcript.is_empty()
        || app.tool_projection.has_open_calls()
        || app.active_tool_label.is_some()
        || !app.assistant_buffer.trim().is_empty();
    let status_rows = u16::from(
        has_active_agent_run(app)
            || app.pending_question.is_some()
            || app.process_state == RuntimeDisplayState::ConnectionLost,
    );
    // One row each for the workspace, transcript and shortcuts, plus two composer borders.
    let available = area.height.saturating_sub(status_rows + 5);
    let input_rows = input_row_count(app, area.width).min(available);
    let panel_space = if app.home_risk_panel.is_some() || app.pending_question.is_some() {
        available.saturating_sub(1)
    } else {
        available.saturating_sub(input_rows)
    };
    let panel_rows = panel_height(app, panel_space).min(panel_space);
    let input_rows = input_rows.min(available.saturating_sub(panel_rows));
    let input_gap = u16::from(
        area.height > SHORT_TERMINAL_ROWS
            && available > input_rows
            && has_timeline
            && status_rows == 0
            && panel_rows == 0,
    );
    let panel_above_input = app.command_panel_open() || app.home_risk_panel.is_some();
    let (above_rows, below_rows) = if panel_above_input {
        (panel_rows, 0)
    } else {
        (0, panel_rows)
    };
    let [header, history, status, above_input, _, input, below_input, shortcuts] =
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(status_rows),
            Constraint::Length(above_rows),
            Constraint::Length(input_gap),
            Constraint::Length(input_rows + 2),
            Constraint::Length(below_rows),
            Constraint::Length(1),
        ])
        .areas(area);
    render_workspace_header(frame, header, app);
    if has_timeline {
        render_transcript(frame, history, app, transcript_view);
    } else {
        render_welcome(frame, history);
        app.transcript_scroll = 0;
        app.transcript_max_scroll = 0;
        app.transcript_selection = None;
    }
    if status_rows > 0 {
        render_status_indicator(frame, status, app);
    }
    if panel_rows > 0 {
        let panel_area = if panel_above_input {
            above_input
        } else {
            below_input
        };
        app.panel_area = Some(panel_area);
        render_panel(frame, panel_area, app);
    }
    render_input(frame, input, app);
    render_shortcuts(frame, shortcuts, app);
    render_image_preview(frame, app, history);
}

fn render_read_only_session(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    transcript_view: &TranscriptView,
) {
    let notice_rows = u16::from(app.message.is_some());
    let [history, notice, hints, error, status] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(notice_rows),
        Constraint::Length(1),
    ])
    .areas(area);
    render_transcript(frame, history, app, transcript_view);
    frame.render_widget(
        Paragraph::new("Session open in another window").style(Style::default().fg(theme().accent)),
        notice,
    );
    frame.render_widget(
        Paragraph::new("R Retry · F Fork").style(Style::default().fg(theme().muted)),
        hints,
    );
    if let Some(message) = app.message.as_deref() {
        frame.render_widget(Paragraph::new(message), error);
    }
    render_status(frame, status, app);
    render_image_preview(frame, app, history);
}

fn render_image_preview(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(preview) = app.image_preview.as_mut() else {
        return;
    };
    frame.render_widget(Clear, area);
    let available = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let preview = match preview {
        ImagePreview::Details(details) => {
            app.image_preview_area = Some(render_image_details(frame, available, details));
            return;
        }
        ImagePreview::Pixels(preview) => preview,
    };
    let image_bounds = Rect::new(
        available.x,
        available.y,
        available.width,
        available.height.saturating_sub(1),
    );
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
    let zoom = if preview.protocol.is_none() {
        "Loading preview…".to_string()
    } else if preview.view.zoom_steps == 0 {
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

fn render_image_details(frame: &mut Frame, area: Rect, details: &ImagePreviewDetails) -> Rect {
    let width = area.width.min(72);
    let height = area.height.min(4);
    let content = Rect::new(
        area.x.saturating_add((area.width - width) / 2),
        area.y.saturating_add((area.height - height) / 2),
        width,
        height,
    );
    let path = fit_middle_columns(
        details.path.to_string_lossy().as_ref(),
        width.saturating_sub(6) as usize,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from("Image information · Esc to close"),
            Line::from(format!(
                "{} × {} · {}",
                details.width, details.height, details.mime_type
            )),
            Line::from(format!("Size: {} bytes", details.byte_len)),
            Line::from(format!("Path: {path}")),
        ])
        .style(Style::default().fg(theme().muted)),
        content,
    );
    content
}

fn composer_layout(app: &App, width: u16) -> text_layout::TextLayout {
    if app.secret_input_active() {
        return text_layout::TextLayout::plain(&input_for_display(app, &app.input), width);
    }
    let images = app
        .draft_image_attachments
        .iter()
        .map(|image| image.start..image.end)
        .collect::<Vec<_>>();
    text_layout::TextLayout::composer(&app.input, width, &images)
}

pub(super) fn input_row_count(app: &App, width: u16) -> u16 {
    let layout = composer_layout(app, composer_text_width(width));
    let caret = layout
        .caret(input_display_offset(app, clamped_cursor(app)))
        .0
        + 1;
    layout.height().max(caret).clamp(1, 10) as u16
}

fn composer_text_width(width: u16) -> u16 {
    width.saturating_sub(PROMPT_TEXT_COLUMN + 1).max(1)
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
    if app.command_panel_open() {
        let rows = matching_commands(&app.input).len() as u16;
        return rows.min(max_height).max(1);
    }
    if app.message.is_some() || app.secret_input_active() {
        let rows = app
            .message
            .as_deref()
            .map_or(1, |message| message.lines().count() as u16);
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
    app.transcript_max_scroll = transcript_view
        .total_rows
        .saturating_sub(u64::from(area.height));
    if app.transcript_follow_bottom {
        app.transcript_scroll = app.transcript_max_scroll;
    } else {
        app.transcript_scroll = app.transcript_scroll.min(app.transcript_max_scroll);
    }
    output_preview::prepare_visible(app, transcript_view, area.height);
    let window = transcript_render_window(transcript_view, app.transcript_scroll, area.height);
    app.transcript_viewport = Some(area);
    let content = Rect {
        width: area.width.saturating_sub(1),
        ..area
    };
    for (offset, line) in window
        .lines
        .iter()
        .skip(window.local_scroll.into())
        .take(area.height.into())
        .enumerate()
    {
        if line.style.bg == theme().user_prompt().bg {
            frame.buffer_mut().set_style(
                Rect::new(content.x, content.y + offset as u16, content.width, 1),
                theme().user_prompt(),
            );
        }
    }
    frame.render_widget(
        Paragraph::new(window.lines).scroll((window.local_scroll, 0)),
        content,
    );
    let mut selectable_area = content;
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
    mouse::set_bash_blocks(app, selectable_area, &transcript_view.bash_block_rows);
    mouse::set_text_layout(
        app,
        Arc::clone(&transcript_view.layout),
        app.transcript_scroll as usize,
    );
    mouse::render_bash_selection(frame, app);
    render_transcript_selection(frame.buffer_mut(), selectable_area, app);
    render_inline_images(frame, selectable_area, app, transcript_view);
}

fn render_inline_images(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    transcript_view: &TranscriptView,
) {
    // Images are resized in the frame loop.
    for image in &transcript_view.images {
        let Some(row) = image.row.checked_sub(app.transcript_scroll) else {
            continue;
        };
        if row.saturating_add(u64::from(INLINE_IMAGE_ROWS)) > u64::from(area.height) {
            continue;
        }
        let row = u16::try_from(row).unwrap_or(u16::MAX);
        let image_area = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(row),
            area.width.saturating_sub(4).min(64),
            INLINE_IMAGE_ROWS,
        );
        if let Some(error) = app.inline_image_errors.get(image.key.as_str()) {
            frame.render_widget(
                Paragraph::new(format!("Image preview unavailable: {error}"))
                    .style(Style::default().fg(theme().muted))
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
                    .style(Style::default().fg(theme().muted)),
                image_area,
            );
        }
    }
}

pub(super) fn transcript_render_window(
    transcript_view: &TranscriptView,
    scroll: u64,
    height: u16,
) -> TranscriptRenderWindow {
    if transcript_view.lines.is_empty() {
        return TranscriptRenderWindow {
            lines: Vec::new(),
            local_scroll: 0,
        };
    }
    let start = transcript_view
        .line_start_rows
        .partition_point(|row| *row <= scroll)
        .saturating_sub(1);
    let origin_row = transcript_view.line_start_rows[start];
    let visible_end = scroll.saturating_add(u64::from(height)).saturating_add(1);
    let end = transcript_view
        .line_start_rows
        .partition_point(|row| *row < visible_end)
        .max(start.saturating_add(1))
        .min(transcript_view.lines.len());
    TranscriptRenderWindow {
        lines: transcript_view.lines[start..end].to_vec(),
        local_scroll: u16::try_from(scroll.saturating_sub(origin_row)).unwrap_or(u16::MAX),
    }
}

pub(super) fn render_transcript_selection(buffer: &mut Buffer, area: Rect, app: &App) {
    let (Some(layout), Some(selection)) = (app.text_layout.as_ref(), app.transcript_selection)
    else {
        return;
    };
    paint_selection(buffer, area, layout, app.text_scroll, selection);
}

fn paint_selection(
    buffer: &mut Buffer,
    area: Rect,
    layout: &text_layout::TextLayout,
    scroll: usize,
    selection: TextSelection,
) {
    for (row, columns) in layout.selected_cells(selection) {
        let Some(row) = row
            .checked_sub(scroll)
            .filter(|row| *row < area.height as usize)
        else {
            continue;
        };
        for column in columns.start.min(area.width as usize)..columns.end.min(area.width as usize) {
            if let Some(cell) = buffer.cell_mut((area.x + column as u16, area.y + row as u16)) {
                cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

/// Align assistant text with the command and user-message text.
pub(super) fn render_summary_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    let projected = centaeris_core::session::visualization::project_markdown(text, true);
    let mut markdown =
        render_markdown_lines(projected.trim_start(), width.saturating_sub(2), false).0;
    indent_assistant_lines(&mut markdown);
    markdown
}

fn indent_assistant_lines(markdown: &mut [Line<'static>]) {
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
pub(super) fn materialize_assistant_prefix(app: &mut App) -> bool {
    let Some(mut end) = app
        .assistant_buffer
        .rfind('\n')
        .map(|index| index.saturating_add(1))
    else {
        return false;
    };
    for block in centaeris_core::session::visualization::visual_blocks(&app.assistant_buffer) {
        if !block.complete || (block.start < end && block.end > end) {
            end = end.min(block.start);
        }
    }
    end = markdown_table::stable_prefix(&app.assistant_buffer, end);
    if end <= app.assistant_emitted_bytes {
        return false;
    }
    let source = centaeris_core::session::visualization::project_markdown_range(
        &app.assistant_buffer,
        app.assistant_emitted_bytes..end,
        false,
    )
    .expect("source-aligned assistant prefix");
    let (_, in_code_block) = render_markdown_lines(
        source.as_str(),
        app.render_width,
        app.assistant_tail_in_code_block,
    );
    app.assistant_stream_started = true;
    app.assistant_emitted_bytes = end;
    app.assistant_tail_in_code_block = in_code_block;
    append_live_assistant_markdown(app, source, false);
    true
}

pub(super) fn materialize_assistant_tail(app: &mut App, separator: bool) {
    let source = centaeris_core::session::visualization::project_markdown_range(
        &app.assistant_buffer,
        app.assistant_emitted_bytes..app.assistant_buffer.len(),
        separator,
    )
    .expect("source-aligned assistant tail");
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
    app.transcript.push(
        TranscriptLine::LiveAssistant {
            markdown,
            separator,
        }
        .for_run(app.assistant_run_id.as_deref(), app.assistant_is_final),
    );
}

/// 只渲染当前未闭合尾块；已闭合行已经进入 owned transcript source。
pub(super) fn build_assistant_live_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let projected = centaeris_core::session::visualization::project_markdown_range(
        &app.assistant_buffer,
        app.assistant_emitted_bytes..app.assistant_buffer.len(),
        false,
    )
    .expect("source-aligned assistant preview");
    let assistant = projected.as_str();
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
    let transcript = runs::visible_transcript(app);
    let mut lines = Vec::new();
    let mut source_line_indices = Vec::new();
    let mut bash_block_indices = Vec::new();
    let mut image_line_indices = Vec::new();
    let mut hanging_indents = Vec::new();
    let mut decorations = Vec::new();
    let mut spacing = Vec::new();
    let mut cursor = 0;
    while cursor < transcript.len() {
        let item = &transcript[cursor];
        match item {
            TranscriptLine::Tool(tool) => {
                let expanded = tool.is_bash && app.expanded_bash_tools.contains(&tool.key);
                let adjacent_collapsed_bash = tool.is_bash
                    && !expanded
                    && tool.images.is_empty()
                    && cursor > 0
                    && matches!(
                        &transcript[cursor - 1],
                        TranscriptLine::Tool(previous)
                            if previous.is_bash && previous.images.is_empty()
                                && !app.expanded_bash_tools.contains(&previous.key)
                    );
                if !adjacent_collapsed_bash {
                    space_transcript_blocks(&lines, &mut spacing);
                }
                let first_line = lines.len();
                source_line_indices.push((tool.key.clone(), lines.len()));
                if tool.is_bash {
                    decorations.push((lines.len(), "› ".len()));
                    lines.push(presentation::bash_header(tool, width, expanded));
                    if expanded {
                        if let Some(command) = &tool.command {
                            lines.extend(command.lines().enumerate().map(|(index, text)| {
                                Line::styled(
                                    format!("{}{text}", if index == 0 { "  $ " } else { "    " }),
                                    Style::default().fg(theme().muted),
                                )
                            }));
                        }
                        lines.extend(output_preview::bash_output_lines(app, tool));
                    }
                    bash_block_indices.push((tool.key.clone(), first_line..lines.len()));
                } else {
                    let (header, indent) = presentation::tool_header(tool);
                    hanging_indents.push((lines.len(), indent));
                    lines.push(header);
                    lines.extend(output_preview::preview_lines(app, tool, width));
                }
                for image in &tool.images {
                    image_line_indices.push((image.key.clone(), lines.len()));
                    push_inline_image_lines(&mut lines, image.path.as_str(), width);
                }
                if tool.images.is_empty() && (!tool.is_bash || expanded) {
                    lines.push(Line::default());
                }
            }
            TranscriptLine::ReferencedAnswer { key } => {
                space_transcript_blocks(&lines, &mut spacing);
                source_line_indices.push((key.clone(), lines.len()));
                let loaded = output_preview::assistant_answer_text(app, key);
                lines.extend(render_summary_lines(
                    loaded.as_deref().unwrap_or("Loading answer…"),
                    width,
                ));
                lines.push(Line::from(""));
            }
            TranscriptLine::RunFailure { key, reason } => {
                space_transcript_blocks(&lines, &mut spacing);
                source_line_indices.push((key.clone(), lines.len()));
                let loaded = output_preview::run_error_text(app, key);
                lines.extend(run_failure_lines(
                    loaded.as_deref().unwrap_or(reason),
                    width,
                ));
            }
            TranscriptLine::RunActivity { key, label } => {
                source_line_indices.push((key.clone(), lines.len()));
                lines.push(Line::styled(
                    format!("  · {label}"),
                    Style::default().fg(theme().muted),
                ));
            }
            TranscriptLine::User(text) => {
                space_transcript_blocks(&lines, &mut spacing);
                for (line, prefix_bytes) in user_prompt_lines(text) {
                    decorations.push((lines.len(), prefix_bytes));
                    hanging_indents.push((lines.len(), usize::from(PROMPT_TEXT_COLUMN)));
                    lines.push(line);
                }
                lines.push(Line::default());
            }
            _ => {
                let end = transcript[cursor..]
                    .iter()
                    .position(|item| {
                        matches!(
                            item,
                            TranscriptLine::User(_)
                                | TranscriptLine::Tool(_)
                                | TranscriptLine::RunFailure { .. }
                                | TranscriptLine::RunActivity { .. }
                                | TranscriptLine::ReferencedAnswer { .. }
                        )
                    })
                    .map_or(transcript.len(), |offset| cursor + offset);
                let block = transcript_to_lines(&transcript[cursor..end], width);
                if !block.is_empty() && !matches!(item, TranscriptLine::Supplement(_)) {
                    space_transcript_blocks(&lines, &mut spacing);
                }
                lines.extend(block);
                cursor = end;
                continue;
            }
        }
        cursor += 1;
    }
    if app.assistant_is_final
        || app
            .assistant_run_id
            .as_deref()
            .is_none_or(|id| runs::process_visible(app, id))
    {
        let live = build_assistant_live_lines(app, width);
        let continues_assistant = matches!(
            transcript.last(),
            Some(TranscriptLine::LiveAssistant {
                separator: false,
                ..
            })
        );
        if !live.is_empty() && !continues_assistant {
            space_transcript_blocks(&lines, &mut spacing);
        }
        lines.extend(live);
    }

    let mut byte = 0;
    let mut decorated = decorations.iter().peekable();
    let starts = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let start = byte;
            let prefix = decorated
                .next_if(|(line_index, _)| *line_index == index)
                .map_or(0, |(_, bytes)| *bytes);
            byte += line.to_string().len() + 1 - prefix;
            start
        })
        .collect::<Vec<_>>();
    let layout = Arc::new(text_layout::TextLayout::transcript(
        lines,
        width.saturating_sub(1),
        &hanging_indents,
        &decorations,
        &spacing,
    ));
    let source_rows = source_line_indices
        .into_iter()
        .map(|(key, index)| (key, layout.position(starts[index]).0 as u64))
        .collect();
    let total_rows = layout.height() as u64;
    let bash_block_rows = bash_block_indices
        .into_iter()
        .map(|(key, indices)| {
            let start = layout.position(starts[indices.start]).0 as u64;
            // End on the last command/output row, before any spacer for the next block.
            let last_byte = starts
                .get(indices.end)
                .map_or(layout.text.len(), |start| start - 1);
            let end = layout.position(last_byte).0 as u64 + 1;
            (key, start..end)
        })
        .collect();
    let images = image_line_indices
        .into_iter()
        .map(|(key, index)| TranscriptImagePlacement {
            key,
            row: layout.position(starts[index]).0 as u64,
        })
        .collect();
    TranscriptView {
        lines: layout.lines.clone(),
        line_start_rows: (0..total_rows).collect(),
        source_rows,
        bash_block_rows,
        images,
        total_rows,
        layout,
    }
}

fn space_transcript_blocks(lines: &[Line<'_>], spacing: &mut Vec<usize>) {
    if lines.last().is_some_and(|line| line.width() > 0) {
        spacing.push(lines.len());
    }
}

fn push_inline_image_lines(lines: &mut Vec<Line<'static>>, path: &str, width: u16) {
    lines.extend((0..INLINE_IMAGE_ROWS).map(|_| Line::from("")));
    lines.push(Line::from(vec![
        Span::styled("  Path: ", Style::default().fg(theme().muted)),
        Span::styled(
            fit_middle_columns(path, width.saturating_sub(9) as usize),
            Style::default().fg(theme().muted),
        ),
    ]));
    lines.push(Line::from(""));
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
    let mut tables = markdown_table::tables(text, in_code_block)
        .into_iter()
        .peekable();
    let mut offset = 0;
    let mut table_end = 0;
    for raw_line in text.split_inclusive('\n') {
        let start = offset;
        offset += raw_line.len();
        if start < table_end {
            continue;
        }
        let trimmed = raw_line.trim_end();
        if let Some(fence) = code_fence(trimmed) {
            code = !code;
            if let (true, Some(info)) = (code, fence) {
                lines.push(Line::from(Span::styled(
                    format!("  {info}"),
                    Style::default().fg(theme().muted),
                )));
            }
            continue;
        }
        if code {
            lines.push(Line::from(Span::styled(
                format!("  {trimmed}"),
                Style::default().bg(theme().code_bg),
            )));
            continue;
        }
        if tables
            .peek()
            .is_some_and(|table| (start..offset).contains(&table.range.start))
        {
            let table = tables.next().expect("table starting on this line");
            table_end = table.range.end;
            lines.extend(table.render(width.saturating_sub(1)));
            continue;
        }
        let heading_level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if let Some(heading) = (1..=6)
            .contains(&heading_level)
            .then(|| trimmed[heading_level..].strip_prefix(' '))
            .flatten()
        {
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
            let mut inline = render_inline_markdown(item);
            inline.insert(0, Span::styled("• ", Style::default().fg(theme().accent)));
            lines.push(Line::from(inline));
            continue;
        }
        lines.push(Line::from(render_inline_markdown(trimmed)));
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
pub(super) fn render_inline_markdown(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    let mut plain = String::new();
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
                if !bold.is_empty() {
                    spans.push(Span::styled(
                        bold.to_string(),
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
                if !code.is_empty() {
                    spans.push(Span::styled(
                        code.to_string(),
                        Style::default().fg(theme().inline_code_fg),
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

pub(super) fn input_display_offset(app: &App, byte: usize) -> usize {
    if app.secret_input_active() {
        app.input[..byte].graphemes(true).count() * '•'.len_utf8()
    } else {
        byte
    }
}

pub(super) fn input_source_offset(app: &App, byte: usize) -> usize {
    if app.secret_input_active() {
        app.input
            .grapheme_indices(true)
            .nth(byte / '•'.len_utf8())
            .map_or(app.input.len(), |(byte, _)| byte)
    } else {
        byte.min(app.input.len())
    }
}

pub(super) fn render_input(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.mouse_state.input_focused && mouse::editor_enabled(app);
    let border = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme().composer_border(focused));
    let inner = border.inner(area);
    frame.render_widget(border, area);
    let title_width = area.width.saturating_sub(4);
    if area.height >= 2 && title_width > 0 {
        let title = status_line(app, title_width.into());
        if !title.spans.is_empty() {
            let title_area = Rect::new(area.x + 2, area.bottom() - 1, title_width, 1);
            frame.render_widget(
                Paragraph::new(title).alignment(Alignment::Right),
                title_area,
            );
        }
    }
    let content = Rect::new(
        area.x.saturating_add(PROMPT_TEXT_COLUMN),
        inner.y,
        composer_text_width(area.width),
        inner.height,
    );
    let mut layout = composer_layout(app, content.width);
    let mut text_area = content;
    if layout.height().max(
        layout
            .caret(input_display_offset(app, clamped_cursor(app)))
            .0
            + 1,
    ) > content.height as usize
    {
        text_area.width = text_area.width.saturating_sub(1);
        layout = composer_layout(app, text_area.width);
    }
    let (row, column) = layout.caret(input_display_offset(app, clamped_cursor(app)));
    if !app.input_scroll_override {
        if row < app.input_scroll {
            app.input_scroll = row;
        } else if row >= app.input_scroll + text_area.height as usize {
            app.input_scroll = row
                .saturating_add(1)
                .saturating_sub(text_area.height as usize);
        }
    }
    let maximum = layout
        .height()
        .max(row + 1)
        .saturating_sub(text_area.height as usize);
    app.input_scroll = app.input_scroll.min(maximum);
    frame.render_widget(
        Paragraph::new(&USER_PROMPT_PREFIX[1..]).style(theme().composer_border(focused)),
        Rect::new(inner.x, inner.y, PROMPT_TEXT_COLUMN - 1, 1),
    );
    frame.render_widget(
        Paragraph::new(
            layout
                .lines
                .iter()
                .skip(app.input_scroll)
                .take(text_area.height as usize)
                .cloned()
                .collect::<Vec<_>>(),
        ),
        text_area,
    );
    if let Some(range) = input_selection_range(app) {
        paint_selection(
            frame.buffer_mut(),
            text_area,
            &layout,
            app.input_scroll,
            TextSelection {
                anchor: input_display_offset(app, range.start),
                head: input_display_offset(app, range.end),
            },
        );
    }
    if app.input.is_empty() {
        let placeholder = if let Some(prompt) = app.model_credential_prompt.as_ref() {
            Some(format!("API key for {}", prompt.provider_name))
        } else if app.secret_input_active() {
            Some("API key (masked)".into())
        } else if app.pending_question.is_some() {
            Some("Answer the pending question".into())
        } else {
            None
        };
        if let Some(placeholder) = placeholder {
            frame.render_widget(
                Paragraph::new(placeholder).style(Style::default().fg(theme().muted)),
                text_area,
            );
        }
    }
    if let Some(y) = row
        .checked_sub(app.input_scroll)
        .filter(|row| *row < text_area.height as usize)
    {
        let x = text_area
            .x
            .saturating_add(column as u16)
            .min(text_area.right().saturating_sub(1));
        let y = text_area.y + y as u16;
        if !app.secret_input_active()
            && app.pending_question.is_none()
            && input_selection_range(app).is_none()
        {
            if let Some(ghost) = command_completion_suffix(&app.input, app.selected_command) {
                frame.render_widget(
                    Paragraph::new(ghost).style(
                        Style::default()
                            .fg(theme().ghost)
                            .add_modifier(Modifier::DIM),
                    ),
                    Rect::new(x, y, text_area.right().saturating_sub(x), 1),
                );
            }
        }
        if mouse::editor_enabled(app) {
            frame.set_cursor_position((x, y));
        }
    }
    app.input_area = Some(text_area);
    app.input_layout = Some(layout);
}

pub(super) fn input_for_display(app: &App, input: &str) -> String {
    if app.secret_input_active() {
        "•".repeat(input.graphemes(true).count())
    } else {
        input.to_string()
    }
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

    if app.command_panel_open() {
        render_command_panel(frame, area, app);
        return;
    }

    let message = app.message.clone().or_else(|| {
        app.model_credential_prompt.as_ref().map(|prompt| {
            format!(
                "Enter the API key for {} · Esc to cancel",
                prompt.provider_name
            )
        })
    });
    if let Some(message) = message {
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::default().fg(theme().muted))
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
                .fg(theme().muted)
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
                picker_view::marker(index == selected),
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
                theme().selected_row()
            } else {
                Style::default()
            }),
        );
    }
    items.push(
        ListItem::new(Line::from(vec![
            picker_view::marker(selected == panel.workspaces.len()),
            Span::raw("Continue in the home directory"),
        ]))
        .style(if selected == panel.workspaces.len() {
            theme().selected_row()
        } else {
            Style::default().fg(theme().muted)
        }),
    );
    if let Some(notice) = panel.notice.as_deref() {
        items.push(ListItem::new(Line::from(Span::styled(
            fit_middle(notice, panel_area.width as usize),
            Style::default().fg(theme().muted),
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

pub(super) fn command_panel_area(area: Rect) -> Rect {
    let inset = PROMPT_TEXT_COLUMN - 2;
    Rect {
        x: area.x.saturating_add(inset),
        width: area.width.saturating_sub(inset).min(76),
        ..area
    }
}

pub(super) fn render_command_panel(frame: &mut Frame, area: Rect, app: &App) {
    if area.height < 1 {
        return;
    }

    let commands = matching_commands(app.input.as_str());
    let height = (commands.len() as u16).min(area.height).max(1);
    let panel_area = command_panel_area(Rect { height, ..area });
    let items = if commands.is_empty() {
        vec![ListItem::new(Line::from(vec![
            Span::styled("unknown", Style::default().fg(theme().muted)),
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
                        theme().selected_row()
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
        picker_view::marker(selected),
        Span::styled(
            format!("{:<width$}", command.name, width = COMMAND_NAME_WIDTH - 2),
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
        for option in question
            .options
            .iter()
            .skip(app.mouse_state.question_scroll)
            .take(panel_area.height.saturating_sub(3) as usize)
        {
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
    let content = picker_view::content(frame, area);
    app.panel_area = Some(area);
    let is_effort = app
        .model_panel
        .as_ref()
        .is_some_and(|panel| panel.stage != model_picker::Stage::Models);
    let title = if is_effort {
        format!(
            "Pick reasoning effort · {}",
            model_picker::selected_model(app).map_or("", |model| model.display_name.as_str())
        )
    } else {
        "Models".into()
    };
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

    picker_view::title(
        frame,
        Rect::new(chunks[0].x, chunks[0].y, chunks[0].width, 1),
        title,
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

    frame.render_widget(
        Paragraph::new(format!("Filter: {}", panel.query))
            .style(Style::default().fg(theme().muted)),
        Rect::new(chunks[0].x, chunks[0].y + 2, chunks[0].width, 1),
    );
    let selected_provider = panel
        .selected_provider
        .min(panel.providers.len().saturating_sub(1));
    let tab_labels = if is_effort {
        Vec::new()
    } else {
        panel
            .providers
            .iter()
            .map(|provider| {
                if provider.configured {
                    provider.name.clone()
                } else {
                    format!("{} (setup)", provider.name)
                }
            })
            .collect::<Vec<_>>()
    };
    let tab_widths = tab_labels
        .iter()
        .map(|label| label.chars().map(character_width).sum::<usize>())
        .collect::<Vec<_>>();
    let available_width = chunks[0].width as usize;
    let mut first_tab = 0usize;
    while first_tab < selected_provider
        && selected_provider < tab_widths.len()
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
            theme().selected_row()
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
            Paragraph::new(if app.pending_model_request.is_some() {
                "Applying…"
            } else if is_effort {
                "Type to filter · ↑↓ select · Enter apply · Esc back"
            } else {
                "Type to filter · Tab provider · ↑↓ select · Enter next · Esc back"
            })
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

    let rows = model_picker::rows(app);
    let selected_model = rows
        .iter()
        .position(|row| row.index == model_picker::selection(app))
        .unwrap_or(0);
    let list_area = picker_view::text_area(chunks[2], rows.len());
    let label_columns = usize::from(list_area.width.saturating_sub(4)).min(32);
    let items = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            picker_view::row(
                &row.label,
                row.description,
                label_columns,
                index == selected_model,
            )
        })
        .collect::<Vec<_>>();
    app.model_list_area = Some(chunks[2]);
    if items.is_empty() {
        app.model_list_offset = 0;
        frame.render_widget(
            Paragraph::new("No matching options.").style(Style::default().fg(theme().muted)),
            chunks[2],
        );
    } else {
        app.model_list_offset = picker_view::render_list(frame, list_area, items, selected_model);
    }

    if let Some(message) = app.message.as_deref() {
        let style = Style::default().fg(theme().muted);
        frame.render_widget(Paragraph::new(message).style(style), chunks[3]);
    }
    frame.render_widget(
        Paragraph::new(if app.pending_model_request.is_some() {
            "Applying…"
        } else if is_effort {
            "Type to filter · ↑↓ select · Enter apply · Esc back"
        } else {
            "Type to filter · Tab provider · ↑↓ select · Enter next · Esc back"
        })
        .style(Style::default().fg(theme().muted)),
        chunks[4],
    );
}

pub(super) fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let line = status_line(app, area.width as usize);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}

fn render_workspace_header(frame: &mut Frame, area: Rect, app: &App) {
    let style = Style::default().fg(theme().muted);
    let label = status_usage::context_label(app).unwrap_or_default();
    let context_width = (Line::raw(label.as_str()).width() as u16).min(area.width / 2);
    let [path_area, context_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(context_width)]).areas(area);
    frame.render_widget(
        Paragraph::new(fit_middle_columns(
            &app.session_cwd,
            path_area.width.saturating_sub(1) as usize,
        ))
        .style(style),
        path_area,
    );
    frame.render_widget(Paragraph::new(label).style(style), context_area);
}

fn render_shortcuts(frame: &mut Frame, area: Rect, app: &App) {
    let hints = if app.secret_input_active() {
        "Enter save   Esc back"
    } else if app.home_risk_panel.is_some() {
        "↑↓ select   Enter confirm   Esc back"
    } else if app.pending_question.is_some() {
        "Enter answer   Esc stop"
    } else if app.command_panel_open() {
        "↑↓ select   Enter open   Tab complete"
    } else if app.pending_esc_stop {
        "Esc again to stop"
    } else if has_active_agent_run(app) {
        "Esc Esc stop   Ctrl+\\ sessions   /help commands"
    } else {
        "Enter send   Shift+Enter newline   Ctrl+\\ sessions   /help commands"
    };
    frame.render_widget(
        Paragraph::new(hints).style(Style::default().fg(theme().muted)),
        area,
    );
}

pub(super) fn render_status_indicator(frame: &mut Frame, area: Rect, app: &mut App) {
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const STOP: &str = "[stop]";
    let elapsed = app.agent_run_started_at.map(|start| start.elapsed());
    let glyph = if app.pending_question.is_some()
        || matches!(
            app.process_state,
            RuntimeDisplayState::WaitingUser | RuntimeDisplayState::ConnectionLost
        ) {
        "·"
    } else {
        let tick = elapsed.map_or(0, |duration| {
            duration.as_millis() / STATUS_REDRAW_INTERVAL.as_millis()
        });
        SPINNER[(tick % SPINNER.len() as u128) as usize]
    };
    let style = Style::default().fg(theme().muted);
    let stop_width =
        if has_active_agent_run(app) && app.process_state != RuntimeDisplayState::ConnectionLost {
            STOP.len() as u16
        } else {
            0
        };
    let mut output = status_usage::output_label(app).unwrap_or_default();
    if Line::raw(output.as_str()).width() + stop_width as usize + 16 > area.width as usize {
        output.clear();
    }
    let output_width = if output.is_empty() {
        0
    } else {
        Line::raw(output.as_str()).width() as u16 + 2
    };
    let [phase_area, output_area, stop_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(output_width),
        Constraint::Length(stop_width),
    ])
    .areas(area);
    let header = status_header(app);
    let time = elapsed.map_or_else(String::new, |elapsed| {
        let gap = if header.is_empty() { "" } else { "  " };
        format!("{gap}{}", format_elapsed(elapsed))
    });
    let label_width = phase_area.width.saturating_sub(3 + time.len() as u16);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{glyph} "), style),
            Span::raw(fit_middle_columns(&header, label_width as usize)),
            Span::styled(time, style),
        ])),
        phase_area,
    );
    frame.render_widget(Paragraph::new(output).style(style), output_area);
    if stop_width == 0 {
        return;
    }
    let hovered = app
        .mouse_state
        .position
        .is_some_and(|position| stop_area.contains(position));
    let stop_style = if hovered {
        theme().hovered_row()
    } else {
        style
    };
    frame.render_widget(Paragraph::new(STOP).style(stop_style), stop_area);
    app.mouse_state.stop_area = Some(stop_area);
}

pub(super) fn status_header(app: &App) -> String {
    if app.process_state == RuntimeDisplayState::ConnectionLost {
        return "Connection lost".to_string();
    }
    if app.tool_protocol_error {
        return "Protocol error".to_string();
    }
    if app.pending_question.is_some() {
        return "Waiting for input".to_string();
    }
    if let StatusEasterEgg::Showing(egg) = app.status_easter_egg {
        return match egg {
            EasterEgg::Wired => "A faint signal crossed the Wired...".to_string(),
            EasterEgg::LongTunnel => "Emerging from the long tunnel...".to_string(),
            EasterEgg::Tachikoma => format!("Tachikoma ×{}", app.live_subagent_ids.len()),
            EasterEgg::KeepsakeClover => "That Keepsake Clover...".to_string(),
        };
    }
    match app.process_state {
        RuntimeDisplayState::Idle => "Idle",
        RuntimeDisplayState::ConnectionLost => "Connection lost",
        RuntimeDisplayState::PendingPhase => "",
        RuntimeDisplayState::Thinking => "Thinking...",
        RuntimeDisplayState::Responding => "Responding...",
        RuntimeDisplayState::ToolRunning => {
            return app
                .tool_projection
                .active_description()
                .map(str::to_owned)
                .or_else(|| status_inline(app))
                .unwrap_or_else(|| "Running tools...".into());
        }
        RuntimeDisplayState::Compacting => "Compacting...",
        RuntimeDisplayState::Waiting(WaitReason::Unspecified) => "Waiting...",
        RuntimeDisplayState::Waiting(WaitReason::Model) => "Waiting for model...",
        RuntimeDisplayState::Waiting(WaitReason::Tasks) => "Waiting for tasks...",
        RuntimeDisplayState::WaitingUser => "Waiting for input",
    }
    .to_string()
}

pub(super) fn status_inline(app: &App) -> Option<String> {
    app.active_tool_label
        .clone()
        .or_else(|| app.tool_projection.active_label())
}

pub(super) fn status_line(app: &App, width: usize) -> Line<'static> {
    let Some(model) = app.model_display.as_deref() else {
        return Line::default();
    };
    let effort = app
        .model_effort
        .as_deref()
        .map_or_else(String::new, |effort| format!(" ({effort})"));
    let model = fit_middle_columns(model, width.saturating_sub(effort.len() + 2));
    Line::styled(
        format!(" {model}{effort} "),
        Style::default().fg(theme().muted),
    )
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

fn run_failure_lines(text: &str, _width: u16) -> Vec<Line<'static>> {
    let mut lines = text
        .split('\n')
        .map(|line| Line::raw(line.to_owned()))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(Line::raw(""));
    }
    for (index, line) in lines.iter_mut().enumerate() {
        line.spans.insert(
            0,
            Span::styled(
                if index == 0 { "! " } else { "  " },
                Style::default().fg(ratatui::style::Color::Red),
            ),
        );
    }
    lines
}

fn user_prompt_lines(text: &str) -> impl Iterator<Item = (Line<'static>, usize)> + '_ {
    text.split('\n').enumerate().map(|(index, text)| {
        let prefix = if index == 0 {
            USER_PROMPT_PREFIX
        } else {
            USER_PROMPT_CONTINUATION
        };
        (
            Line::styled(format!("{prefix}{text}"), theme().user_prompt()),
            prefix.len(),
        )
    })
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
        match item.content() {
            TranscriptLine::RunItem { .. }
            | TranscriptLine::RunBoundary { .. }
            | TranscriptLine::RunActivity { .. } => {}
            TranscriptLine::ReferencedAnswer { .. } => {
                live_in_code_block = false;
                if emit {
                    lines.extend(render_summary_lines("Loading answer…", width));
                }
            }
            TranscriptLine::RunFailure { reason, .. } => {
                live_in_code_block = false;
                if emit {
                    lines.extend(run_failure_lines(reason, width));
                }
            }
            TranscriptLine::User(text) => {
                live_in_code_block = false;
                if emit {
                    lines.extend(user_prompt_lines(text).map(|(line, _)| line));
                    lines.push(Line::default());
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
                let style = Style::default().fg(theme().muted);
                let summary = if subagent.summary.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", subagent.summary)
                };
                lines.push(Line::from(vec![
                    Span::styled("  ", style),
                    Span::styled(
                        fit_middle(
                            &format!("{}{}", subagent.title, summary),
                            width.saturating_sub(2) as usize,
                        ),
                        style,
                    ),
                ]));
            }
            TranscriptLine::Reasoning { .. } => {}
            TranscriptLine::Supplement(text) => {
                live_in_code_block = false;
                if emit {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "  └ ",
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
                lines.push(presentation::tool_header(tool).0);
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
                        Span::styled("  Error: ", Style::default().fg(ratatui::style::Color::Red)),
                        Span::styled(text.clone(), Style::default().fg(theme().muted)),
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
                        fit_middle_columns(text, (width as usize).saturating_sub(4).max(1))
                    }
                    TextResultLine::Hidden(hidden) => format!("… {hidden} lines hidden"),
                })
                .collect::<Vec<_>>();
            push_text_result_block(lines, &rendered);
        }
        ToolResultBlock::Diff { path, rows, .. } => {
            if show_diff_path {
                lines.push(Line::from(vec![
                    Span::styled("  │ ", tool_result_style()),
                    Span::styled(path.clone(), tool_result_style()),
                ]));
            }
            let prefix = "  │ ";
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
    for text in result_lines {
        lines.push(Line::from(vec![
            Span::styled("  │ ", tool_result_style()),
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
