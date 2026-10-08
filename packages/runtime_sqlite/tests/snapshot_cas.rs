use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::session::state::SessionStateSnapshot;
use centaeris_core::session::store::AgentRuntimeSnapshotStorePort;
use centaeris_runtime_sqlite::SqliteRuntimeStore;

struct SnapshotDatabase(PathBuf);

impl SnapshotDatabase {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "centaeris-snapshot-cas-{}-{nonce}.sqlite3",
            std::process::id()
        )))
    }

    fn store(&self) -> SqliteRuntimeStore {
        SqliteRuntimeStore::new(&self.0).expect("independent SQLite store connection")
    }
}

impl Drop for SnapshotDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn snapshot(key: &str) -> String {
    let mut state = SessionStateSnapshot::new("parent".to_string(), 1);
    state
        .metadata
        .insert(key.to_string(), "retained".to_string());
    serde_json::to_string(&state).expect("snapshot JSON")
}

#[test]
fn snapshot_cas_competing_recovery_and_projection_writers_retry_without_lost_metadata() {
    let database = SnapshotDatabase::new();
    let store = database.store();
    let base = serde_json::to_string(&SessionStateSnapshot::new("parent".to_string(), 1))
        .expect("base snapshot");
    store
        .save_agent_runtime_snapshot("parent", &base, 1)
        .unwrap();
    let writers = [database.store(), database.store()];
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = writers
        .into_iter()
        .zip(["recoveredWait", "subagentResult"])
        .map(|(writer, key)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let observed = writer
                    .load_agent_runtime_snapshot("parent")
                    .unwrap()
                    .unwrap();
                barrier.wait();
                let saved = writer
                    .compare_and_save_agent_runtime_snapshot(
                        "parent",
                        Some(&observed),
                        &snapshot(key),
                        2,
                    )
                    .unwrap();
                (key, saved)
            })
        })
        .collect();
    let outcomes: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|(_, saved)| *saved).count(), 1);
    let loser = outcomes.iter().find(|(_, saved)| !saved).unwrap().0;
    let winner_snapshot = store
        .load_agent_runtime_snapshot("parent")
        .unwrap()
        .unwrap();
    let mut merged: SessionStateSnapshot = serde_json::from_str(&winner_snapshot).unwrap();
    merged
        .metadata
        .insert(loser.to_string(), "retained".to_string());
    assert!(store
        .compare_and_save_agent_runtime_snapshot(
            "parent",
            Some(&winner_snapshot),
            &serde_json::to_string(&merged).unwrap(),
            3,
        )
        .unwrap());
    let final_snapshot: SessionStateSnapshot = serde_json::from_str(
        &store
            .load_agent_runtime_snapshot("parent")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        final_snapshot.metadata.get("recoveredWait").unwrap(),
        "retained"
    );
    assert_eq!(
        final_snapshot.metadata.get("subagentResult").unwrap(),
        "retained"
    );
}

#[test]
fn snapshot_cas_absent_row_has_only_one_initializer() {
    let database = SnapshotDatabase::new();
    let writers = [database.store(), database.store()];
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = writers
        .into_iter()
        .zip(["first", "second"])
        .map(|(writer, key)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                assert!(writer
                    .load_agent_runtime_snapshot("parent")
                    .unwrap()
                    .is_none());
                barrier.wait();
                (
                    key,
                    writer
                        .compare_and_save_agent_runtime_snapshot("parent", None, &snapshot(key), 1)
                        .unwrap(),
                )
            })
        })
        .collect();
    let outcomes: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(outcomes.iter().filter(|(_, saved)| *saved).count(), 1);
    let winner = outcomes.iter().find(|(_, saved)| *saved).unwrap().0;
    assert_eq!(
        database
            .store()
            .load_agent_runtime_snapshot("parent")
            .unwrap(),
        Some(snapshot(winner))
    );
}

#[test]
fn snapshot_cas_conflicts_do_not_modify_content_or_update_time() {
    let database = SnapshotDatabase::new();
    let store = database.store();
    let original = snapshot("original");
    store
        .save_agent_runtime_snapshot("parent", &original, 10)
        .unwrap();
    for expected in [None, Some("stale snapshot")] {
        assert!(!store
            .compare_and_save_agent_runtime_snapshot(
                "parent",
                expected,
                &snapshot("replacement"),
                99
            )
            .unwrap());
    }
    let row: (String, i64) = rusqlite::Connection::open(&database.0)
        .unwrap()
        .query_row(
            "SELECT snapshot_json, updated_at_ms FROM session_runtime_snapshots WHERE session_id='parent'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, (original, 10));
    assert!(!store
        .compare_and_save_agent_runtime_snapshot(
            "missing",
            Some("stale snapshot"),
            &snapshot("replacement"),
            99,
        )
        .unwrap());
    assert!(store
        .load_agent_runtime_snapshot("missing")
        .unwrap()
        .is_none());
}

#[test]
fn snapshot_cas_compares_exact_bytes_and_changes_only_the_named_session() {
    let database = SnapshotDatabase::new();
    let store = database.store();
    let original = snapshot("original");
    store
        .save_agent_runtime_snapshot("parent", &original, 1)
        .unwrap();
    store
        .save_agent_runtime_snapshot("other", "other-session-snapshot", 1)
        .unwrap();
    let equivalent_json = format!("{original}\n");
    assert!(!store
        .compare_and_save_agent_runtime_snapshot(
            "parent",
            Some(&equivalent_json),
            &snapshot("replacement"),
            2,
        )
        .unwrap());
    assert!(store
        .compare_and_save_agent_runtime_snapshot(
            "parent",
            Some(&original),
            &snapshot("replacement"),
            2
        )
        .unwrap());
    assert_eq!(
        store.load_agent_runtime_snapshot("parent").unwrap(),
        Some(snapshot("replacement"))
    );
    assert_eq!(
        store.load_agent_runtime_snapshot("other").unwrap(),
        Some("other-session-snapshot".to_string())
    );
}
