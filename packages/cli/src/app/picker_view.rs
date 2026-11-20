use ratatui::layout::Margin;

use super::*;

pub(super) fn content(frame: &mut Frame, area: Rect) -> Rect {
    frame.render_widget(Clear, area);
    area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    })
}

pub(super) fn title(frame: &mut Frame, area: Rect, title: impl Into<String>) {
    frame.render_widget(
        Paragraph::new(title.into()).style(Style::default().add_modifier(Modifier::BOLD)),
        area,
    );
}

pub(super) fn marker(selected: bool) -> Span<'static> {
    Span::raw(if selected { "❯ " } else { "  " })
}

pub(super) fn row(
    label: &str,
    description: String,
    label_columns: usize,
    selected: bool,
) -> ListItem<'static> {
    row_with_prefix(label, description, label_columns, selected, []).style(if selected {
        theme().selected_row()
    } else {
        Style::default()
    })
}

pub(super) fn row_with_prefix(
    label: &str,
    description: String,
    label_columns: usize,
    selected: bool,
    prefix: impl IntoIterator<Item = Span<'static>>,
) -> ListItem<'static> {
    let label = Span::raw(fit_middle_columns(label, label_columns));
    let padding = label_columns.saturating_sub(label.width());
    let mut spans = vec![marker(selected)];
    spans.extend(prefix);
    spans.extend([
        label,
        Span::raw(" ".repeat(padding + 2)),
        Span::styled(description, Style::default().fg(theme().muted)),
    ]);
    ListItem::new(Line::from(spans))
}

pub(super) fn text_area(area: Rect, count: usize) -> Rect {
    Rect {
        width: area
            .width
            .saturating_sub(u16::from(count > usize::from(area.height))),
        ..area
    }
}

pub(super) fn render_list(
    frame: &mut Frame,
    area: Rect,
    items: Vec<ListItem<'static>>,
    selected: usize,
) -> usize {
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(List::new(items), area, &mut state);
    state.offset()
}
