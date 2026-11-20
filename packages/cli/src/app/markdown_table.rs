use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use std::ops::Range;

use super::theme::theme;

pub(super) struct Table {
    pub range: Range<usize>,
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Line<'static>>>,
}

/// Retain a trailing run of pipe lines until its column widths can be computed
/// together. The live preview can still display these lines while more arrive.
pub(super) fn stable_prefix(text: &str, end: usize) -> usize {
    let mut start = end;
    let mut table_start = end;
    for line in text[..end].split_inclusive('\n').rev() {
        if line.trim().is_empty() {
            break;
        }
        start -= line.len();
        if line.contains('|') {
            table_start = start;
        }
    }
    table_start
}

pub(super) fn tables(text: &str, in_code_block: bool) -> Vec<Table> {
    if !text.contains('|') {
        return Vec::new();
    }
    // A streaming fragment may start inside a fence opened by the prior piece.
    let prefixed;
    let (source, prefix_len) = if in_code_block {
        prefixed = format!("```\n{text}");
        (prefixed.as_str(), 4)
    } else {
        (text, 0)
    };
    let mut tables = Vec::new();
    let mut active: Option<Table> = None;
    let mut depth = 0;
    let mut style = Style::default();
    for (event, range) in Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter() {
        if matches!(event, Event::End(_)) {
            depth -= 1;
        }
        match event {
            Event::Start(Tag::Table(alignments)) if depth == 0 => {
                active = Some(Table {
                    range: range.start - prefix_len..range.end - prefix_len,
                    alignments,
                    rows: Vec::new(),
                });
                depth += 1;
                continue;
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = active.take() {
                    tables.push(table);
                }
                continue;
            }
            _ => {}
        }
        if let Some(table) = active.as_mut() {
            match &event {
                Event::Start(Tag::TableHead | Tag::TableRow) => table.rows.push(Vec::new()),
                Event::Start(Tag::TableCell) => {
                    table
                        .rows
                        .last_mut()
                        .expect("table row")
                        .push(Line::default());
                    style = Style::default();
                }
                Event::Start(Tag::Strong) => style = style.add_modifier(Modifier::BOLD),
                Event::Start(Tag::Emphasis) => style = style.add_modifier(Modifier::ITALIC),
                Event::End(TagEnd::Strong) => style = style.remove_modifier(Modifier::BOLD),
                Event::End(TagEnd::Emphasis) => style = style.remove_modifier(Modifier::ITALIC),
                Event::Text(text) | Event::Code(text) | Event::InlineHtml(text) => {
                    if let Some(cell) = table.rows.last_mut().and_then(|row| row.last_mut()) {
                        let style = if matches!(event, Event::Code(_)) {
                            style.fg(theme().inline_code_fg)
                        } else {
                            style
                        };
                        cell.spans.push(Span::styled(text.to_string(), style));
                    }
                }
                _ => {}
            }
        }
        if matches!(event, Event::Start(_)) {
            depth += 1;
        }
    }
    tables
}

impl Table {
    pub fn render(self, width: u16) -> Vec<Line<'static>> {
        let mut widths = vec![1_usize; self.alignments.len()];
        for row in &self.rows {
            for (column, cell) in widths.iter_mut().zip(row) {
                *column = (*column).max(cell.width());
            }
        }
        if widths.iter().sum::<usize>() + 3 * widths.len().saturating_sub(1) > usize::from(width) {
            // Short windows wrap each logical row in the shared text layout.
            // Drop column padding but keep every cell's text and delimiters.
            widths.fill(1);
        }
        let divider = widths
            .iter()
            .map(|width| "─".repeat(*width))
            .collect::<Vec<_>>()
            .join("─┼─");
        let muted = Style::default().fg(theme().muted);
        let mut lines = Vec::new();
        for (index, row) in self.rows.into_iter().enumerate() {
            let mut spans = Vec::new();
            for (column, (&width, align)) in widths.iter().zip(&self.alignments).enumerate() {
                if column > 0 {
                    spans.push(Span::styled(" │ ", muted));
                }
                let cell = row.get(column);
                let pad = width.saturating_sub(cell.map_or(0, Line::width));
                let left_pad = match align {
                    Alignment::Right => pad,
                    Alignment::Center => pad / 2,
                    _ => 0,
                };
                if left_pad > 0 {
                    spans.push(Span::raw(" ".repeat(left_pad)));
                }
                if let Some(cell) = cell {
                    spans.extend(cell.spans.iter().cloned());
                }
                if column + 1 < widths.len() && pad > left_pad {
                    spans.push(Span::raw(" ".repeat(pad - left_pad)));
                }
            }
            let line = Line::from(spans);
            if index == 0 {
                lines.push(line.style(Style::default().add_modifier(Modifier::BOLD)));
                lines.push(Line::styled(divider.clone(), muted));
            } else {
                lines.push(line);
            }
        }
        lines
    }
}
