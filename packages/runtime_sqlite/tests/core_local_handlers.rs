use centaeris_core::session::reliability::{
    AcquireResourceClaimDisposition, AcquireResourceClaimRequest, ReleaseResourceClaimRequest,
    ResourceClaimStorePort,
};
use centaeris_runtime_sqlite::SqliteRuntimeStore;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn store() -> (SqliteRuntimeStore, PathBuf) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    store_at(unique)
}

fn store_at(unique: u128) -> (SqliteRuntimeStore, PathBuf) {
    // Parallel tests may observe the same clock tick, regardless of its units.
    static NEXT_STORE_ID: AtomicU64 = AtomicU64::new(0);
    let store_id = NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "centaeris-core-local-handlers-{}-{unique}-{store_id}.db",
        std::process::id()
    ));
    let store = SqliteRuntimeStore::new(path.as_path()).expect("store");
    (store, path)
}

fn remove_store(path: PathBuf) {
    for attempt in 0..20 {
        match std::fs::remove_file(path.as_path()) {
            Ok(()) => return,
            Err(error) if attempt == 19 => panic!("remove store: {error}"),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    unreachable!();
}

fn claim(owner: &str, now_ms: i64) -> AcquireResourceClaimRequest {
    AcquireResourceClaimRequest {
        resource_kind: "file".to_string(),
        resource_key: "workspace/result.txt".to_string(),
        owner: owner.to_string(),
        owner_kind: "tool_runtime".to_string(),
        session_id: None,
        branch_id: None,
        now_ms,
        ttl_ms: 30_000,
        metadata_json: "{}".to_string(),
    }
}

#[test]
fn stores_created_in_the_same_clock_tick_have_independent_claims() {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let (first, first_path) = store_at(timestamp);
    first
        .acquire_resource_claim(claim("task-a", 1_000))
        .expect("first store claim");
    let (second, second_path) = store_at(timestamp);
    let second_claim = second
        .get_resource_claim("file", "workspace/result.txt")
        .expect("second store claim");
    drop(first);
    drop(second);
    if second_path != first_path {
        remove_store(second_path);
    }
    remove_store(first_path);
    assert!(
        second_claim.is_none(),
        "independent test stores share claims"
    );
}

#[test]
fn sqlite_claim_reports_the_existing_owner_to_a_conflicting_writer() {
    let (store, path) = store();

    store
        .acquire_resource_claim(claim("task-a", 1_000))
        .expect("first claim");
    let conflict = store
        .acquire_resource_claim(claim("task-b", 1_001))
        .expect("conflicting claim result");

    assert_eq!(
        conflict.disposition,
        AcquireResourceClaimDisposition::Conflict
    );
    assert_eq!(conflict.claim.owner, "task-a");
    remove_store(path);
}

#[test]
fn sqlite_claim_is_absent_after_its_owner_releases_it() {
    let (store, path) = store();

    store
        .acquire_resource_claim(claim("task-a", 1_000))
        .expect("claim");
    assert!(store
        .release_resource_claim(ReleaseResourceClaimRequest {
            resource_kind: "file".to_string(),
            resource_key: "workspace/result.txt".to_string(),
            owner: "task-a".to_string(),
            released_at_ms: 1_001,
        })
        .expect("release claim"));
    assert!(store
        .get_resource_claim("file", "workspace/result.txt")
        .expect("load claim")
        .is_none());
    remove_store(path);
}
