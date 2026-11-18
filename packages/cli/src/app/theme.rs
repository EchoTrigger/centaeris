use ratatui::style::{Color, Modifier, Style};

/// Terminal-native semantic palette. Content inherits the user's foreground;
/// Markdown code, shell tokens and status markers use terminal ANSI colors.
pub(super) struct Theme {
    pub(super) accent: Color,
    pub(super) muted: Color,
    pub(super) heading: Color,
    pub(super) code_bg: Color,
    pub(super) inline_code_fg: Color,
    pub(super) diff_add_bg: Color,
    pub(super) diff_delete_bg: Color,
    pub(super) ghost: Color,
    pub(super) timestamp: Color,
}

static THEME: Theme = Theme {
    accent: Color::Reset,
    muted: Color::DarkGray,
    heading: Color::Reset,
    code_bg: Color::Reset,
    inline_code_fg: Color::Green,
    diff_add_bg: Color::Reset,
    diff_delete_bg: Color::Reset,
    ghost: Color::DarkGray,
    timestamp: Color::DarkGray,
};

pub(super) fn theme() -> &'static Theme {
    &THEME
}

impl Theme {
    pub(super) fn composer_border(&self, focused: bool) -> Style {
        Style::default().fg(if focused { Color::Gray } else { self.muted })
    }

    pub(super) fn user_prompt(&self) -> Style {
        Style::default().bg(Color::Rgb(42, 42, 42)).fg(Color::White)
    }

    pub(super) fn selected_row(&self) -> Style {
        Style::default()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    }

    pub(super) fn hovered_row(&self) -> Style {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    }
}
