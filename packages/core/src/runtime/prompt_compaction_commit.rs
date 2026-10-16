//! A bounded summary receipt identity carried to its observing main request.
use super::*;

const COMMIT_REF: &str = "compaction_commit_ref_v1_json";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionSourceRequest {
    pub(crate) agent_run_id: String,
    pub(crate) request_id: String,
    pub(crate) request_turn_id: String,
    pub(crate) prompt_sha256: String,
}

#[derive(Debug, Clone)]
pub struct PreparedCompactionCommit {
    pub(crate) event: RuntimeEventProjection,
    pub(crate) source: CompactionSourceRequest,
    pub(crate) acknowledged: bool,
}

pub(super) struct CompactionAckSnapshot {
    pub(super) raw: String,
    pub(super) observed_main_request_id: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommitReference {
    schema: String,
    event_id: String,
    turn_id: String,
    at_ms: i64,
    source: CompactionSourceRequest,
    event_sha256: String,
    acknowledged: bool,
}

pub(super) fn remember_commit(
    session: &mut SessionStateSnapshot,
    events: &[RuntimeEventProjection],
    source: Option<CompactionSourceRequest>,
) -> Result<(), String> {
    let Some(source) = source else { return Ok(()) };
    let commits = events
        .iter()
        .filter(|event| event.event_type == "PromptCompaction" && event.status == "done")
        .collect::<Vec<_>>();
    if commits.len() > 1 {
        return Err("multiple summary commits in one request preparation".into());
    }
    let Some(event) = commits.first() else {
        return Ok(());
    };
    let id = event
        .payload
        .pointer("/compaction/summaryMessageId")
        .and_then(Value::as_str)
        .ok_or("compaction summary identity missing")?;
    let summary = session
        .messages
        .iter_mut()
        .find(|message| message.message_id == id)
        .ok_or("compaction summary missing from saved Session")?;
    if event.session_id != session.session_id
        || event
            .payload
            .pointer("/compaction/summaryMarkdown")
            .and_then(Value::as_str)
            != Some(summary.content.as_str())
    {
        return Err("compaction receipt summary binding mismatch".into());
    }
    let reference = CommitReference {
        schema: "compaction_commit_ref_v1".into(),
        event_id: event.event_id.clone(),
        turn_id: event.turn_id.clone(),
        at_ms: event.at_ms,
        source: source.clone(),
        event_sha256: canonical_json::sha256("centaeris.compaction_commit.v1", &(event, &source))?,
        acknowledged: false,
    };
    summary.metadata.insert(
        COMMIT_REF.into(),
        serde_json::to_string(&reference).map_err(|error| error.to_string())?,
    );
    refresh_session_context_window(session);
    Ok(())
}

pub(super) fn active_commit(
    session: &SessionStateSnapshot,
) -> Result<Option<PreparedCompactionCommit>, String> {
    let Some(summary) = session.context_window.iter().find(|message| {
        message.metadata.get("kind").map(String::as_str) == Some("context_compaction")
    }) else {
        return Ok(None);
    };
    let Some(raw) = summary.metadata.get(COMMIT_REF) else {
        return Ok(None);
    };
    if raw.len() > 4096 {
        return Err("compaction commit reference exceeds supported bound".into());
    }
    let reference: CommitReference = serde_json::from_str(raw)
        .map_err(|error| format!("invalid compaction commit reference: {error}"))?;
    if reference.schema != "compaction_commit_ref_v1"
        || reference.source.agent_run_id.trim().is_empty()
        || reference.source.request_id.trim().is_empty()
        || reference.event_id.trim().is_empty()
        || reference.turn_id.trim().is_empty()
    {
        return Err("unsupported compaction commit reference identity".into());
    }
    let compaction_id = summary
        .metadata
        .get("compaction_id")
        .ok_or("summary compaction identity missing")?;
    let mut event = build_runtime_event_prompt_compaction_event(
        &session.session_id,
        &reference.turn_id,
        "prompt_compaction",
        "done",
        "上下文已压缩",
        None,
        json!({
            "compactionId":compaction_id,"summaryMessageId":summary.message_id,"summaryMarkdown":summary.content,"firstKeptMessageId":summary.metadata.get("first_kept_message_id"),
        }),
    );
    event.event_id = reference.event_id;
    event.at_ms = reference.at_ms;
    if canonical_json::sha256(
        "centaeris.compaction_commit.v1",
        &(&event, &reference.source),
    )? != reference.event_sha256
    {
        return Err("compaction commit reference content changed".into());
    }
    Ok(Some(PreparedCompactionCommit {
        event,
        source: reference.source,
        acknowledged: reference.acknowledged,
    }))
}

pub(super) fn validate_observed_commit(
    started: &ModelRequestStartedV1,
    commit: &PreparedCompactionCommit,
) -> Result<(), String> {
    if started.purpose != ModelRequestPurposeV1::Main
        || commit.event.session_id != started.session_id
        || commit.event.event_type != "PromptCompaction"
        || commit.event.status != "done"
    {
        return Err("compaction receipt request scope mismatch".into());
    }
    let payload = commit
        .event
        .payload
        .get("compaction")
        .ok_or("compaction receipt payload missing")?;
    let id = payload
        .get("summaryMessageId")
        .and_then(Value::as_str)
        .ok_or("compaction receipt summary identity missing")?;
    let text = payload
        .get("summaryMarkdown")
        .and_then(Value::as_str)
        .ok_or("compaction receipt summary body missing")?;
    let matches=started.observations.iter().filter(|observation|matches!(observation,ModelObservationV1::ContextMessage {message} if message.message_id==id && message.role==crate::model::prepared_prompt::ModelMessageRoleV1::System && message.content==text)).count();
    if matches != 1 {
        return Err("compaction receipt is not observed by its main request".into());
    }
    Ok(())
}

pub(super) fn acknowledge_commit(
    session: &mut SessionStateSnapshot,
    commit: &PreparedCompactionCommit,
) -> Result<bool, String> {
    let id = commit
        .event
        .payload
        .pointer("/compaction/summaryMessageId")
        .and_then(Value::as_str)
        .ok_or("summary identity missing")?;
    let Some(summary) = session
        .messages
        .iter_mut()
        .find(|message| message.message_id == id)
    else {
        return Err("acknowledged summary missing from snapshot".into());
    };
    let raw = summary
        .metadata
        .get(COMMIT_REF)
        .ok_or("acknowledged summary receipt missing")?;
    let mut reference: CommitReference =
        serde_json::from_str(raw).map_err(|error| error.to_string())?;
    if reference.event_id != commit.event.event_id
        || reference.source.agent_run_id != commit.source.agent_run_id
        || reference.source.request_id != commit.source.request_id
    {
        return Err("acknowledged summary receipt identity mismatch".into());
    }
    if reference.acknowledged {
        return Ok(false);
    }
    reference.acknowledged = true;
    summary.metadata.insert(
        COMMIT_REF.into(),
        serde_json::to_string(&reference).map_err(|error| error.to_string())?,
    );
    refresh_session_context_window(session);
    Ok(true)
}

const RECONSTRUCTED_MAIN_PROOF: &str = "compaction_reconstructed_main_proof_v1_json";
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReconstructedMainProof {
    schema: String,
    observed_main_request_id: String,
    compaction_event_id: String,
    source: CompactionSourceRequest,
    snapshot_sha256: String,
}

pub(crate) fn reconstructed_source_request(
    source: &SessionLogRecord,
) -> Result<CompactionSourceRequest, String> {
    let observations = source.payload["observations"]
        .as_array()
        .ok_or("reconstructed compaction source observations missing")?;
    let prompts = observations
        .iter()
        .filter(|observation| observation["kind"] == "compaction_prompt")
        .collect::<Vec<_>>();
    if prompts.len() != 1 {
        return Err("reconstructed compaction source prompt missing".into());
    }
    let text = prompts[0]
        .pointer("/message/content")
        .and_then(Value::as_str)
        .ok_or("reconstructed compaction source body missing")?;
    Ok(CompactionSourceRequest {
        agent_run_id: source
            .agent_run_id
            .clone()
            .ok_or("reconstructed source AgentRun missing")?,
        request_id: source.payload["requestId"]
            .as_str()
            .ok_or("reconstructed source request missing")?
            .into(),
        request_turn_id: source
            .turn_id
            .clone()
            .ok_or("reconstructed source turn missing")?,
        prompt_sha256: canonical_json::sha256("centaeris.compaction_prompt.v1", &text)?,
    })
}

pub(crate) fn record_reconstructed_main_proof_from_source(
    snapshot: &mut SessionStateSnapshot,
    main: &SessionLogRecord,
    compaction: &SessionLogRecord,
    source: CompactionSourceRequest,
) -> Result<(), String> {
    snapshot.metadata.remove(RECONSTRUCTED_MAIN_PROOF);
    let proof = ReconstructedMainProof {
        schema: "compaction_reconstructed_main_proof_v1".into(),
        observed_main_request_id: main.payload["requestId"]
            .as_str()
            .ok_or("reconstructed main request identity missing")?
            .into(),
        compaction_event_id: compaction.event_id.clone(),
        source,
        snapshot_sha256: canonical_json::sha256(
            "centaeris.compaction_reconstructed_snapshot.v1",
            snapshot,
        )?,
    };
    snapshot.metadata.insert(
        RECONSTRUCTED_MAIN_PROOF.into(),
        serde_json::to_string(&proof).map_err(|error| error.to_string())?,
    );
    Ok(())
}

pub(super) fn matches_reconstructed_main_proof(
    snapshot: &mut SessionStateSnapshot,
    request_id: &str,
    commit: &PreparedCompactionCommit,
) -> Result<bool, String> {
    let Some(raw) = snapshot.metadata.remove(RECONSTRUCTED_MAIN_PROOF) else {
        return Ok(false);
    };
    let proof: ReconstructedMainProof = serde_json::from_str(&raw)
        .map_err(|error| format!("invalid reconstructed main proof: {error}"))?;
    let actual =
        canonical_json::sha256("centaeris.compaction_reconstructed_snapshot.v1", snapshot)?;
    snapshot
        .metadata
        .insert(RECONSTRUCTED_MAIN_PROOF.into(), raw);
    Ok(proof.schema == "compaction_reconstructed_main_proof_v1"
        && proof.observed_main_request_id == request_id
        && proof.compaction_event_id == commit.event.event_id
        && proof.source == commit.source
        && proof.snapshot_sha256 == actual)
}
