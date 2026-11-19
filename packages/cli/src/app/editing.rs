use super::*;
use unicode_segmentation::UnicodeSegmentation;

pub(super) struct ComposerDraft {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    images: Vec<DraftImageAttachment>,
    next_image_number: usize,
    retired_images: HashSet<PathBuf>,
    history: History,
}

impl ComposerDraft {
    pub fn take(app: &mut App) -> Self {
        app.session.input_layout = None;
        app.session.input_scroll = 0;
        app.session.input_scroll_override = false;
        Self {
            text: std::mem::take(&mut app.session.input),
            cursor: std::mem::take(&mut app.session.input_cursor),
            anchor: app.session.input_selection_anchor.take(),
            images: std::mem::take(&mut app.session.draft_image_attachments),
            next_image_number: std::mem::replace(&mut app.session.next_image_number, 1),
            retired_images: std::mem::take(&mut app.session.retired_image_paths),
            history: std::mem::take(&mut app.session.edit_history),
        }
    }

    pub fn restore(self, app: &mut App) {
        clear_composer(app);
        app.session.input = self.text;
        app.session.input_cursor = self.cursor;
        app.session.input_selection_anchor = self.anchor;
        app.session.draft_image_attachments = self.images;
        app.session.next_image_number = self.next_image_number;
        app.session.retired_image_paths = self.retired_images;
        app.session.edit_history = self.history;
    }
}

#[derive(Clone)]
struct Snapshot {
    text: String,
    cursor: usize,
    anchor: Option<usize>,
    images: Vec<DraftImageAttachment>,
}

impl Snapshot {
    fn capture(app: &App) -> Self {
        Self {
            text: app.session.input.clone(),
            cursor: app.session.input_cursor,
            anchor: app.session.input_selection_anchor,
            images: app.session.draft_image_attachments.clone(),
        }
    }
    fn restore(self, app: &mut App) {
        app.session.retired_image_paths.extend(
            app.session
                .draft_image_attachments
                .iter()
                .map(|image| image.local_path.clone()),
        );
        app.session.input = self.text;
        app.session.input_cursor = self.cursor;
        app.session.input_selection_anchor = self.anchor;
        app.session.draft_image_attachments = self.images;
        app.session.input_scroll_override = false;
    }
}

#[derive(Default)]
pub(super) struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
    epoch: u64,
}

impl History {
    pub fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
        self.epoch = self.epoch.wrapping_add(1);
    }
}

pub(super) fn event<T>(app: &mut App, action: impl FnOnce(&mut App) -> T) -> T {
    let before =
        (app.edit_depth == 0 && !app.secret_input_active()).then(|| Snapshot::capture(app));
    let epoch = app.session.edit_history.epoch;
    let foreground = app.foreground_generation;
    app.edit_depth += 1;
    let result = action(app);
    app.edit_depth -= 1;
    if let Some(before) = before.filter(|before| {
        foreground == app.foreground_generation
            && epoch == app.session.edit_history.epoch
            && !app.secret_input_active()
            && (before.text != app.session.input
                || before.images != app.session.draft_image_attachments)
    }) {
        app.session.edit_history.past.push(before);
        app.session.edit_history.future.clear();
        if app.session.edit_history.past.len() > 100 {
            app.session.edit_history.past.remove(0);
        }
    }
    result
}

pub(super) fn undo(app: &mut App, redo: bool) {
    if app.secret_input_active() {
        return;
    }
    let current = Snapshot::capture(app);
    let snapshot = if redo {
        app.session.edit_history.future.pop()
    } else {
        app.session.edit_history.past.pop()
    };
    if let Some(snapshot) = snapshot {
        if redo {
            app.session.edit_history.past.push(current);
        } else {
            app.session.edit_history.future.push(current);
        }
        snapshot.restore(app);
    }
}

pub(super) fn accepts_text(key: KeyEvent) -> bool {
    !key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        || cfg!(windows)
            && key.modifiers.difference(KeyModifiers::SHIFT)
                == (KeyModifiers::CONTROL | KeyModifiers::ALT)
}

pub(super) fn previous(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(byte, _)| byte)
}
pub(super) fn next(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .graphemes(true)
        .next()
        .map_or(cursor, |grapheme| cursor + grapheme.len())
}

pub(super) fn navigate(app: &mut App, key: KeyEvent) {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let anchor = app
        .session
        .input_selection_anchor
        .unwrap_or(app.session.input_cursor);
    let cursor = app.session.input_cursor;
    if !shift
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && input_selection_range(app).is_some()
        && matches!(key.code, KeyCode::Left | KeyCode::Right)
    {
        if key.code == KeyCode::Left {
            move_cursor_left(app);
        } else {
            move_cursor_right(app);
        }
        return;
    }
    app.session.input_selection_anchor = None;
    match key.code {
        KeyCode::Left | KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.session.input_cursor = if key.code == KeyCode::Left {
                app.session.input[..cursor]
                    .unicode_word_indices()
                    .next_back()
                    .map_or(0, |(byte, _)| byte)
            } else {
                app.session.input[cursor..]
                    .unicode_word_indices()
                    .next()
                    .map_or(app.session.input.len(), |(byte, word)| {
                        cursor + byte + word.len()
                    })
            };
            if let Some(image) = app.session.draft_image_attachments.iter().find(|image| {
                image.start < app.session.input_cursor && app.session.input_cursor < image.end
            }) {
                app.session.input_cursor = if key.code == KeyCode::Left {
                    image.start
                } else {
                    image.end
                };
            }
        }
        KeyCode::Left => move_cursor_left(app),
        KeyCode::Right => move_cursor_right(app),
        KeyCode::Home => app.session.input_cursor = 0,
        KeyCode::End => app.session.input_cursor = app.session.input.len(),
        KeyCode::Up | KeyCode::Down => {
            if let Some(layout) = app.session.input_layout.as_ref() {
                let (row, column) = layout.caret(input_display_offset(app, cursor));
                let target = if key.code == KeyCode::Up {
                    row.saturating_sub(1)
                } else {
                    row + 1
                };
                app.session.input_cursor =
                    input_source_offset(app, layout.offset_at(target, column));
            }
        }
        _ => {}
    }
    app.session.input_scroll_override = false;
    app.session.input_selection_anchor = shift.then_some(anchor);
}
