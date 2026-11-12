use centaeris_core::runtime::subagent::SUBAGENT_RUN_JOB_KIND;
use centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND;
use centaeris_core::session::RuntimeJobLeaseFence;
use postgres::Transaction;

use super::runtime::lock_current_append_lease;
use super::PostgresRuntimeStore;

#[derive(Debug, Clone)]
pub(super) enum SubagentPersistenceScope {
    Execution {
        parent_session_id: String,
        execution_session_id: String,
        fence: RuntimeJobLeaseFence,
    },
    TerminalProjection {
        parent_session_id: String,
    },
}

impl PostgresRuntimeStore {
    /// Keep Core's ordinary writes under the same authority as this delivery.
    /// The view shares connection pools; it does not borrow another Job's lease.
    pub(crate) fn hosted_execution_view(
        &self,
        parent_session_id: &str,
        execution_session_id: &str,
        fence: RuntimeJobLeaseFence,
    ) -> Result<Self, String> {
        let target_matches_kind = match fence.job_kind.as_str() {
            AGENT_RUN_LIFECYCLE_JOB_KIND => execution_session_id == parent_session_id,
            SUBAGENT_RUN_JOB_KIND => execution_session_id != parent_session_id,
            _ => false,
        };
        if parent_session_id.trim().is_empty()
            || execution_session_id.trim().is_empty()
            || fence.job_id.trim().is_empty()
            || fence.lease_owner.trim().is_empty()
            || !target_matches_kind
        {
            return Err("hosted_persistence_execution_scope_invalid".into());
        }
        let mut view = self.clone();
        view.subagent_persistence_scope = Some(SubagentPersistenceScope::Execution {
            parent_session_id: parent_session_id.into(),
            execution_session_id: execution_session_id.into(),
            fence,
        });
        Ok(view)
    }

    /// Terminal child notifications may update their surviving parent Session.
    /// They no longer hold a running child lease, and must not recreate a purge.
    pub(crate) fn subagent_terminal_projection_view(
        &self,
        parent_session_id: &str,
    ) -> Result<Self, String> {
        if parent_session_id.trim().is_empty() {
            return Err("hosted_persistence_projection_scope_invalid".into());
        }
        let mut view = self.clone();
        view.subagent_persistence_scope = Some(SubagentPersistenceScope::TerminalProjection {
            parent_session_id: parent_session_id.into(),
        });
        Ok(view)
    }

    /// Call inside the transaction that performs the write. The Session -> Job
    /// lock order matches Session log appends and permanent Session erasure.
    pub(super) fn lock_subagent_persistence_write(
        &self,
        tx: &mut Transaction<'_>,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        let Some(scope) = &self.subagent_persistence_scope else {
            return Ok(());
        };
        let (parent_session_id, target_session_id) = match scope {
            SubagentPersistenceScope::Execution {
                parent_session_id,
                execution_session_id,
                ..
            } => (parent_session_id, execution_session_id),
            SubagentPersistenceScope::TerminalProjection { parent_session_id } => {
                (parent_session_id, parent_session_id)
            }
        };
        if session_id.is_some_and(|session_id| session_id != target_session_id) {
            return Err("hosted_persistence_session_scope_rejected".into());
        }
        let active = tx
            .query_opt(
                "SELECT status='active' AND \"purgedAt\" IS NULL FROM public.app_core_session WHERE id=$1 FOR UPDATE",
                &[parent_session_id],
            )
            .map_err(|error| format!("lock hosted persistence Session failed: {error}"))?
            .is_some_and(|row| row.get::<_, bool>(0));
        if !active {
            return Err("hosted_persistence_session_unavailable".into());
        }
        if let SubagentPersistenceScope::Execution { fence, .. } = scope {
            lock_current_append_lease(tx, parent_session_id, fence)?;
        }
        Ok(())
    }

    pub(super) fn with_subagent_persistence_transaction<T: Send>(
        &self,
        session_id: Option<&str>,
        write: impl FnOnce(&mut Transaction<'_>) -> Result<T, String> + Send,
    ) -> Result<T, String> {
        self.with_client(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin hosted persistence write failed: {error}"))?;
            self.lock_subagent_persistence_write(&mut tx, session_id)?;
            let result = write(&mut tx)?;
            tx.commit()
                .map_err(|error| format!("commit hosted persistence write failed: {error}"))?;
            Ok(result)
        })
    }

    /// The scoped child snapshot is durable before readers receive its hint.
    /// Call inside the already-fenced snapshot transaction, including CAS.
    pub(super) fn notify_subagent_snapshot(&self, tx: &mut Transaction<'_>) -> Result<(), String> {
        if let Some(SubagentPersistenceScope::Execution {
            parent_session_id,
            fence,
            ..
        }) = &self.subagent_persistence_scope
        {
            if fence.job_kind == SUBAGENT_RUN_JOB_KIND {
                let key = format!("session:{parent_session_id}:subagents");
                tx.query_one("SELECT public.workspace_presentation_bump_v1($1)", &[&key])
                    .map_err(|error| {
                        format!("record subagent snapshot presentation change failed: {error}")
                    })?;
            }
        }
        Ok(())
    }
}
