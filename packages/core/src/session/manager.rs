use crate::session::state::SessionStateSnapshot;
use crate::session::store::AgentRuntimeSnapshotStorePort;

#[derive(Debug, Clone)]
pub struct SessionManager<S: AgentRuntimeSnapshotStorePort> {
    store: S,
}

impl<S: AgentRuntimeSnapshotStorePort> SessionManager<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn load_session(&self, session_id: &str) -> Result<Option<SessionStateSnapshot>, String> {
        let Some(raw) = self.store.load_agent_runtime_snapshot(session_id)? else {
            return Ok(None);
        };
        let state = serde_json::from_str::<SessionStateSnapshot>(raw.as_str())
            .map_err(|err| format!("deserialize session state failed: {err}"))?;
        Ok(Some(state))
    }

    pub fn load_or_create_session(&self, session_id: &str) -> Result<SessionStateSnapshot, String> {
        if let Some(existing) = self.load_session(session_id)? {
            return Ok(existing);
        }
        Ok(SessionStateSnapshot::new(session_id.to_string(), now_ms()))
    }

    pub fn save_session(&self, session: &SessionStateSnapshot) -> Result<(), String> {
        let now = now_ms();
        let snapshot_json = serde_json::to_string(session)
            .map_err(|err| format!("serialize session state failed: {err}"))?;
        self.store.save_agent_runtime_snapshot(
            session.session_id.as_str(),
            snapshot_json.as_str(),
            now,
        )
    }
}

fn now_ms() -> i64 {
    crate::runtime::contracts::current_timestamp_ms()
}
