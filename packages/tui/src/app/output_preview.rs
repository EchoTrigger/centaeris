use super::*;
use centaeris_core::session::transcript::{
    TranscriptContentRangeV1, TranscriptContentRefV1, TRANSCRIPT_PROJECTION_VERSION_V1,
};
const PAGE_BYTES: usize = 16 * 1024;
pub(super) const PREVIEW_ROWS: usize = 6;

pub(super) struct OutputPreview {
    content: String,
    readable_range: Option<(u64, u64)>,
    identity: Option<(String, String, TranscriptContentRefV1)>,
    pending: Option<RuntimeResponse>,
    requested: String,
    has_more: bool,
    error: Option<String>,
}

const CACHE_ENTRIES: usize = 32;
const MAX_PENDING: usize = 4;

pub(super) fn prepare_visible(app: &mut App, view: &TranscriptView, height: u16) {
    let bottom = app.transcript_scroll.saturating_add(u64::from(height));
    let keys: Vec<String> = view
        .source_rows
        .iter()
        .enumerate()
        .filter_map(|(index, (key, row))| {
            let end = view
                .source_rows
                .get(index + 1)
                .map_or(view.total_rows, |(_, row)| *row);
            (*row < bottom && end > app.transcript_scroll).then(|| key.clone())
        })
        .take(CACHE_ENTRIES)
        .collect();
    for key in &keys {
        if app.output_preview.contains_key(key) {
            continue;
        }
        if app
            .output_preview
            .values()
            .filter(|detail| detail.pending.is_some())
            .count()
            >= MAX_PENDING
        {
            break;
        }
        let Some(tool) = app.transcript.iter().find_map(|line| match line.content() {
            TranscriptLine::Tool(tool) if &tool.key == key && tool.full_text.is_none() => {
                Some(tool)
            }
            _ => None,
        }) else {
            continue;
        };
        let Some((session, generation, reference)) =
            app.transcript_paging.as_ref().and_then(|state| {
                state.tool_output_reference(key).map(|reference| {
                    (
                        state.session_id().to_string(),
                        state.projection_generation().to_string(),
                        reference,
                    )
                })
            })
        else {
            continue;
        };
        if app.output_preview.len() >= CACHE_ENTRIES {
            let Some(evicted) = app
                .output_preview
                .keys()
                .find(|cached| !keys.contains(cached))
                .cloned()
            else {
                break;
            };
            app.output_preview.remove(&evicted);
        }
        let mut detail = inline_preview(tool);
        detail.identity = Some((session.clone(), generation.clone(), reference.clone()));
        let result = app.runtime.as_mut().ok_or_else(|| "Runtime is unavailable".to_string()).and_then(|runtime| runtime.request_async("transcript/content-range", json!({"request": {
            "schema":"transcript.content.range.read.v1", "sessionId":session,
            "projectionVersion":TRANSCRIPT_PROJECTION_VERSION_V1, "projectionGeneration":generation,
            "refId":reference.ref_id,"revision":reference.revision,"byteLength":reference.byte_length,
            "offset":detail.requested,"maxBytes":PAGE_BYTES,
        }})));
        match result {
            Ok(response) => detail.pending = Some(response),
            Err(error) => detail.error = Some(error),
        }
        app.output_preview.insert(key.clone(), detail);
        invalidate_transcript_layout(app);
    }
}

fn inline_preview(tool: &ToolTranscriptLine) -> OutputPreview {
    let mut detail = OutputPreview {
        content: String::new(),
        readable_range: tool.readable_range,
        identity: None,
        pending: None,
        requested: tool.readable_range.map_or(0, |r| r.0).to_string(),
        has_more: false,
        error: None,
    };
    if let Some(text) = &tool.full_text {
        let start = detail
            .requested
            .parse::<usize>()
            .unwrap_or(0)
            .min(text.len());
        let mut end = start.saturating_add(PAGE_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if let Some(content) = text.get(start..end) {
            detail.content = content.into();
            detail.has_more = end < text.len();
        } else {
            detail.error = Some("Invalid output body range".into());
        }
    } else {
        let mut lines = Vec::new();
        for block in &tool.result_blocks {
            match block {
                ToolResultBlock::Text { lines: source } => {
                    lines.extend(source.iter().map(|line| match line {
                        TextResultLine::Text(text) => text.clone(),
                        TextResultLine::Hidden(_) => "…".into(),
                    }))
                }
                ToolResultBlock::Diff {
                    rows, hidden_lines, ..
                } => {
                    lines.extend(rows.iter().map(|row| {
                        format!(
                            "{}{}",
                            match row.kind {
                                DiffRowKind::Insert => "+",
                                DiffRowKind::Delete => "-",
                                _ => " ",
                            },
                            row.text
                        )
                    }));
                    if *hidden_lines > 0 {
                        lines.push("…".into());
                    }
                }
            }
        }
        if lines.is_empty() {
            if let Some(text) = empty_tool_result_detail(tool) {
                lines.push(text);
            }
        }
        detail.content = lines.join("\n");
    }
    detail
}

fn accept_page(detail: &mut OutputPreview, value: Value) -> Result<(), String> {
    let page: TranscriptContentRangeV1 =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    page.validate()?;
    let (session, generation, reference) =
        detail.identity.as_ref().ok_or("Missing output identity")?;
    if &page.session_id != session
        || &page.projection_generation != generation
        || page.ref_id != reference.ref_id
        || page.revision != reference.revision
        || page.byte_length != reference.byte_length
        || page.start_offset != detail.requested
        || page.content.len() > PAGE_BYTES
        || (page.has_more && page.start_offset == page.end_offset)
    {
        return Err("Output page identity or range mismatch".into());
    }
    detail.content = page.content;
    detail.has_more = page.has_more;
    Ok(())
}
pub(super) fn poll(app: &mut App) -> bool {
    let before = app.output_preview.len();
    app.output_preview.retain(|key, detail| {
        let exists = app
            .transcript
            .iter()
            .any(|line| matches!(line.content(), TranscriptLine::Tool(tool) if &tool.key == key));
        exists
            && detail
                .identity
                .as_ref()
                .is_none_or(|(session, generation, reference)| {
                    app.transcript_paging.as_ref().is_some_and(|state| {
                        state.session_id() == session
                            && state.projection_generation() == generation
                            && state.tool_output_reference(key).as_ref() == Some(reference)
                    })
                })
    });
    let mut changed = before != app.output_preview.len();
    for detail in app.output_preview.values_mut() {
        let Some(pending) = detail.pending.as_ref() else {
            continue;
        };
        let result = match pending.try_recv() {
            Ok(None) => continue,
            Ok(Some(value)) => accept_page(detail, value),
            Err(error) => Err(error.to_string()),
        };
        detail.pending = None;
        if let Err(error) = result {
            detail.error = Some(error);
        }
        changed = true;
    }
    if changed {
        invalidate_transcript_layout(app);
    }
    changed
}

pub(super) fn preview_lines(
    app: &App,
    tool: &ToolTranscriptLine,
    width: u16,
) -> Vec<Line<'static>> {
    let inline = inline_preview(tool);
    let detail = app.output_preview.get(&tool.key).unwrap_or(&inline);
    if detail.pending.is_some() {
        return vec![Line::styled(
            "    └ Loading…",
            Style::default().fg(theme().muted),
        )];
    }
    if let Some(error) = &detail.error {
        return vec![Line::styled(
            format!("    └ Cannot read output: {error}"),
            Style::default().fg(theme().muted),
        )];
    }
    let start = detail.requested.parse::<u64>().unwrap_or(0);
    let available = detail
        .readable_range
        .map_or(detail.content.len(), |(offset, len)| {
            (offset.saturating_add(len).saturating_sub(start) as usize).min(detail.content.len())
        });
    let body = detail
        .content
        .get(..available)
        .unwrap_or("Invalid output body range");
    let usable = usize::from(width.saturating_sub(6).max(1));
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    for ch in body.chars() {
        if ch == '\n' {
            rows.push(std::mem::take(&mut row));
            used = 0;
        } else {
            let size = character_width(ch);
            if used > 0 && used + size > usable {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push(ch);
            used += size;
        }
        if rows.len() > PREVIEW_ROWS {
            break;
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    let more_content = detail.has_more
        && detail.readable_range.is_none_or(|(offset, len)| {
            start + (detail.content.len() as u64) < offset.saturating_add(len)
        });
    let omitted = rows.len() > PREVIEW_ROWS || more_content;
    rows.truncate(PREVIEW_ROWS);
    if omitted {
        rows.push("…".into());
    }
    rows.into_iter()
        .enumerate()
        .map(|(index, text)| {
            Line::styled(
                if text.trim().is_empty() {
                    String::new()
                } else {
                    format!("{}{}", if index == 0 { "    └ " } else { "      " }, text)
                },
                Style::default().fg(theme().muted),
            )
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_pages_validate_identity_utf8_bounds_and_progress() {
        let mut preview = OutputPreview {
            content: String::new(),
            readable_range: None,
            identity: Some((
                "s".into(),
                "g".into(),
                TranscriptContentRefV1 {
                    ref_id: "r".into(),
                    revision: "1".into(),
                    byte_length: "4".into(),
                },
            )),
            pending: None,
            requested: "0".into(),
            has_more: false,
            error: None,
        };
        let page = json!({"schema":"transcript.content.range.v1","sessionId":"s","projectionVersion":TRANSCRIPT_PROJECTION_VERSION_V1,
            "projectionGeneration":"g","refId":"r","revision":"1","byteLength":"4","startOffset":"0","endOffset":"3","content":"中","hasMore":true});
        accept_page(&mut preview, page.clone()).unwrap();
        assert_eq!(preview.content, "中");
        assert!(preview.has_more);
        let mut stale = page.clone();
        stale["projectionGeneration"] = json!("stale");
        assert!(accept_page(&mut preview, stale).is_err());
        let mut invalid = page;
        invalid["endOffset"] = json!("2");
        assert!(accept_page(&mut preview, invalid).is_err());
        assert_eq!(preview.content, "中");
    }
}
