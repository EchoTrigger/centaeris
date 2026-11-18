use super::*;

const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(450);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Surface {
    Sessions,
    #[default]
    Main,
    Image,
    Models,
    Home,
    Question,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScrollTarget {
    Input,
    Transcript,
    Models,
    Commands,
    Question,
    Sessions,
}

#[derive(Clone, Copy)]
struct Scrollbar {
    area: Rect,
    total: usize,
    visible: usize,
    offset: usize,
    target: ScrollTarget,
}

struct HoverRegion {
    area: Rect,
    selected: bool,
}

struct BashSelection {
    key: String,
    last_click: Option<Instant>,
}

impl Scrollbar {
    fn thumb(self) -> (usize, usize) {
        let height = usize::from(self.area.height);
        let length = (height * self.visible / self.total.max(1)).clamp(1, height.max(1));
        let top = (height - length) * self.offset.min(self.total.saturating_sub(self.visible))
            / self.total.saturating_sub(self.visible).max(1);
        (top, length)
    }
    fn offset_at(self, row: u16, grab: usize) -> usize {
        let (_, length) = self.thumb();
        let available = usize::from(self.area.height).saturating_sub(length);
        let top = usize::from(row.saturating_sub(self.area.y))
            .saturating_sub(grab)
            .min(available);
        top * self.total.saturating_sub(self.visible) / available.max(1)
    }
}

pub(super) struct State {
    pub position: Option<Position>,
    pub input_focused: bool,
    pub question_scroll: usize,
    pub(super) stop_area: Option<Rect>,
    surface: Surface,
    bars: Vec<Scrollbar>,
    scrollbar_drag: Option<(Scrollbar, usize)>,
    last_press: Option<(Instant, Position, u8)>,
    bash_blocks: Vec<(Rect, String)>,
    bash_press: Option<(Position, String)>,
    bash_selection: Option<BashSelection>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            position: None,
            input_focused: true,
            question_scroll: 0,
            stop_area: None,
            surface: Surface::Main,
            bars: Vec::new(),
            scrollbar_drag: None,
            last_press: None,
            bash_blocks: Vec::new(),
            bash_press: None,
            bash_selection: None,
        }
    }
}

impl State {
    fn bash_key_at(&self, position: Position) -> Option<&str> {
        self.bash_blocks
            .iter()
            .find(|(area, _)| area.contains(position))
            .map(|(_, key)| key.as_str())
    }
}

fn surface(app: &App) -> Surface {
    if app.image_preview.is_some() {
        Surface::Image
    } else if app.session_picker.is_some() {
        Surface::Sessions
    } else if app.model_panel.is_some() {
        Surface::Models
    } else if app.home_risk_panel.is_some() {
        Surface::Home
    } else if app.pending_question.is_some() {
        Surface::Question
    } else {
        Surface::Main
    }
}

pub(super) fn editor_enabled(app: &App) -> bool {
    matches!(surface(app), Surface::Main | Surface::Question)
}

pub(super) fn cancel_capture(app: &mut App) {
    app.mouse_drag = None;
    app.mouse_state.scrollbar_drag = None;
    app.mouse_state.last_press = None;
    app.mouse_state.bash_press = None;
    app.mouse_state.bash_selection = None;
    if let Some(ImagePreview::Pixels(preview)) = app.image_preview.as_mut() {
        preview.drag = None;
    }
}

pub(super) fn resize(app: &mut App) {
    cancel_capture(app);
    app.mouse_state.bars.clear();
    app.mouse_state.bash_blocks.clear();
    app.mouse_state.stop_area = None;
    session_picker::invalidate_geometry(app);
}

pub(super) fn set_bash_blocks(app: &mut App, area: Rect, blocks: &[(String, Range<u64>)]) {
    app.mouse_state.bash_blocks.clear();
    let scroll = app.transcript_scroll;
    let viewport_end = scroll.saturating_add(u64::from(area.height));
    for (key, rows) in blocks {
        let start = rows.start.max(scroll);
        let end = rows.end.min(viewport_end);
        if start < end && area.width > 0 {
            let hit = Rect::new(
                area.x,
                area.y + (start - scroll) as u16,
                area.width,
                (end - start) as u16,
            );
            app.mouse_state.bash_blocks.push((hit, key.clone()));
        }
    }
}

pub(super) fn render_bash_selection(frame: &mut Frame, app: &App) {
    if let Some(selection) = app.mouse_state.bash_selection.as_ref() {
        if let Some((area, _)) = app
            .mouse_state
            .bash_blocks
            .iter()
            .find(|(_, key)| *key == selection.key)
        {
            frame.buffer_mut().set_style(
                *area,
                Style {
                    bg: theme().user_prompt().bg,
                    ..Style::default()
                },
            );
        }
    }
}

pub(super) fn copy(app: &mut App, text: &str) {
    if !text.is_empty() {
        if let Err(error) = app.clipboard.store(text) {
            show_message(app, error);
        }
    }
}

pub(super) fn set_text_layout(
    app: &mut App,
    layout: std::sync::Arc<text_layout::TextLayout>,
    scroll: usize,
) {
    if let (Some(previous), Some(selection)) = (app.text_layout.as_ref(), app.transcript_selection)
    {
        let end = selection.range().end;
        if previous.text.get(..end) != layout.text.get(..end) {
            app.transcript_selection = None;
            app.mouse_drag = None;
        }
    }
    app.text_layout = Some(layout);
    app.text_scroll = scroll;
}

pub(super) fn embedded_area(app: &App) -> Option<Rect> {
    app.panel_area.map(|outer| {
        if app.command_panel_open() {
            command_panel_area(outer)
        } else {
            Rect::new(
                outer.x.saturating_add(2),
                outer.y,
                outer.width.saturating_sub(2).min(94),
                outer.height,
            )
        }
    })
}

pub(super) fn prepare_frame(app: &mut App) {
    app.mouse_state.bash_blocks.clear();
    app.mouse_state.stop_area = None;
    let current = surface(app);
    if current != app.mouse_state.surface {
        cancel_capture(app);
        app.transcript_selection = None;
        app.mouse_state.surface = current;
        app.mouse_state.input_focused = editor_enabled(app);
    }
}

fn add_list(
    bars: &mut Vec<Scrollbar>,
    hover: &mut Vec<HoverRegion>,
    area: Rect,
    rows: Range<usize>,
    selected: Option<usize>,
    target: ScrollTarget,
) {
    let offset = rows.start;
    let total = rows.end;
    let text_width = picker_view::text_area(area, total).width;
    for row in 0..usize::from(area.height).min(total.saturating_sub(offset)) {
        hover.push(HoverRegion {
            area: Rect::new(area.x, area.y + row as u16, text_width, 1),
            selected: selected == Some(offset + row),
        });
    }
    if total > area.height as usize && area.height > 0 {
        bars.push(Scrollbar {
            area: Rect::new(area.right().saturating_sub(1), area.y, 1, area.height),
            total,
            visible: area.height as usize,
            offset,
            target,
        });
    }
}

pub(super) fn decorate(frame: &mut Frame, app: &mut App) {
    let current = surface(app);
    let mut bars = Vec::new();
    let mut hover = Vec::new();
    match current {
        Surface::Main => {
            if let Some(area) = app.transcript_viewport {
                let total = app.text_layout.as_ref().map_or(0, |layout| layout.height());
                if total > area.height as usize {
                    bars.push(Scrollbar {
                        area: Rect::new(area.right().saturating_sub(1), area.y, 1, area.height),
                        total,
                        visible: area.height as usize,
                        offset: app.transcript_scroll as usize,
                        target: ScrollTarget::Transcript,
                    });
                }
            }
        }
        Surface::Models => {
            hover.extend(
                app.model_provider_hit_regions
                    .iter()
                    .enumerate()
                    .map(|(index, area)| HoverRegion {
                        area: *area,
                        selected: app
                            .model_panel
                            .as_ref()
                            .is_some_and(|panel| panel.selected_provider == index),
                    }),
            );
            if let Some(area) = app.model_list_area {
                let rows = model_picker::rows(app);
                let selected = rows
                    .iter()
                    .position(|row| row.index == model_picker::selection(app))
                    .unwrap_or(0);
                add_list(
                    &mut bars,
                    &mut hover,
                    area,
                    app.model_list_offset..rows.len(),
                    Some(selected),
                    ScrollTarget::Models,
                );
            }
        }
        Surface::Sessions => {
            if let Some((area, rows, selected)) = session_picker::list_view(app) {
                add_list(
                    &mut bars,
                    &mut hover,
                    area,
                    rows,
                    Some(selected),
                    ScrollTarget::Sessions,
                );
            }
        }
        _ => {}
    }
    if let Some(area) = embedded_area(app) {
        if current == Surface::Main && app.command_panel_open() {
            let count = matching_commands(&app.input).len();
            let offset = app
                .selected_command
                .saturating_sub(area.height.saturating_sub(1) as usize);
            add_list(
                &mut bars,
                &mut hover,
                area,
                offset..count,
                Some(app.selected_command.min(count.saturating_sub(1))),
                ScrollTarget::Commands,
            );
        } else if current == Surface::Question {
            let body = Rect::new(
                area.x,
                area.y + 2,
                area.width,
                area.height.saturating_sub(3),
            );
            add_list(
                &mut bars,
                &mut hover,
                body,
                app.mouse_state.question_scroll
                    ..app
                        .pending_question
                        .as_ref()
                        .map_or(0, |question| question.options.len()),
                None,
                ScrollTarget::Question,
            );
        } else if current == Surface::Home {
            for row in 0..app
                .home_risk_panel
                .as_ref()
                .map_or(0, |panel| panel.workspaces.len() + 1)
                .min(area.height.saturating_sub(2) as usize)
            {
                hover.push(HoverRegion {
                    area: Rect::new(area.x, area.y + 2 + row as u16, area.width, 1),
                    selected: app
                        .home_risk_panel
                        .as_ref()
                        .is_some_and(|panel| panel.selected == row),
                });
            }
        }
    }
    if editor_enabled(app) {
        if let (Some(area), Some(layout)) = (app.input_area, app.input_layout.as_ref()) {
            if layout.height().max(
                layout
                    .caret(input_display_offset(app, clamped_cursor(app)))
                    .0
                    + 1,
            ) > area.height as usize
                && area.height > 0
            {
                bars.push(Scrollbar {
                    area: Rect::new(area.right(), area.y, 1, area.height),
                    total: layout.height().max(
                        layout
                            .caret(input_display_offset(app, clamped_cursor(app)))
                            .0
                            + 1,
                    ),
                    visible: area.height as usize,
                    offset: app.input_scroll,
                    target: ScrollTarget::Input,
                });
            }
        }
    }
    for bar in &bars {
        let (top, length) = bar.thumb();
        for row in 0..bar.area.height {
            if let Some(cell) = frame.buffer_mut().cell_mut((bar.area.x, bar.area.y + row)) {
                cell.set_symbol(if (top..top + length).contains(&(row as usize)) {
                    "█"
                } else {
                    "│"
                })
                .set_style(Style::default().fg(theme().muted));
            }
        }
    }
    let highlight = |selected| {
        if current == Surface::Sessions {
            session_picker::row_highlight(selected)
        } else if selected {
            theme().selected_row()
        } else {
            theme().hovered_row()
        }
    };
    for region in hover.iter().filter(|region| region.selected) {
        paint_row(frame, region.area, highlight(true));
    }
    if let Some(position) = app.mouse_state.position {
        if let Some(region) = hover.iter().find(|region| region.area.contains(position)) {
            if !region.selected {
                paint_row(frame, region.area, highlight(false));
            }
        }
    }
    app.mouse_state.bars = bars;
}

fn paint_row(frame: &mut Frame, area: Rect, style: Style) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_style(style);
            }
        }
    }
}

fn apply_scroll(app: &mut App, bar: Scrollbar, offset: usize) {
    let selected = if offset == 0 {
        0
    } else {
        offset + bar.visible.saturating_sub(1)
    };
    match bar.target {
        ScrollTarget::Input => {
            app.input_scroll = offset;
            app.input_scroll_override = true;
        }
        ScrollTarget::Transcript => {
            app.transcript_scroll = (offset as u64).min(app.transcript_max_scroll);
            app.transcript_follow_bottom = app.transcript_scroll == app.transcript_max_scroll;
        }
        ScrollTarget::Models => {
            if let Some(row) = model_picker::rows(app).get(selected) {
                model_picker::select(app, row.index);
            }
        }
        ScrollTarget::Commands => app.selected_command = selected,
        ScrollTarget::Question => app.mouse_state.question_scroll = offset,
        ScrollTarget::Sessions => session_picker::scroll_to(app, selected),
    }
}

fn panel_wheel(app: &mut App, up: bool, position: Position) {
    let key = KeyEvent::new(
        if up { KeyCode::Up } else { KeyCode::Down },
        KeyModifiers::NONE,
    );
    match surface(app) {
        Surface::Models => {
            handle_model_panel_key(key, app);
        }
        Surface::Home => {
            handle_home_risk_key(key, app);
        }
        Surface::Question => {
            let visible =
                embedded_area(app).map_or(0, |area| area.height.saturating_sub(3) as usize);
            let max = app
                .pending_question
                .as_ref()
                .map_or(0, |question| question.options.len().saturating_sub(visible));
            app.mouse_state.question_scroll = if up {
                app.mouse_state.question_scroll.saturating_sub(1)
            } else {
                (app.mouse_state.question_scroll + 1).min(max)
            };
        }
        Surface::Main
            if app.panel_area.is_some_and(|area| area.contains(position))
                && app.command_panel_open() =>
        {
            move_command_selection(app, if up { -1 } else { 1 })
        }
        _ => {}
    }
}

pub(super) fn handle(app: &mut App, event: MouseEvent) {
    let current = surface(app);
    if current != app.mouse_state.surface {
        cancel_capture(app);
        app.mouse_state.bars.clear();
        app.mouse_state.surface = current;
    }
    let position = Position::new(event.column, event.row);
    app.mouse_state.position = Some(position);
    if current == Surface::Image {
        image_mouse(app, event);
        return;
    }
    if let Some((bar, grab)) = app.mouse_state.scrollbar_drag {
        if matches!(
            event.kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        ) {
            apply_scroll(app, bar, bar.offset_at(event.row, grab));
        }
        if matches!(event.kind, MouseEventKind::Up(_)) {
            app.mouse_state.scrollbar_drag = None;
        }
        return;
    }
    if event.kind == MouseEventKind::Down(MouseButton::Left) {
        app.mouse_state.bash_press = (current == Surface::Main)
            .then(|| app.mouse_state.bash_key_at(position))
            .flatten()
            .map(|key| (position, key.to_owned()));
        if app.mouse_state.bash_press.is_none() {
            app.mouse_state.bash_selection = None;
        }
        if matches!(current, Surface::Main | Surface::Question)
            && !app.command_panel_open()
            && !app.secret_input_active()
            && has_active_agent_run(app)
            && app
                .mouse_state
                .stop_area
                .is_some_and(|area| area.contains(position))
        {
            app.mouse_drag = None;
            app.pending_esc_stop = false;
            if let Err(error) = stop_controlled_tasks(app, "tui_user_stop") {
                show_message(app, error);
            }
            return;
        }
        if let Some(bar) = app
            .mouse_state
            .bars
            .iter()
            .find(|bar| bar.area.contains(position))
            .copied()
        {
            let (top, length) = bar.thumb();
            let row = event.row.saturating_sub(bar.area.y) as usize;
            let grab = if (top..top + length).contains(&row) {
                row - top
            } else {
                length / 2
            };
            app.mouse_drag = None;
            app.mouse_state.scrollbar_drag = Some((bar, grab));
            apply_scroll(app, bar, bar.offset_at(event.row, grab));
            return;
        }
    }
    if current == Surface::Sessions {
        session_picker::mouse(app, event);
        return;
    }
    let input = editor_enabled(app) && app.input_area.is_some_and(|area| area.contains(position));
    let body = current == Surface::Main
        && app
            .transcript_area
            .is_some_and(|area| area.contains(position));
    if matches!(
        event.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        app.mouse_state.bash_press = None;
        if let Some(selection) = app.mouse_state.bash_selection.as_mut() {
            selection.last_click = None;
        }
        let up = event.kind == MouseEventKind::ScrollUp;
        if let Some(bar) = app
            .mouse_state
            .bars
            .iter()
            .find(|bar| bar.area.contains(position))
            .copied()
        {
            let offset = if up {
                bar.offset.saturating_sub(3)
            } else {
                (bar.offset + 3).min(bar.total.saturating_sub(bar.visible))
            };
            apply_scroll(app, bar, offset);
            return;
        }
        if input {
            let max = app.input_layout.as_ref().map_or(0, |layout| {
                layout
                    .height()
                    .max(
                        layout
                            .caret(input_display_offset(app, clamped_cursor(app)))
                            .0
                            + 1,
                    )
                    .saturating_sub(app.input_area.map_or(0, |area| area.height as usize))
            });
            app.input_scroll = if up {
                app.input_scroll.saturating_sub(3)
            } else {
                (app.input_scroll + 3).min(max)
            };
            app.input_scroll_override = true;
        } else if body {
            scroll_transcript(app, if up { -3 } else { 3 });
        } else if app.panel_area.is_some_and(|area| area.contains(position)) {
            panel_wheel(app, up, position);
        }
        return;
    }
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.mouse_drag = None;
            app.mouse_state.input_focused = input;
            if input {
                match open_draft_image_preview(app, event.column, event.row) {
                    Ok(true) => return,
                    Err(error) => {
                        show_message(app, error);
                        return;
                    }
                    _ => {}
                }
            } else if app.panel_area.is_some_and(|area| area.contains(position)) {
                app.transcript_selection = None;
                app.input_selection_anchor = None;
                handle_panel_click(event, app);
                return;
            }
            if !input && !body {
                return;
            }
            let now = Instant::now();
            let clicks = if app.mouse_state.bash_press.is_some() {
                app.mouse_state.last_press = None;
                1
            } else {
                let clicks = app
                    .mouse_state
                    .last_press
                    .filter(|(time, at, _)| {
                        *at == position && now.duration_since(*time) < MULTI_CLICK_INTERVAL
                    })
                    .map_or(1, |(_, _, count)| count % 3 + 1);
                app.mouse_state.last_press = Some((now, position, clicks));
                clicks
            };
            let byte = if clicks > 1 {
                let (area, layout, scroll) = if input {
                    (app.input_area, app.input_layout.as_ref(), app.input_scroll)
                } else {
                    (
                        app.transcript_area,
                        app.text_layout.as_deref(),
                        app.text_scroll,
                    )
                };
                area.zip(layout).map(|(area, layout)| {
                    let byte = layout.byte_under(
                        scroll + event.row.saturating_sub(area.y) as usize,
                        event.column.saturating_sub(area.x) as usize,
                    );
                    if input {
                        input_source_offset(app, byte)
                    } else {
                        byte
                    }
                })
            } else if input {
                raw_input_byte_from_mouse(app, event.column, event.row)
            } else {
                transcript_point_from_mouse(app, event.column, event.row)
            };
            if let Some(byte) = byte {
                if input {
                    app.transcript_selection = None;
                    app.input_scroll_override = false;
                    if clicks == 1 {
                        app.input_cursor =
                            input_byte_from_mouse(app, event.column, event.row).unwrap_or(byte);
                        app.input_selection_anchor = Some(app.input_cursor);
                        app.mouse_drag = Some(MouseDrag::Input);
                    } else {
                        let layout = text_layout::TextLayout::plain(&app.input, u16::MAX);
                        let selected = layout.semantic_selection(byte, clicks == 3);
                        app.input_selection_anchor = Some(selected.anchor);
                        app.input_cursor = selected.head;
                        copy_input(app);
                    }
                } else {
                    app.input_selection_anchor = None;
                    app.transcript_selection = Some(if clicks == 1 {
                        text_layout::Selection {
                            anchor: byte,
                            head: byte,
                        }
                    } else {
                        app.text_layout
                            .as_ref()
                            .expect("body layout")
                            .semantic_selection(byte, clicks == 3)
                    });
                    if clicks == 1 {
                        app.mouse_drag = Some(MouseDrag::Transcript);
                    } else {
                        copy_body(app);
                    }
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) if complete_bash_click(app, position) => {}
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
            if app.mouse_state.bash_press.take().is_some() {
                app.mouse_state.bash_selection = None;
            }
            match app.mouse_drag {
                Some(MouseDrag::Input) => {
                    if let Some(byte) = input_byte_from_mouse(app, event.column, event.row) {
                        app.input_cursor = byte;
                    }
                }
                Some(MouseDrag::Transcript) => {
                    if let Some(byte) = transcript_point_from_mouse(app, event.column, event.row) {
                        if let Some(selection) = app.transcript_selection.as_mut() {
                            selection.head = byte;
                        }
                    }
                }
                None => {}
            }
            if matches!(event.kind, MouseEventKind::Up(_)) {
                match app.mouse_drag.take() {
                    Some(MouseDrag::Input) => copy_input(app),
                    Some(MouseDrag::Transcript) => copy_body(app),
                    None => {}
                }
            }
        }
        _ => {}
    }
}

fn complete_bash_click(app: &mut App, position: Position) -> bool {
    let Some((pressed, key)) = app.mouse_state.bash_press.take() else {
        return false;
    };
    if pressed != position || app.mouse_state.bash_key_at(position) != Some(key.as_str()) {
        app.mouse_state.bash_selection = None;
        return false;
    }

    let now = Instant::now();
    let toggle = app
        .mouse_state
        .bash_selection
        .as_ref()
        .is_some_and(|selection| {
            selection.key == key
                && selection
                    .last_click
                    .is_some_and(|last| now.duration_since(last) < MULTI_CLICK_INTERVAL)
        });
    if toggle {
        if app.expanded_bash_tools.remove(&key) {
            app.output_preview.remove(&key);
        } else {
            app.expanded_bash_tools.insert(key.clone());
            app.transcript_follow_bottom = false;
        }
        invalidate_transcript_layout(app);
    }
    app.mouse_state.bash_selection = Some(BashSelection {
        key,
        last_click: (!toggle).then_some(now),
    });
    app.mouse_drag = None;
    app.transcript_selection = None;
    app.input_selection_anchor = None;
    true
}

pub(super) fn drag_scroll_tick(app: &mut App) -> bool {
    if surface(app) != app.mouse_state.surface {
        cancel_capture(app);
        return false;
    }
    let Some(position) = app.mouse_state.position else {
        return false;
    };
    let (area, offset, maximum) = match app.mouse_drag {
        Some(MouseDrag::Input) => {
            let Some(area) = app.input_area else {
                return false;
            };
            let maximum = app.input_layout.as_ref().map_or(0, |layout| {
                layout
                    .height()
                    .max(
                        layout
                            .caret(input_display_offset(app, clamped_cursor(app)))
                            .0
                            + 1,
                    )
                    .saturating_sub(area.height as usize)
            });
            (area, app.input_scroll, maximum)
        }
        Some(MouseDrag::Transcript) if surface(app) == Surface::Main => {
            let Some(area) = app.transcript_area else {
                return false;
            };
            (
                area,
                app.transcript_scroll as usize,
                app.transcript_max_scroll as usize,
            )
        }
        _ => return false,
    };
    let next = if position.y < area.y {
        offset.saturating_sub(1)
    } else if position.y >= area.bottom() {
        (offset + 1).min(maximum)
    } else {
        return false;
    };
    if next == offset {
        return false;
    }
    if app.mouse_drag == Some(MouseDrag::Input) {
        app.input_scroll = next;
        app.input_scroll_override = true;
        if let Some(byte) = input_byte_from_mouse(app, position.x, position.y) {
            app.input_cursor = byte;
        }
    } else {
        app.transcript_scroll = next as u64;
        app.text_scroll = next;
        app.transcript_follow_bottom = app.transcript_scroll == app.transcript_max_scroll;
        if let Some(byte) = transcript_point_from_mouse(app, position.x, position.y) {
            if let Some(selection) = app.transcript_selection.as_mut() {
                selection.head = byte;
            }
        }
    }
    true
}

fn copy_input(app: &mut App) {
    if !app.secret_input_active() {
        if let Some(range) = input_selection_range(app) {
            let text = app.input[range].to_owned();
            copy(app, &text);
        }
    }
}
fn copy_body(app: &mut App) {
    if let Some(text) = selected_transcript_text(app) {
        copy(app, &text);
    }
}

fn image_mouse(app: &mut App, event: MouseEvent) {
    let position = Position::new(event.column, event.row);
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(ImagePreview::Pixels(preview)) = app.image_preview.as_mut() {
                if preview.image_area.contains(position) {
                    preview.drag = Some(ImagePreviewDrag {
                        start: position,
                        image_area: preview.image_area,
                        view: preview.view,
                    });
                    return;
                }
            }
            if app
                .image_preview_area
                .is_some_and(|area| !area.contains(position))
            {
                close_image_preview(app);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(ImagePreview::Pixels(preview)) = app.image_preview.as_mut() {
                finish_image_preview_drag(preview, position);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if let Some(ImagePreview::Pixels(preview)) = app.image_preview.as_mut() {
                if preview.image_area.contains(position) {
                    zoom_image_preview(
                        preview,
                        if event.kind == MouseEventKind::ScrollUp {
                            1
                        } else {
                            -1
                        },
                        Some(position),
                    );
                }
            }
        }
        _ => {}
    }
}
