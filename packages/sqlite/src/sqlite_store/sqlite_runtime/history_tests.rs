use super::*;
use centaeris_core::runtime::contracts::JsonMap;
use centaeris_core::session::state::{
    ChatMessage, MessageRole, ModelMessageSemanticsV1, SessionStateSnapshot,
};

fn source() -> SessionStateSnapshot {
    let mut state = SessionStateSnapshot::new("history-session".into(), 1);
    for (id, role, text) in [
        (
            "old-user",
            MessageRole::User,
            "Retain the older exact request\n",
        ),
        ("cold-result", MessageRole::Assistant, "Old tool discussion"),
        ("current-user", MessageRole::User, "Current task"),
        ("summary", MessageRole::System, "Summary"),
    ] {
        let mut metadata = JsonMap::new();
        if role == MessageRole::User {
            metadata.insert("message_semantic_kind".into(), "user_request".into());
        }
        if id == "summary" {
            metadata.insert("kind".into(), "context_compaction".into());
            metadata.insert("compaction_id".into(), "compaction".into());
            metadata.insert("first_kept_message_id".into(), "current-user".into());
        }
        state.messages.push(ChatMessage {
            message_id: id.into(),
            role,
            content: text.into(),
            created_at_ms: 1,
            metadata,
        });
        state
            .model_semantics
            .insert(id.into(), ModelMessageSemanticsV1::Plain);
    }
    state
}

fn count_entries(store: &SqliteRuntimeStore) -> i64 {
    store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM session_message_history_entries",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
        })
        .unwrap()
}

#[test]
fn owned_session_save_returns_the_exact_committed_working_state_and_archives_its_delta() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteRuntimeStore::new(directory.path().join("runtime.sqlite")).unwrap();
    let manager = centaeris_core::session::manager::SessionManager::new(store.clone());
    let committed = manager.save_session_owned(source()).unwrap();
    assert_eq!(committed.messages.len(), 2);
    centaeris_core::session::history::validate_session_snapshot_history(&committed).unwrap();
    let expected = prepare_session_snapshot_write(&serde_json::to_string(&committed).unwrap())
        .unwrap()
        .snapshot_json()
        .to_owned();
    assert_eq!(
        store
            .load_agent_runtime_snapshot(&committed.session_id)
            .unwrap()
            .as_deref(),
        Some(expected.as_str()),
    );
    assert_eq!(
        count_entries(&store),
        4,
        "normalization must preserve the archive delta"
    );
    let repeated = manager.save_session_owned(committed).unwrap();
    assert_eq!(count_entries(&store), 4);
    assert_eq!(repeated.messages.len(), 2);
}

#[test]
fn authoritative_recovery_retires_original_requests_without_deleting_immutable_bodies() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteRuntimeStore::new(directory.path().join("runtime.sqlite")).unwrap();
    let manager = centaeris_core::session::manager::SessionManager::new(store.clone());
    let mut recovered = manager.save_session_owned(source()).unwrap();
    recovered.history = Default::default();
    let committed = manager
        .save_recovered_session_owned(recovered, vec!["old-user".into(), "never-archived".into()])
        .unwrap();
    assert_eq!(
        count_entries(&store),
        4,
        "retirement must preserve immutable audit bytes"
    );
    let users = store
        .load_session_history_page(
            &committed.session_id,
            None,
            256,
            SessionHistorySelectionV1::OriginalUserRequests,
        )
        .unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].entry.message.message_id, "current-user");
    assert!(manager
        .save_recovered_session_owned(committed, vec!["current-user".into()])
        .is_err());
}

#[test]
fn session_history_is_immutable_bounded_and_paged_with_stable_latest_revisions() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteRuntimeStore::new(directory.path().join("runtime.sqlite")).unwrap();
    let source = source();
    store
        .save_agent_runtime_snapshot(
            &source.session_id,
            &serde_json::to_string(&source).unwrap(),
            1,
        )
        .unwrap();
    let raw = store
        .load_agent_runtime_snapshot(&source.session_id)
        .unwrap()
        .unwrap();
    let mut current: SessionStateSnapshot = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        current
            .messages
            .iter()
            .map(|m| m.message_id.as_str())
            .collect::<Vec<_>>(),
        ["current-user", "summary"]
    );
    assert_eq!(count_entries(&store), 4);
    store
        .save_agent_runtime_snapshot(&source.session_id, &raw, 2)
        .unwrap();
    assert_eq!(
        count_entries(&store),
        4,
        "unchanged working state must not duplicate history"
    );
    let first = store
        .load_session_history_page(
            &source.session_id,
            None,
            1,
            SessionHistorySelectionV1::OriginalUserRequests,
        )
        .unwrap();
    assert_eq!(first[0].entry.message.message_id, "current-user");
    let older = store
        .load_session_history_page(
            &source.session_id,
            Some(first[0].position),
            1,
            SessionHistorySelectionV1::OriginalUserRequests,
        )
        .unwrap();
    assert_eq!(
        older[0].entry.message.content,
        "Retain the older exact request\n"
    );
    let position = first[0].position;
    current.messages[0].content = "Revised current task".into();
    assert!(store
        .compare_and_save_agent_runtime_snapshot(
            &source.session_id,
            Some(&raw),
            &serde_json::to_string(&current).unwrap(),
            3
        )
        .unwrap());
    assert_eq!(count_entries(&store), 5);
    let latest = store
        .load_session_history_page(
            &source.session_id,
            None,
            1,
            SessionHistorySelectionV1::OriginalUserRequests,
        )
        .unwrap();
    assert_eq!(latest[0].position, position);
    assert_eq!(latest[0].entry.message.content, "Revised current task");
    let mut stale = current.clone();
    stale.messages[0].content = "Stale result".into();
    assert!(!store
        .compare_and_save_agent_runtime_snapshot(
            &source.session_id,
            Some(&raw),
            &serde_json::to_string(&stale).unwrap(),
            4
        )
        .unwrap());
    assert_eq!(
        count_entries(&store),
        5,
        "rejected CAS must append no immutable body"
    );
    store
        .with_conn(|conn| {
            assert!(conn
                .execute(
                    "UPDATE session_message_history_entries SET entry_json='{}'",
                    []
                )
                .is_err());
            Ok(())
        })
        .unwrap();
    store.delete_session_data(&source.session_id).unwrap();
    assert_eq!(count_entries(&store), 0);
}

fn make_legacy_store(path: &std::path::Path, raw: &str) {
    drop(SqliteRuntimeStore::new(path).unwrap());
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("DROP TABLE session_message_history_heads; DROP TABLE session_message_history_entries; DELETE FROM schema_migrations WHERE version=7;").unwrap();
    conn.execute("INSERT INTO session_runtime_snapshots(session_id,snapshot_json,updated_at_ms) VALUES('history-session',?1,1)", [raw]).unwrap();
}

#[test]
fn session_history_forward_migration_preserves_snapshot_only_messages_and_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.sqlite");
    let mut legacy = serde_json::to_value(source()).unwrap();
    legacy.as_object_mut().unwrap().remove("history");
    make_legacy_store(&path, &serde_json::to_string(&legacy).unwrap());
    let store = SqliteRuntimeStore::new(&path).unwrap();
    let raw = store
        .load_agent_runtime_snapshot("history-session")
        .unwrap()
        .unwrap();
    let bounded: SessionStateSnapshot = serde_json::from_str(&raw).unwrap();
    assert_eq!(bounded.messages.len(), 2);
    assert_eq!(count_entries(&store), 4);
    let archived = store
        .load_session_history_page("history-session", None, 256, SessionHistorySelectionV1::All)
        .unwrap();
    assert_eq!(archived.len(), 4);
    assert_eq!(
        archived[3].entry.message.content,
        "Retain the older exact request\n"
    );
    assert!(matches!(
        archived[3].entry.model_semantics,
        ModelMessageSemanticsV1::Plain
    ));
    drop(store);
    assert!(
        SqliteRuntimeStore::new(&path).is_ok(),
        "current schema reopen must preserve migrated history"
    );
}

#[test]
fn damaged_legacy_history_rolls_back_the_complete_forward_migration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("damaged.sqlite");
    let mut legacy = serde_json::to_value(source()).unwrap();
    legacy.as_object_mut().unwrap().remove("history");
    legacy["model_semantics"]
        .as_object_mut()
        .unwrap()
        .remove("old-user");
    let raw = serde_json::to_string(&legacy).unwrap();
    make_legacy_store(&path, &raw);
    assert!(SqliteRuntimeStore::new(&path)
        .unwrap_err()
        .contains("model_message_semantics_missing"));
    let conn = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| row
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        6
    );
    assert_eq!(
        conn.query_row(
            "SELECT snapshot_json FROM session_runtime_snapshots",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        raw
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='session_message_history_heads'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
