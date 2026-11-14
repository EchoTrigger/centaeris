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
        if state.session_id != session_id {
            return Err("session_snapshot_identity_mismatch".into());
        }
        crate::session::history::validate_session_snapshot_history(&state)?;
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

    /// Commits and returns the same normalized working state for further publication.
    pub fn save_session_owned(
        &self,
        session: SessionStateSnapshot,
    ) -> Result<SessionStateSnapshot, String> {
        let prepared = crate::session::history::prepare_session_snapshot_state_write(session)?;
        self.store
            .save_prepared_agent_runtime_snapshot(prepared, now_ms())
    }

    /// Commits a reconstructed view and its authoritative inactive-source decisions.
    pub fn save_recovered_session_owned(
        &self,
        session: SessionStateSnapshot,
        retired_message_ids: Vec<String>,
    ) -> Result<SessionStateSnapshot, String> {
        let prepared = crate::session::history::prepare_recovered_session_snapshot_write(
            session,
            retired_message_ids,
        )?;
        self.store
            .save_prepared_agent_runtime_snapshot(prepared, now_ms())
    }
}

fn now_ms() -> i64 {
    crate::runtime::contracts::current_timestamp_ms()
}
