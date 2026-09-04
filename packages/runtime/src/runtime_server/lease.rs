use centaeris_core::runtime::TurnControl;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

static NEXT_LEASE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeClientKind {
    Desktop,
    Tui,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRunLease {
    pub lease_id: String,
    pub session_id: String,
    pub agent_run_id: String,
    pub owner_id: String,
    pub owner_kind: RuntimeClientKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionBusy {
    pub active_agent_run_id: String,
    pub origin_owner_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartAgentRunError {
    Invalid(String),
    SessionBusy(SessionBusy),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerExitDisposition {
    Interrupt(AgentRunLease),
    Transfer(AgentRunLease),
}

#[derive(Clone, Debug)]
pub struct ActiveAgentRun {
    pub lease: AgentRunLease,
    pub turn_id: String,
    pub control: TurnControl,
    cancellation_reason: Arc<Mutex<Option<String>>>,
    owner_exit_observed: bool,
}

impl ActiveAgentRun {
    pub fn close_with_cancellation<TResult>(
        &self,
        reason: &str,
        request: impl FnOnce() -> Result<TResult, String>,
    ) -> Result<TResult, String> {
        self.control.close_with(|| {
            let result = request()?;
            let mut cancellation_reason = self
                .cancellation_reason
                .lock()
                .map_err(|_| "active agent run cancellation lock poisoned".to_string())?;
            if cancellation_reason.is_none() {
                *cancellation_reason = Some(reason.to_string());
            }
            Ok(result)
        })
    }

    pub fn cancellation_reason(&self) -> Result<Option<String>, String> {
        self.cancellation_reason
            .lock()
            .map_err(|_| "active agent run cancellation lock poisoned".to_string())
            .map(|reason| reason.clone())
    }
}

/// Serializes only short state transitions. Actual model/tool work never runs
/// under this mutex, so different sessions can continue independently.
#[derive(Debug, Default)]
struct AgentRunRegistryState {
    active_by_session: HashMap<String, ActiveAgentRun>,
    deleting_session_ids: HashSet<String>,
}

#[derive(Debug, Default)]
pub struct AgentRunRegistry {
    state: Mutex<AgentRunRegistryState>,
    changed: Condvar,
}

impl AgentRunRegistry {
    pub fn start(
        &self,
        session_id: &str,
        agent_run_id: &str,
        turn_id: &str,
        owner_id: &str,
        owner_kind: RuntimeClientKind,
        control: TurnControl,
    ) -> Result<AgentRunLease, StartAgentRunError> {
        let session_id =
            required_identifier(session_id, "sessionId").map_err(StartAgentRunError::Invalid)?;
        let agent_run_id =
            required_identifier(agent_run_id, "agentRunId").map_err(StartAgentRunError::Invalid)?;
        let turn_id =
            required_identifier(turn_id, "turnId").map_err(StartAgentRunError::Invalid)?;
        let owner_id =
            required_identifier(owner_id, "ownerId").map_err(StartAgentRunError::Invalid)?;
        let mut state = self
            .state
            .lock()
            .expect("Session AgentRun registry lock poisoned");
        if state.deleting_session_ids.contains(session_id.as_str()) {
            return Err(StartAgentRunError::Invalid(format!(
                "session is being deleted: {session_id}"
            )));
        }
        if let Some(existing) = state.active_by_session.get(session_id.as_str()) {
            return Err(StartAgentRunError::SessionBusy(SessionBusy {
                active_agent_run_id: existing.lease.agent_run_id.clone(),
                origin_owner_id: existing.lease.owner_id.clone(),
            }));
        }
        if state
            .active_by_session
            .values()
            .any(|existing| existing.lease.agent_run_id == agent_run_id)
        {
            return Err(StartAgentRunError::Invalid(format!(
                "agentRunId is already active: {agent_run_id}"
            )));
        }
        let lease = AgentRunLease {
            lease_id: format!("lease:{}", NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed)),
            session_id,
            agent_run_id,
            owner_id,
            owner_kind,
        };
        state.active_by_session.insert(
            lease.session_id.clone(),
            ActiveAgentRun {
                lease: lease.clone(),
                turn_id,
                control,
                cancellation_reason: Arc::new(Mutex::new(None)),
                owner_exit_observed: false,
            },
        );
        Ok(lease)
    }

    pub fn finish(&self, lease_id: &str) -> Result<AgentRunLease, String> {
        let lease_id = required_identifier(lease_id, "leaseId")?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?;
        let session_id = state
            .active_by_session
            .iter()
            .find_map(|(session_id, active)| {
                (active.lease.lease_id == lease_id).then(|| session_id.clone())
            })
            .ok_or_else(|| format!("unknown agent run lease: {lease_id}"))?;
        let active_agent_run = state
            .active_by_session
            .remove(session_id.as_str())
            .ok_or_else(|| format!("agent run lease disappeared before finish: {lease_id}"))?;
        drop(state);
        self.changed.notify_all();
        active_agent_run.control.close()?;
        Ok(active_agent_run.lease)
    }

    pub fn active(&self, agent_run_id: &str) -> Result<Option<ActiveAgentRun>, String> {
        let agent_run_id = required_identifier(agent_run_id, "agentRunId")?;
        self.state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())
            .map(|state| {
                state
                    .active_by_session
                    .values()
                    .find(|item| item.lease.agent_run_id == agent_run_id)
                    .cloned()
            })
    }

    pub fn active_for_session(&self, session_id: &str) -> Result<Option<ActiveAgentRun>, String> {
        let session_id = required_identifier(session_id, "sessionId")?;
        self.state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())
            .map(|state| state.active_by_session.get(session_id.as_str()).cloned())
    }

    /// Returns exactly the AgentRuns affected by one host exit. Interrupted AgentRuns
    /// remain active until the session actor persists their terminal state and
    /// calls `finish`, preventing a second turn from racing that transition.
    pub fn owner_exited(
        &self,
        owner_id: &str,
        desktop_owner_ids: &[String],
    ) -> Result<Vec<OwnerExitDisposition>, String> {
        let owner_id = required_identifier(owner_id, "ownerId")?;
        let mut desktop_owner_ids = desktop_owner_ids
            .iter()
            .map(|owner_id| required_identifier(owner_id, "desktopOwnerId"))
            .collect::<Result<Vec<_>, _>>()?;
        desktop_owner_ids.sort();
        desktop_owner_ids.dedup();
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?;
        let mut dispositions = Vec::new();
        for active_agent_run in state
            .active_by_session
            .values_mut()
            .filter(|active_agent_run| {
                active_agent_run.lease.owner_id == owner_id && !active_agent_run.owner_exit_observed
            })
        {
            if active_agent_run.lease.owner_kind == RuntimeClientKind::Tui
                && desktop_owner_ids.len() == 1
            {
                active_agent_run.lease.owner_id = desktop_owner_ids[0].clone();
                active_agent_run.lease.owner_kind = RuntimeClientKind::Desktop;
                active_agent_run.owner_exit_observed = false;
                dispositions.push(OwnerExitDisposition::Transfer(
                    active_agent_run.lease.clone(),
                ));
            } else {
                active_agent_run.owner_exit_observed = true;
                dispositions.push(OwnerExitDisposition::Interrupt(
                    active_agent_run.lease.clone(),
                ));
            }
        }
        dispositions.sort_by(|left, right| {
            disposition_agent_run_id(left).cmp(disposition_agent_run_id(right))
        });
        Ok(dispositions)
    }

    pub fn active_agent_run_count(&self) -> Result<usize, String> {
        self.state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())
            .map(|state| state.active_by_session.len())
    }

    pub fn owner_id_for_lease(&self, lease_id: &str) -> Result<String, String> {
        let lease_id = required_identifier(lease_id, "leaseId")?;
        self.state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?
            .active_by_session
            .values()
            .find(|active| active.lease.lease_id == lease_id)
            .map(|active| active.lease.owner_id.clone())
            .ok_or_else(|| format!("unknown agent run lease: {lease_id}"))
    }

    pub fn with_session_deletion<TResult>(
        &self,
        session_ids: &[String],
        delete: impl FnOnce() -> Result<TResult, String>,
    ) -> Result<TResult, String> {
        let mut session_ids = session_ids
            .iter()
            .map(|session_id| required_identifier(session_id, "sessionId"))
            .collect::<Result<Vec<_>, _>>()?;
        session_ids.sort();
        session_ids.dedup();
        if session_ids.is_empty() {
            return Err("session deletion requires at least one sessionId".to_string());
        }
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?;
            if let Some(session_id) = session_ids
                .iter()
                .find(|session_id| state.deleting_session_ids.contains(session_id.as_str()))
            {
                return Err(format!("session deletion is already active: {session_id}"));
            }
            state
                .deleting_session_ids
                .extend(session_ids.iter().cloned());
        }
        let registration = SessionDeletionRegistration {
            registry: self,
            session_ids,
        };
        let result = delete();
        drop(registration);
        result
    }

    pub fn wait_until_sessions_inactive(&self, session_ids: &[String]) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?;
        while session_ids
            .iter()
            .any(|session_id| state.active_by_session.contains_key(session_id.as_str()))
        {
            state = self
                .changed
                .wait(state)
                .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?;
        }
        Ok(())
    }
}

struct SessionDeletionRegistration<'a> {
    registry: &'a AgentRunRegistry,
    session_ids: Vec<String>,
}

impl Drop for SessionDeletionRegistration<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.registry.state.lock() {
            for session_id in &self.session_ids {
                state.deleting_session_ids.remove(session_id);
            }
        }
    }
}

fn disposition_agent_run_id(disposition: &OwnerExitDisposition) -> &str {
    match disposition {
        OwnerExitDisposition::Interrupt(lease) | OwnerExitDisposition::Transfer(lease) => {
            lease.agent_run_id.as_str()
        }
    }
}

fn required_identifier(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_agent_run(
        registry: &AgentRunRegistry,
        session_id: &str,
        agent_run_id: &str,
        owner_id: &str,
        owner_kind: RuntimeClientKind,
    ) -> Result<AgentRunLease, StartAgentRunError> {
        registry.start(
            session_id,
            agent_run_id,
            format!("turn:{agent_run_id}").as_str(),
            owner_id,
            owner_kind,
            TurnControl::new(),
        )
    }

    #[test]
    fn accepts_different_sessions_and_rejects_a_second_run_in_one_session() {
        let registry = AgentRunRegistry::default();
        let first = start_agent_run(
            &registry,
            "chat-a",
            "agent-run-a",
            "host-a",
            RuntimeClientKind::Desktop,
        )
        .expect("start first AgentRun");
        start_agent_run(
            &registry,
            "chat-b",
            "agent-run-b",
            "host-b",
            RuntimeClientKind::Desktop,
        )
        .expect("start independent session");
        assert!(matches!(
            start_agent_run(
                &registry,
                "chat-c",
                "agent-run-b",
                "host-c",
                RuntimeClientKind::Desktop,
            ),
            Err(StartAgentRunError::Invalid(message)) if message.contains("already active")
        ));

        let busy = start_agent_run(
            &registry,
            "chat-a",
            "agent-run-c",
            "host-c",
            RuntimeClientKind::Tui,
        )
        .expect_err("same session must be busy");
        let StartAgentRunError::SessionBusy(busy) = busy else {
            panic!("same session must return SessionBusy");
        };
        assert_eq!(busy.active_agent_run_id, first.agent_run_id);
        assert_eq!(busy.origin_owner_id, "host-a");
    }

    #[test]
    fn owner_exit_transfers_only_tui_run_with_exactly_one_desktop() {
        let registry = AgentRunRegistry::default();
        let desktop_owned = start_agent_run(
            &registry,
            "chat-a",
            "agent-run-a",
            "desktop-a",
            RuntimeClientKind::Desktop,
        )
        .expect("start Desktop AgentRun");
        let tui_owned = start_agent_run(
            &registry,
            "chat-b",
            "agent-run-b",
            "tui-a",
            RuntimeClientKind::Tui,
        )
        .expect("start TUI AgentRun");
        start_agent_run(
            &registry,
            "chat-c",
            "agent-run-c",
            "tui-b",
            RuntimeClientKind::Tui,
        )
        .expect("start ambiguous TUI AgentRun");

        assert_eq!(
            registry
                .owner_exited("tui-a", &["desktop-a".to_string()])
                .expect("TUI owner exit"),
            vec![OwnerExitDisposition::Transfer(AgentRunLease {
                owner_id: "desktop-a".to_string(),
                owner_kind: RuntimeClientKind::Desktop,
                ..tui_owned.clone()
            })]
        );
        assert_eq!(
            registry
                .owner_id_for_lease(tui_owned.lease_id.as_str())
                .expect("transferred owner"),
            "desktop-a"
        );
        assert_eq!(
            registry
                .owner_exited("desktop-a", &[])
                .expect("Desktop owner exit"),
            vec![
                OwnerExitDisposition::Interrupt(desktop_owned),
                OwnerExitDisposition::Interrupt(AgentRunLease {
                    owner_id: "desktop-a".to_string(),
                    owner_kind: RuntimeClientKind::Desktop,
                    ..tui_owned
                }),
            ]
        );
        assert!(matches!(
            registry
                .owner_exited(
                    "tui-b",
                    &["desktop-a".to_string(), "desktop-b".to_string()]
                )
                .expect("ambiguous Desktop exit")
                .as_slice(),
            [OwnerExitDisposition::Interrupt(lease)] if lease.agent_run_id == "agent-run-c"
        ));
    }

    #[test]
    fn active_agent_run_count_tracks_start_and_finish() {
        let registry = AgentRunRegistry::default();
        let lease = start_agent_run(
            &registry,
            "chat-a",
            "agent-run-a",
            "desktop-a",
            RuntimeClientKind::Desktop,
        )
        .expect("start AgentRun");
        assert_eq!(registry.active_agent_run_count().expect("active count"), 1);

        registry
            .finish(lease.lease_id.as_str())
            .expect("finish AgentRun");
        assert_eq!(registry.active_agent_run_count().expect("active count"), 0);
    }

    #[test]
    fn session_deletion_blocks_new_agent_run_until_cleanup_finishes() {
        let registry = AgentRunRegistry::default();
        registry
            .with_session_deletion(&["chat-a".to_string()], || {
                assert!(matches!(
                    start_agent_run(
                        &registry,
                        "chat-a",
                        "agent-run-a",
                        "desktop-a",
                        RuntimeClientKind::Desktop,
                    ),
                    Err(StartAgentRunError::Invalid(message)) if message.contains("being deleted")
                ));
                Ok(())
            })
            .expect("delete Session");
        start_agent_run(
            &registry,
            "chat-a",
            "agent-run-a",
            "desktop-a",
            RuntimeClientKind::Desktop,
        )
        .expect("deletion guard must release after cleanup");
    }

    #[test]
    fn unknown_or_empty_leases_fail_loudly() {
        let registry = AgentRunRegistry::default();
        assert!(registry.finish(" ").unwrap_err().contains("leaseId"));
        assert!(registry
            .finish("lease:banana")
            .unwrap_err()
            .contains("unknown"));
        assert!(matches!(
            start_agent_run(
                &registry,
                " ",
                "agent-run-a",
                "owner-a",
                RuntimeClientKind::Desktop,
            ),
            Err(StartAgentRunError::Invalid(_))
        ));
    }
}
