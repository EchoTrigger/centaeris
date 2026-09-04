use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::session::manager::SessionManager;
use centaeris_core::session::state::{ChatMessage, MessageRole};
use centaeris_runtime_sqlite::SqliteRuntimeStore;

fn temp_db_path(suffix: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock moved backwards")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "centaeris_core_session_{suffix}_{}_{}.db",
        std::process::id(),
        nanos
    ))
}

#[test]
fn load_or_create_and_save_roundtrip() {
    let db_path = temp_db_path("roundtrip");
    let store = SqliteRuntimeStore::new(&db_path).expect("create sqlite runtime store");
    let manager = SessionManager::new(store.clone());
    let mut session = manager
        .load_or_create_session("chat-session")
        .expect("load or create session");
    session.messages.push(ChatMessage {
        message_id: "message-session-1".to_string(),
        role: MessageRole::User,
        content: "hello".to_string(),
        created_at_ms: 1,
        metadata: HashMap::new(),
    });

    manager.save_session(&session).expect("save session");
    let loaded = manager
        .load_session("chat-session")
        .expect("load session")
        .expect("session exists");
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.messages[0].content, "hello");

    let _ = std::fs::remove_file(db_path);
}
