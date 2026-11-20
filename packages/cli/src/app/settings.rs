use super::*;
use ratatui::layout::{Constraint, Flex, Layout};
use ratatui::widgets::{Block, Padding};
use serde::{Deserialize, Serialize};
use std::path::Path;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const SCHEMA_VERSION: u32 = 1;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredPreferences {
    schema_version: u32,
    dashboard_preview: bool,
}

pub(super) struct Preferences {
    dashboard_preview: bool,
    path: Option<PathBuf>,
    load_error: Option<String>,
    pending: Option<mpsc::Receiver<Result<bool, String>>>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            dashboard_preview: true,
            path: None,
            load_error: None,
            pending: None,
        }
    }
}

impl Preferences {
    pub(super) fn load(path: PathBuf) -> Self {
        let result = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<StoredPreferences>(&bytes)
                .map_err(|error| format!("Invalid UI preferences: {error}"))
                .and_then(|stored| {
                    if stored.schema_version == SCHEMA_VERSION {
                        Ok(stored.dashboard_preview)
                    } else {
                        Err("Unsupported UI preferences schema".into())
                    }
                }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(format!("Read UI preferences failed: {error}")),
        };
        Self {
            dashboard_preview: result.as_ref().copied().unwrap_or(true),
            path: Some(path),
            load_error: result.err(),
            pending: None,
        }
    }

    pub(super) fn dashboard_preview(&self) -> bool {
        self.dashboard_preview
    }

    fn save_dashboard_preview(&mut self, enabled: bool) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(error.clone());
        }
        if self.pending.is_some() {
            return Err("Saving UI preferences…".into());
        }
        if enabled == self.dashboard_preview {
            return Ok(());
        }
        let path = self
            .path
            .clone()
            .ok_or("UI preferences path is unavailable")?;
        let bytes = serde_json::to_vec_pretty(&StoredPreferences {
            schema_version: SCHEMA_VERSION,
            dashboard_preview: enabled,
        })
        .map_err(|error| error.to_string())?;
        let (send, receive) = mpsc::channel();
        thread::Builder::new()
            .name("centaeris-save-ui-preferences".into())
            .spawn(move || {
                let result =
                    centaeris_cli_runtime::write_file_atomically(&path, &bytes, "UI preferences")
                        .map(|()| enabled);
                let _ = send.send(result);
            })
            .map_err(|error| error.to_string())?;
        self.pending = Some(receive);
        Ok(())
    }

    fn poll(&mut self) -> Option<Result<(), String>> {
        let result = match self.pending.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("UI preferences save ended early".into()),
        };
        self.pending = None;
        Some(result.map(|enabled| self.dashboard_preview = enabled))
    }

    pub(super) fn finish_write(&mut self) -> Result<(), String> {
        if let Some(response) = self.pending.take() {
            self.dashboard_preview = response
                .recv()
                .map_err(|_| "UI preferences save ended early".to_string())??;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingAction {
    Placeholder,
    DashboardPreview,
    BashPath,
    DefaultModel,
    Hints,
}

#[derive(Clone, Copy)]
struct Setting {
    label: &'static str,
    action: SettingAction,
}

const fn blank(label: &'static str) -> Setting {
    Setting {
        label,
        action: SettingAction::Placeholder,
    }
}

const SECTIONS: &[(&str, &[Setting])] = &[
    (
        "Appearance",
        &[
            blank("Compact mode"),
            blank("Default screen mode"),
            blank("Show timestamps"),
            blank("Timeline sidebar"),
            Setting {
                label: "Dashboard preview",
                action: SettingAction::DashboardPreview,
            },
            blank("Snap prompt to top on send"),
            blank("Disable Vim input mode"),
            blank("Vim scrollback navigation"),
            blank("Theme"),
            blank("Auto dark theme"),
            blank("Auto light theme"),
            blank("Render Mermaid diagrams"),
            blank("Show thinking blocks"),
            blank("Max thoughts width"),
            blank("Respect manual folds"),
            blank("Group tool calls"),
            blank("Collapsed edit blocks"),
            blank("Match display refresh rate"),
        ],
    ),
    (
        "Mouse",
        &[
            blank("Scroll speed"),
            blank("Scroll input"),
            blank("Scroll lines"),
            blank("Invert scroll"),
            blank("Text selection"),
        ],
    ),
    (
        "Editor & Input",
        &[
            blank("Combine queued prompts"),
            blank("Follow-up behavior"),
            blank("Confirm before rewind"),
            blank("Multiline"),
            blank("Prompt suggestions"),
            blank("Voice shortcut"),
            blank("Voice capture"),
            blank("Voice language"),
        ],
    ),
    (
        "Agent & Approval",
        &[
            blank("Permission mode"),
            blank("Remember tool approvals"),
            blank("Default selected permission"),
            blank("Ask-Question timeout"),
            blank("Plan mode"),
        ],
    ),
    ("Privacy", &[blank("Coding data, retention, and training")]),
    (
        "Models",
        &[
            Setting {
                label: "Default model",
                action: SettingAction::DefaultModel,
            },
            blank("Subagent model inheritance"),
            blank("Fork secondary model"),
        ],
    ),
    (
        "Advanced",
        &[
            blank("Show tips"),
            Setting {
                label: "Show contextual hints",
                action: SettingAction::Hints,
            },
            blank("Auto-update"),
            blank("Hunk tracker"),
            Setting {
                label: "Bash path",
                action: SettingAction::BashPath,
            },
        ],
    ),
];

const HINTS: &[Setting] = &[
    blank("Undo"),
    blank("Plan mode"),
    blank("Image input"),
    blank("Send now"),
    blank("Small screen"),
    blank("Word select"),
    blank("Copy and export"),
    blank("SSH wrap"),
];

#[derive(Clone, Copy)]
enum Row {
    Header(&'static str),
    Setting(Setting),
}

#[derive(Default)]
struct Field {
    text: String,
    cursor: usize,
}

impl Field {
    fn new(text: String) -> Self {
        Self {
            cursor: text.len(),
            text,
        }
    }

    fn insert(&mut self, text: &str) {
        let value: String = text.chars().filter(|ch| !ch.is_control()).collect();
        self.text.insert_str(self.cursor, &value);
        self.cursor += value.len();
        // Pasting a combining mark or joiner may merge with the next grapheme.
        self.cursor = self
            .text
            .grapheme_indices(true)
            .find_map(|(byte, _)| (byte >= self.cursor).then_some(byte))
            .unwrap_or(self.text.len());
    }

    fn edit(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Left => self.cursor = editing::previous(&self.text, self.cursor),
            KeyCode::Right => self.cursor = editing::next(&self.text, self.cursor),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace if self.cursor > 0 => {
                let start = editing::previous(&self.text, self.cursor);
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text
                    .drain(self.cursor..editing::next(&self.text, self.cursor));
            }
            KeyCode::Char(ch) if !ch.is_control() && editing::accepts_text(key) => {
                self.insert(ch.encode_utf8(&mut [0; 4]));
            }
            _ => return false,
        }
        true
    }
}

#[derive(Default)]
struct Browse {
    filter: Field,
    selected: usize,
}

enum Mode {
    Browse,
    Filter,
    Bash(Field),
    Reset(SettingAction),
}

struct BashRequest {
    response: RuntimeResponse,
    saving: bool,
}

pub(super) struct Panel {
    browse: Browse,
    parent: Option<Browse>,
    mode: Mode,
    bash_path: Option<String>,
    bash_loaded: bool,
    automatic_bash: Option<PathBuf>,
    bash_detection: Option<mpsc::Receiver<Option<PathBuf>>>,
    bash_request: Option<BashRequest>,
    model_requested: bool,
    rows: Vec<Row>,
    hits: Vec<(Rect, usize)>,
    filter_area: Rect,
    notice: Option<String>,
}

impl Panel {
    fn refresh_rows(&mut self) {
        let query = self.browse.filter.text.to_ascii_lowercase();
        let matches = |label: &str, group: &str| {
            query.split_whitespace().all(|word| {
                label.to_ascii_lowercase().contains(word)
                    || group.to_ascii_lowercase().contains(word)
            })
        };
        let mut rows = Vec::new();
        if self.parent.is_some() {
            rows.extend(
                HINTS
                    .iter()
                    .copied()
                    .filter(|setting| matches(setting.label, ""))
                    .map(Row::Setting),
            );
        } else {
            for (group, settings) in SECTIONS {
                let mut settings = settings
                    .iter()
                    .copied()
                    .filter(|setting| matches(setting.label, group))
                    .peekable();
                if settings.peek().is_some() {
                    rows.push(Row::Header(group));
                    rows.extend(settings.map(Row::Setting));
                }
            }
        }
        self.browse.selected = rows
            .iter()
            .position(|row| matches!(row, Row::Setting(_)))
            .unwrap_or(0);
        self.rows = rows;
        self.hits.clear();
        self.model_requested = false;
    }

    fn move_selection(&mut self, delta: i32) {
        let mut selected = self.browse.selected;
        for _ in 0..delta.unsigned_abs() {
            let next = if delta < 0 {
                self.rows[..selected]
                    .iter()
                    .rposition(|row| matches!(row, Row::Setting(_)))
            } else {
                self.rows
                    .iter()
                    .enumerate()
                    .skip(selected + 1)
                    .find_map(|(index, row)| matches!(row, Row::Setting(_)).then_some(index))
            };
            let Some(next) = next else { break };
            selected = next;
        }
        self.model_requested &= self.browse.selected == selected;
        self.browse.selected = selected;
        self.hits.clear();
    }

    fn selected(&self) -> Option<Setting> {
        match self.rows.get(self.browse.selected) {
            Some(Row::Setting(setting)) => Some(*setting),
            _ => None,
        }
    }

    fn edit_field(&mut self) -> Option<&mut Field> {
        match &mut self.mode {
            Mode::Filter => Some(&mut self.browse.filter),
            Mode::Bash(field) if self.bash_request.is_none() => Some(field),
            _ => None,
        }
    }
}

pub(super) fn open(app: &mut App) {
    let (send, detection) = mpsc::channel();
    let bash_detection = thread::Builder::new()
        .name("centaeris-detect-bash".into())
        .spawn(move || {
            let _ = send.send(centaeris_agent::local_execution_host::resolve_bash_path(None).ok());
        })
        .ok()
        .map(|_| detection);
    let bash = app
        .runtime
        .as_mut()
        .ok_or_else(|| "Runtime is disconnected".to_string())
        .and_then(|runtime| {
            runtime.request_async("agent_runtime_config_get", json!({"request": {}}))
        });
    let mut panel = Panel {
        browse: Browse::default(),
        parent: None,
        mode: Mode::Browse,
        bash_path: None,
        bash_loaded: false,
        automatic_bash: None,
        bash_detection,
        notice: bash
            .as_ref()
            .err()
            .cloned()
            .or_else(|| app.ui_preferences.load_error.clone()),
        bash_request: bash.ok().map(|response| BashRequest {
            response,
            saving: false,
        }),
        model_requested: false,
        rows: Vec::new(),
        hits: Vec::new(),
        filter_area: Rect::default(),
    };
    panel.refresh_rows();
    app.settings_panel = Some(panel);
    mouse::resize(app);
}

pub(super) fn visible(app: &App) -> bool {
    app.settings_panel.is_some() && app.model_panel.is_none() && !app.secret_input_active()
}

pub(super) fn take_model_request(app: &mut App) -> bool {
    app.settings_panel
        .as_mut()
        .is_some_and(|panel| std::mem::take(&mut panel.model_requested))
}

pub(super) fn invalidate_geometry(app: &mut App) {
    if let Some(panel) = &mut app.settings_panel {
        panel.hits.clear();
        panel.filter_area = Rect::default();
    }
}

pub(super) fn poll(app: &mut App) -> bool {
    let preferences = app.ui_preferences.poll();
    let Some(panel) = &mut app.settings_panel else {
        let changed = preferences.is_some();
        if let Some(Err(error)) = preferences {
            show_message(app, error);
        }
        return changed;
    };
    let mut changed = preferences.is_some();
    if let Some(result) = preferences {
        panel.notice = result.err();
    }
    if let Some(detection) = &panel.bash_detection {
        match detection.try_recv() {
            Ok(path) => {
                panel.automatic_bash = path;
                panel.bash_detection = None;
                changed = true;
            }
            Err(mpsc::TryRecvError::Disconnected) => panel.bash_detection = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
    if let Some(request) = &panel.bash_request {
        let result = match request.response.try_recv() {
            Ok(None) => return changed,
            Ok(Some(config)) => Ok(config),
            Err(error) => Err(error.to_string()),
        };
        let saving = request.saving;
        panel.bash_request = None;
        match result {
            Ok(config) => {
                panel.bash_path = config["bashPath"].as_str().map(str::to_owned);
                panel.bash_loaded = true;
                if saving && matches!(panel.mode, Mode::Bash(_)) {
                    panel.mode = Mode::Browse;
                }
                if saving {
                    panel.notice = None;
                }
            }
            Err(error) => panel.notice = Some(error),
        }
        changed = true;
    }
    changed
}

fn apply(app: &mut App, action: SettingAction, reset: bool) {
    let panel = app.settings_panel.as_mut().expect("settings");
    if action == SettingAction::Placeholder {
        return;
    }
    app.session.message = None;
    panel.notice = None;
    match action {
        SettingAction::DashboardPreview => {
            let enabled = reset || !app.ui_preferences.dashboard_preview();
            if let Err(error) = app.ui_preferences.save_dashboard_preview(enabled) {
                panel.notice = Some(error);
            }
        }
        SettingAction::BashPath if panel.bash_loaded && panel.bash_request.is_none() => {
            if reset {
                save_bash_path(app, "");
            } else {
                panel.mode = Mode::Bash(Field::new(panel.bash_path.clone().unwrap_or_default()));
            }
        }
        SettingAction::DefaultModel => {
            let result = start_model_command(app, ModelCommand::SettingsList);
            let panel = app.settings_panel.as_mut().expect("settings");
            match result {
                Ok(()) => panel.model_requested = true,
                Err(error) => panel.notice = Some(error),
            }
        }
        SettingAction::Hints => {
            panel.parent = Some(std::mem::take(&mut panel.browse));
            panel.mode = Mode::Browse;
            panel.refresh_rows();
        }
        _ => {}
    }
}

fn save_bash_path(app: &mut App, path: &str) {
    let result = app
        .runtime
        .as_mut()
        .ok_or_else(|| "Runtime is disconnected".to_string())
        .and_then(|runtime| {
            runtime.request_async(
                "agent_runtime_config_set",
                json!({"request": {"bashPath": path}}),
            )
        });
    let panel = app.settings_panel.as_mut().expect("settings");
    match result {
        Ok(response) => {
            panel.bash_request = Some(BashRequest {
                response,
                saving: true,
            });
            panel.notice = None;
        }
        Err(error) => panel.notice = Some(error),
    }
}

pub(super) fn paste(app: &mut App, text: &str) {
    let panel = app.settings_panel.as_mut().expect("settings");
    let Some(field) = panel.edit_field() else {
        return;
    };
    field.insert(text);
    if matches!(panel.mode, Mode::Filter) {
        panel.refresh_rows();
    }
}

pub(super) fn key(app: &mut App, key: KeyEvent) {
    let panel = app.settings_panel.as_mut().expect("settings");
    if key.kind != KeyEventKind::Press
        && (key.code == KeyCode::Enter
            || matches!(panel.mode, Mode::Browse) && matches!(key.code, KeyCode::Char(' ' | 'd')))
    {
        return;
    }
    if key.code == KeyCode::Char('v') && key.modifiers == KeyModifiers::CONTROL {
        if panel.edit_field().is_some() {
            match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
                Ok(text) => paste(app, &text),
                Err(error) => panel.notice = Some(error.to_string()),
            }
        }
        return;
    }
    match key.code {
        KeyCode::Esc => match panel.mode {
            Mode::Filter | Mode::Bash(_) | Mode::Reset(_) => panel.mode = Mode::Browse,
            Mode::Browse => {
                if panel
                    .bash_request
                    .as_ref()
                    .is_some_and(|request| request.saving)
                {
                    panel.notice = Some("Saving Bash path…".into());
                    return;
                }
                if let Some(parent) = panel.parent.take() {
                    let selected = parent.selected;
                    panel.browse = parent;
                    panel.refresh_rows();
                    panel.browse.selected = selected;
                } else {
                    app.settings_panel = None;
                    mouse::resize(app);
                }
            }
        },
        KeyCode::Enter if matches!(panel.mode, Mode::Filter) => panel.mode = Mode::Browse,
        KeyCode::Enter => match &panel.mode {
            Mode::Bash(field) if panel.bash_request.is_none() => {
                let path = field.text.trim().to_owned();
                save_bash_path(app, &path);
            }
            Mode::Reset(action) => {
                let action = *action;
                panel.mode = Mode::Browse;
                apply(app, action, true);
            }
            Mode::Browse => {
                if let Some(setting) = panel.selected() {
                    apply(app, setting.action, false);
                }
            }
            _ => {}
        },
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
            if matches!(panel.mode, Mode::Browse | Mode::Filter) =>
        {
            panel.move_selection(match key.code {
                KeyCode::Up => -1,
                KeyCode::Down => 1,
                KeyCode::PageUp => -10,
                _ => 10,
            });
        }
        _ => {
            if let Some(field) = panel.edit_field() {
                let changed = field.edit(key);
                if changed && matches!(panel.mode, Mode::Filter) {
                    panel.refresh_rows();
                }
            } else if matches!(panel.mode, Mode::Browse) && key.modifiers.is_empty() {
                match key.code {
                    KeyCode::Char('/' | 'i') => {
                        panel.mode = Mode::Filter;
                        panel.model_requested = false;
                    }
                    KeyCode::Char('j') => panel.move_selection(1),
                    KeyCode::Char('k') => panel.move_selection(-1),
                    KeyCode::Char(' ')
                        if panel.selected().is_some_and(|setting| {
                            setting.action == SettingAction::DashboardPreview
                        }) =>
                    {
                        apply(app, SettingAction::DashboardPreview, false);
                    }
                    KeyCode::Char('d') => {
                        if let Some(setting) = panel.selected().filter(|setting| {
                            setting.action == SettingAction::DashboardPreview
                                || setting.action == SettingAction::BashPath
                                    && panel.bash_loaded
                                    && panel.bash_request.is_none()
                        }) {
                            panel.mode = Mode::Reset(setting.action);
                            panel.hits.clear();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

pub(super) fn mouse(app: &mut App, event: MouseEvent) {
    let panel = app.settings_panel.as_mut().expect("settings");
    if !matches!(panel.mode, Mode::Browse | Mode::Filter) {
        return;
    }
    match event.kind {
        MouseEventKind::ScrollUp => panel.move_selection(-3),
        MouseEventKind::ScrollDown => panel.move_selection(3),
        MouseEventKind::Down(MouseButton::Left) => {
            let at = Position::new(event.column, event.row);
            if panel.filter_area.contains(at) {
                panel.mode = Mode::Filter;
                panel.model_requested = false;
                return;
            }
            let Some((rect, index)) = panel.hits.iter().find(|(rect, _)| rect.contains(at)) else {
                return;
            };
            let selected = panel.browse.selected == *index;
            let value_clicked = at.x >= rect.x + rect.width / 2 + 4;
            panel.model_requested &= selected;
            panel.browse.selected = *index;
            panel.mode = Mode::Browse;
            if selected || value_clicked {
                if let Some(setting) = panel.selected() {
                    apply(app, setting.action, false);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn render(frame: &mut Frame, area: Rect, app: &mut App) {
    let panel = app.settings_panel.as_mut().expect("settings");
    panel.hits.clear();
    panel.filter_area = Rect::default();
    let area = picker_view::content(frame, area);
    if area.height < 7 || area.width < 28 {
        frame.render_widget(Paragraph::new("Terminal too small."), area);
        return;
    }
    let muted = Style::default().fg(theme().muted);
    let heading = match panel.mode {
        Mode::Bash(_) => "Settings › Bash path",
        Mode::Reset(_) => "Settings › Restore default",
        _ if panel.parent.is_some() => "Settings › Contextual hints",
        _ => "Settings",
    };
    let width = (u32::from(area.width) * 7 / 10) as u16;
    let width = width.clamp(64, 110).min(area.width);
    let [window] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let top = u16::from(window.height >= 12);
    let border = Block::bordered()
        .border_style(muted)
        .title(Line::from(vec![
            Span::styled("─ ", muted),
            Span::styled(heading, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(" ", muted),
        ]))
        .padding(Padding::new(1, 1, top, 0));
    let inner = border.inner(window);
    frame.render_widget(border, window);

    let footer = Paragraph::new(shortcuts(panel))
        .style(muted)
        .wrap(Wrap { trim: true });
    let footer_height = footer.line_count(inner.width) as u16;
    let notice = panel.notice.as_deref().or(app.session.message.as_deref());
    let filter_height = u16::from(!matches!(panel.mode, Mode::Reset(_)));
    let [filter, body, status, _, footer_area] = Layout::vertical([
        Constraint::Length(filter_height),
        Constraint::Min(1),
        Constraint::Length(u16::from(notice.is_some())),
        Constraint::Length(u16::from(inner.height >= 10)),
        Constraint::Length(footer_height),
    ])
    .areas(inner);
    match &panel.mode {
        Mode::Bash(field) => {
            let saving = panel.bash_request.is_some();
            app.frame_caret = render_field(frame, filter, "Path ", field, !saving);
            if let Some(path) = &panel.automatic_bash {
                frame.render_widget(
                    Paragraph::new(format!("Automatic  {}", display_path(path)))
                        .style(muted)
                        .wrap(Wrap { trim: false }),
                    body,
                );
            }
        }
        Mode::Reset(action) => {
            let label = if *action == SettingAction::BashPath {
                "Bash path"
            } else {
                "Dashboard preview"
            };
            frame.render_widget(
                Paragraph::new(format!("Restore {label} to its default?"))
                    .wrap(Wrap { trim: true }),
                body,
            );
        }
        Mode::Browse | Mode::Filter => {
            let focused = matches!(panel.mode, Mode::Filter);
            panel.filter_area = filter;
            app.frame_caret = render_field(frame, filter, "Filter ", &panel.browse.filter, focused);
            let values_width = usize::from(body.width) / 2;
            let items = panel
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| match row {
                    Row::Header(label) => {
                        ListItem::new(Line::styled(*label, muted.add_modifier(Modifier::BOLD)))
                    }
                    Row::Setting(setting) => {
                        let value = match setting.action {
                            SettingAction::DashboardPreview => {
                                if app.ui_preferences.pending.is_some() {
                                    "Saving…".into()
                                } else if app.ui_preferences.dashboard_preview() {
                                    "On".into()
                                } else {
                                    "Off".into()
                                }
                            }
                            SettingAction::BashPath
                                if panel
                                    .bash_request
                                    .as_ref()
                                    .is_some_and(|request| request.saving) =>
                            {
                                "Saving…".into()
                            }
                            SettingAction::BashPath if panel.bash_request.is_some() => {
                                "Loading…".into()
                            }
                            SettingAction::BashPath if panel.bash_loaded => {
                                match (&panel.bash_path, &panel.automatic_bash) {
                                    (Some(path), _) => display_path(Path::new(path)),
                                    (None, Some(path)) => format!("Auto · {}", display_path(path)),
                                    (None, None) => "Automatic".into(),
                                }
                            }
                            SettingAction::DefaultModel if panel.model_requested => {
                                "Loading…".into()
                            }
                            SettingAction::DefaultModel => {
                                app.model_display.clone().unwrap_or_default()
                            }
                            SettingAction::Hints => "›".into(),
                            _ => String::new(),
                        };
                        let selected = index == panel.browse.selected;
                        let row = picker_view::row(
                            setting.label,
                            fit_middle_columns(&value, values_width.saturating_sub(4)),
                            values_width,
                            selected,
                        );
                        if setting.action == SettingAction::Placeholder {
                            row.style(if selected {
                                theme().selected_row().fg(theme().muted)
                            } else {
                                muted
                            })
                        } else {
                            row
                        }
                    }
                })
                .collect::<Vec<_>>();
            let offset = picker_view::render_list(frame, body, items, panel.browse.selected);
            panel.hits = panel
                .rows
                .iter()
                .enumerate()
                .skip(offset)
                .take(usize::from(body.height))
                .filter(|(_, row)| matches!(row, Row::Setting(_)))
                .map(|(index, _)| {
                    (
                        Rect::new(body.x, body.y + (index - offset) as u16, body.width, 1),
                        index,
                    )
                })
                .collect();
            if panel.rows.is_empty() {
                frame.render_widget(Paragraph::new("No matching settings.").style(muted), body);
            }
        }
    }
    if let Some(notice) = notice {
        frame.render_widget(Paragraph::new(notice).style(muted), status);
    }
    frame.render_widget(footer, footer_area);
}

fn shortcuts(panel: &Panel) -> &'static str {
    match panel.mode {
        Mode::Bash(_) if panel.bash_request.is_some() => "Saving… · Esc Back",
        Mode::Bash(_) => "Enter Save · Esc Back · Empty for automatic",
        Mode::Reset(_) => "Enter Restore · Esc Cancel",
        Mode::Filter => "Enter Done · Esc Back",
        Mode::Browse => match panel.selected().map(|setting| setting.action) {
            Some(SettingAction::DashboardPreview | SettingAction::BashPath) => {
                "↑↓ Select · Enter Edit · d Default · / Filter · Esc Back"
            }
            None | Some(SettingAction::Placeholder) => "↑↓ Select · / Filter · Esc Back",
            Some(_) => "↑↓ Select · Enter Edit · / Filter · Esc Back",
        },
    }
}

fn render_field(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    field: &Field,
    focused: bool,
) -> Option<Position> {
    let width = usize::from(area.width).saturating_sub(label.width()).max(1);
    let mut start = 0;
    let mut caret = 0;
    if focused {
        for (offset, grapheme) in field.text[..field.cursor].grapheme_indices(true).rev() {
            let next = caret + grapheme.width();
            if next >= width {
                start = offset + grapheme.len();
                break;
            }
            caret = next;
        }
    }
    let line = Line::from(vec![
        Span::styled(label, Style::default().fg(theme().muted)),
        Span::raw(&field.text[start..]),
    ]);
    frame.render_widget(Paragraph::new(line), area);
    (focused && !area.is_empty()).then(|| {
        Position::new(
            (area.x + (label.width() + caret) as u16).min(area.right() - 1),
            area.y,
        )
    })
}
