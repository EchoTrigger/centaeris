use centaeris_core::execution::{
    ExecutionCommandRequest, ExecutionError, ExecutionFileSystemError, ExecutionFileSystemOutput,
    ExecutionFileSystemRequest, ExecutionHostBinding, ExecutionHostCommandOutput,
    ExecutionHostKind, ExecutionHostMode, ExecutionHostRunner, ExecutionHostStatus,
    ExecutionPolicy,
};
use centaeris_core::extension::skills::SkillCatalogLoadConfig;
use centaeris_core::session::reliability::{
    AcquireResourceClaimDisposition, AcquireResourceClaimRequest, ReleaseResourceClaimRequest,
    ResourceClaimStorePort,
};
use centaeris_core::tool::layer::{
    FileMutationCommitPort, FileMutationCommitRequest, ToolInvocationRequest, ToolLayer,
};
use centaeris_runtime_sqlite::SqliteRuntimeStore;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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

fn file_claim(resource_key: &str, owner: &str) -> AcquireResourceClaimRequest {
    AcquireResourceClaimRequest {
        resource_key: resource_key.into(),
        ..claim(
            owner,
            centaeris_core::runtime::contracts::current_timestamp_ms(),
        )
    }
}

fn write_request(call_id: &str, content: &str) -> ToolInvocationRequest {
    ToolInvocationRequest {
        tool_call_id: call_id.into(),
        tool_name: "write".into(),
        args_json: json!({"path": "result.txt", "content": content}).to_string(),
    }
}

struct ReentrantWriteCommit {
    writer: ToolLayer,
    store: SqliteRuntimeStore,
    resource_key: String,
    fail_commit: bool,
}

impl FileMutationCommitPort for ReentrantWriteCommit {
    fn commit_file_mutation(&self, _request: FileMutationCommitRequest) -> Result<(), String> {
        let second = tokio::runtime::Handle::current().block_on(
            self.writer
                .execute_async(write_request("second-write", "second")),
        );
        assert_eq!(second.status, "error");
        assert_eq!(
            second
                .details
                .pointer("/toolError/fileErrorKind")
                .and_then(|v| v.as_str()),
            Some("write_conflict")
        );
        let competitor = self
            .store
            .acquire_resource_claim(file_claim(&self.resource_key, "task-b"))?;
        assert_eq!(
            competitor.disposition,
            AcquireResourceClaimDisposition::Conflict,
            "a rejected same-owner write must preserve the active write's durable claim"
        );
        assert_eq!(competitor.claim.owner, "task-a");
        if self.fail_commit {
            Err("mutation commit rejected".into())
        } else {
            Ok(())
        }
    }
}

struct FileSystemRunner;

impl ExecutionHostRunner for FileSystemRunner {
    fn kind(&self) -> ExecutionHostKind {
        ExecutionHostKind::LocalProcess
    }

    fn status(&self, _policy: &ExecutionPolicy) -> Result<ExecutionHostStatus, ExecutionError> {
        Ok(ExecutionHostStatus::transient_ready(false))
    }

    fn run_file_system_operation(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        centaeris_core::execution::run_policy_scoped_execution_file_system_operation(request)
    }

    fn run_host_command(
        &self,
        _operation_id: Option<&str>,
        _request: ExecutionCommandRequest,
        _cancellation_probe: Option<&centaeris_core::execution::ExecutionCancellationProbe>,
    ) -> Result<ExecutionHostCommandOutput, ExecutionError> {
        unreachable!("file mutation tests do not execute commands")
    }
}

#[tokio::test]
async fn rejected_same_owner_write_preserves_claim_until_active_write_finishes() {
    // A live guard and a recovered same-owner claim must both preserve ownership.
    for recovered_claim in [false, true] {
        for fail_commit in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().canonicalize().unwrap();
            let target = root.join("result.txt");
            std::fs::write(&target, "before").unwrap();
            let resource_key = target.to_string_lossy().to_string();
            let store = SqliteRuntimeStore::new(root.join("runtime.db")).unwrap();
            let binding = ExecutionHostBinding::new(
                ExecutionHostMode::Local,
                Arc::new(FileSystemRunner),
                root.clone(),
                ExecutionPolicy::workspace_write_no_network(&root),
            )
            .unwrap();
            let writer = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
                SkillCatalogLoadConfig::default(),
                Arc::new(binding),
            )
            .unwrap()
            .with_execution_owner("task-a")
            .with_resource_claim_store(Arc::new(store.clone()));
            store
                .acquire_resource_claim(file_claim(&resource_key, "task-b"))
                .unwrap();
            let blocked = writer
                .execute_async(write_request("blocked-write", "blocked"))
                .await;
            assert_eq!(blocked.status, "error");
            assert_eq!(
                store
                    .get_resource_claim("file", &resource_key)
                    .unwrap()
                    .unwrap()
                    .owner,
                "task-b"
            );
            assert!(store
                .release_resource_claim(ReleaseResourceClaimRequest {
                    resource_kind: "file".into(),
                    resource_key: resource_key.clone(),
                    owner: "task-b".into(),
                    released_at_ms: centaeris_core::runtime::contracts::current_timestamp_ms(),
                })
                .unwrap());
            if recovered_claim {
                store
                    .acquire_resource_claim(file_claim(&resource_key, "task-a"))
                    .unwrap();
            }
            let observer = Arc::new(ReentrantWriteCommit {
                writer: writer.clone(),
                store: store.clone(),
                resource_key: resource_key.clone(),
                fail_commit,
            });
            let result = writer
                .with_file_mutation_commit_port(observer)
                .execute_async(write_request("first-write", "first"))
                .await;
            assert_eq!(result.status, if fail_commit { "error" } else { "ok" });
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                if fail_commit { "before" } else { "first" }
            );
            assert!(store
                .get_resource_claim("file", &resource_key)
                .unwrap()
                .is_none());
            let next = store
                .acquire_resource_claim(file_claim(&resource_key, "task-b"))
                .unwrap();
            assert_eq!(next.disposition, AcquireResourceClaimDisposition::Acquired);
        }
    }
}
