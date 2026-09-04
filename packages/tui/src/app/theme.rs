use ratatui::style::Color;

/// Terminal-native semantic palette. Content inherits the user's foreground;
/// only state uses ANSI status colors.
pub(super) struct Theme {
    pub(super) accent: Color,
    pub(super) muted: Color,
    pub(super) success: Color,
    pub(super) error: Color,
    pub(super) warning: Color,
    pub(super) heading: Color,
    pub(super) code_bg: Color,
    pub(super) inline_code_bg: Color,
    pub(super) diff_add_bg: Color,
    pub(super) diff_delete_bg: Color,
    pub(super) ghost: Color,
    pub(super) timestamp: Color,
}

static THEME: Theme = Theme {
    accent: Color::Reset,
    muted: Color::DarkGray,
    success: Color::Green,
    error: Color::Red,
    warning: Color::Yellow,
    heading: Color::Reset,
    code_bg: Color::Reset,
    inline_code_bg: Color::Reset,
    diff_add_bg: Color::Reset,
    diff_delete_bg: Color::Reset,
    ghost: Color::DarkGray,
    timestamp: Color::DarkGray,
};

pub(super) fn theme() -> &'static Theme {
    &THEME
}
