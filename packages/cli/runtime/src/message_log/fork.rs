use super::{
    existing_session_log_file_path, lock_session_logs_for_read, lock_session_logs_for_write,
    observation_cas, read_session_document_unlocked, session_metadata_record, SessionDocument,
};
use crate::local_attachments::LocalModelInputImageResolver;
use crate::session_files::{SessionFileItem, SessionFiles};
use centaeris_core::model::prepared_prompt::ModelInputImageResolverPort;
use centaeris_core::runtime::contracts::current_timestamp_ms;
use centaeris_core::session::fork::{ForkHistory, SessionFork};
use centaeris_core::session::{
    recovery::recover_session_records, wire_record_value, SequencedSessionRecord,
    SessionManifestV1, SessionMetadataV1,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

pub(crate) fn fork_before_user_turn(session_id: &str) -> Result<Value, String> {
    let sessions = SessionFiles::new(crate::user_data_layout::sessions_dir_path());
    let (path, document) = {
        let _guard = lock_session_logs_for_read()?;
        let path = existing_session_log_file_path(session_id)?;
        let document = read_session_document_unlocked(&path, Some(session_id))?;
        (path, document)
    };
    // Admission persists provenance before the start event. Only openings in
    // the copied journal can participate, even if new receipts arrive now.
    let openings = crate::operation_receipts::external_prompt_openings(session_id)?;
    let fork = SessionFork::before_latest_user_turn(session_id, &document.records, &openings)?;
    let source = sessions.project_document(&path, document)?;

    let mut attachments = Vec::new();
    for image in &fork.attachments {
        let bytes = LocalModelInputImageResolver.resolve(&image.input_ref, &image.content_type)?;
        let mut file = tempfile::Builder::new()
            .prefix("centaeris-fork-image-")
            .suffix(".png")
            .tempfile()
            .map_err(|error| format!("create fork draft image failed: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("write fork draft image failed: {error}"))?;
        let path = file.into_temp_path();
        let display_path = path
            .to_str()
            .ok_or("Fork draft image path cannot be represented as text")?;
        let attachment = json!({"placeholder": image.placeholder, "localPath": display_path});
        attachments.push((path, attachment));
    }

    let forked = sessions.create_fork(&source, &fork, current_timestamp_ms())?;
    let attachments = attachments
        .into_iter()
        .map(|(mut path, attachment)| {
            path.disable_cleanup(true);
            attachment
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "id": forked.id,
        "cwd": forked.cwd,
        "title": forked.title,
        "draft": fork.draft,
        "attachments": attachments,
    }))
}

pub(crate) fn publish(
    sessions: &SessionFiles,
    path: &Path,
    manifest: SessionManifestV1,
    metadata: SessionMetadataV1,
    history: ForkHistory,
) -> Result<SessionFileItem, String> {
    let session_id = manifest.session_id.clone();
    let metadata = session_metadata_record(&session_id, metadata, 1, manifest.created_at_ms)?;
    let records = std::iter::once(metadata)
        .chain(history.records)
        .collect::<Vec<_>>();
    recover_session_records(&session_id, &records)?;
    let run_ids = records
        .iter()
        .filter_map(|record| record.agent_run_id.clone())
        .collect::<HashSet<_>>();
    let manifest_wire = serde_json::to_value(&manifest)
        .map_err(|error| format!("serialize fork manifest failed: {error}"))?;
    let mut wires = records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let sequence = u64::try_from(index + 1).map_err(|_| "fork sequence overflow")?;
            wire_record_value(&SequencedSessionRecord {
                sequence,
                event: record.clone(),
            })
            .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let forked = sessions.project_document(path, SessionDocument { manifest, records })?;

    let _guard = lock_session_logs_for_write()?;
    observation_cas::validate_session_log_path(path, &session_id)?;
    let parent = path.parent().ok_or("Fork log path has no parent")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create fork log parent failed: {error}"))?;
    if path.exists() {
        return Err("Fork session log already exists".to_string());
    }
    let run_ids = run_ids.iter().map(String::as_str).collect::<Vec<_>>();
    crate::session_catalog::mark_dirty(path, &run_ids)?;
    observation_cas::compact_and_install_new_wires(path, &session_id, &mut wires)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("create temporary fork log failed: {error}"))?;
    {
        let mut writer = BufWriter::new(temporary.as_file_mut());
        for wire in std::iter::once(&manifest_wire).chain(&wires) {
            serde_json::to_writer(&mut writer, wire)
                .map_err(|error| format!("write fork log failed: {error}"))?;
            writer
                .write_all(b"\n")
                .map_err(|error| format!("write fork log failed: {error}"))?;
        }
        writer
            .flush()
            .map_err(|error| format!("flush fork log failed: {error}"))?;
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("sync fork log failed: {error}"))?;
    crate::operation_receipts::persist_fork_external_prompt_openings(
        &session_id,
        &history.openings,
    )?;
    temporary
        .into_temp_path()
        .persist_noclobber(path)
        .map_err(|error| format!("publish fork log failed: {error}"))?;
    Ok(forked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_fork_observations_can_be_installed_before_log_publication() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("forked-session.jsonl");
        let mut wire = [json!({
            "type": "model_request_started", "sessionId": "forked-session",
            "payload": {"observations": [{"kind": "system_prompt", "content": "remembered"}]},
        })];
        let original = wire.clone();
        observation_cas::compact_and_install_new_wires(&path, "forked-session", &mut wire).unwrap();
        assert!(!path.exists());
        assert!(wire[0]["payload"]["observations"]["manifestDigest"].is_string());
        observation_cas::hydrate_wires(&path, "forked-session", &mut wire).unwrap();
        assert_eq!(wire, original);
    }
}
