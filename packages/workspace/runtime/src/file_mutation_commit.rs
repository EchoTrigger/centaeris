use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use centaeris_core::runtime::contracts::{EventVisibility, RuntimeEvent};
use centaeris_core::session::store::RuntimeStore;
use centaeris_core::tool::layer::{FileMutationCommitPort, FileMutationCommitRequest};
use hosted_execution::memory::{is_memory_uri, MemoryPath};
use sha2::{Digest, Sha256};

pub(crate) struct WorkspaceFileMutationCommitPort {
    store: Arc<dyn RuntimeStore + Send + Sync>,
    session_id: String,
    agent_run_id: String,
}

impl WorkspaceFileMutationCommitPort {
    pub(crate) fn new(
        store: Arc<dyn RuntimeStore + Send + Sync>,
        session_id: String,
        agent_run_id: String,
    ) -> Result<Self, String> {
        if session_id.trim().is_empty() || agent_run_id.trim().is_empty() {
            return Err("file mutation commit requires Session and AgentRun identity".to_string());
        }
        Ok(Self {
            store,
            session_id,
            agent_run_id,
        })
    }

    fn event(&self, request: &FileMutationCommitRequest) -> Result<RuntimeEvent, String> {
        if request.schema != "file_mutation_pre_apply_commit_v1"
            || !matches!(request.tool_name.as_str(), "write" | "edit")
            || request.tool_call_id.trim().is_empty()
            || !valid_mutation_path(request)
            || request.session_id.as_deref() != Some(self.session_id.as_str())
        {
            return Err("hosted file mutation commit binding mismatch".to_string());
        }
        let payload = serde_json::to_string(request)
            .map_err(|error| format!("encode workspace file mutation fact failed: {error}"))?;
        let identity = format!("{}\0{}", self.agent_run_id, payload);
        let digest = format!("{:x}", Sha256::digest(identity.as_bytes()));
        Ok(RuntimeEvent {
            event_id: format!(
                "file_mutation_commit:{}:{}",
                self.agent_run_id,
                &digest[..24]
            ),
            session_id: self.session_id.clone(),
            task_id: Some(self.agent_run_id.clone()),
            event_type: "file_mutation_pre_apply_committed".to_string(),
            at_ms: now_ms()?,
            visibility: EventVisibility::Internal,
            payload_json: payload,
        })
    }
}

impl FileMutationCommitPort for WorkspaceFileMutationCommitPort {
    fn commit_file_mutation(&self, request: FileMutationCommitRequest) -> Result<(), String> {
        let event = self.event(&request)?;
        if let Err(append_error) = self.store.append_event(event.clone()) {
            let mut offset = 0usize;
            let existing = loop {
                let page = self
                    .store
                    .list_events(self.session_id.as_str(), 256, offset)
                    .map_err(|load_error| {
                        format!(
                            "file mutation commit append failed: {append_error}; replay lookup failed: {load_error}"
                        )
                    })?;
                if let Some(existing) = page
                    .iter()
                    .find(|candidate| candidate.event_id == event.event_id)
                    .cloned()
                {
                    break existing;
                }
                if page.len() < 256 {
                    return Err(format!(
                        "file mutation commit append failed: {append_error}"
                    ));
                }
                offset = offset
                    .checked_add(page.len())
                    .ok_or_else(|| "file mutation replay lookup offset overflow".to_string())?;
            };
            if existing.session_id != event.session_id
                || existing.task_id != event.task_id
                || existing.event_type != event.event_type
                || existing.visibility != event.visibility
                || existing.payload_json != event.payload_json
            {
                return Err("file mutation commit idempotency conflict".to_string());
            }
        }
        Ok(())
    }
}

fn now_ms() -> Result<i64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock before unix epoch: {error}"))?
        .as_millis();
    i64::try_from(millis).map_err(|_| "current timestamp exceeds i64".to_string())
}

fn is_workspace_mutation_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.contains("://") {
        return false;
    }
    for component in trimmed.split('/') {
        if component == ".." {
            return false;
        }
    }
    if let Some(relative) = trimmed.strip_prefix("/mnt/data/") {
        return !relative.is_empty() && !relative.starts_with('/');
    }
    !trimmed.starts_with('/')
}

fn valid_mutation_path(request: &FileMutationCommitRequest) -> bool {
    if is_memory_uri(request.path.as_str()) {
        return request.target_path.is_none()
            && MemoryPath::parse(request.path.as_str()).is_ok_and(|path| path.is_file());
    }
    is_workspace_mutation_path(&request.path)
        && request.target_path.as_deref().is_none_or(|target_path| {
            !is_memory_uri(target_path) && is_workspace_mutation_path(target_path)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_runtime_sqlite::SqliteRuntimeStore;

    fn request(path: &str) -> FileMutationCommitRequest {
        request_with_tool(path, "edit")
    }

    fn request_with_tool(path: &str, tool_name: &str) -> FileMutationCommitRequest {
        FileMutationCommitRequest {
            schema: "file_mutation_pre_apply_commit_v1".to_string(),
            tool_call_id: "call_edit_1".to_string(),
            tool_name: tool_name.to_string(),
            operation: "add".to_string(),
            path: path.to_string(),
            target_path: None,
            previous_file_hash: None,
            read_snapshot_hash: None,
            file_hash: Some(format!("sha256:{}", "a".repeat(64))),
            bytes_written: Some(4),
            added_lines: Some(1),
            removed_lines: Some(0),
            session_id: Some("sess_workspace_1".to_string()),
            execution_owner: "runtime_server".to_string(),
        }
    }

    #[test]
    fn mutation_commit_is_durable_idempotent_and_prefix_bound() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-file-mutation-commit-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(path.as_path());
        let store = Arc::new(SqliteRuntimeStore::new(path.as_path()).expect("store"));
        let port = WorkspaceFileMutationCommitPort::new(
            store.clone(),
            "sess_workspace_1".to_string(),
            "agent_run_workspace_1".to_string(),
        )
        .expect("port");

        port.commit_file_mutation(request("/mnt/data/project/index.md"))
            .expect("first commit");
        port.commit_file_mutation(request("/mnt/data/project/index.md"))
            .expect("idempotent replay");
        port.commit_file_mutation(request("/mnt/data/project/other.md"))
            .expect("same tool call can commit a second operation");
        assert_eq!(
            store
                .list_events("sess_workspace_1", 10, 0)
                .expect("events")
                .len(),
            2
        );
        assert!(port
            .commit_file_mutation(request("/tmp/input.md"))
            .expect_err("non-workspace mutation is forbidden")
            .contains("binding mismatch"));
        drop(port);
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mutation_commit_accepts_canonical_write_tool_name() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-file-mutation-commit-write-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(path.as_path());
        let store = Arc::new(SqliteRuntimeStore::new(path.as_path()).expect("store"));
        let port = WorkspaceFileMutationCommitPort::new(
            store.clone(),
            "sess_workspace_1".to_string(),
            "agent_run_workspace_1".to_string(),
        )
        .expect("port");
        port.commit_file_mutation(request_with_tool("/mnt/data/project/write.md", "write"))
            .expect("canonical write tool name must commit");
        drop(port);
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mutation_commit_accepts_canonical_edit_tool_name() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-file-mutation-commit-edit-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(path.as_path());
        let store = Arc::new(SqliteRuntimeStore::new(path.as_path()).expect("store"));
        let port = WorkspaceFileMutationCommitPort::new(
            store.clone(),
            "sess_workspace_1".to_string(),
            "agent_run_workspace_1".to_string(),
        )
        .expect("port");
        port.commit_file_mutation(request_with_tool("/mnt/data/project/edit.md", "edit"))
            .expect("canonical edit tool name must commit");
        drop(port);
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mutation_commit_rejects_banana_and_empty_tool_name() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-file-mutation-commit-banana-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(path.as_path());
        let store = Arc::new(SqliteRuntimeStore::new(path.as_path()).expect("store"));
        let port = WorkspaceFileMutationCommitPort::new(
            store,
            "sess_workspace_1".to_string(),
            "agent_run_workspace_1".to_string(),
        )
        .expect("port");
        for tool_name in ["banana", ""] {
            assert!(
                port.commit_file_mutation(request_with_tool(
                    "/mnt/data/project/banana.md",
                    tool_name,
                ))
                .expect_err("non-canonical tool name must loud-fail")
                .contains("binding mismatch"),
                "tool name {tool_name:?} must be rejected"
            );
        }
        drop(port);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mutation_commit_accepts_workspace_relative_display_path() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-file-mutation-commit-display-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(path.as_path());
        let store = Arc::new(SqliteRuntimeStore::new(path.as_path()).expect("store"));
        let port = WorkspaceFileMutationCommitPort::new(
            store,
            "sess_workspace_1".to_string(),
            "agent_run_workspace_1".to_string(),
        )
        .expect("port");
        for display_path in [
            "diag.txt",
            "project/index.md",
            "/mnt/data/mutation-proof.txt",
            "/mnt/data/draft..md",
        ] {
            port.commit_file_mutation(request_with_tool(display_path, "write"))
                .expect("workspace display or absolute path must commit");
        }
        drop(port);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mutation_commit_accepts_only_canonical_agent_memory_files() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-file-mutation-commit-memory-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(path.as_path());
        let store = Arc::new(SqliteRuntimeStore::new(path.as_path()).expect("store"));
        let port = WorkspaceFileMutationCommitPort::new(
            store,
            "sess_workspace_1".to_string(),
            "agent_run_workspace_1".to_string(),
        )
        .expect("port");
        let mut memory = request_with_tool("plastic-memories://self/MEMORY.md", "write");
        port.commit_file_mutation(memory.clone())
            .expect("Memory index commit");
        memory.path = "plastic-memories://self/topics/banana.md".to_string();
        port.commit_file_mutation(memory.clone())
            .expect("Memory topic commit");
        for invalid in [
            "plastic-memories:banana",
            "plastic-memories://other/MEMORY.md",
            "plastic-memories://self/topics/Bad.md",
            "plastic-memories://self/",
        ] {
            memory.path = invalid.to_string();
            assert!(
                port.commit_file_mutation(memory.clone()).is_err(),
                "{invalid}"
            );
        }
        drop(port);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mutation_commit_rejects_parent_traversal_and_non_workspace_absolute_paths() {
        let path = std::env::temp_dir().join(format!(
            "centaeris-file-mutation-commit-traversal-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(path.as_path());
        let store = Arc::new(SqliteRuntimeStore::new(path.as_path()).expect("store"));
        let port = WorkspaceFileMutationCommitPort::new(
            store,
            "sess_workspace_1".to_string(),
            "agent_run_workspace_1".to_string(),
        )
        .expect("port");
        for bad_path in [
            "../escape.md",
            "a/../b.md",
            "/tmp/input.md",
            "/mnt/data/",
            "/etc/passwd",
            "banana://workspace/file.md",
            "",
        ] {
            assert!(
                port.commit_file_mutation(request_with_tool(bad_path, "write"))
                    .expect_err("traversal or non-workspace path must loud-fail")
                    .contains("binding mismatch"),
                "path {bad_path:?} must be rejected"
            );
        }
        drop(port);
        let _ = std::fs::remove_file(path);
    }
}
