use super::*;
use crate::tool_projection::{tool_outcome, ToolOutcome};
use ratatui::style::Color;

fn tool_header(tool: &ToolTranscriptLine) -> Line<'static> {
    // Descriptions remain in the projection; the transcript heading shows the actual command.
    let title = if tool.action_kind == ToolActionKind::Command {
        if let Some(command) = tool.command.as_deref() {
            let mut display = tool.clone();
            display.subject = command.into();
            display.description_title = false;
            stable_tool_title(&display)
        } else {
            stable_tool_title(tool)
        }
    } else {
        stable_tool_title(tool)
    };
    let color = if tool.running {
        theme().muted
    } else if tool.interrupted {
        Color::Yellow
    } else {
        match tool_outcome(&tool.result_states) {
            ToolOutcome::Succeeded => Color::Green,
            ToolOutcome::Failed | ToolOutcome::Denied => Color::Red,
            ToolOutcome::Aborted => Color::Yellow,
        }
    };
    let mut spans = vec![Span::styled("● ", Style::default().fg(color))];
    let (verb, body) = title.split_once(' ').unwrap_or((&title, ""));
    spans.push(Span::styled(
        verb.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    if !body.is_empty() {
        spans.push(Span::raw(" "));
        if tool.action_kind == ToolActionKind::Command {
            spans.extend(command_spans(body));
        } else {
            spans.push(Span::raw(body.to_string()));
        }
    }
    Line::from(spans)
}

// Conservative lexical accents shared by PowerShell and POSIX commands. This is
// not a shell parser: preserve every character, including unknown syntax.
fn command_spans(command: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = command.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let first = chars[i];
        i += 1;
        let color = if first == '\'' || first == '"' {
            while i < chars.len() {
                let ch = chars[i];
                i += 1;
                if ch == first {
                    break;
                }
                if matches!(ch, '\\' | '`') && i < chars.len() {
                    i += 1;
                }
            }
            Some(Color::Green)
        } else if first.is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            None
        } else if "|;&<>()".contains(first) {
            Some(Color::Magenta)
        } else {
            while i < chars.len() && !chars[i].is_whitespace() && !"|;&<>()\"'".contains(chars[i]) {
                i += 1;
            }
            match first {
                '-' => Some(Color::Cyan),
                '$' => Some(Color::Yellow),
                _ => None,
            }
        };
        let text: String = chars[start..i].iter().collect();
        spans.push(Span::styled(
            text,
            color.map_or(Style::default(), |fg| Style::default().fg(fg)),
        ));
    }
    spans
}

pub(super) fn tool_lines(tool: &ToolTranscriptLine, width: u16) -> Vec<Line<'static>> {
    let mut header = tool_header(tool);
    let marker = header.spans.remove(0);
    let mut lines =
        hard_wrap_input_lines(vec![header], usize::from(width.saturating_sub(2).max(1)));
    for (index, line) in lines.iter_mut().enumerate() {
        line.spans.insert(
            0,
            if index == 0 {
                marker.clone()
            } else {
                Span::raw("  ")
            },
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_accents_preserve_powershell_posix_and_unknown_text() {
        for command in [
            "Get-ChildItem -LiteralPath 'D:\\Projects' | Select-Object Name,FullName; rg --files docs",
            "printf '%s' \"$HOME\" && echo done",
            "unknown.exe /option 文件 `escaped $env:PATH",
        ] {
            let spans = command_spans(command);
            assert_eq!(spans.iter().map(|span| span.content.as_ref()).collect::<String>(), command);
        }
    }
}
