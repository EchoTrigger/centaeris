use super::*;
use crossterm::cursor::{Hide, MoveTo, Show};
use ratatui_image::picker::Capability;
use std::io::Write;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Once,
};

static SURFACE_ACTIVE: AtomicBool = AtomicBool::new(false);

pub(super) struct Surface {
    pub(super) terminal: Terminal<CrosstermBackend<Vec<u8>>>,
    sync_frames: bool,
    caret: Option<Position>,
    frame_bytes: Vec<u8>,
}

impl Surface {
    pub(super) fn enter() -> io::Result<Self> {
        // Construct the backend before changing terminal modes.
        let terminal = Terminal::new(CrosstermBackend::new(Vec::new()))?;
        let mut surface = Self {
            terminal,
            caret: None,
            frame_bytes: Vec::new(),
            sync_frames: !env_set("TMUX")
                || [
                    "NVIM",
                    "NVIM_LISTEN_ADDRESS",
                    "VIM_TERMINAL",
                    "INSIDE_EMACS",
                ]
                .into_iter()
                .any(env_set),
        };
        install_panic_hook();
        surface.activate()?;
        Ok(surface)
    }

    fn activate(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        SURFACE_ACTIVE.store(true, Ordering::Relaxed);
        self.caret = None;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            Hide,
            SetCursorStyle::SteadyBar,
            SetTitle("Centaeris")
        )
    }

    pub(super) fn draw(
        &mut self,
        paint: impl FnOnce(&mut Frame) -> Option<Position>,
    ) -> io::Result<Rect> {
        self.terminal.autoresize()?;
        let (area, caret) = {
            let mut frame = self.terminal.get_frame();
            (frame.area(), paint(&mut frame))
        };
        self.terminal.flush()?;
        self.terminal.swap_buffers();

        let cells = self.terminal.backend_mut().writer_mut();
        self.frame_bytes.clear();
        encode_frame(
            &mut self.frame_bytes,
            cells,
            self.caret,
            caret,
            self.sync_frames,
            cfg!(windows),
        )?;
        cells.clear();
        let mut stdout = io::stdout().lock();
        stdout.write_all(&self.frame_bytes)?;
        stdout.flush()?;
        self.caret = caret;
        Ok(area)
    }

    pub(super) fn leave(&mut self) -> io::Result<()> {
        restore_surface()
    }

    pub(super) fn clear_video_images(&mut self, ids: Vec<u32>) -> io::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut output = io::stdout().lock();
        for id in ids {
            write!(output, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\")?;
        }
        output.flush()
    }

    pub(super) fn with_suspended<T>(&mut self, run: impl FnOnce() -> T) -> io::Result<T> {
        self.leave()?;
        let result = run();
        self.activate()?;
        self.terminal.clear()?;
        Ok(result)
    }
}

fn encode_frame(
    output: &mut Vec<u8>,
    cells: &[u8],
    previous: Option<Position>,
    caret: Option<Position>,
    synchronized: bool,
    windows: bool,
) -> io::Result<()> {
    if cells.is_empty() && previous == caret {
        return Ok(());
    }
    if synchronized {
        output.queue(BeginSynchronizedUpdate)?;
    }
    // The diff moves the terminal cursor through output cells. Keep the caret
    // hidden during those moves where synchronized drawing cannot be assumed.
    let hide = previous.is_some()
        && (caret.is_none() || (!cells.is_empty() && (windows || !synchronized)));
    if hide {
        write!(output, "{Hide}")?;
    }
    output.extend_from_slice(cells);
    if let Some(caret) = caret {
        write!(output, "{}", MoveTo(caret.x, caret.y))?;
        if previous.is_none() || hide {
            write!(output, "{Show}")?;
        }
    }
    if synchronized {
        output.queue(EndSynchronizedUpdate)?;
    }
    Ok(())
}

impl Drop for Surface {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

fn restore_surface() -> io::Result<()> {
    if !SURFACE_ACTIVE.swap(false, Ordering::Relaxed) {
        return Ok(());
    }
    let raw = disable_raw_mode();
    let screen = execute!(
        io::stdout(),
        EndSynchronizedUpdate,
        DisableMouseCapture,
        DisableBracketedPaste,
        SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen,
        Show
    );
    raw.and(screen)
}

fn install_panic_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        // Release uses panic = "abort", so Drop alone cannot restore the surface.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = restore_surface();
            previous(info);
        }));
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Brand {
    Kitty,
    Ghostty,
    WezTerm,
    Warp,
    Iterm2,
    Unknown,
}

fn env_set(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty())
}

fn brand(term: &str) -> Brand {
    let program = env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match program.as_str() {
        "kitty" => Brand::Kitty,
        "ghostty" => Brand::Ghostty,
        "wezterm" => Brand::WezTerm,
        "warp" | "warpterminal" => Brand::Warp,
        "iterm2" | "iterm.app" => Brand::Iterm2,
        "" if term == "xterm-ghostty" => Brand::Ghostty,
        "" if term == "wezterm" || env_set("WEZTERM_PANE") => Brand::WezTerm,
        "" if term == "xterm-kitty" || env_set("KITTY_WINDOW_ID") => Brand::Kitty,
        "" if env::var("LC_TERMINAL").is_ok_and(|value| value.eq_ignore_ascii_case("iterm2"))
            || env_set("ITERM_SESSION_ID") =>
        {
            Brand::Iterm2
        }
        _ => Brand::Unknown,
    }
}

pub(super) struct ImagePolicy {
    pub(super) picker: Picker,
    pub(super) scrollback: bool,
}

fn image_protocol(
    brand: Brand,
    blocked: bool,
    kitty_negotiated: bool,
    term_features: Option<&str>,
    ssh: bool,
) -> (ProtocolType, bool) {
    if blocked {
        return (ProtocolType::Halfblocks, false);
    }
    match brand {
        Brand::Kitty | Brand::Ghostty | Brand::WezTerm if kitty_negotiated => {
            (ProtocolType::Kitty, true)
        }
        Brand::Warp if kitty_negotiated => (ProtocolType::Kitty, false),
        Brand::Iterm2 if term_features.map_or(ssh, |features| features.contains('F')) => {
            (ProtocolType::Iterm2, false)
        }
        _ => (ProtocolType::Halfblocks, false),
    }
}

impl Default for ImagePolicy {
    fn default() -> Self {
        let mut picker = Picker::from_fontsize((10, 20));
        picker.set_protocol_type(ProtocolType::Halfblocks);
        Self {
            picker,
            scrollback: false,
        }
    }
}

impl ImagePolicy {
    pub(super) fn detect() -> Result<Self, String> {
        let term = env::var("TERM").unwrap_or_default();
        let blocked = cfg!(windows)
            || env_set("TMUX")
            || term.starts_with("tmux")
            || env::var("TERM_PROGRAM").is_ok_and(|program| program == "tmux");
        if blocked {
            return Ok(Self::default());
        }
        let brand = brand(&term);
        let features = env::var("TERM_FEATURES").ok();
        let ssh = env_set("SSH_CONNECTION") || env_set("SSH_CLIENT") || env_set("SSH_TTY");
        let (eligible, _) = image_protocol(brand, blocked, true, features.as_deref(), ssh);
        if eligible == ProtocolType::Halfblocks {
            return Ok(Self::default());
        }
        let mut picker = Picker::from_query_stdio()
            .map_err(|error| format!("detect terminal image protocol failed: {error}"))?;
        // Picker also guesses from the environment. Only accept a Kitty capability reply.
        let (draft, scrollback) = image_protocol(
            brand,
            blocked,
            picker.capabilities().contains(&Capability::Kitty),
            features.as_deref(),
            ssh,
        );
        picker.set_protocol_type(draft);
        Ok(Self { picker, scrollback })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_through_changed_cells_only_shows_the_caret_at_the_editor() {
        let mut bytes = Vec::new();
        let caret = Position::new(3, 12);
        encode_frame(
            &mut bytes,
            b"thinking",
            Some(caret),
            Some(caret),
            false,
            true,
        )
        .unwrap();
        assert_eq!(bytes, b"\x1b[?25lthinking\x1b[13;4H\x1b[?25h");
        bytes.clear();
        encode_frame(&mut bytes, b"list", Some(caret), None, false, true).unwrap();
        assert_eq!(bytes, b"\x1b[?25llist");
    }

    #[test]
    fn terminal_image_policy_gates_draft_and_scrollback_independently() {
        for brand in [Brand::Kitty, Brand::Ghostty, Brand::WezTerm] {
            assert_eq!(
                image_protocol(brand, false, true, None, false),
                (ProtocolType::Kitty, true)
            );
            assert_eq!(
                image_protocol(brand, false, false, None, false),
                (ProtocolType::Halfblocks, false)
            );
            assert_eq!(
                image_protocol(brand, true, true, None, false),
                (ProtocolType::Halfblocks, false)
            );
        }
        assert_eq!(
            image_protocol(Brand::Warp, false, true, None, false),
            (ProtocolType::Kitty, false)
        );
        assert_eq!(
            image_protocol(Brand::Unknown, false, true, Some("F"), true),
            (ProtocolType::Halfblocks, false)
        );
        assert_eq!(
            image_protocol(Brand::Iterm2, false, false, Some("F"), false),
            (ProtocolType::Iterm2, false)
        );
        assert_eq!(
            image_protocol(Brand::Iterm2, false, false, None, true),
            (ProtocolType::Iterm2, false)
        );
        assert_eq!(
            image_protocol(Brand::Iterm2, false, false, Some(""), true),
            (ProtocolType::Halfblocks, false)
        );
        assert_eq!(
            image_protocol(Brand::Iterm2, false, false, None, false),
            (ProtocolType::Halfblocks, false)
        );
    }
}
