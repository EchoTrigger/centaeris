use centaeris_core::session::external_context::{ExternalContextObject, ExternalContextStorePort};
use centaeris_core::session::store::AgentRuntimeSnapshotStorePort;
use centaeris_runtime_sqlite::SqliteRuntimeStore;

fn main() {
    let output = std::env::var("CENTAERIS_UPGRADE_FIXTURE_OUT").unwrap();
    let store = SqliteRuntimeStore::new(std::path::Path::new(&output).join("runtime.db")).unwrap();
    store.save_agent_runtime_snapshot("session-1", "{\"fixture\":\"released-v1\"}", 1).unwrap();
    store.upsert_external_context_object(ExternalContextObject {
        schema_version: "external_context.v1".into(), object_id: "source-object-1".into(),
        object_kind: "text".into(), source_provider_id: "fixture".into(), source_tool_name: "read".into(),
        title: "notice.md".into(), content: "notice contents".into(),
        metadata: serde_json::json!({"fixture": "released-v1"}), updated_at_ms: 1,
    }).unwrap();
}
