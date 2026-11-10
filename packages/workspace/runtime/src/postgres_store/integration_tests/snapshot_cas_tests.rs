use super::*;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_snapshot_cas_rejects_stale_writers_and_preserves_concurrent_results() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let url = test_url();
    reset_store(&url);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    assert!(store
        .compare_and_save_agent_runtime_snapshot("parent", None, "{}", 1)
        .unwrap());
    assert!(!store
        .compare_and_save_agent_runtime_snapshot("parent", None, "bad", 2)
        .unwrap());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let writers: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|key| {
            let store = PostgresRuntimeStore::new(&url).unwrap();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let expected = store
                    .load_agent_runtime_snapshot("parent")
                    .unwrap()
                    .unwrap();
                barrier.wait();
                let candidate = serde_json::json!({key:true}).to_string();
                let won = store
                    .compare_and_save_agent_runtime_snapshot(
                        "parent",
                        Some(&expected),
                        &candidate,
                        3,
                    )
                    .unwrap();
                if !won {
                    let latest = store
                        .load_agent_runtime_snapshot("parent")
                        .unwrap()
                        .unwrap();
                    let mut merged: serde_json::Value = serde_json::from_str(&latest).unwrap();
                    merged[key] = serde_json::json!(true);
                    assert!(store
                        .compare_and_save_agent_runtime_snapshot(
                            "parent",
                            Some(&latest),
                            &merged.to_string(),
                            4
                        )
                        .unwrap());
                }
                won
            })
        })
        .collect();
    assert_eq!(
        writers
            .into_iter()
            .map(|writer| usize::from(writer.join().unwrap()))
            .sum::<usize>(),
        1
    );
    let actual: serde_json::Value = serde_json::from_str(
        &store
            .load_agent_runtime_snapshot("parent")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(actual, serde_json::json!({"a":true,"b":true}));
    assert!(!store
        .compare_and_save_agent_runtime_snapshot("missing", Some("{}"), "bad", 5)
        .unwrap());
    assert!(store
        .load_agent_runtime_snapshot("missing")
        .unwrap()
        .is_none());
}
