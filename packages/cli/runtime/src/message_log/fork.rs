use super::{
    existing_session_log_file_path, find_session_log_file_path, lock_session_logs_for_read,
    lock_session_logs_for_write, observation_cas, read_session_document_unlocked,
    session_metadata_record, SessionDocument,
};
use crate::errors::RuntimeHostError;
use crate::local_attachments::LocalModelInputImageResolver;
use crate::operation_receipts::{self, OperationId};
use crate::runtime_rpc_transport::EventWriter;
use crate::session_files::{self, SessionFileItem, SessionFiles};
use centaeris_core::model::prepared_prompt::ModelInputImageResolverPort;
use centaeris_core::runtime::contracts::current_timestamp_ms;
use centaeris_core::session::fork::{ForkHistory, ForkSelectionError, SessionFork};
use centaeris_core::session::{
    recovery::recover_session_records, wire_record_value, SequencedSessionRecord,
    SessionManifestV1, SessionMetadataV1,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

const FORK_METHOD: &str = "_centaeris/session/fork";
static FORK_OPERATIONS: Mutex<()> = Mutex::new(());

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForkCommandParams {
    request: ForkRequest,
    #[serde(default)]
    recover_only: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForkRequest {
    operation_id: OperationId,
    session_id: String,
    before_turn_id: String,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ForkReceipt {
    operation_id: String,
    source_session_id: String,
    source_turn_id: String,
    session_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
enum ForkOperation {
    Prepared {
        #[serde(rename = "createdAtMs")]
        created_at_ms: i64,
    },
    Published {
        receipt: ForkReceipt,
    },
}

pub(crate) fn fork_command(
    event_writer: &EventWriter,
    params: Value,
) -> Result<Value, RuntimeHostError> {
    let ForkCommandParams {
        request,
        recover_only,
    } = serde_json::from_value(params)
        .map_err(|error| RuntimeHostError::invalid_request(error.to_string()))?;
    for (field, value) in [
        ("sessionId", request.session_id.as_str()),
        ("beforeTurnId", request.before_turn_id.as_str()),
    ] {
        if value.is_empty() || value.trim() != value {
            return Err(RuntimeHostError::invalid_request(format!(
                "invalid {field}"
            )));
        }
    }
    let operation_id = request.operation_id.as_str();
    let request_digest = operation_receipts::request_digest(&request).map_err(fork_failed)?;
    let receipt = ForkReceipt {
        operation_id: operation_id.to_string(),
        source_session_id: request.session_id,
        source_turn_id: request.before_turn_id,
        session_id: operation_receipts::deterministic_identity(
            "session-",
            FORK_METHOD,
            operation_id,
        ),
    };
    let sessions = SessionFiles::new(crate::user_data_layout::sessions_dir_path());
    let _operations = FORK_OPERATIONS
        .lock()
        .map_err(|_| fork_failed("Session fork operation lock poisoned"))?;
    let _profile_operations =
        crate::local_host::lock_profile_file("session-fork", false).map_err(fork_failed)?;
    let prepared_at = match operation_receipts::read(FORK_METHOD, operation_id)
        .map_err(fork_failed)?
    {
        Some(stored) if stored.request_digest != request_digest => {
            return Err(RuntimeHostError::new(
                "operation_id_conflict",
                format!("operationId was already used for a different {FORK_METHOD} request: {operation_id}"),
            ));
        }
        Some(stored) => match serde_json::from_value(stored.result)
            .map_err(|error| fork_failed(format!("decode stored fork operation failed: {error}")))?
        {
            ForkOperation::Published { receipt: stored } => {
                if stored != receipt {
                    return Err(fork_failed("stored fork receipt identity mismatch"));
                }
                return serde_json::to_value(stored)
                    .map_err(|error| fork_failed(error.to_string()));
            }
            ForkOperation::Prepared { created_at_ms } => Some(created_at_ms),
        },
        None => None,
    };
    let published = match prepared_at {
        Some(created_at_ms) => {
            prepared_child_is_published(&sessions, &receipt.session_id, created_at_ms)
                .map_err(fork_failed)?
        }
        None => false,
    };
    if !published {
        if recover_only {
            return Err(RuntimeHostError::new(
                "session_fork_stale",
                "No child was published for this fork operation",
            ));
        }
        publish_requested_fork(
            event_writer,
            &sessions,
            &receipt,
            &request_digest,
            prepared_at,
        )?;
    }
    let response =
        serde_json::to_value(&receipt).map_err(|error| fork_failed(error.to_string()))?;
    save_fork_operation(
        operation_id,
        &request_digest,
        ForkOperation::Published { receipt },
    )
    .map_err(fork_failed)?;
    Ok(response)
}

fn fork_failed(message: impl Into<String>) -> RuntimeHostError {
    RuntimeHostError::new("session_fork_failed", message.into())
}

fn fork_selection_failed(error: ForkSelectionError) -> RuntimeHostError {
    let code = match error {
        ForkSelectionError::ActiveRun => "session_fork_active",
        ForkSelectionError::MissingOpening
        | ForkSelectionError::MissingOpeningMessage
        | ForkSelectionError::StaleOpening
        | ForkSelectionError::AmbiguousOpening => "session_fork_stale",
        ForkSelectionError::InvalidHistory(_) => "session_fork_failed",
    };
    RuntimeHostError::new(code, error.to_string())
}

fn save_fork_operation(
    operation_id: &str,
    request_digest: &str,
    operation: ForkOperation,
) -> Result<(), String> {
    let value = serde_json::to_value(operation)
        .map_err(|error| format!("serialize fork operation failed: {error}"))?;
    operation_receipts::write(FORK_METHOD, operation_id, request_digest.to_owned(), value)
}

fn prepared_child_is_published(
    sessions: &SessionFiles,
    session_id: &str,
    created_at_ms: i64,
) -> Result<bool, String> {
    let _guard = lock_session_logs_for_read()?;
    let path = sessions.fork_path(session_id, created_at_ms)?;
    if !path.try_exists().map_err(|error| error.to_string())? {
        return Ok(false);
    }
    let document = read_session_document_unlocked(&path, Some(session_id))?;
    if document.manifest.created_at_ms != created_at_ms {
        return Err("prepared fork destination identity mismatch".into());
    }
    Ok(true)
}

fn publish_requested_fork(
    event_writer: &EventWriter,
    sessions: &SessionFiles,
    receipt: &ForkReceipt,
    request_digest: &str,
    prepared_at: Option<i64>,
) -> Result<(), RuntimeHostError> {
    let source_id = &receipt.source_session_id;
    let _source_writer = event_writer
        .try_session_writer(source_id)
        .map_err(fork_failed)?
        .ok_or_else(|| {
            RuntimeHostError::new(
                "session_fork_active",
                format!("Session {source_id} is open in another window"),
            )
        })?;
    let _prompt_admission = crate::agent_runtime::lock_prompt_admission().map_err(fork_failed)?;
    let _session_files = session_files::lock_session_files().map_err(fork_failed)?;
    let sort_order = sessions.fork_sort_order().map_err(fork_failed)?;
    let _logs = lock_session_logs_for_write().map_err(fork_failed)?;
    if event_writer
        .active_agent_run_for_session(source_id)
        .map_err(fork_failed)?
        .is_some()
    {
        return Err(fork_selection_failed(ForkSelectionError::ActiveRun));
    }
    let path = find_session_log_file_path(source_id)
        .map_err(fork_failed)?
        .ok_or_else(|| {
            RuntimeHostError::new("session_fork_stale", "The source Session is unavailable")
        })?;
    let document = read_session_document_unlocked(&path, Some(source_id)).map_err(fork_failed)?;
    let openings = operation_receipts::external_prompt_openings(source_id).map_err(fork_failed)?;
    let fork = SessionFork::before_user_turn(
        source_id,
        &document.records,
        &openings,
        &receipt.source_turn_id,
    )
    .map_err(fork_selection_failed)?;
    let source = sessions
        .project_document(&path, document)
        .map_err(fork_failed)?;
    let created_at_ms = prepared_at.unwrap_or_else(current_timestamp_ms);
    let history = fork.rebase(&receipt.session_id).map_err(fork_failed)?;
    // Recovery uses this destination after a crash between log and receipt publication.
    if prepared_at.is_none() {
        save_fork_operation(
            &receipt.operation_id,
            request_digest,
            ForkOperation::Prepared { created_at_ms },
        )
        .map_err(fork_failed)?;
    }
    publish_unlocked(
        sessions,
        &source,
        &receipt.session_id,
        created_at_ms,
        sort_order,
        history,
    )
    .map_err(fork_failed)?;
    Ok(())
}

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
    let openings = operation_receipts::external_prompt_openings(session_id)?;
    let fork = SessionFork::before_latest_user_turn(session_id, &document.records, &openings)?;
    let source = sessions.project_document(&path, document)?;
    let draft = fork.draft()?;

    let mut attachments = Vec::new();
    for image in &draft.attachments {
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

    let _session_files = session_files::lock_session_files()?;
    let sort_order = sessions.fork_sort_order()?;
    let child_id = session_files::random_session_id()?;
    let history = fork.rebase(&child_id)?;
    let _logs = lock_session_logs_for_write()?;
    let forked = publish_unlocked(
        &sessions,
        &source,
        &child_id,
        current_timestamp_ms(),
        sort_order,
        history,
    )?;
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
        "draft": draft.text,
        "attachments": attachments,
    }))
}

fn publish_unlocked(
    sessions: &SessionFiles,
    source: &SessionFileItem,
    session_id: &str,
    created_at_ms: i64,
    sort_order: i64,
    history: ForkHistory,
) -> Result<SessionFileItem, String> {
    let path = sessions.fork_path(session_id, created_at_ms)?;
    let manifest = SessionManifestV1::new(
        session_id.to_owned(),
        created_at_ms,
        centaeris_core::runtime::CORE_PROTOCOL_VERSION,
    )
    .map_err(|error| error.to_string())?;
    let metadata = SessionMetadataV1 {
        record_id: format!("session:{session_id}:meta:1"),
        title: source.title.clone(),
        cwd: session_files::normalize_cwd(Path::new(&source.cwd))?,
        session_kind: "main".to_string(),
        parent_session_id: None,
        runtime_job_id: None,
        sort_order: Some(sort_order),
        is_pinned: false,
        is_unread: false,
    };
    let metadata = session_metadata_record(session_id, metadata, 1, created_at_ms)?;
    let records = std::iter::once(metadata)
        .chain(history.records)
        .collect::<Vec<_>>();
    recover_session_records(session_id, &records)?;
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
    let forked = sessions.project_document(&path, SessionDocument { manifest, records })?;

    observation_cas::validate_session_log_path(&path, session_id)?;
    let parent = path.parent().ok_or("Fork log path has no parent")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create fork log parent failed: {error}"))?;
    if path.exists() {
        return Err("Fork session log already exists".to_string());
    }
    let run_ids = run_ids.iter().map(String::as_str).collect::<Vec<_>>();
    crate::session_catalog::mark_dirty(&path, &run_ids)?;
    observation_cas::compact_and_install_new_wires(&path, session_id, &mut wires)?;
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
    operation_receipts::persist_fork_external_prompt_openings(session_id, &history.openings)?;
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
