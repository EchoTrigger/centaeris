//! Run with `cargo rustc --example sqlite_abort -- -C panic=abort`, then execute
//! the binary. Including the adapter source keeps private transaction SQL shared.
include!("../src/lib.rs");

use centaeris_core::session::external_context::{
    ExternalContextObject, ExternalContextObjectLink, ExternalContextStorePort,
};
use centaeris_core::session::reliability::{
    CompleteRuntimeJobRequest, RuntimeBackoffPolicy, RuntimeJobRecord, RuntimeJobStatus,
    RuntimeJobStorePort, ScheduleRuntimeJobRequest,
};
use centaeris_core::session::store::UpsertExternalContextLinkAndCompleteJobRequest;
use std::io::Write;

fn seed(store: &SqliteRuntimeStore) {
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest {
            job: RuntimeJobRecord {
                job_id: "job".into(),
                job_kind: "test.job".into(),
                status: RuntimeJobStatus::Running,
                run_at_ms: 100,
                lease_owner: Some("owner".into()),
                lease_expires_at_ms: Some(1000),
                heartbeat_at_ms: Some(1),
                retry_count: 0,
                max_retries: 1,
                backoff_policy: RuntimeBackoffPolicy::default(),
                idempotency_key: "test.job:job".into(),
                session_id: Some("session".into()),
                branch_id: None,
                checkpoint_id: None,
                payload_ref: None,
                output_refs: vec![],
                last_error: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        })
        .unwrap();
}

fn child(path: PathBuf, after_commit: bool) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let connection = tokio_rusqlite::Connection::open(path).await.unwrap();
            connection
                .call(move |conn| {
                    configure_conn(conn).unwrap();
                    let request = UpsertExternalContextLinkAndCompleteJobRequest {
                        object: Some(ExternalContextObject {
                            schema_version: "external_context.v1".into(),
                            object_id: "object".into(),
                            object_kind: "testObject".into(),
                            source_provider_id: "provider".into(),
                            source_tool_name: "tool".into(),
                            title: "title".into(),
                            content: "content".into(),
                            metadata: serde_json::json!({}),
                            updated_at_ms: 1,
                        }),
                        link: Some(ExternalContextObjectLink {
                            session_id: "session".into(),
                            object_id: "object".into(),
                            turn_id: Some("turn".into()),
                            tool_call_id: Some("call".into()),
                            source_provider_id: "provider".into(),
                            source_tool_name: "tool".into(),
                            linked_at_ms: 1,
                        }),
                        complete_job: CompleteRuntimeJobRequest {
                            job_id: "job".into(),
                            lease_owner: "owner".into(),
                            output_refs: vec!["object".into()],
                            completed_at_ms: 999,
                        },
                    };
                    sqlite_transactions::upsert_external_context_link_and_complete_job_conn(
                        conn,
                        request,
                        &mut |event| {
                            let abort = if after_commit {
                                matches!(event, sqlite_transactions::TransactionEvent::Committed)
                            } else {
                                matches!(event, sqlite_transactions::TransactionEvent::BeforeCommit)
                            };
                            if abort {
                                println!("database abort boundary reached");
                                std::io::stdout().flush().unwrap();
                                panic!("database worker abort");
                            }
                        },
                    )
                    .unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
    panic!("database abort boundary not reached");
}

fn main() {
    if !cfg!(panic = "abort") {
        eprintln!("compile this executable with -C panic=abort");
        std::process::exit(2);
    }
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--child") {
        child(PathBuf::from(&args[2]), args[3] == "committed");
        return;
    }
    for committed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.db");
        seed(&SqliteRuntimeStore::new(&path).unwrap());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--child")
            .arg(&path)
            .arg(if committed {
                "committed"
            } else {
                "uncommitted"
            })
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("database abort boundary reached"),
            "child failed before reaching the database boundary"
        );
        let store = SqliteRuntimeStore::new(path).unwrap();
        assert_eq!(
            store.get_runtime_job("job").unwrap().unwrap().status,
            if committed {
                RuntimeJobStatus::Succeeded
            } else {
                RuntimeJobStatus::Running
            }
        );
        assert_eq!(
            store
                .load_external_context_object("object")
                .unwrap()
                .is_some(),
            committed
        );
        store
            .with_conn(|conn| {
                for table in ["external_context_links", "runtime_job_outbox"] {
                    let rows: i64 = conn
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                        .unwrap();
                    assert_eq!(rows, i64::from(committed));
                }
                Ok(())
            })
            .unwrap();
    }
    println!("panic=abort database worker: 2 process-exit cases passed");
}
