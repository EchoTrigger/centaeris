use std::collections::HashMap;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use centaeris_cli_runtime::rpc_client::{NativeSubagentResponse, RuntimeClient};
use centaeris_core::runtime::subagent::{SessionSubagentTasks, SubagentTask, SubagentTaskScope};
use centaeris_core::session::reliability::{RuntimeJobStatus, SubagentJobCancellationReceipt};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use serde_json::Value;

use super::theme::theme;
use super::view::fit_middle_columns;

mod detail;
use detail::Detail;

const MAX_DOCK_ROWS: u16 = 6;
const ACTIVE_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
pub(super) struct State {
    parent: Option<String>,
    agents: Vec<SessionSubagentTasks>,
    list: Option<ListRequest>,
    refresh: bool,
    next_active_refresh: Option<Instant>,
    stops: HashMap<String, Pending<SubagentJobCancellationReceipt>>,
    acknowledged_stops: HashMap<String, RuntimeJobStatus>,
    activity: HashMap<String, Activity>,
    list_error: Option<String>,
    error: Option<String>,
    detail: Option<Detail>,
    dock: Dock,
    elapsed_second: i64,
    terminal_revision: u64,
}

#[derive(Debug)]
struct Pending<T> {
    parent: String,
    result: NativeSubagentResponse<T>,
}

#[derive(Debug)]
struct ListRequest {
    scope: SubagentTaskScope,
    pending: Pending<Vec<SessionSubagentTasks>>,
}

impl<T> Pending<T> {
    fn poll(&self) -> Option<Result<T, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("Subagent request was interrupted".into())),
        }
    }
}

#[derive(Debug, Default)]
struct Activity {
    role: Option<String>,
    progress: Option<String>,
}

#[derive(Debug, Default)]
struct Dock {
    collapsed: bool,
    show_completed: bool,
    focused: bool,
    selected: Option<String>,
    first: usize,
}

/// Geometry and exact task identities from the last painted dock.
#[derive(Debug, Default)]
pub(super) struct DockLayout {
    parent: Option<String>,
    header: Rect,
    rows: Vec<DockRow>,
}

#[derive(Debug)]
struct DockRow {
    rect: Rect,
    stop: Option<Rect>,
    agent_id: String,
    child_session_id: String,
    task_id: String,
}

impl State {
    /// Polls stamped asynchronous results and schedules reads for the current parent.
    pub(super) fn poll(&mut self, runtime: Option<&RuntimeClient>, parent: Option<&str>) -> bool {
        let mut changed = parent != self.parent.as_deref();
        if changed {
            *self = Self {
                parent: parent.map(str::to_owned),
                refresh: parent.is_some(),
                ..Self::default()
            };
        }
        let Some(parent) = parent else { return changed };

        if let Some(result) = self.list.as_ref().and_then(|read| read.pending.poll()) {
            if let Some(read) = self
                .list
                .take()
                .filter(|read| read.pending.parent == parent)
            {
                match result {
                    Ok(mut agents) => {
                        changed |= self.list_error.take().is_some();
                        if read.scope == SubagentTaskScope::Active {
                            self.retain_terminal_history(&mut agents);
                        }
                        agents.sort_by(|left, right| {
                            let left = current(left);
                            let right = current(right);
                            sort_rank(left).cmp(&sort_rank(right))
                        });
                        self.activity.retain(|task, _| {
                            agents.iter().any(|agent| {
                                agent
                                    .active
                                    .as_ref()
                                    .is_some_and(|active| &active.task_id == task)
                            })
                        });
                        self.acknowledged_stops.retain(|task, _| {
                            agents.iter().any(|agent| {
                                agent
                                    .active
                                    .as_ref()
                                    .is_some_and(|active| &active.task_id == task)
                            })
                        });
                        if !self.terminal_links().eq(agents
                            .iter()
                            .filter_map(|agent| agent.last_terminal.as_ref()))
                        {
                            self.terminal_revision = self.terminal_revision.wrapping_add(1);
                        }
                        changed |= self.agents != agents;
                        self.agents = agents;
                        self.keep_visible_selection();
                    }
                    Err(error) => {
                        changed |= self.list_error.as_ref() != Some(&error);
                        self.list_error = Some(error);
                    }
                }
            }
        }

        let finished: Vec<_> = self
            .stops
            .iter()
            .filter_map(|(id, pending)| pending.poll().map(|result| (id.clone(), result)))
            .collect();
        for (task, result) in finished {
            if self
                .stops
                .remove(&task)
                .is_none_or(|pending| pending.parent != parent)
            {
                continue;
            }
            match result {
                Ok(receipt) => {
                    self.acknowledged_stops.insert(task, receipt.status);
                    if let Some(detail) = &mut self.detail {
                        detail.invalidate();
                    }
                }
                Err(error) => {
                    self.set_error(error);
                }
            }
            self.refresh = true;
            changed = true;
        }

        let now = Instant::now();
        let has_active = self.agents.iter().any(|agent| agent.active.is_some());
        let active_due = has_active
            && self
                .next_active_refresh
                .is_some_and(|deadline| now >= deadline);
        if (self.refresh || active_due) && self.list.is_none() {
            if let Some(runtime) = runtime {
                let scope = if std::mem::take(&mut self.refresh) {
                    SubagentTaskScope::IncludeTerminal
                } else {
                    SubagentTaskScope::Active
                };
                self.next_active_refresh = Some(now + ACTIVE_REFRESH_INTERVAL);
                match runtime.list_subagents_async(parent.to_owned(), scope) {
                    Ok(result) => {
                        self.list = Some(ListRequest {
                            scope,
                            pending: Pending {
                                parent: parent.into(),
                                result,
                            },
                        });
                    }
                    Err(error) => {
                        changed |= self.list_error.as_ref() != Some(&error);
                        self.list_error = Some(error);
                    }
                }
            }
        }
        if has_active {
            let second = wall_ms() / 1000;
            changed |= self.elapsed_second != second;
            self.elapsed_second = second;
        }

        if let Some(detail) = &mut self.detail {
            let active = self.agents.iter().any(|agent| {
                agent
                    .active
                    .as_ref()
                    .is_some_and(|task| task.child_session_id == detail.child())
            });
            changed |= detail.poll(runtime, parent, active);
        }
        changed
    }

    fn retain_terminal_history(&mut self, active: &mut Vec<SessionSubagentTasks>) {
        let mut active_by_agent: HashMap<_, _> = active
            .iter_mut()
            .filter_map(|agent| {
                let identity = agent.active.as_ref()?.agent_id.clone();
                Some((identity, agent))
            })
            .collect();
        let mut completed = Vec::new();
        for previous in &self.agents {
            let Some(identity) = current(previous) else {
                continue;
            };
            let latest = active_by_agent.get_mut(&identity.agent_id);
            if let Some(task) = previous.active.as_ref() {
                self.refresh |= latest
                    .as_ref()
                    .and_then(|agent| agent.active.as_ref())
                    .is_none_or(|active| active.task_id != task.task_id);
            }
            if let Some(terminal) = &previous.last_terminal {
                if let Some(latest) = latest {
                    latest.last_terminal = Some(terminal.clone());
                } else {
                    completed.push(SessionSubagentTasks {
                        active: None,
                        last_terminal: Some(terminal.clone()),
                    });
                }
            }
        }
        active.extend(completed);
    }

    /// Queues a durable refresh for the next poll of this parent Session.
    pub(super) fn request_refresh(&mut self) {
        self.refresh = true;
    }

    /// Consumes runtime hints while the authoritative list remains the source of row identity and status.
    pub(super) fn observe_session_update(&mut self, update: &Value) -> bool {
        let Some(session) = update["sessionId"].as_str() else {
            return false;
        };
        let event = &update["payload"]["event"];
        let event_type = event["type"].as_str().unwrap_or_default();
        let payload = &event["payload"];
        if self.parent.as_deref() == Some(session) && event_type.starts_with("Subagent") {
            self.request_refresh();
            if let Some(detail) = &mut self.detail {
                if payload["childSessionId"].as_str() == Some(detail.child()) {
                    detail.invalidate();
                }
            }
            let Some(task_id) = payload["runtimeJobId"].as_str() else {
                return false;
            };
            let role = payload["role"]
                .as_str()
                .filter(|role| !matches!(*role, "agent" | "worker"));
            if let Some(role) = role {
                self.activity.entry(task_id.to_owned()).or_default().role = Some(role.to_owned());
                return true;
            }
        }

        if let Some(detail) = &mut self.detail {
            if detail.child() == session {
                detail.invalidate();
            }
        }
        let Some(task) = self
            .agents
            .iter()
            .filter_map(|agent| agent.active.as_ref())
            .find(|task| {
                task.child_session_id == session
                    && update["agentRunId"].as_str() == Some(&task.task_id)
            })
        else {
            return false;
        };
        let progress = match event_type {
            "ToolCall" | "ToolProgress" => payload["message"]
                .as_str()
                .or_else(|| payload["toolName"].as_str())
                .map(single_line),
            "ToolResult" => None,
            _ => return false,
        };
        let activity = self.activity.entry(task.task_id.clone()).or_default();
        let changed = activity.progress != progress;
        activity.progress = progress;
        changed
    }

    /// Returns the capped dock height the parent can allocate above the composer.
    pub(super) fn row_budget(&self, available: u16) -> u16 {
        if self.agents.is_empty() && self.error.is_none() && self.list_error.is_none() {
            return 0;
        }
        let rows = if self.dock.collapsed {
            0
        } else {
            self.visible_tasks().count()
        };
        let requested = u16::try_from(rows.saturating_add(1)).unwrap_or(u16::MAX);
        requested.min(available).min(MAX_DOCK_ROWS)
    }

    /// Moves keyboard focus into the dock, which shares the parent viewport with the composer.
    pub(super) fn focus_dock(&mut self) -> bool {
        if self.row_budget(1) == 0 {
            return false;
        }
        self.dock.focused = true;
        true
    }

    pub(super) fn blur_dock(&mut self) {
        self.dock.focused = false;
    }

    pub(super) fn dock_focused(&self) -> bool {
        self.dock.focused
    }

    /// Paints the dock from prepared state and returns the input geometry for this frame.
    pub(super) fn render_dock(&self, frame: &mut Frame, area: Rect, can_stop: bool) -> DockLayout {
        if area.is_empty() {
            return DockLayout::default();
        }
        let header = Rect { height: 1, ..area };
        let toggle = if self.dock.show_completed {
            "h Hide completed"
        } else {
            "h Show completed"
        };
        let chevron = if self.dock.collapsed { '▸' } else { '▾' };
        let mut spans = vec![Span::raw(format!(" {chevron} Subagents"))];
        if self
            .agents
            .iter()
            .any(|agent| agent.last_terminal.is_some())
        {
            spans.push(Span::styled(format!("  {toggle}"), muted()));
        }
        if let Some(error) = self.error.as_ref().or(self.list_error.as_ref()) {
            spans.push(Span::styled(format!("  {error}"), muted()));
        }
        let style = self.selection_style(self.dock.selected.is_none());
        frame.render_widget(Paragraph::new(Line::from(spans)).style(style), header);

        let remaining = usize::from(area.height - 1);
        let tasks: Vec<_> = self.visible_tasks().collect();
        let first = self.first_visible(&tasks, remaining);
        let rows = tasks
            .into_iter()
            .skip(first)
            .take(remaining)
            .enumerate()
            .map(|(index, task)| {
                let rect = Rect {
                    y: area.y + 1 + index as u16,
                    height: 1,
                    ..area
                };
                self.render_task(frame, rect, task, can_stop)
            })
            .collect();
        DockLayout {
            parent: self.parent.clone(),
            header,
            rows,
        }
    }

    fn render_task(
        &self,
        frame: &mut Frame,
        rect: Rect,
        task: &SubagentTask,
        can_stop: bool,
    ) -> DockRow {
        let selected = self.dock.selected.as_deref() == Some(&task.agent_id);
        let style = self.selection_style(selected);
        let (mark, color) = agent_mark(&task.child_session_id);
        let activity = self.activity.get(&task.task_id);
        let role = activity
            .and_then(|hint| hint.role.as_deref())
            .unwrap_or("Subagent");
        let mut title = format!(" {role}  {}", single_line(&task.description));
        if let Some(progress) = activity.and_then(|hint| hint.progress.as_deref()) {
            title.push_str("  ·  ");
            title.push_str(progress);
        }
        let receipt = self.acknowledged_stops.get(&task.task_id);
        let stopping = receipt.is_some() || task.cancellation_requested;
        let status = match receipt {
            Some(received) if received.is_terminal() => status(received),
            _ if stopping && !task.status.is_terminal() => "Stopping",
            _ => status(&task.status),
        };
        let suffix = format!(
            "  {status} {}",
            elapsed(task, self.elapsed_second.saturating_mul(1000))
        );
        let stop = (can_stop
            && !task.status.is_terminal()
            && !stopping
            && !self.stops.contains_key(&task.task_id)
            && rect.width >= 26)
            .then(|| Rect {
                x: rect.right() - 7,
                width: 6,
                ..rect
            });
        let available = usize::from(rect.width).saturating_sub(if stop.is_some() { 8 } else { 0 });
        let title_width =
            available.saturating_sub(mark.chars().count() + suffix.chars().count() + 1);
        let line = Line::from(vec![
            Span::raw(" "),
            Span::styled(mark, Style::default().fg(color)),
            Span::raw(fit_middle_columns(&title, title_width)),
            Span::styled(suffix, muted()),
        ]);
        let content = Rect {
            width: available as u16,
            ..rect
        };
        frame.render_widget(Paragraph::new(line).style(style), content);
        if let Some(stop) = stop {
            frame.render_widget(Paragraph::new("[stop]").style(style), stop);
        }
        DockRow {
            rect,
            stop,
            agent_id: task.agent_id.clone(),
            child_session_id: task.child_session_id.clone(),
            task_id: task.task_id.clone(),
        }
    }

    /// Handles dock navigation and actions against the task shown in the last frame.
    pub(super) fn handle_dock_key(
        &mut self,
        key: KeyEvent,
        layout: &DockLayout,
        runtime: Option<&RuntimeClient>,
        can_stop: bool,
    ) -> bool {
        if !self.dock.focused
            || layout.parent != self.parent
            || key.kind == KeyEventKind::Release
            || !key.modifiers.is_empty()
        {
            return false;
        }
        match key.code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Char('h') => {
                self.dock.show_completed = !self.dock.show_completed;
                self.dock.collapsed = false;
                self.dock.first = 0;
                self.keep_visible_selection();
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(row) = self.selected_row(layout) {
                    let child = row.child_session_id.clone();
                    self.open_child(&child);
                } else if self.dock.selected.is_none() {
                    self.toggle_dock();
                }
            }
            KeyCode::Char('x') if can_stop && key.kind == KeyEventKind::Press => {
                if let Some(row) = self.selected_row(layout) {
                    self.stop(row, runtime);
                }
            }
            KeyCode::Esc => self.blur_dock(),
            _ => return false,
        }
        true
    }

    /// Handles a single click in dock geometry, including its exact painted stop target.
    pub(super) fn handle_dock_mouse(
        &mut self,
        mouse: MouseEvent,
        layout: &DockLayout,
        runtime: Option<&RuntimeClient>,
        can_stop: bool,
    ) -> bool {
        if layout.parent != self.parent || mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return false;
        }
        let position = Position::new(mouse.column, mouse.row);
        if layout.header.contains(position) {
            self.focus_dock();
            self.toggle_dock();
            return true;
        }
        let Some(row) = layout.rows.iter().find(|row| row.rect.contains(position)) else {
            return false;
        };
        self.focus_dock();
        self.dock.selected = Some(row.agent_id.clone());
        if row.stop.is_some_and(|rect| rect.contains(position)) {
            if can_stop {
                self.stop(row, runtime);
            }
        } else {
            self.open_child(&row.child_session_id);
        }
        true
    }

    fn stop(&mut self, row: &DockRow, runtime: Option<&RuntimeClient>) {
        let displayed = self.agents.iter().any(|agent| {
            agent.active.as_ref().is_some_and(|task| {
                task.agent_id == row.agent_id
                    && task.task_id == row.task_id
                    && !task.cancellation_requested
            })
        });
        if !displayed
            || self.stops.contains_key(&row.task_id)
            || self.acknowledged_stops.contains_key(&row.task_id)
        {
            return;
        }
        let (Some(parent), Some(runtime)) = (self.parent.clone(), runtime) else {
            return;
        };
        self.error = None;
        match runtime.cancel_subagent_async(parent.clone(), row.task_id.clone()) {
            Ok(result) => {
                self.stops
                    .insert(row.task_id.clone(), Pending { parent, result });
            }
            Err(error) => {
                self.set_error(error);
            }
        }
    }

    fn selected_row<'a>(&self, layout: &'a DockLayout) -> Option<&'a DockRow> {
        layout
            .rows
            .iter()
            .find(|row| self.dock.selected.as_deref() == Some(&row.agent_id))
    }

    fn first_visible(&self, rows: &[&SubagentTask], capacity: usize) -> usize {
        let first = self.dock.first.min(rows.len().saturating_sub(capacity));
        let selected = self
            .dock
            .selected
            .as_deref()
            .and_then(|id| rows.iter().position(|task| task.agent_id == id));
        selected.map_or(first, |index| {
            first.clamp(index.saturating_sub(capacity.saturating_sub(1)), index)
        })
    }

    fn move_selection(&mut self, by: isize) {
        let tasks: Vec<_> = self.visible_tasks().collect();
        let index = self
            .dock
            .selected
            .as_deref()
            .and_then(|id| tasks.iter().position(|task| task.agent_id == id))
            .map_or(0, |index| index + 1);
        let next = index.saturating_add_signed(by).min(tasks.len());
        let selected = next
            .checked_sub(1)
            .map(|index| tasks[index].agent_id.clone());
        self.dock.first = self.first_visible(&tasks, usize::from(MAX_DOCK_ROWS - 1));
        self.dock.selected = selected;
    }

    fn toggle_dock(&mut self) {
        self.dock.collapsed = !self.dock.collapsed;
        self.dock.selected = None;
        self.dock.first = 0;
    }

    fn keep_visible_selection(&mut self) {
        if self
            .dock
            .selected
            .as_deref()
            .is_some_and(|id| !self.visible_tasks().any(|task| task.agent_id == id))
        {
            self.dock.selected = None;
        }
    }

    fn visible_tasks(&self) -> impl Iterator<Item = &SubagentTask> {
        self.agents.iter().filter_map(|agent| {
            (!self.dock.collapsed)
                .then(|| {
                    agent.active.as_ref().or_else(|| {
                        self.dock
                            .show_completed
                            .then_some(agent.last_terminal.as_ref())
                            .flatten()
                    })
                })
                .flatten()
        })
    }

    fn selection_style(&self, selected: bool) -> Style {
        if self.dock.focused && selected {
            theme().selected_row()
        } else {
            Style::default()
        }
    }

    fn set_error(&mut self, error: String) -> bool {
        let changed = self.error.as_deref() != Some(&error);
        self.error = Some(error);
        changed
    }

    /// Last terminal attempts for the clickable parent transcript, independent of dock visibility.
    pub(super) fn terminal_links(&self) -> impl Iterator<Item = &SubagentTask> {
        self.agents
            .iter()
            .filter_map(|agent| agent.last_terminal.as_ref())
    }

    pub(super) fn terminal_revision(&self) -> u64 {
        self.terminal_revision
    }

    /// Opens a child link from the dock or parent transcript, leaving parent input untouched.
    pub(super) fn open_child(&mut self, child_session_id: &str) -> bool {
        let Some(parent) = &self.parent else {
            return false;
        };
        if child_session_id.is_empty() || child_session_id == parent {
            return false;
        }
        let description = self
            .agents
            .iter()
            .filter_map(current)
            .find(|task| task.child_session_id == child_session_id)
            .map(|task| single_line(&task.description));
        self.detail = Some(Detail::new(child_session_id.to_owned(), description));
        true
    }

    pub(super) fn detail_open(&self) -> bool {
        self.detail.is_some()
    }

    pub(super) fn render_detail(&mut self, frame: &mut Frame, area: Rect) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        frame.render_widget(Clear, area);
        detail.render(frame, area);
    }

    /// Handles the fullscreen viewer before the parent key router sees the event.
    pub(super) fn handle_detail_key(&mut self, key: KeyEvent) -> bool {
        let Some(detail) = &mut self.detail else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            return true;
        }
        if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE {
            self.detail = None;
        } else {
            detail.handle_key(key);
        }
        true
    }

    /// Fullscreen detail consumes mouse input and scrolls its own viewport.
    pub(super) fn handle_detail_mouse(&mut self, mouse: MouseEvent) -> bool {
        let Some(detail) = &mut self.detail else {
            return false;
        };
        detail.handle_mouse(mouse.kind);
        true
    }
}

pub(super) fn terminal_link_line(task: &SubagentTask, width: u16) -> Line<'static> {
    let (mark, color) = agent_mark(&task.child_session_id);
    let label = format!(
        "  Subagent {}  ·  {}",
        status(&task.status),
        single_line(&task.description)
    );
    Line::from(vec![
        Span::styled(mark, Style::default().fg(color)),
        Span::styled(
            fit_middle_columns(&label, usize::from(width).saturating_sub(3)),
            muted(),
        ),
    ])
}

fn current(agent: &SessionSubagentTasks) -> Option<&SubagentTask> {
    agent.active.as_ref().or(agent.last_terminal.as_ref())
}

fn sort_rank(task: Option<&SubagentTask>) -> (u8, i64, &str) {
    let Some(task) = task else { return (3, 0, "") };
    let rank = match task.status {
        RuntimeJobStatus::Running => 0,
        RuntimeJobStatus::Queued | RuntimeJobStatus::Leased => 1,
        _ => 2,
    };
    (rank, task.submitted_at_ms, &task.agent_id)
}

fn status(status: &RuntimeJobStatus) -> &'static str {
    match status {
        RuntimeJobStatus::Queued | RuntimeJobStatus::Leased => "Queued",
        RuntimeJobStatus::Running => "Running",
        RuntimeJobStatus::Succeeded => "Completed",
        RuntimeJobStatus::Failed | RuntimeJobStatus::DeadLettered => "Failed",
        RuntimeJobStatus::Cancelled => "Cancelled",
    }
}

fn elapsed(task: &SubagentTask, now: i64) -> String {
    let end = if task.status.is_terminal() {
        task.updated_at_ms
    } else {
        now
    };
    let seconds = end.saturating_sub(task.submitted_at_ms).max(0) / 1000;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| i64::try_from(time.as_millis()).ok())
        .unwrap_or_default()
}

fn muted() -> Style {
    Style::default().fg(theme().muted)
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn agent_mark(seed: &str) -> (String, Color) {
    const PALETTE: [Color; 4] = [
        Color::Rgb(0x86, 0xd6, 0xae),
        Color::Rgb(0xb5, 0xd9, 0x8a),
        Color::Rgb(0xe6, 0xd2, 0x7e),
        Color::Rgb(0xe8, 0xae, 0x78),
    ];
    const DOTS: [[u32; 4]; 2] = [[0, 1, 2, 6], [3, 4, 5, 7]];
    let hash = seed.encode_utf16().fold(2_166_136_261_u32, |hash, unit| {
        (hash ^ u32::from(unit)).wrapping_mul(16_777_619)
    });
    let mut braille = [0_u32; 3];
    for y in 0..5 {
        for x in 0..5 {
            if ((hash >> (y * 3 + x.min(4 - x))) & 1) != 0 || (x == 2 && y == 2) {
                // Fold the centre and next row of the Web mark into a single Braille row.
                let row = if y >= 3 { y - 1 } else { y };
                braille[x / 2] |= 1 << DOTS[x % 2][row];
            }
        }
    }
    let mark = braille
        .into_iter()
        .filter_map(|dots| char::from_u32(0x2800 + dots))
        .collect();
    (mark, PALETTE[(hash % 4) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: RuntimeJobStatus) -> SubagentTask {
        SubagentTask {
            task_id: id.into(),
            agent_id: "agent-1".into(),
            child_session_id: "session-agent-1".into(),
            parent_agent_run_id: "parent-run".into(),
            description: id.into(),
            status,
            submitted_at_ms: 1000,
            updated_at_ms: 2000,
            cancellation_requested: false,
        }
    }

    #[test]
    fn stale_painted_stop_cannot_target_followup() {
        let active = task("task-b", RuntimeJobStatus::Running);
        let mut state = State {
            parent: Some("parent".into()),
            agents: vec![SessionSubagentTasks {
                active: Some(active.clone()),
                last_terminal: None,
            }],
            ..State::default()
        };
        let mut row = DockRow {
            rect: Rect::default(),
            stop: None,
            agent_id: active.agent_id,
            child_session_id: active.child_session_id,
            task_id: "task-a".into(),
        };
        let runtime = RuntimeClient::from_test_request_handler(|_| Value::Null);
        state.stop(&row, Some(&runtime));
        assert!(state.stops.is_empty());
        row.task_id = active.task_id;
        state.stop(&row, Some(&runtime));
        assert!(state.stops.contains_key("task-b"));
    }

    #[test]
    fn active_refresh_preserves_terminal_link_during_followup() {
        let mut state = State {
            agents: vec![SessionSubagentTasks {
                active: Some(task("task-b", RuntimeJobStatus::Running)),
                last_terminal: Some(task("task-a", RuntimeJobStatus::Succeeded)),
            }],
            ..State::default()
        };
        let mut listed = vec![SessionSubagentTasks {
            active: Some(task("task-c", RuntimeJobStatus::Queued)),
            last_terminal: None,
        }];
        state.retain_terminal_history(&mut listed);
        state.agents = listed;
        assert!(state.refresh);
        assert_eq!(state.visible_tasks().next().unwrap().task_id, "task-c");
        assert_eq!(state.terminal_links().next().unwrap().task_id, "task-a");
        assert_eq!(state.row_budget(10), 2);
    }

    #[test]
    fn completed_child_link_opens_detail_without_changing_parent_draft() {
        use super::super::{handle_key, mouse, view, App, AppConfig};
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = App::new(AppConfig {
            workspace_root: "D:/workspace".into(),
            session_cwd: "D:/workspace".into(),
            warn_home_on_first_prompt: false,
        });
        app.session.input = "Parent draft".into();
        app.session.subagents = State {
            parent: Some("parent".into()),
            agents: vec![SessionSubagentTasks {
                active: None,
                last_terminal: Some(task("child history", RuntimeJobStatus::Succeeded)),
            }],
            ..State::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(85, 24)).unwrap();
        let transcript = view::build_transcript_view(&app, 85);
        terminal
            .draw(|frame| view::render(frame, &mut app, &transcript))
            .unwrap();
        let link = app.subagent_links[0].0;
        mouse::handle(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: link.x,
                row: link.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.session.subagents.detail_open());
        handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(!app.session.subagents.detail_open());
        assert_eq!(app.session.input, "Parent draft");
    }
}
