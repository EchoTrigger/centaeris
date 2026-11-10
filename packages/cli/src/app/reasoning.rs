use super::*;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReasoningPayload {
    block_id: String,
    request_id: String,
    text: String,
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotReasoning {
    block_id: String,
    request_id: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    revision: u64,
    text: String,
    reasoning: Option<SnapshotReasoning>,
}

fn replace_reasoning(app: &mut App, key: String, text: String) {
    if let Some(TranscriptLine::Reasoning { text: previous, .. }) = app.transcript.iter_mut().find(
        |line| matches!(line, TranscriptLine::Reasoning { key: existing, .. } if existing == &key),
    ) {
        *previous = text;
    } else {
        app.transcript.push(TranscriptLine::Reasoning {
            key,
            text,
            final_started: false,
        });
    }
    invalidate_transcript_layout(app);
}

pub(super) fn apply_reasoning(app: &mut App, payload: &Value) -> Result<(), String> {
    let value: ReasoningPayload = serde_json::from_value(payload.clone())
        .map_err(|_| "Reasoning payload fields are invalid".to_string())?;
    if value.request_id.is_empty()
        || value.block_id.is_empty()
        || !matches!(value.status.as_str(), "streaming" | "done" | "interrupted")
    {
        return Err("Reasoning identity or status is invalid".to_string());
    }
    replace_reasoning(app, value.block_id, value.text);
    Ok(())
}

pub(super) fn apply_snapshot(
    app: &mut App,
    event: &Value,
    run: Option<&str>,
) -> Result<(), String> {
    let value: Snapshot = serde_json::from_value(event["payload"].clone())
        .map_err(|_| "ModelSnapshot payload fields are invalid".to_string())?;
    let turn = event["turnId"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "ModelSnapshot turnId is missing".to_string())?;
    if value.revision == 0
        || value
            .reasoning
            .as_ref()
            .is_some_and(|value| value.block_id.is_empty() || value.request_id.is_empty())
    {
        return Err("ModelSnapshot revision or reasoning identity is invalid".to_string());
    }
    let identity = format!("{}:{turn}", run.unwrap_or_default());
    if app
        .snapshot_revisions
        .get(&identity)
        .is_some_and(|revision| *revision >= value.revision)
    {
        return Ok(());
    }
    app.snapshot_revisions.insert(identity, value.revision);
    if let Some(reasoning) = value.reasoning {
        replace_reasoning(app, reasoning.block_id, reasoning.text);
    }
    replace_assistant_buffer(app, value.text);
    Ok(())
}
