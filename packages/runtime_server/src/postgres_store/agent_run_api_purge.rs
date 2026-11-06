//! Content erasure follows terminal execution and settled physical obligations.
use super::PostgresRuntimeStore;
use crate::contract::AgentRunControl;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentRunApiPurgeRequest {
    pub schema: String,
    pub agent_run_control: AgentRunControl,
}

impl AgentRunApiPurgeRequest {
    pub(crate) fn validate(&self, signing_key: &[u8]) -> Result<(), String> {
        let control = &self.agent_run_control;
        if self.schema != "runtime.agent_run.api_purge.v1"
            || control.authorization.execution_policy.is_none()
        {
            return Err("agent_run_api_purge_request_invalid".into());
        }
        control
            .authorization
            .checked_binding(&control.agent_run_id)?
            .verify(
                &control.authorization_digest,
                signing_key,
                &control.authorization_signature,
            )
    }
}

impl PostgresRuntimeStore {
    pub(crate) fn purge_agent_run_api_session(
        &self,
        control: &AgentRunControl,
    ) -> Result<(), String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let session = &control.authorization.session_id;
            let run = &control.agent_run_id;
            let digest = &control.authorization_digest;
            tx.query_opt(
                "SELECT id FROM public.app_core_session
                 WHERE id=$1 AND workspace_id=$2 AND purpose='agentRunApi'
                 AND public.agent_run_api_session_retired(id)
                 AND public.agent_run_api_session_settled(id) FOR UPDATE",
                &[session, &control.authorization.workspace_id],
            ).map_err(|e| e.to_string())?.ok_or("agent_run_api_purge_session_rejected")?;
            tx.query_opt(
                "SELECT r.id FROM public.app_core_agentrun r
                 JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id
                 WHERE r.id=$1 AND r.session_id=$2 AND g.digest=$3 FOR SHARE OF r,g",
                &[run, session, digest],
            ).map_err(|e| e.to_string())?.ok_or("agent_run_api_purge_run_rejected")?;
            let pending = tx.query_one(
                r#"SELECT
                   EXISTS(SELECT 1 FROM runtime.runtime_jobs WHERE session_id=$1 AND
                    (status NOT IN('succeeded','failed','cancelled','dead_lettered')
                     OR (job_kind='agent_run.lifecycle' AND status<>'succeeded')))
                   OR EXISTS(SELECT 1 FROM runtime.resident_sandboxes WHERE environment_id IN (
                    SELECT id FROM public.app_core_sessionenvironment WHERE session_id=$1))
                   OR EXISTS(SELECT 1 FROM runtime.execution_instances i
                    WHERE i.session_id=$1 AND i.allocation_execution_id=i.execution_id
                     AND NOT EXISTS(SELECT 1 FROM runtime.environment_creation_attempts a
                        WHERE a.allocation_execution_id=i.execution_id AND a.instance_id=i.instance_id AND a.state='notDispatched')
                     AND NOT EXISTS(SELECT 1 FROM runtime.runtime_jobs j
                        WHERE j.job_id='environment.host_retire:'||i.instance_id AND j.idempotency_key=i.instance_id AND j.status='succeeded'))
                   OR EXISTS(SELECT 1 FROM public.app_core_agentrun r WHERE r.session_id=$1
                    AND NOT (r."preAdmissionCancelledAt" IS NOT NULL AND NOT EXISTS(
                     SELECT 1 FROM runtime.execution_instances i WHERE i.agent_run_id=r.id))
                    AND NOT EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts c
                     JOIN public.app_core_agentrunenvironmentrelease p
                       ON p.agent_run_id=c.agent_run_id AND p.environment_id=c.environment_id
                       AND p.authorization_digest=c.authorization_digest
                     JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id AND g.digest=c.authorization_digest
                     WHERE c.agent_run_id=r.id AND c.environment_id=r.environment_id
                       AND c.cleanup_complete AND (c.physical_removed OR c.allocation_kind='neverAllocated')
                       AND p.controller_epoch>=c.controller_epoch))"#,
                &[session],
            ).map_err(|e| e.to_string())?.get::<_, bool>(0);
            if pending {
                return Err("agent_run_api_purge_liabilities_pending".into());
            }
            super::runtime::delete_session_records(&mut tx, session)?;
            tx.commit().map_err(|e| e.to_string())
        })
    }
}
