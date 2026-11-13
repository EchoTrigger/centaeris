use super::*;
use sha2::{Digest, Sha256};

#[test]
#[ignore = "requires exclusively owned disposable PostgreSQL"]
fn postgres_image_history_upgrades_preserves_sessions_and_is_immutable_and_purgeable() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    store
        .save_agent_runtime_snapshot("image-session", "retained snapshot", 7)
        .unwrap();
    let mut db = Client::connect(&url, NoTls).unwrap();
    super::super::schema::restore_model_input_image_fixture(&mut db);
    drop(store);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        store
            .load_agent_runtime_snapshot("image-session")
            .unwrap()
            .as_deref(),
        Some("retained snapshot")
    );
    let bytes = b"verified observation bytes";
    let digest = format!("sha256:{:x}", Sha256::digest(bytes));
    store
        .save_model_input_image("image-session", &digest, bytes)
        .unwrap();
    store
        .save_model_input_image("image-session", &digest, bytes)
        .unwrap();
    assert!(store
        .save_model_input_image("image-session", &digest, b"changed")
        .is_err());
    assert!(store
        .load_model_input_image("other-session", &digest)
        .unwrap()
        .is_none());
    assert!(db
        .execute(
            "UPDATE runtime.session_model_input_images SET content='changed'::bytea",
            &[]
        )
        .is_err());
    drop(store);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert_eq!(
        store
            .load_model_input_image("image-session", &digest)
            .unwrap()
            .unwrap(),
        bytes
    );
    store
        .save_model_input_image("other-session", &digest, bytes)
        .unwrap();
    store.delete_session_data("image-session").unwrap();
    assert!(store
        .load_model_input_image("image-session", &digest)
        .unwrap()
        .is_none());
    assert!(store
        .load_model_input_image("other-session", &digest)
        .unwrap()
        .is_some());
    db.batch_execute(
        "ALTER TABLE runtime.session_model_input_images DISABLE TRIGGER model_input_image_facts",
    )
    .unwrap();
    assert!(PostgresRuntimeStore::new(&url)
        .unwrap_err()
        .contains("immutable image trigger"));
    db.batch_execute(
        "ALTER TABLE runtime.session_model_input_images ENABLE TRIGGER model_input_image_facts",
    )
    .unwrap();
}
