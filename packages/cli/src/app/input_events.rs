use std::{
    io,
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

// Native Windows paste has no start/end marker. Keep rapid text together until
// a short idle gap, before Enter or Tab can reach any panel's action handler.
// This is deliberately a heuristic: a plain line followed by Enter retains its
// normal submit meaning unless multiline content or a Windows drop path identifies paste.
const TEXT_IDLE: Duration = Duration::from_millis(12);
const READ_SLICE: Duration = Duration::from_millis(8);

#[derive(Default)]
pub(super) struct InputEvents {
    keys: Vec<KeyEvent>,
    last_text_at: Option<Instant>,
}

impl InputEvents {
    pub(super) fn poll(&mut self, timeout: Duration) -> io::Result<Vec<Event>> {
        self.poll_with(timeout, event::poll, event::read, Instant::now)
    }

    fn poll_with(
        &mut self,
        timeout: Duration,
        mut available: impl FnMut(Duration) -> io::Result<bool>,
        mut read: impl FnMut() -> io::Result<Event>,
        mut clock: impl FnMut() -> Instant,
    ) -> io::Result<Vec<Event>> {
        let now = clock();
        if available(Duration::ZERO)? {
            // A read slice may yield to rendering while a paste is still queued.
            // Consumer scheduling time is not evidence of an input idle gap.
            if self.last_text_at.is_some() {
                self.last_text_at = Some(now);
            }
        } else {
            let ready = self.flush_due(now);
            if !ready.is_empty() {
                return Ok(ready);
            }
            if !available(self.wait(timeout, now))? {
                return Ok(self.flush_due(clock()));
            }
        }

        let started = clock();
        let mut ready = self.push(read()?, started);
        while ready.is_empty() && clock().duration_since(started) < READ_SLICE {
            let now = clock();
            let remaining = READ_SLICE.saturating_sub(now.duration_since(started));
            if !available(self.wait(remaining, now))? {
                ready.extend(self.flush_due(clock()));
                break;
            }
            let event = read()?;
            ready.extend(self.push(event, clock()));
        }
        Ok(ready)
    }

    fn wait(&self, timeout: Duration, now: Instant) -> Duration {
        self.last_text_at.map_or(timeout, |last| {
            timeout.min(TEXT_IDLE.saturating_sub(now.duration_since(last)))
        })
    }

    pub(super) fn push(&mut self, event: Event, now: Instant) -> Vec<Event> {
        // Releases add no text and must not split or extend a paste burst.
        if matches!(&event, Event::Key(key) if key.kind == KeyEventKind::Release) {
            return self.flush_due(now);
        }
        let mut ready = self.flush_due(now);
        match event {
            Event::Key(key) if self.text_character(key).is_some() => {
                self.keys.push(key);
                self.last_text_at = Some(now);
            }
            event => {
                ready.extend(self.flush());
                ready.push(event);
            }
        }
        ready
    }

    pub(super) fn flush_due(&mut self, now: Instant) -> Vec<Event> {
        if self
            .last_text_at
            .is_some_and(|last| now.duration_since(last) >= TEXT_IDLE)
        {
            self.flush()
        } else {
            Vec::new()
        }
    }

    fn text_character(&self, key: KeyEvent) -> Option<char> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        if key.code == KeyCode::Char('j') && key.modifiers == KeyModifiers::CONTROL {
            return self
                .keys
                .last()
                .filter(|previous| {
                    previous.code == KeyCode::Enter || previous.code == KeyCode::Char('\r')
                })
                .map(|_| '\n');
        }
        key_character(key)
    }

    fn flush(&mut self) -> Vec<Event> {
        self.last_text_at = None;
        let keys = std::mem::take(&mut self.keys);
        if keys.is_empty() {
            return Vec::new();
        }
        let raw: String = keys
            .iter()
            .filter_map(|key| {
                if key.code == KeyCode::Char('j') && key.modifiers == KeyModifiers::CONTROL {
                    Some('\n')
                } else {
                    key_character(*key)
                }
            })
            .collect();
        let mut after_break = false;
        let mut multiline = false;
        for character in raw.chars() {
            if matches!(character, '\r' | '\n') {
                after_break = true;
            } else if after_break {
                multiline = true;
            }
        }
        let path_drop = cfg!(windows) && keys.len() >= 8 && absolute_drop_path(&raw);
        if path_drop || raw.contains("\r\n") || keys.len() >= 3 && (multiline || raw.contains('\t'))
        {
            vec![Event::Paste(normalize_newlines(raw))]
        } else {
            keys.into_iter().map(Event::Key).collect()
        }
    }
}

fn absolute_drop_path(text: &str) -> bool {
    let text = text.trim();
    let path = text
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(text);
    std::path::Path::new(path).is_absolute()
}

fn key_character(key: KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(character)
            if key.modifiers.is_empty()
                || key.modifiers == KeyModifiers::SHIFT
                || cfg!(windows)
                    && key.modifiers.difference(KeyModifiers::SHIFT)
                        == (KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(character)
        }
        KeyCode::Enter if key.modifiers.is_empty() => Some('\r'),
        KeyCode::Tab if key.modifiers.is_empty() => Some('\t'),
        _ => None,
    }
}

pub(super) fn normalize_newlines(text: String) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn burst(events: Vec<Event>) -> Vec<Event> {
        let mut input = InputEvents::default();
        let start = Instant::now();
        let mut ready = Vec::new();
        for event in events {
            ready.extend(input.push(event, start));
        }
        ready.extend(input.flush_due(start + Duration::from_millis(20)));
        ready
    }

    #[test]
    fn rapid_multiline_keys_include_the_trailing_enter_in_one_paste() {
        let text = "| 中文 | 👩‍💻 |\n| e\u{301} | 2 |\n";
        let events = text
            .chars()
            .flat_map(|character| {
                let code = if character == '\n' {
                    KeyCode::Enter
                } else {
                    KeyCode::Char(character)
                };
                [
                    key(code),
                    Event::Key(KeyEvent::new_with_kind(
                        code,
                        KeyModifiers::NONE,
                        KeyEventKind::Release,
                    )),
                ]
            })
            .collect();
        assert_eq!(burst(events), vec![Event::Paste(text.into())]);
    }

    #[test]
    fn crlf_is_one_newline_and_tabs_do_not_become_completion_keys() {
        assert_eq!(
            burst(vec![
                key(KeyCode::Char('a')),
                key(KeyCode::Enter),
                Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
                key(KeyCode::Char('b')),
                key(KeyCode::Enter)
            ]),
            vec![Event::Paste("a\nb\n".into())]
        );
        assert_eq!(
            burst(vec![
                key(KeyCode::Char('a')),
                key(KeyCode::Tab),
                key(KeyCode::Char('b'))
            ]),
            vec![Event::Paste("a\tb".into())]
        );
    }

    #[test]
    fn plain_typing_ime_text_and_submit_keep_their_key_events() {
        let events = "中文abc"
            .chars()
            .map(|character| key(KeyCode::Char(character)))
            .chain([key(KeyCode::Enter)])
            .collect::<Vec<_>>();
        assert_eq!(burst(events.clone()), events);
        let mut input = InputEvents::default();
        let start = Instant::now();
        assert!(input.push(key(KeyCode::Char('a')), start).is_empty());
        assert_eq!(
            input.push(key(KeyCode::Enter), start + Duration::from_millis(30)),
            vec![key(KeyCode::Char('a'))]
        );
        assert_eq!(
            input.flush_due(start + Duration::from_millis(50)),
            vec![key(KeyCode::Enter)]
        );
    }

    #[cfg(windows)]
    #[test]
    fn rapid_windows_drop_paths_coalesce_without_consuming_prose() {
        for text in [
            r"C:\Pictures\image.png",
            r#""C:\My Pictures\截图.png""#,
            r#""\\server\share\image name.png""#,
        ] {
            let events = text
                .chars()
                .map(|character| {
                    Event::Key(KeyEvent::new(
                        KeyCode::Char(character),
                        if character == ':' || character == '"' {
                            KeyModifiers::SHIFT
                        } else {
                            KeyModifiers::NONE
                        },
                    ))
                })
                .collect();
            assert_eq!(burst(events), vec![Event::Paste(text.into())]);
        }
        for text in [r#"说明 "C:\Pictures\image.png""#, r"a.png", "hello world"] {
            let events = text
                .chars()
                .map(|character| key(KeyCode::Char(character)))
                .collect::<Vec<_>>();
            assert_eq!(burst(events.clone()), events);
        }
    }

    #[test]
    fn nontext_and_modified_keys_flush_pending_text_in_order() {
        let start = Instant::now();
        let mut input = InputEvents::default();
        input.push(key(KeyCode::Char('a')), start);
        input.push(key(KeyCode::Enter), start);
        input.push(key(KeyCode::Char('b')), start);
        let resize = Event::Resize(80, 24);
        assert_eq!(
            input.push(resize.clone(), start),
            vec![Event::Paste("a\nb".into()), resize]
        );
        let modified = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(input.push(modified.clone(), start), vec![modified]);
        let repeat = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        assert_eq!(input.push(repeat.clone(), start), vec![repeat]);
        let explicit = Event::Paste("explicit\n".into());
        assert_eq!(input.push(explicit.clone(), start), vec![explicit]);
    }

    #[test]
    fn queued_paste_survives_read_slices_and_a_slow_render() {
        use std::{
            cell::{Cell, RefCell},
            collections::VecDeque,
        };

        let queued = RefCell::new(VecDeque::from([
            key(KeyCode::Char('a')),
            key(KeyCode::Enter),
            key(KeyCode::Char('b')),
            key(KeyCode::Enter),
        ]));
        let time = Cell::new(Instant::now());
        let available = |_: Duration| Ok(!queued.borrow().is_empty());
        let read = || {
            time.set(time.get() + Duration::from_millis(5));
            Ok(queued.borrow_mut().pop_front().unwrap())
        };
        let mut input = InputEvents::default();
        assert!(input
            .poll_with(Duration::from_millis(100), available, read, || time.get())
            .unwrap()
            .is_empty());
        time.set(time.get() + Duration::from_millis(100));
        assert!(input
            .poll_with(Duration::from_millis(100), available, read, || time.get())
            .unwrap()
            .is_empty());
        time.set(time.get() + Duration::from_millis(100));
        assert_eq!(
            input
                .poll_with(Duration::from_millis(100), available, read, || time.get())
                .unwrap(),
            vec![Event::Paste("a\nb\n".into())]
        );
    }
}
