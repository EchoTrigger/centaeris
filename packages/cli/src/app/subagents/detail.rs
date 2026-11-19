use std::collections::HashMap;
use std::thread;
use std::time::Instant;

use centaeris_cli_runtime::rpc_client::RuntimeClient;
use centaeris_core::session::transcript::{
    TranscriptContentRangeV1, TranscriptContentRefV1, TRANSCRIPT_CONTENT_RANGE_MAX_BYTES,
    TRANSCRIPT_PROJECTION_VERSION_V1,
};
use centaeris_core::session::visualization::MAX_ANSWER_BYTES;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;
use serde_json::{json, Value};

use super::super::paging::{
    request_transcript_page_with, request_transcript_patches_with, TranscriptPagingState,
};
use super::super::view::{fit_middle_columns, transcript_to_lines};
use super::super::{TranscriptLine, TRANSCRIPT_PROJECTION_POLL_INTERVAL};
use super::{muted, Pending, ACTIVE_REFRESH_INTERVAL};

#[derive(Debug, Default)]
struct Viewer {
    lines: Vec<Line<'static>>,
    width: u16,
    height: u16,
    total: u16,
    scroll: u16,
    follow_bottom: bool,
    prepend_from: Option<u16>,
}

#[derive(Debug, Clone)]
struct Answer {
    reference: TranscriptContentRefV1,
    text: String,
}

#[derive(Debug)]
struct Snapshot {
    paging: TranscriptPagingState,
    history: Vec<TranscriptLine>,
    answers: HashMap<String, Answer>,
}

#[derive(Debug)]
struct DetailRequest {
    child: String,
    read: ReadKind,
    pending: Pending<Option<Snapshot>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadKind {
    Open,
    Older,
    Update,
}

#[derive(Debug)]
pub(super) struct Detail {
    child: String,
    description: Option<String>,
    snapshot: Option<Snapshot>,
    pending: Option<DetailRequest>,
    dirty: bool,
    older: bool,
    next_poll: Option<Instant>,
    error: Option<String>,
    view: Viewer,
}

impl Detail {
    pub(super) fn new(child: String, description: Option<String>) -> Self {
        Self {
            child,
            description,
            snapshot: None,
            pending: None,
            dirty: true,
            older: false,
            next_poll: None,
            error: None,
            view: Viewer {
                follow_bottom: true,
                ..Viewer::default()
            },
        }
    }

    pub(super) fn child(&self) -> &str {
        &self.child
    }

    pub(super) fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub(super) fn poll(
        &mut self,
        runtime: Option<&RuntimeClient>,
        parent: &str,
        active: bool,
    ) -> bool {
        let mut changed = false;
        if let Some(result) = self
            .pending
            .as_ref()
            .and_then(|request| request.pending.poll())
        {
            if let Some(request) = self
                .pending
                .take()
                .filter(|request| request.child == self.child && request.pending.parent == parent)
            {
                match result {
                    Ok(snapshot) => {
                        if let Some(snapshot) = snapshot {
                            if request.read == ReadKind::Older {
                                self.view.prepend_from = Some(self.view.total);
                            }
                            self.view.width = 0;
                            self.snapshot = Some(snapshot);
                        }
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
                changed = true;
            }
        }
        let now = Instant::now();
        if active && self.next_poll.is_some_and(|deadline| now >= deadline) {
            self.dirty = true;
        }
        if self.pending.is_none() {
            if let Some(runtime) = runtime {
                let read = if self.older {
                    Some(ReadKind::Older)
                } else if self.dirty {
                    Some(if self.snapshot.is_some() {
                        ReadKind::Update
                    } else {
                        ReadKind::Open
                    })
                } else {
                    None
                };
                if let Some(read) = read {
                    self.request(runtime, parent, read, now);
                    changed = true;
                }
            }
        }
        changed
    }

    fn request(&mut self, runtime: &RuntimeClient, parent: &str, read: ReadKind, now: Instant) {
        if read == ReadKind::Older {
            self.older = false;
        } else {
            self.dirty = false;
        }
        self.next_poll = Some(now + ACTIVE_REFRESH_INTERVAL);
        let child = self.child.clone();
        let paging = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.paging.clone());
        let answers = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.answers.clone())
            .unwrap_or_default();
        match runtime.read_subagent_transcript_async(
            parent.to_owned(),
            self.child.clone(),
            move |request| load_snapshot(read, &child, request, paging, answers),
        ) {
            Ok(result) => {
                self.pending = Some(DetailRequest {
                    child: self.child.clone(),
                    read,
                    pending: Pending {
                        parent: parent.to_owned(),
                        result,
                    },
                });
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent) {
        let page = self.view.height.saturating_sub(1).max(1);
        match (key.code, key.modifiers) {
            (KeyCode::Up, KeyModifiers::NONE) => self.scroll_up(1),
            (KeyCode::Down, KeyModifiers::NONE) => self.scroll_down(1),
            (KeyCode::PageUp, KeyModifiers::NONE) => self.scroll_up(page),
            (KeyCode::PageDown, KeyModifiers::NONE) => self.scroll_down(page),
            (KeyCode::Home, KeyModifiers::CONTROL) => self.scroll_up(self.view.scroll.max(1)),
            (KeyCode::End, KeyModifiers::CONTROL) => self.scroll_down(u16::MAX),
            _ => {}
        }
    }

    pub(super) fn handle_mouse(&mut self, kind: MouseEventKind) {
        match kind {
            MouseEventKind::ScrollUp => self.scroll_up(3),
            MouseEventKind::ScrollDown => self.scroll_down(3),
            _ => {}
        }
    }

    fn scroll_up(&mut self, rows: u16) {
        self.view.follow_bottom = false;
        self.view.scroll = self.view.scroll.saturating_sub(rows);
        if self.view.scroll == 0
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.paging.older_cursor().is_some())
            && self
                .pending
                .as_ref()
                .is_none_or(|request| request.read != ReadKind::Older)
        {
            self.older = true;
        }
    }

    fn scroll_down(&mut self, rows: u16) {
        let bottom = self.view.total.saturating_sub(self.view.height);
        self.view.scroll = self.view.scroll.saturating_add(rows).min(bottom);
        self.view.follow_bottom = self.view.scroll == bottom;
    }

    pub(super) fn render(&mut self, frame: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        let header = Rect { height: 1, ..area };
        let title = self.description.as_ref().map_or_else(
            || " Subagent".to_owned(),
            |label| format!(" Subagent  {label}"),
        );
        frame.render_widget(
            Paragraph::new(fit_middle_columns(&title, usize::from(area.width))),
            header,
        );
        if area.height == 1 {
            return;
        }
        let footer = Rect {
            y: area.bottom() - 1,
            height: 1,
            ..area
        };
        let message = match (&self.error, &self.snapshot, &self.pending) {
            (Some(error), _, _) => format!(" Esc Back  ·  {error}"),
            (None, None, Some(_)) => " Esc Back  ·  Loading transcript…".into(),
            (None, None, None) => " Esc Back  ·  No transcript yet".into(),
            (None, Some(_), _) => " Esc Back".into(),
        };
        frame.render_widget(Paragraph::new(Line::styled(message, muted())), footer);

        let area = Rect {
            y: area.y + 1,
            height: area.height - 2,
            ..area
        };
        if area.is_empty() {
            return;
        }
        self.view.height = area.height;
        if self.view.width != area.width {
            let history = self
                .snapshot
                .as_ref()
                .map_or(&[][..], |snapshot| snapshot.history.as_slice());
            self.view.lines = transcript_to_lines(history, area.width);
            self.view.width = area.width;
        }
        let paragraph = Paragraph::new(self.view.lines.clone()).wrap(Wrap { trim: false });
        self.view.total = u16::try_from(paragraph.line_count(area.width)).unwrap_or(u16::MAX);
        if let Some(previous) = self.view.prepend_from.take() {
            self.view.scroll = self
                .view
                .scroll
                .saturating_add(self.view.total.saturating_sub(previous));
        }
        let bottom = self.view.total.saturating_sub(area.height);
        self.view.scroll = if self.view.follow_bottom {
            bottom
        } else {
            self.view.scroll.min(bottom)
        };
        frame.render_widget(paragraph.scroll((self.view.scroll, 0)), area);
    }
}

fn load_snapshot(
    read: ReadKind,
    child: &str,
    request: &mut dyn FnMut(&str, Value) -> Result<Value, String>,
    paging: Option<TranscriptPagingState>,
    mut answers: HashMap<String, Answer>,
) -> Result<Snapshot, String> {
    let mut paging = match paging {
        Some(paging) => paging,
        None => TranscriptPagingState::open(request_transcript_page_with(
            child,
            None,
            None,
            None,
            |value| request("transcript/page", value),
            || thread::sleep(TRANSCRIPT_PROJECTION_POLL_INTERVAL),
        )?)?,
    };
    match read {
        ReadKind::Open => {}
        ReadKind::Older => {
            if let Some(cursor) = paging.older_cursor() {
                let page = request_transcript_page_with(
                    child,
                    Some(paging.projection_generation()),
                    Some(paging.source_high_water()),
                    Some(cursor),
                    |value| request("transcript/page", value),
                    || thread::sleep(TRANSCRIPT_PROJECTION_POLL_INTERVAL),
                )?;
                paging.apply_older_page(page)?;
            }
        }
        ReadKind::Update => {
            request_transcript_patches_with(
                &mut paging,
                |value| request("transcript/patches", value),
                || thread::sleep(TRANSCRIPT_PROJECTION_POLL_INTERVAL),
            )?;
        }
    }
    let mut history = paging.materialize_history(false);
    let mut budget = MAX_ANSWER_BYTES;
    // Prefer the latest answers when a page contains more referenced content than one read can display.
    for item in history.iter_mut().rev() {
        let TranscriptLine::ReferencedAnswer { key } = item.content() else {
            continue;
        };
        let Some(reference) = paging.assistant_answer_reference(key) else {
            continue;
        };
        let bytes = reference
            .byte_length
            .parse::<usize>()
            .map_err(|_| "invalid answer byte length")?;
        if bytes <= budget
            && answers
                .get(key)
                .is_none_or(|answer| answer.reference != reference)
        {
            let text = read_answer(&paging, request, &reference)?;
            budget = budget.saturating_sub(text.len());
            answers.insert(
                key.clone(),
                Answer {
                    reference: reference.clone(),
                    text,
                },
            );
        }
        let text = answers
            .get(key)
            .filter(|answer| answer.reference == reference)
            .map_or_else(
                || format!("[answer exceeds the transcript display budget: {bytes} bytes]"),
                |answer| answer.text.clone(),
            );
        *item.content_mut() = TranscriptLine::Summary(text);
    }
    Ok(Snapshot {
        paging,
        history,
        answers,
    })
}

fn read_answer(
    paging: &TranscriptPagingState,
    request: &mut dyn FnMut(&str, Value) -> Result<Value, String>,
    reference: &TranscriptContentRefV1,
) -> Result<String, String> {
    let mut offset = String::from("0");
    let mut text = String::new();
    loop {
        let response = request(
            "transcript/content-range",
            json!({"request": {
                "schema": "transcript.content.range.read.v1",
                "sessionId": paging.session_id(),
                "projectionVersion": TRANSCRIPT_PROJECTION_VERSION_V1,
                "projectionGeneration": paging.projection_generation(),
                "refId": reference.ref_id,
                "revision": reference.revision,
                "byteLength": reference.byte_length,
                "offset": offset,
                "maxBytes": TRANSCRIPT_CONTENT_RANGE_MAX_BYTES,
            }}),
        )?;
        let page: TranscriptContentRangeV1 =
            serde_json::from_value(response).map_err(|error| error.to_string())?;
        page.validate()?;
        if page.session_id != paging.session_id()
            || page.projection_generation != paging.projection_generation()
            || page.ref_id != reference.ref_id
            || page.revision != reference.revision
            || page.byte_length != reference.byte_length
            || page.start_offset != offset
            || (page.has_more && page.start_offset == page.end_offset)
        {
            return Err("Subagent answer content identity mismatch".into());
        }
        text.push_str(&page.content);
        if !page.has_more {
            return Ok(text);
        }
        offset = page.end_offset;
    }
}
