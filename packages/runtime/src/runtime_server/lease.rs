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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionBusy {
    pub active_agent_run_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartAgentRunError {
    Invalid(String),
    SessionBusy(SessionBusy),
}

#[derive(Clone, Debug)]
pub struct ActiveAgentRun {
    pub lease: AgentRunLease,
    pub turn_id: String,
    pub control: TurnControl,
    cancellation_reason: Arc<Mutex<Option<CancellationCause>>>,
}

#[derive(Clone, Debug)]
struct CancellationCause {
    reason: String,
    reason_type: &'static str,
}

impl ActiveAgentRun {
    pub fn close_with_cancellation<TResult>(
        &self,
        reason: &str,
        request: impl FnOnce() -> Result<TResult, String>,
    ) -> Result<TResult, String> {
        self.close_with_cause(reason, "cancelled", request)
    }

    pub fn close_with_shutdown<TResult>(
        &self,
        request: impl FnOnce() -> Result<TResult, String>,
    ) -> Result<TResult, String> {
        self.close_with_cause("runtime_server_shutdown", "shutdown", request)
    }

    fn close_with_cause<TResult>(
        &self,
        reason: &str,
        reason_type: &'static str,
        request: impl FnOnce() -> Result<TResult, String>,
    ) -> Result<TResult, String> {
        self.control.close_with(|| {
            let result = request()?;
            let mut cancellation_reason = self
                .cancellation_reason
                .lock()
                .map_err(|_| "active agent run cancellation lock poisoned".to_string())?;
            if cancellation_reason.is_none() {
                *cancellation_reason = Some(CancellationCause {
                    reason: reason.to_string(),
                    reason_type,
                });
            }
            Ok(result)
        })
    }

    pub fn cancellation_reason(&self) -> Result<Option<String>, String> {
        self.cancellation_reason
            .lock()
            .map_err(|_| "active agent run cancellation lock poisoned".to_string())
            .map(|reason| reason.as_ref().map(|cause| cause.reason.clone()))
    }

    pub fn cancellation_reason_type(&self) -> Result<Option<&'static str>, String> {
        self.cancellation_reason
            .lock()
            .map_err(|_| "active agent run cancellation lock poisoned".to_string())
            .map(|reason| reason.as_ref().map(|cause| cause.reason_type))
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
        control: TurnControl,
    ) -> Result<AgentRunLease, StartAgentRunError> {
        let session_id =
            required_identifier(session_id, "sessionId").map_err(StartAgentRunError::Invalid)?;
        let agent_run_id =
            required_identifier(agent_run_id, "agentRunId").map_err(StartAgentRunError::Invalid)?;
        let turn_id =
            required_identifier(turn_id, "turnId").map_err(StartAgentRunError::Invalid)?;
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
        };
        state.active_by_session.insert(
            lease.session_id.clone(),
            ActiveAgentRun {
                lease: lease.clone(),
                turn_id,
                control,
                cancellation_reason: Arc::new(Mutex::new(None)),
            },
        );
        Ok(lease)
    }

    pub fn finish(&self, lease_id: &str) -> Result<AgentRunLease, String> {
        self.finish_with(lease_id, TurnControl::close)
    }

    fn finish_with(
        &self,
        lease_id: &str,
        close: impl FnOnce(&TurnControl) -> Result<(), String>,
    ) -> Result<AgentRunLease, String> {
        let lease_id = required_identifier(lease_id, "leaseId")?;
        let active = self
            .state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?
            .active_by_session
            .values()
            .find(|active| active.lease.lease_id == lease_id)
            .cloned()
            .ok_or_else(|| format!("unknown agent run lease: {lease_id}"))?;
        // Keep the session occupied while Core closes input admission. Do not
        // hold the registry mutex: close may wait on a control callback that
        // itself needs the registry, and unrelated sessions must keep running.
        close(&active.control)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?;
        let session_id = active.lease.session_id;
        if state
            .active_by_session
            .get(&session_id)
            .is_none_or(|current| current.lease.lease_id != lease_id)
        {
            return Err(format!("unknown agent run lease: {lease_id}"));
        }
        let finished = state
            .active_by_session
            .remove(&session_id)
            .ok_or_else(|| format!("agent run lease disappeared before finish: {lease_id}"))?;
        drop(state);
        self.changed.notify_all();
        Ok(finished.lease)
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

    pub fn active_agent_runs(&self) -> Result<Vec<ActiveAgentRun>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?;
        let mut runs = state
            .active_by_session
            .values()
            .cloned()
            .collect::<Vec<_>>();
        runs.sort_by(|left, right| left.lease.agent_run_id.cmp(&right.lease.agent_run_id));
        Ok(runs)
    }

    pub fn active_agent_run_count(&self) -> Result<usize, String> {
        self.state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())
            .map(|state| state.active_by_session.len())
    }

    pub fn require_lease(&self, lease_id: &str) -> Result<(), String> {
        let lease_id = required_identifier(lease_id, "leaseId")?;
        self.state
            .lock()
            .map_err(|_| "Session AgentRun registry lock poisoned".to_string())?
            .active_by_session
            .values()
            .find(|active| active.lease.lease_id == lease_id)
            .map(|_| ())
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

    #[test]
    fn shutdown_is_typed_and_the_first_stop_cause_wins() {
        for shutdown_first in [false, true] {
            let registry = AgentRunRegistry::default();
            registry
                .start("session", "run", "turn", TurnControl::new())
                .unwrap();
            let active = registry.active("run").unwrap().unwrap();
            if shutdown_first {
                active.close_with_shutdown(|| Ok(())).unwrap();
                active
                    .close_with_cancellation("user_cancelled", || Ok(()))
                    .unwrap();
                assert_eq!(active.cancellation_reason_type().unwrap(), Some("shutdown"));
                assert_eq!(
                    active.cancellation_reason().unwrap().as_deref(),
                    Some("runtime_server_shutdown")
                );
            } else {
                // A caller-controlled reason cannot acquire service semantics.
                active
                    .close_with_cancellation("runtime_server_shutdown", || Ok(()))
                    .unwrap();
                active.close_with_shutdown(|| Ok(())).unwrap();
                assert_eq!(
                    active.cancellation_reason_type().unwrap(),
                    Some("cancelled")
                );
            }
        }
    }

    #[test]
    fn failed_stop_request_does_not_claim_a_cancellation_cause() {
        let registry = AgentRunRegistry::default();
        registry
            .start("session", "run", "turn", TurnControl::new())
            .unwrap();
        let active = registry.active("run").unwrap().unwrap();
        assert!(active
            .close_with_shutdown(|| Err::<(), _>("request failed".into()))
            .is_err());
        assert_eq!(active.cancellation_reason().unwrap(), None);
        assert_eq!(active.cancellation_reason_type().unwrap(), None);
        active
            .close_with_cancellation("user_cancelled", || Ok(()))
            .unwrap();
        assert_eq!(
            active.cancellation_reason_type().unwrap(),
            Some("cancelled")
        );
    }

    fn start_agent_run(
        registry: &AgentRunRegistry,
        session_id: &str,
        agent_run_id: &str,
    ) -> Result<AgentRunLease, StartAgentRunError> {
        registry.start(
            session_id,
            agent_run_id,
            format!("turn:{agent_run_id}").as_str(),
            TurnControl::new(),
        )
    }

    #[test]
    fn finishing_keeps_session_owned_until_control_closes_without_blocking_other_sessions() {
        let registry = AgentRunRegistry::default();
        let first = start_agent_run(&registry, "chat-a", "run-a").unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let registry = &registry;
            let lease_id = first.lease_id.clone();
            let closing = scope.spawn(move || {
                registry.finish_with(&lease_id, |control| {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    control.close()
                })
            });
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let during = registry.active_for_session("chat-a").unwrap();
            let contender = start_agent_run(registry, "chat-a", "run-c");
            let independent = start_agent_run(registry, "chat-b", "run-b");
            release_tx.send(()).unwrap();
            closing.join().unwrap().unwrap();
            assert_eq!(
                during.map(|run| run.lease.agent_run_id),
                Some("run-a".to_string())
            );
            assert!(matches!(contender, Err(StartAgentRunError::SessionBusy(_))));
            assert!(independent.is_ok());
        });
        let next = start_agent_run(&registry, "chat-a", "run-next").unwrap();
        registry.finish(&next.lease_id).unwrap();
    }

    #[test]
    fn concurrent_finish_cannot_release_a_replacement_lease() {
        let registry = AgentRunRegistry::default();
        let first = start_agent_run(&registry, "chat-a", "run-a").unwrap();
        let old_control = registry.active("run-a").unwrap().unwrap().control;
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let registry = &registry;
            let lease_id = first.lease_id.clone();
            let late = scope.spawn(move || {
                registry.finish_with(&lease_id, |control| {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    control.close()
                })
            });
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            registry.finish(&first.lease_id).unwrap();
            let replacement = start_agent_run(registry, "chat-a", "run-b").unwrap();
            release_tx.send(()).unwrap();
            assert!(late.join().unwrap().is_err());
            assert_eq!(
                registry
                    .active_for_session("chat-a")
                    .unwrap()
                    .unwrap()
                    .lease,
                replacement
            );
        });
        assert!(old_control
            .enqueue_supplement_with("late input".to_string(), || Ok(()))
            .is_err());
    }

    #[test]
    fn failed_control_close_keeps_ownership_until_a_successful_retry() {
        let registry = AgentRunRegistry::default();
        let first = start_agent_run(&registry, "chat-a", "run-a").unwrap();
        assert_eq!(
            registry.finish_with(&first.lease_id, |_| Err("close failed".to_string())),
            Err("close failed".to_string())
        );
        assert!(matches!(
            start_agent_run(&registry, "chat-a", "run-b",),
            Err(StartAgentRunError::SessionBusy(_))
        ));
        registry.finish(&first.lease_id).unwrap();
        assert!(start_agent_run(&registry, "chat-a", "run-b",).is_ok());
    }

    #[test]
    fn accepts_different_sessions_and_rejects_a_second_run_in_one_session() {
        let registry = AgentRunRegistry::default();
        let first =
            start_agent_run(&registry, "chat-a", "agent-run-a").expect("start first AgentRun");
        start_agent_run(&registry, "chat-b", "agent-run-b").expect("start independent session");
        assert!(matches!(
            start_agent_run(
                &registry,
                "chat-c",
                "agent-run-b",
            ),
            Err(StartAgentRunError::Invalid(message)) if message.contains("already active")
        ));

        let busy = start_agent_run(&registry, "chat-a", "agent-run-c")
            .expect_err("same session must be busy");
        let StartAgentRunError::SessionBusy(busy) = busy else {
            panic!("same session must return SessionBusy");
        };
        assert_eq!(busy.active_agent_run_id, first.agent_run_id);
    }

    #[test]
    fn active_agent_run_count_tracks_start_and_finish() {
        let registry = AgentRunRegistry::default();
        let lease = start_agent_run(&registry, "chat-a", "agent-run-a").expect("start AgentRun");
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
                    ),
                    Err(StartAgentRunError::Invalid(message)) if message.contains("being deleted")
                ));
                Ok(())
            })
            .expect("delete Session");
        start_agent_run(&registry, "chat-a", "agent-run-a")
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
            start_agent_run(&registry, " ", "agent-run-a",),
            Err(StartAgentRunError::Invalid(_))
        ));
    }
}
