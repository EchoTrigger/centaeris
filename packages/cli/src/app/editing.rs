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
        let mut history = std::mem::take(&mut app.edit_history);
        app.edit_history.depth = history.depth;
        history.depth = 0;
        app.input_layout = None;
        app.input_scroll = 0;
        app.input_scroll_override = false;
        Self {
            text: std::mem::take(&mut app.input),
            cursor: std::mem::take(&mut app.input_cursor),
            anchor: app.input_selection_anchor.take(),
            images: std::mem::take(&mut app.draft_image_attachments),
            next_image_number: std::mem::replace(&mut app.next_image_number, 1),
            retired_images: std::mem::take(&mut app.retired_image_paths),
            history,
        }
    }

    pub fn restore(mut self, app: &mut App) {
        clear_composer(app);
        self.history.depth = app.edit_history.depth;
        app.input = self.text;
        app.input_cursor = self.cursor;
        app.input_selection_anchor = self.anchor;
        app.draft_image_attachments = self.images;
        app.next_image_number = self.next_image_number;
        app.retired_image_paths = self.retired_images;
        app.edit_history = self.history;
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
            text: app.input.clone(),
            cursor: app.input_cursor,
            anchor: app.input_selection_anchor,
            images: app.draft_image_attachments.clone(),
        }
    }
    fn restore(self, app: &mut App) {
        app.retired_image_paths.extend(
            app.draft_image_attachments
                .iter()
                .map(|image| image.local_path.clone()),
        );
        app.input = self.text;
        app.input_cursor = self.cursor;
        app.input_selection_anchor = self.anchor;
        app.draft_image_attachments = self.images;
        app.input_scroll_override = false;
    }
}

#[derive(Default)]
pub(super) struct History {
    past: Vec<Snapshot>,
    future: Vec<Snapshot>,
    depth: usize,
    epoch: u64,
}

impl History {
    pub(super) fn swap_session(&mut self, other: &mut Self) {
        std::mem::swap(&mut self.past, &mut other.past);
        std::mem::swap(&mut self.future, &mut other.future);
        // A key event may switch sessions while its edit transaction is open.
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
        self.epoch = self.epoch.wrapping_add(1);
    }
}

pub(super) fn event<T>(app: &mut App, action: impl FnOnce(&mut App) -> T) -> T {
    let before =
        (app.edit_history.depth == 0 && !app.secret_input_active()).then(|| Snapshot::capture(app));
    let epoch = app.edit_history.epoch;
    app.edit_history.depth += 1;
    let result = action(app);
    app.edit_history.depth -= 1;
    if let Some(before) = before.filter(|before| {
        epoch == app.edit_history.epoch
            && !app.secret_input_active()
            && (before.text != app.input || before.images != app.draft_image_attachments)
    }) {
        app.edit_history.past.push(before);
        app.edit_history.future.clear();
        if app.edit_history.past.len() > 100 {
            app.edit_history.past.remove(0);
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
        app.edit_history.future.pop()
    } else {
        app.edit_history.past.pop()
    };
    if let Some(snapshot) = snapshot {
        if redo {
            app.edit_history.past.push(current);
        } else {
            app.edit_history.future.push(current);
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
    let anchor = app.input_selection_anchor.unwrap_or(app.input_cursor);
    let cursor = app.input_cursor;
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
    app.input_selection_anchor = None;
    match key.code {
        KeyCode::Left | KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.input_cursor = if key.code == KeyCode::Left {
                app.input[..cursor]
                    .unicode_word_indices()
                    .next_back()
                    .map_or(0, |(byte, _)| byte)
            } else {
                app.input[cursor..]
                    .unicode_word_indices()
                    .next()
                    .map_or(app.input.len(), |(byte, word)| cursor + byte + word.len())
            };
            if let Some(image) = app
                .draft_image_attachments
                .iter()
                .find(|image| image.start < app.input_cursor && app.input_cursor < image.end)
            {
                app.input_cursor = if key.code == KeyCode::Left {
                    image.start
                } else {
                    image.end
                };
            }
        }
        KeyCode::Left => move_cursor_left(app),
        KeyCode::Right => move_cursor_right(app),
        KeyCode::Home => app.input_cursor = 0,
        KeyCode::End => app.input_cursor = app.input.len(),
        KeyCode::Up | KeyCode::Down => {
            if let Some(layout) = app.input_layout.as_ref() {
                let (row, column) = layout.caret(input_display_offset(app, cursor));
                let target = if key.code == KeyCode::Up {
                    row.saturating_sub(1)
                } else {
                    row + 1
                };
                app.input_cursor = input_source_offset(app, layout.offset_at(target, column));
            }
        }
        _ => {}
    }
    app.input_scroll_override = false;
    app.input_selection_anchor = shift.then_some(anchor);
}
