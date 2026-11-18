use super::*;
use crate::tool_projection::{tool_outcome, ToolOutcome};
use ratatui::style::Color;

pub(super) fn tool_header(tool: &ToolTranscriptLine) -> (Line<'static>, usize) {
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
    let failed = !tool.running
        && !tool.interrupted
        && matches!(
            tool_outcome(&tool.result_states),
            ToolOutcome::Failed | ToolOutcome::Denied
        );
    let mut spans = vec![Span::raw("  ")];
    let (verb, body) = if let Some(body) = title.strip_prefix("Failed to run ") {
        ("Failed to run", body)
    } else {
        title.split_once(' ').unwrap_or((&title, ""))
    };
    let mut indent = 2;
    spans.push(Span::styled(
        verb.to_string(),
        Style::default()
            .fg(if failed { Color::Red } else { theme().muted })
            .add_modifier(Modifier::BOLD),
    ));
    if !body.is_empty() {
        spans.push(Span::raw(" "));
        if tool.action_kind == ToolActionKind::Command {
            indent += verb.len() + 1;
            spans.extend(command_spans(body));
        } else {
            spans.push(Span::raw(body.to_string()));
        }
    }
    (Line::from(spans), indent)
}

pub(super) fn bash_header(tool: &ToolTranscriptLine, width: u16, expanded: bool) -> Line<'static> {
    let (verb, color) = if tool.running {
        ("Running", theme().muted)
    } else if tool.interrupted {
        ("Stopped", theme().muted)
    } else {
        match tool_outcome(&tool.result_states) {
            ToolOutcome::Succeeded => ("Ran", theme().muted),
            ToolOutcome::Failed => ("Failed", Color::Red),
            ToolOutcome::Denied => ("Denied", Color::Red),
            ToolOutcome::Aborted => ("Stopped", theme().muted),
        }
    };
    let subject = if tool.description_title {
        let subject = tool.subject.trim();
        subject
            .split_once(' ')
            .filter(|(word, _)| {
                ["run", "running", "ran"]
                    .iter()
                    .any(|verb| word.eq_ignore_ascii_case(verb))
            })
            .map_or(subject, |(_, remaining)| remaining.trim())
    } else {
        &tool.subject
    };
    let subject = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    let subject = if subject.is_empty() {
        tool.command
            .as_deref()
            .unwrap_or("Bash")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        subject
    };
    let status = tool
        .exit_code
        .filter(|code| *code != 0)
        .map_or_else(String::new, |code| format!(" (exit {code})"));
    let marker = if expanded { "⌄ " } else { "› " };
    let width = usize::from(width.saturating_sub(1));
    let chrome = 2 + verb.len() + 1 + status.len();
    let style = Style::default().fg(color);
    if width <= chrome {
        let label = format!("{marker}{verb}");
        let label_width = width.saturating_sub(status.len());
        return Line::styled(
            format!(
                "{}{}",
                fit_middle_columns(&label, label_width),
                fit_middle_columns(&status, width)
            ),
            style,
        );
    }
    Line::from(vec![
        Span::styled(marker, Style::default().fg(theme().muted)),
        Span::styled(format!("{verb} "), style),
        Span::raw(fit_middle_columns(&subject, width - chrome)),
        Span::styled(status, style),
    ])
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
