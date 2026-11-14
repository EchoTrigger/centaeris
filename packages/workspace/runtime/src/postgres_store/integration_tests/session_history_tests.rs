use super::*;
use centaeris_core::session::history::{SessionHistoryRecordV1, SessionHistorySelectionV1};
use centaeris_core::session::state::{
    ChatMessage, MessageRole, ModelMessageSemanticsV1, SessionStateSnapshot,
};

pub(super) fn compacted_snapshot(session_id: &str) -> SessionStateSnapshot {
    let mut snapshot = SessionStateSnapshot::new(session_id.into(), 1);
    for index in 0..300 {
        let message = ChatMessage {
            message_id: format!("cold-{index:04}"),
            role: MessageRole::User,
            content: format!("original historical input {index}"),
            created_at_ms: index,
            metadata: [("message_semantic_kind".into(), "user_request".into())]
                .into_iter()
                .collect(),
        };
        snapshot
            .model_semantics
            .insert(message.message_id.clone(), ModelMessageSemanticsV1::Plain);
        snapshot.messages.push(message);
    }
    let tail = ChatMessage {
        message_id: "tail".into(),
        role: MessageRole::User,
        content: "current objective".into(),
        created_at_ms: 301,
        metadata: [("message_semantic_kind".into(), "user_request".into())]
            .into_iter()
            .collect(),
    };
    let summary = ChatMessage {
        message_id: "summary".into(),
        role: MessageRole::System,
        content: "earlier work summarized".into(),
        created_at_ms: 302,
        metadata: [
            ("kind".into(), "context_compaction".into()),
            ("first_kept_message_id".into(), "tail".into()),
        ]
        .into_iter()
        .collect(),
    };
    for message in [tail, summary] {
        snapshot
            .model_semantics
            .insert(message.message_id.clone(), ModelMessageSemanticsV1::Plain);
        snapshot.messages.push(message);
    }
    snapshot
}

fn history(
    store: &PostgresRuntimeStore,
    session_id: &str,
    selection: SessionHistorySelectionV1,
) -> Vec<SessionHistoryRecordV1> {
    let mut before = None;
    let mut result = Vec::new();
    loop {
        let page = store
            .load_session_history_page(session_id, before, 64, selection)
            .unwrap();
        if page.is_empty() {
            return result;
        }
        before = page.last().map(|record| record.position);
        result.extend(page);
    }
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_session_history_migration_bounds_state_and_preserves_revisions_cas_and_purge() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let source = compacted_snapshot("history-session");
    store
        .save_agent_runtime_snapshot(
            &source.session_id,
            &serde_json::to_string(&source).unwrap(),
            1,
        )
        .unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    super::super::schema::restore_session_history_fixture(&mut db);
    let old: serde_json::Value = serde_json::from_str(
        &db.query_one(
            "SELECT snapshot_json FROM runtime.session_runtime_snapshots",
            &[],
        )
        .unwrap()
        .get::<_, String>(0),
    )
    .unwrap();
    assert!(old.get("history").is_none());
    assert_eq!(old["messages"].as_array().unwrap().len(), 302);
    drop(store);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let raw = store
        .load_agent_runtime_snapshot(&source.session_id)
        .unwrap()
        .unwrap();
    let mut current: SessionStateSnapshot = serde_json::from_str(&raw).unwrap();
    assert_eq!(current.messages.len(), 2);
    assert_eq!(
        current
            .context_window
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        ["summary", "tail"]
    );
    let all = history(&store, &source.session_id, SessionHistorySelectionV1::All);
    assert_eq!(all.len(), 302);
    assert!(all
        .windows(2)
        .all(|pair| pair[0].position > pair[1].position));
    assert_eq!(
        history(
            &store,
            &source.session_id,
            SessionHistorySelectionV1::OriginalUserRequests
        )
        .len(),
        301
    );
    let old_tail = all
        .iter()
        .find(|record| record.entry.message.message_id == "tail")
        .unwrap();
    assert!(db
        .execute(
            "UPDATE runtime.session_message_history_entries SET entry_json='{}'",
            &[]
        )
        .is_err());
    current
        .messages
        .iter_mut()
        .find(|message| message.message_id == "tail")
        .unwrap()
        .content = "corrected objective".into();
    store
        .save_agent_runtime_snapshot(
            &source.session_id,
            &serde_json::to_string(&current).unwrap(),
            2,
        )
        .unwrap();
    let revised = history(&store, &source.session_id, SessionHistorySelectionV1::All);
    let new_tail = revised
        .iter()
        .find(|record| record.entry.message.message_id == "tail")
        .unwrap();
    assert_eq!(new_tail.position, old_tail.position);
    assert_ne!(new_tail.entry.record_id, old_tail.entry.record_id);
    assert_eq!(new_tail.entry.message.content, "corrected objective");
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.session_message_history_entries",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        303
    );
    let before = store
        .load_agent_runtime_snapshot(&source.session_id)
        .unwrap()
        .unwrap();
    current
        .messages
        .iter_mut()
        .find(|message| message.message_id == "tail")
        .unwrap()
        .content = "uncommitted stale result".into();
    assert!(!store
        .compare_and_save_agent_runtime_snapshot(
            &source.session_id,
            Some("stale"),
            &serde_json::to_string(&current).unwrap(),
            3
        )
        .unwrap());
    assert_eq!(
        store
            .load_agent_runtime_snapshot(&source.session_id)
            .unwrap()
            .unwrap(),
        before
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.session_message_history_entries",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        303
    );
    let mut current: SessionStateSnapshot = serde_json::from_str(&before).unwrap();
    current
        .messages
        .retain(|message| message.message_id != "summary");
    current.model_semantics.remove("summary");
    store
        .save_agent_runtime_snapshot(
            &source.session_id,
            &serde_json::to_string(&current).unwrap(),
            4,
        )
        .unwrap();
    assert_eq!(
        history(&store, &source.session_id, SessionHistorySelectionV1::All).len(),
        301
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.session_message_history_entries",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        303
    );
    db.batch_execute("ALTER TABLE runtime.session_message_history_entries DISABLE TRIGGER session_message_history_entry_facts").unwrap();
    assert!(PostgresRuntimeStore::new(&url)
        .unwrap_err()
        .contains("immutable Session history trigger"));
    db.batch_execute("ALTER TABLE runtime.session_message_history_entries ENABLE TRIGGER session_message_history_entry_facts").unwrap();
    store.delete_session_data(&source.session_id).unwrap();
    for table in [
        "session_message_history_heads",
        "session_message_history_entries",
    ] {
        assert_eq!(
            db.query_one(&format!("SELECT count(*) FROM runtime.{table}"), &[])
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
}

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_session_history_upgrade_rejects_unknown_legacy_state_without_partial_writes() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    store
        .save_agent_runtime_snapshot(
            "legacy",
            &session_snapshot_fixture("legacy", "unchanged"),
            1,
        )
        .unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    super::super::schema::restore_session_history_fixture(&mut db);
    db.execute(
        "UPDATE runtime.session_runtime_snapshots SET snapshot_json=$1",
        &[&"{\"unsupported\":true}"],
    )
    .unwrap();
    drop(store);
    assert!(PostgresRuntimeStore::new(&url)
        .unwrap_err()
        .contains("decode legacy Session snapshot"));
    assert_eq!(
        db.query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
            .unwrap()
            .get::<_, i64>(0),
        22
    );
    assert!(!db
        .query_one(
            "SELECT to_regclass('runtime.session_message_history_entries') IS NOT NULL",
            &[]
        )
        .unwrap()
        .get::<_, bool>(0));
    assert_eq!(
        db.query_one(
            "SELECT snapshot_json FROM runtime.session_runtime_snapshots WHERE session_id='legacy'",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "{\"unsupported\":true}"
    );
}
