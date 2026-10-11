//! Actual local scan entry, native Session readiness and file-backed Job facts.
use super::*;
use centaeris_core::runtime::subagent::{build_subagent_run_job, SubagentRunJobRequest};
use centaeris_core::session::external_context::{
    ExternalContextObject, EXTERNAL_CONTEXT_SCHEMA_VERSION,
};
use centaeris_core::session::reliability::{RuntimeJobOutboxPort, ScheduleRuntimeJobRequest};
use std::ffi::OsString;

struct Profile {
    root: PathBuf,
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl Profile {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "centaeris-packet-progress-{}-{}",
            std::process::id(),
            current_timestamp_ms()
        ));
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        let previous = [
            ("CENTAERIS_DESKTOP_DATA_DIR", root.clone()),
            ("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR", root.join("sessions")),
            (
                "CENTAERIS_AGENT_RUNTIME_DB_PATH",
                root.join("runtime.sqlite3"),
            ),
        ]
        .into_iter()
        .map(|(name, value)| {
            let old = std::env::var_os(name);
            std::env::set_var(name, value);
            (name, old)
        })
        .collect();
        Self { root, previous }
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        for (name, value) in &self.previous {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn packet(parent: &str, child: &str, job: &RuntimeJobRecord, cwd: &str) -> serde_json::Value {
    let read = centaeris_core::tool::list_tool_contracts()
        .into_iter()
        .find(|contract| contract.name == "read")
        .unwrap();
    let digest = read.contract_digest().unwrap();
    serde_json::json!({ "workPacket": {
        "run_context": {
            "schema": "agent_run_context_v1", "sessionId": parent,
            "branchId": child, "turnId": "child-turn", "agentRunId": job.job_id,
            "agentRef": { "agentId": "valid", "agentRunId": job.job_id },
            "parentAgentRef": { "agentId": "main", "agentRunId": "parent-run" },
            "parentTurnId": "parent-turn", "depth": 1, "cwd": cwd,
            "cancellationScopeId": "cancel-child", "parentCancellationScopeId": "cancel-parent",
            "cancelOnParentCancel": true, "createdAtMs": 1
        },
        "task_brief": { "task_id": null, "objective": "Inspect a file", "success_criteria": [],
            "constraints": [], "output_hint": "Inspection" },
        "hot_view": { "summary": "", "recent_message_ids": [], "state_kv": {} },
        "object_refs": [], "allowedTools": ["read"],
        "delegatedToolContracts": [{ "name": read.name, "providerId": read.provider_id.unwrap(),
            "contractDigest": digest, "concurrencySafe": read.concurrency_safe }],
        "writablePathPrefixes": [],
        "output_contract": { "response_mode": "summary", "expected_sections": [],
            "require_artifact_refs": false, "max_summary_chars": 100 },
        "parent_checkpoint_id": null, "context_mode": "Borrow"
    }})
}

fn object(id: &str, content: String) -> ExternalContextObject {
    ExternalContextObject {
        schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.into(),
        object_id: id.into(),
        object_kind: "subagent_work_packet".into(),
        source_provider_id: "centaeris.core".into(),
        source_tool_name: "agent".into(),
        title: id.into(),
        content,
        metadata: serde_json::json!({}),
        updated_at_ms: 1,
    }
}

fn job(parent: &str, name: &str, at: i64) -> RuntimeJobRecord {
    build_subagent_run_job(SubagentRunJobRequest {
        session_id: parent.into(),
        parent_turn_id: "parent-turn".into(),
        tool_call_id: name.into(),
        subagent_id: name.into(),
        work_packet_ref: format!("packet:{name}"),
        checkpoint_id: None,
        run_at_ms: at,
        created_at_ms: at,
        max_retries: 0,
    })
}

#[test]
fn bad_work_packets_do_not_starve_valid_jobs() {
    let _guard = message_log::test_env_mutex()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let profile = Profile::new();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let cwd = profile.root.join("workspace").to_string_lossy().to_string();
        let parent = sessions::create(sessions::SessionCreateRequest {
            title: None,
            cwd: cwd.clone(),
        })
        .unwrap();
        let store = agent_runtime::agent_runtime_store_actor().unwrap();
        let bad = ["missing", "malformed-json", "unsupported-schema", "corrupt-content"]
            .map(|name| job(&parent.id, name, 1));
        for row in &bad {
            store
                .schedule_runtime_job(ScheduleRuntimeJobRequest { job: row.clone() })
                .await
                .unwrap();
        }
        store
            .upsert_external_context_object(object("packet:malformed-json", "{".into()))
            .await
            .unwrap();
        let mut unsupported = packet(&parent.id, "unused-child", &bad[2], &cwd);
        unsupported["workPacket"]["run_context"]["schema"] = "unsupported-schema".into();
        store
            .upsert_external_context_object(object(
                "packet:unsupported-schema",
                unsupported.to_string(),
            ))
            .await
            .unwrap();
        store.upsert_external_context_object(object(
            "packet:corrupt-content", packet(&parent.id, "unused-child", &bad[3], &cwd).to_string(),
        )).await.unwrap();
        // Corrupt one encoded object, not the database connection or its schema.
        rusqlite::Connection::open(profile.root.join("runtime.sqlite3")).unwrap()
            .execute("UPDATE external_context_objects SET content_sha256='corrupt' WHERE object_id='packet:corrupt-content'", [])
            .unwrap();
        let valid = job(&parent.id, "valid", 2);
        store
            .upsert_external_context_object(object(
                "packet:valid",
                packet(&parent.id, "session-agent-valid", &valid, &cwd).to_string(),
            ))
            .await
            .unwrap();
        store
            .schedule_runtime_job(ScheduleRuntimeJobRequest { job: valid.clone() })
            .await
            .unwrap();
        sessions::ensure_agent_session(
            "session-agent-valid",
            &parent.id,
            &valid.job_id,
            "valid",
            &cwd,
            2,
        )
        .unwrap();
        message_log::append_agent_turn_queued(
            "session-agent-valid",
            "child-turn",
            &valid.job_id,
            "valid",
            2,
        )
        .unwrap();
        for _ in 0..2 {
            assert_eq!(
                next_queued_parent_session().await.unwrap(),
                Some(parent.id.clone())
            );
        }
        for row in bad {
            let current = store.get_runtime_job(&row.job_id).await.unwrap().unwrap();
            assert_eq!(current.status, RuntimeJobStatus::Failed);
            assert!(current.last_error.is_some());
        }
        assert_eq!(
            store
                .get_runtime_job(&valid.job_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuntimeJobStatus::Queued
        );
        let reopened =
            centaeris_runtime_sqlite::SqliteRuntimeStore::new(profile.root.join("runtime.sqlite3"))
                .unwrap();
        assert_eq!(
            reopened.list_pending_runtime_job_outbox(10).unwrap().len(),
            4
        );
    });
}
