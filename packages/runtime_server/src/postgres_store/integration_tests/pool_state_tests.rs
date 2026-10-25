use super::*;

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_pool_scrubs_session_state_on_the_same_backend() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let url = test_url();
    reset_store(&url);
    let store =
        PostgresRuntimeStore::new_with_pool_limits(&url, 1, Duration::from_millis(100)).unwrap();
    let first: i32 = store.with_client(|client| {
        let pid = client.query_one("SELECT pg_backend_pid()", &[]).map_err(|e| e.to_string())?.get(0);
        client.batch_execute("CREATE TEMP TABLE pool_scoped_state(value integer); PREPARE pool_scoped_plan AS SELECT 7; LISTEN pool_scoped_channel; SELECT pg_advisory_lock(927462); SET statement_timeout='11s'; SET lock_timeout='9s'; SET application_name='pool-scoped'; SET search_path=pg_catalog").map_err(|e| e.to_string())?;
        Ok(pid)
    }).unwrap();
    store.with_client(|client| {
        let row = client.query_one("SELECT pg_backend_pid(),current_schemas(false),current_setting('statement_timeout'),current_setting('lock_timeout'),current_setting('application_name'),to_regclass('pg_temp.pool_scoped_state') IS NULL,(SELECT count(*) FROM pg_prepared_statements WHERE name='pool_scoped_plan'),(SELECT count(*) FROM pg_listening_channels()),(SELECT count(*) FROM pg_locks WHERE pid=pg_backend_pid() AND locktype='advisory')", &[]).map_err(|e| e.to_string())?;
        assert_eq!(row.get::<_,i32>(0), first, "reuse must preserve a clean live backend");
        assert_eq!(row.get::<_,Vec<String>>(1), ["runtime", "public"]);
        assert_eq!(row.get::<_,String>(2), "0");
        assert_eq!(row.get::<_,String>(3), "0");
        assert_eq!(row.get::<_,String>(4), "centaeris-runtime-ordinary");
        assert!(row.get::<_,bool>(5));
        for index in 6..=8 { assert_eq!(row.get::<_,i64>(index), 0); }
        Ok(())
    }).unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_runtime_pool_reset_all_restores_runtime_namespace_and_custom_defaults() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let url = test_url();
    reset_store(&url);
    let url = format!(
        "{url}{}options=-c%20synchronous_commit%3Doff",
        if url.contains('?') { "&" } else { "?" }
    );
    let store =
        PostgresRuntimeStore::new_with_pool_limits(&url, 1, Duration::from_millis(100)).unwrap();
    store
        .with_client(|client| {
            client
                .batch_execute("SET synchronous_commit=on; SET search_path=pg_catalog; RESET ALL")
                .map_err(|e| e.to_string())?;
            let row = client
                .query_one(
                    "SELECT current_schemas(false),current_setting('synchronous_commit')",
                    &[],
                )
                .map_err(|e| e.to_string())?;
            assert_eq!(
                row.get::<_, Vec<String>>(0),
                ["runtime", "public"],
                "the connection startup baseline must restore the Runtime namespace"
            );
            assert_eq!(
                row.get::<_, String>(1),
                "off",
                "unrelated URL startup defaults must survive"
            );
            Ok(())
        })
        .unwrap();
}
