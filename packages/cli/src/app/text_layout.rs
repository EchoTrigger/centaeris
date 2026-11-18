use std::ops::Range;

use ratatui::{
    layout::Alignment,
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn range(self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

#[derive(Debug)]
struct Row {
    bytes: Range<usize>,
    stops: Vec<(usize, usize)>,
}

/// An immutable projection shared by drawing, pointer lookup and copying.
/// Byte offsets always address source text; wrapping and continuation padding
/// only add display cells.
#[derive(Debug)]
pub(super) struct TextLayout {
    pub lines: Vec<Line<'static>>,
    pub text: String,
    rows: Vec<Row>,
    width: usize,
}

impl TextLayout {
    pub fn plain(text: &str, width: u16) -> Self {
        Self::new(
            text.split('\n')
                .map(|line| Line::raw(line.to_owned()))
                .collect(),
            width,
            false,
        )
    }

    pub fn composer(text: &str, width: u16, images: &[Range<usize>]) -> Self {
        Self::build(
            text.split('\n')
                .map(|line| Line::raw(line.to_owned()))
                .collect(),
            width,
            false,
            images,
            &[],
            &[],
            &[],
        )
    }

    pub fn new(source: Vec<Line<'static>>, width: u16, hanging: bool) -> Self {
        Self::build(source, width, hanging, &[], &[], &[], &[])
    }

    pub fn transcript(
        source: Vec<Line<'static>>,
        width: u16,
        hanging_indents: &[(usize, usize)],
        decorations: &[(usize, usize)],
        spacing: &[usize],
    ) -> Self {
        Self::build(
            source,
            width,
            true,
            &[],
            hanging_indents,
            decorations,
            spacing,
        )
    }

    fn build(
        source: Vec<Line<'static>>,
        width: u16,
        hanging: bool,
        images: &[Range<usize>],
        hanging_indents: &[(usize, usize)],
        decorations: &[(usize, usize)],
        spacing: &[usize],
    ) -> Self {
        let width = usize::from(width.max(1));
        let mut result = Self {
            lines: Vec::new(),
            text: String::new(),
            rows: Vec::new(),
            width,
        };
        let mut hanging_indents = hanging_indents.iter().peekable();
        let mut decorations = decorations.iter().peekable();
        let mut spacing = spacing.iter().peekable();
        for (line_index, line) in source.into_iter().enumerate() {
            if line_index > 0 {
                result.text.push('\n');
            }
            let start = result.text.len();
            // Spacers take screen rows without adding line breaks to copied text.
            if spacing.next_if(|&&index| index == line_index).is_some() {
                result.rows.push(Row {
                    bytes: start..start,
                    stops: Vec::new(),
                });
                result.lines.push(Line::default());
            }
            let text = line.to_string();
            let decoration = decorations
                .next_if(|(index, _)| *index == line_index)
                .map_or(0, |(_, bytes)| *bytes);
            result.text.push_str(&text[decoration..]);
            let indent = if !hanging {
                0
            } else if hanging_indents
                .peek()
                .is_some_and(|(index, _)| *index == line_index)
            {
                hanging_indents.next().expect("indent for this line").1
            } else if text.starts_with("  │ ") {
                4
            } else if text.starts_with("● ") || text.starts_with("! ") || text.starts_with("│ ")
            {
                2
            } else {
                text.chars()
                    .take_while(|character| *character == ' ')
                    .count()
            }
            .min(width.saturating_sub(2));
            let mut styles = Vec::new();
            let mut end = 0;
            for span in &line.spans {
                end += span.content.len();
                styles.push((end, line.style.patch(span.style)));
            }
            let mut row_start = start;
            let mut columns = 0;
            let mut stops = Vec::new();
            let mut spans: Vec<Span<'static>> = Vec::new();
            let graphemes = text[..decoration].grapheme_indices(true).chain(
                text[decoration..]
                    .grapheme_indices(true)
                    .map(|(offset, text)| (offset + decoration, text)),
            );
            for (byte, grapheme) in graphemes {
                let cells = UnicodeWidthStr::width(grapheme);
                let offset = start + byte.saturating_sub(decoration);
                let required = images
                    .iter()
                    .find(|image| image.start == offset)
                    .map_or(cells, |image| {
                        UnicodeWidthStr::width(&text[byte..byte + image.len()])
                    });
                if columns > 0 && columns + required > width {
                    result.push_row(&line, row_start..offset, stops, spans, columns);
                    row_start = offset;
                    columns = indent;
                    stops = vec![(indent, row_start)];
                    spans = if indent == 0 {
                        Vec::new()
                    } else {
                        vec![Span::styled(" ".repeat(indent), line.style)]
                    };
                }
                if byte >= decoration && stops.is_empty() {
                    stops.push((columns, offset));
                }
                let style = styles
                    .iter()
                    .find(|(end, _)| *end > byte)
                    .map_or(line.style, |(_, style)| *style);
                if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
                    last.content.to_mut().push_str(grapheme);
                } else {
                    spans.push(Span::styled(grapheme.to_owned(), style));
                }
                columns += cells;
                if byte >= decoration {
                    stops.push((columns, offset + grapheme.len()));
                }
            }
            if stops.is_empty() {
                stops.push((columns, row_start));
            }
            result.push_row(&line, row_start..result.text.len(), stops, spans, columns);
        }
        result
    }

    fn push_row(
        &mut self,
        source: &Line<'static>,
        bytes: Range<usize>,
        mut stops: Vec<(usize, usize)>,
        spans: Vec<Span<'static>>,
        columns: usize,
    ) {
        let padding = match source.alignment {
            Some(Alignment::Right) => self.width.saturating_sub(columns),
            Some(Alignment::Center) => self.width.saturating_sub(columns) / 2,
            _ => 0,
        };
        for (column, _) in &mut stops {
            *column += padding;
        }
        self.rows.push(Row { bytes, stops });
        self.lines.push(Line {
            spans,
            style: source.style,
            alignment: source.alignment,
        });
    }

    pub fn height(&self) -> usize {
        self.rows.len()
    }

    pub fn offset_at(&self, row: usize, column: usize) -> usize {
        let Some(row) = self.rows.get(row.min(self.rows.len().saturating_sub(1))) else {
            return 0;
        };
        for pair in row.stops.windows(2) {
            let [(left, before), (right, after)] = pair else {
                unreachable!()
            };
            if column < *right {
                return if column.saturating_sub(*left) * 2 < right - left {
                    *before
                } else {
                    *after
                };
            }
        }
        row.bytes.end
    }

    pub fn byte_under(&self, row: usize, column: usize) -> usize {
        let Some(row) = self.rows.get(row) else {
            return self.text.len();
        };
        row.stops
            .windows(2)
            .find(|pair| pair[0].0 <= column && column < pair[1].0)
            .map_or(row.bytes.end, |pair| pair[0].1)
    }

    pub fn position(&self, byte: usize) -> (usize, usize) {
        let row = self
            .rows
            .partition_point(|row| row.bytes.start <= byte)
            .saturating_sub(1);
        let column = self
            .rows
            .get(row)
            .and_then(|row| row.stops.iter().find(|(_, offset)| *offset >= byte))
            .map_or(0, |(column, _)| *column);
        (row, column)
    }

    pub fn caret(&self, byte: usize) -> (usize, usize) {
        let (row, column) = self.position(byte);
        if column >= self.width {
            (row + 1, 0)
        } else {
            (row, column)
        }
    }

    pub fn selected(&self, selection: Selection) -> Option<String> {
        let range = selection.range();
        (!range.is_empty())
            .then(|| self.text.get(range).map(str::to_owned))
            .flatten()
    }

    pub fn selected_cells(&self, selection: Selection) -> Vec<(usize, Range<usize>)> {
        let range = selection.range();
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let from = range.start.max(row.bytes.start);
                let to = range.end.min(row.bytes.end);
                if from >= to {
                    return None;
                }
                let column = |byte| {
                    row.stops
                        .iter()
                        .find(|(_, offset)| *offset >= byte)
                        .map_or(0, |(column, _)| *column)
                };
                Some((index, column(from)..column(to)))
            })
            .collect()
    }

    pub fn semantic_selection(&self, byte: usize, line: bool) -> Selection {
        let mut start = self.text[..byte].rfind('\n').map_or(0, |index| index + 1);
        let mut end = self.text[byte..]
            .find('\n')
            .map_or(self.text.len(), |index| byte + index);
        if !line {
            if let Some((index, word)) = self
                .text
                .unicode_word_indices()
                .find(|(index, word)| *index <= byte && byte < index + word.len())
            {
                start = index;
                end = index + word.len();
            } else {
                start = byte;
                end = self.text[byte..]
                    .graphemes(true)
                    .next()
                    .map_or(byte, |grapheme| byte + grapheme.len());
            }
        }
        Selection {
            anchor: start,
            head: end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_cells_of_a_wide_grapheme_select_the_same_text() {
        let layout = TextLayout::plain("ab👩‍💻cd", 20);
        for column in [2, 3] {
            let selection = layout.semantic_selection(layout.byte_under(0, column), false);
            assert_eq!(layout.selected(selection).as_deref(), Some("👩‍💻"));
        }
    }

    #[test]
    fn image_placeholder_moves_to_the_next_row_as_one_unit() {
        let text = "prefix [Image #1] tail";
        let start = "prefix ".len();
        let end = start + "[Image #1]".len();
        let image = start..end;
        let layout = TextLayout::composer(text, 12, std::slice::from_ref(&image));
        assert_eq!(layout.lines[0].to_string(), "prefix ");
        assert!(layout.lines[1].to_string().starts_with("[Image #1]"));
        assert_eq!(
            layout
                .selected(Selection {
                    anchor: start,
                    head: end
                })
                .as_deref(),
            Some("[Image #1]")
        );
    }

    #[test]
    fn reflow_preserves_graphemes_and_copies_only_source_line_breaks() {
        let text = "中文e\u{301}👩‍💻 word\nnext";
        let narrow = TextLayout::plain(text, 5);
        let selection = Selection {
            anchor: 0,
            head: text.len(),
        };
        assert_eq!(narrow.selected(selection).as_deref(), Some(text));
        for row in 0..narrow.height() {
            for column in 0..5 {
                assert!(text.is_char_boundary(narrow.offset_at(row, column)));
            }
        }
        let wide = TextLayout::plain(text, 40);
        assert_eq!(wide.selected(selection), narrow.selected(selection));
        assert_eq!(
            wide.semantic_selection(text.find("word").unwrap(), false)
                .range(),
            "中文e\u{301}👩‍💻 ".len()..text.len() - 5
        );
    }
    #[test]
    fn continuation_padding_has_no_source_bytes() {
        let layout = TextLayout::new(vec![Line::raw("● abcdefgh")], 6, true);
        assert!(layout
            .lines
            .iter()
            .skip(1)
            .all(|line| line.to_string().starts_with("  ")));
        assert_eq!(
            layout
                .selected(Selection {
                    anchor: "● ".len(),
                    head: layout.text.len()
                })
                .as_deref(),
            Some("abcdefgh")
        );
    }
}
