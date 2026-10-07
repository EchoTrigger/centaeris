//! Adapt hosted immutable carriers to Core intake; claims and ACK are not Read.
use std::collections::HashSet;
use std::sync::Arc;

use crate::contract::{AgentRunStart, AgentRunStartInitialInput};
use crate::postgres_store::PostgresRuntimeStore;
use centaeris_core::runtime::{DurableTurnControlBinding, TurnControl};
use centaeris_core::session::turn_input::{
    AcknowledgeTurnInputsRequest, ClaimTurnInputsRequest, CloseTurnInputQueueRequest,
    DurableTurnInput, TurnInputPayload, TurnInputStorePort,
};
use centaeris_core::tool::inputs::{DeclaredInput, ResolvedInputState};
use postgres::Transaction;

type InputMaterializer = dyn Fn(&str, &[DurableTurnInput]) -> Result<(), String> + Send + Sync;

pub(crate) fn turn_control(
    store: Arc<PostgresRuntimeStore>,
    start: &AgentRunStart,
    binding: DurableTurnControlBinding,
    materialize: Arc<InputMaterializer>,
    resolved_inputs: Option<Arc<ResolvedInputState>>,
) -> Result<TurnControl, String> {
    if start.coordination_session_id.is_none() {
        return TurnControl::new_durable(
            store,
            binding,
            Arc::new(move |turn, items| {
                let inputs = items
                    .iter()
                    .cloned()
                    .map(DurableTurnInput::from)
                    .collect::<Vec<_>>();
                materialize(turn, &inputs)
            }),
        );
    }
    let initial = start.initial_input.clone();
    let materialize_inputs = Arc::new(move |turn_id: &str, inputs: &[DurableTurnInput]| {
        let mut pending = Vec::new();
        for item in inputs {
            match (&initial, &item.payload) {
                (
                    AgentRunStartInitialInput::UserInput {
                        input_id,
                        message: body,
                        attachment_refs,
                    },
                    TurnInputPayload::UserSupplement {
                        supplement_id,
                        message,
                        attachments,
                    },
                ) if input_id == supplement_id => {
                    if body != message
                        || attachment_refs
                            != &attachments
                                .iter()
                                .map(|item| item.input_ref.clone())
                                .collect::<Vec<_>>()
                    {
                        return Err("agent_input_initial_body_conflict".into());
                    }
                }
                (
                    AgentRunStartInitialInput::HostEvent { input, .. },
                    TurnInputPayload::HostEvent(host),
                ) if input.input_id == host.input_id => {
                    if input != host {
                        return Err("agent_input_initial_host_conflict".into());
                    }
                }
                _ => pending.push(item.clone()),
            }
        }
        materialize(turn_id, &pending)
    });
    let hosted =
        HostedAgentInputStore::new(store, start.clone()).with_resolved_inputs(resolved_inputs)?;
    TurnControl::new_durable_inputs(Arc::new(hosted), binding, materialize_inputs)
}

pub(crate) struct HostedAgentInputStore {
    store: Arc<PostgresRuntimeStore>,
    start: AgentRunStart,
    resolved_inputs: Option<Arc<ResolvedInputState>>,
}

impl std::fmt::Debug for HostedAgentInputStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostedAgentInputStore")
            .field("agent_run_id", &self.start.agent_run_id)
            .field("has_resolved_inputs", &self.resolved_inputs.is_some())
            .finish_non_exhaustive()
    }
}

impl HostedAgentInputStore {
    pub(crate) fn new(store: Arc<PostgresRuntimeStore>, start: AgentRunStart) -> Self {
        Self {
            store,
            start,
            resolved_inputs: None,
        }
    }

    pub(crate) fn with_resolved_inputs(
        mut self,
        inputs: Option<Arc<ResolvedInputState>>,
    ) -> Result<Self, String> {
        self.resolved_inputs = inputs;
        if self.resolved_inputs.is_some() {
            self.store.with_client_error(|client| {
                let mut tx = client.transaction().map_err(|error| error.to_string())?;
                self.lock_scope(&mut tx)?;
                // ACK does not revoke an accepted input grant. Restore all this
                // Run's captures so checkpoint replay can resolve earlier images.
                let rows = tx.query("SELECT i.attachments::text FROM app_core_agentinputdelivery d JOIN app_core_agentinput i ON i.id=d.input_id JOIN app_core_agentrun r ON r.id=d.queue_id WHERE d.queue_id=$1 AND i.session_id=r.session_id AND i.membership_ref=r.membership_ref ORDER BY i.sequence", &[&self.start.agent_run_id]).map_err(|error| error.to_string())?;
                let declared = rows.iter().map(|row| decode_captures(&row.get::<_, String>(0)))
                    .collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect::<Vec<_>>();
                self.admit_captures(&declared)?;
                tx.commit().map_err(|error| error.to_string())
            })?;
        }
        Ok(self)
    }

    fn admit_captures(&self, captures: &[DeclaredInput]) -> Result<(), String> {
        let mut authorization = self.start.authorization.clone();
        let mut all = self
            .resolved_inputs
            .as_ref()
            .map(|inputs| inputs.declared_inputs())
            .unwrap_or_else(|| authorization.asset_refs.clone());
        for capture in captures {
            if let Some(existing) = all.iter().find(|item| item.input_ref == capture.input_ref) {
                if existing != capture {
                    return Err("agent_input_attachment_identity_conflict".into());
                }
            } else {
                all.push(capture.clone());
            }
        }
        all.sort_by(|left, right| left.input_ref.cmp(&right.input_ref));
        authorization.asset_refs = all;
        // Check the existing hosted count/byte budget without rewriting the
        // signed immutable authorization payload.
        authorization.validate()?;
        if let Some(inputs) = &self.resolved_inputs {
            inputs.admit_declared_inputs(&authorization.asset_refs)?;
        }
        Ok(())
    }

    fn identity(&self, run: &str, job: &str, session: &str, digest: &str) -> Result<(), String> {
        crate::postgres_store::turn_supplement::validate_common_identity(run, job, session, digest)
            .map_err(|error| error.to_string())?;
        if run != self.start.agent_run_id
            || session != self.start.authorization.session_id
            || digest != self.start.authorization_digest
        {
            return Err("agent_input_store_identity_rejected".into());
        }
        Ok(())
    }

    fn lock_scope(&self, tx: &mut Transaction<'_>) -> Result<bool, String> {
        // Match hosted admission's Workspace -> membership -> Agent -> Session
        // order, then lock the queue and lifecycle job. Final appends also hold
        // Session before the job, so admission/empty-close share one fence.
        let workspace = tx
            .query_opt(
                "SELECT id FROM app_core_workspace WHERE id=$1 AND status='active' FOR UPDATE",
                &[&self.start.authorization.workspace_id],
            )
            .map_err(|e| e.to_string())?;
        if workspace.is_none() {
            return Err("agent_input_store_authority_rejected".into());
        }
        let member = tx.query_opt("SELECT m.id FROM app_core_workspacemembership m JOIN app_core_agentrun r ON r.membership_ref=m.id WHERE r.id=$1 AND m.workspace_id=r.workspace_id AND m.user_id=r.user_id AND m.role IN('owner','admin','member') FOR UPDATE OF m",
            &[&self.start.agent_run_id]).map_err(|e| e.to_string())?;
        if member.is_none() {
            return Err("agent_input_store_authority_rejected".into());
        }
        let agent = tx.query_opt("SELECT ag.id FROM app_core_agent ag JOIN app_core_session s ON s.agent_id=ag.id JOIN app_core_agentrun r ON r.session_id=s.id WHERE r.id=$1 AND ag.id=$2 AND ag.status='active' AND ag.owner_id=r.user_id AND ag.workspace_id=r.workspace_id FOR UPDATE OF ag",
            &[&self.start.agent_run_id, &self.start.authorization.agent_id]).map_err(|e| e.to_string())?;
        if agent.is_none() {
            return Err("agent_input_store_authority_rejected".into());
        }
        let session = tx.query_opt("SELECT s.id FROM app_core_session s JOIN app_core_agentrun r ON r.session_id=s.id JOIN auth_user u ON u.id=r.user_id WHERE r.id=$1 AND s.id=$2 AND s.status='active' AND s.owner_id=r.user_id AND s.workspace_id=r.workspace_id AND u.is_active AND EXISTS(SELECT 1 FROM app_core_agentcoordinationsession b WHERE b.agent_id=s.agent_id AND b.session_id=s.id) FOR UPDATE OF s",
            &[&self.start.agent_run_id, &self.start.authorization.session_id]).map_err(|e| e.to_string())?;
        if session.is_none() {
            return Err("agent_input_store_authority_rejected".into());
        }
        let row = tx.query_opt("SELECT q.accepting FROM app_core_agentinputqueue q JOIN app_core_agentrunauthorization a ON a.agent_run_id=q.agent_run_id WHERE q.agent_run_id=$1 AND q.authorization_digest=$2 AND a.digest=q.authorization_digest FOR UPDATE OF q",
            &[&self.start.agent_run_id, &self.start.authorization_digest]).map_err(|e| e.to_string())?
            .ok_or_else(|| "agent_input_store_queue_identity_rejected".to_string())?;
        Ok(row.get(0))
    }

    fn lease(
        &self,
        tx: &mut Transaction<'_>,
        job: &str,
        owner: Option<(&str, i64)>,
        reject_cancelled: bool,
    ) -> Result<(), String> {
        crate::postgres_store::turn_supplement::validate_active_job(
            tx,
            &self.start.agent_run_id,
            job,
            &self.start.authorization.session_id,
            &self.start.authorization_digest,
            owner,
            reject_cancelled,
        )
        .map_err(|error| error.to_string())
    }
}

impl TurnInputStorePort for HostedAgentInputStore {
    fn claim_turn_inputs(
        &self,
        request: ClaimTurnInputsRequest,
    ) -> Result<Vec<DurableTurnInput>, String> {
        self.identity(
            &request.agent_run_id,
            &request.lifecycle_job_id,
            &request.session_id,
            &request.authorization_digest,
        )?;
        if request.lease_owner.is_empty()
            || request.claim_token.is_empty()
            || request.limit == 0
            || request.limit > 256
        {
            return Err("agent_input_claim_identity_rejected".into());
        }
        self.store.with_client_error(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            let accepting = self.lock_scope(&mut tx)?;
            self.lease(&mut tx, &request.lifecycle_job_id, Some((&request.lease_owner, request.now_ms)), true)?;
            let rows = tx.query("SELECT d.input_id,i.input_id,i.sequence,i.created_at_ms,i.body,d.claim_token,d.claim_lease_owner,i.attachments::text FROM app_core_agentinputdelivery d JOIN app_core_agentinput i ON i.id=d.input_id JOIN app_core_agentrun r ON r.id=d.queue_id WHERE d.queue_id=$1 AND d.acknowledged_at_ms IS NULL AND i.session_id=r.session_id AND i.membership_ref=r.membership_ref ORDER BY i.sequence FOR UPDATE OF d",
                &[&request.agent_run_id]).map_err(|e| e.to_string())?;
            let native = tx.query("SELECT a.id,a.input_binding::text,a.delivery_sequence,a.created_at_ms,a.claim_token,a.claim_lease_owner FROM app_core_agentworkconsumeattempt a WHERE a.coordinator_run_id=$1 AND a.acknowledged_at_ms IS NULL ORDER BY a.delivery_sequence FOR UPDATE OF a",
                &[&request.agent_run_id]).map_err(|e| e.to_string())?;
            let empty = rows.is_empty() && native.is_empty();
            if !accepting && !empty { return Err("agent_input_closed_queue_has_pending_input".into()); }
            let mut claimed = Vec::new();
            for row in rows {
                if row.get::<_, Option<String>>(5).as_deref() == Some(&request.claim_token) { continue; }
                if row.get::<_, Option<String>>(6).as_deref() == Some(&request.lease_owner) {
                    return Err("agent_input_claim_in_progress".into());
                }
                let captures = decode_captures(&row.get::<_, String>(7))?;
                self.admit_captures(&captures)?;
                let attachments = captures.iter().map(|item| centaeris_core::session::turn_input::attachments::UserInputAttachment {
                    input_ref: item.input_ref.clone(), display_name: item.display_name.clone(), content_type: item.content_type.clone(),
                }).collect();
                let payload = TurnInputPayload::UserSupplement { supplement_id: row.get(1), message: row.get(4), attachments };
                payload.validate(&request.session_id)?;
                tx.execute("UPDATE app_core_agentinputdelivery SET claim_token=$1,claim_lease_owner=$2 WHERE input_id=$3",
                    &[&request.claim_token, &request.lease_owner, &row.get::<_, i64>(0)]).map_err(|e| e.to_string())?;
                claimed.push(DurableTurnInput { payload,
                    sequence: u64::try_from(row.get::<_, i64>(2)).map_err(|_| "agent_input_sequence_invalid".to_string())?,
                    created_at_ms: row.get(3), claim_token: Some(request.claim_token.clone()),
                    claim_lease_owner: Some(request.lease_owner.clone()) });
                if claimed.len() == request.limit { break; }
            }
            for row in native {
                if claimed.len() == request.limit { break; }
                if row.get::<_, Option<String>>(4).as_deref() == Some(&request.claim_token) { continue; }
                if row.get::<_, Option<String>>(5).as_deref() == Some(&request.lease_owner) {
                    return Err("agent_input_claim_in_progress".into());
                }
                let binding: serde_json::Value = serde_json::from_str(&row.get::<_, String>(1)).map_err(|e| e.to_string())?;
                let input: centaeris_core::session::host_event_input::HostEventInput = serde_json::from_value(binding["initialInput"]["input"].clone()).map_err(|e| e.to_string())?;
                crate::postgres_store::validate_hosted_native_input(&mut tx, &self.start, &input)?;
                tx.execute("UPDATE app_core_agentworkconsumeattempt SET claim_token=$1,claim_lease_owner=$2 WHERE id=$3",
                    &[&request.claim_token, &request.lease_owner, &row.get::<_, String>(0)]).map_err(|e| e.to_string())?;
                claimed.push(DurableTurnInput { payload: TurnInputPayload::HostEvent(input),
                    sequence: u64::try_from(row.get::<_, i64>(2)).map_err(|_| "agent_input_sequence_invalid".to_string())?,
                    created_at_ms: row.get(3), claim_token: Some(request.claim_token.clone()), claim_lease_owner: Some(request.lease_owner.clone()) });
            }
            if request.close_if_empty && empty {
                tx.execute("UPDATE app_core_agentinputqueue SET accepting=false,closed_reason='safe_point_closed',closed_at_ms=$1 WHERE agent_run_id=$2",
                    &[&request.now_ms, &request.agent_run_id]).map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            Ok(claimed)
        })
    }

    fn acknowledge_turn_inputs(&self, request: AcknowledgeTurnInputsRequest) -> Result<(), String> {
        self.identity(
            &request.agent_run_id,
            &request.lifecycle_job_id,
            &request.session_id,
            &request.authorization_digest,
        )?;
        let ids: HashSet<_> = request.input_ids.iter().collect();
        if ids.is_empty()
            || ids.len() != request.input_ids.len()
            || request.claim_token.is_empty()
            || request.lease_owner.is_empty()
        {
            return Err("agent_input_ack_identity_rejected".into());
        }
        self.store.with_client_error(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            self.lock_scope(&mut tx)?;
            self.lease(&mut tx, &request.lifecycle_job_id, Some((&request.lease_owner, request.acknowledged_at_ms)), true)?;
            let rows = tx.query("SELECT d.input_id,i.input_id,d.claim_token,d.claim_lease_owner FROM app_core_agentinputdelivery d JOIN app_core_agentinput i ON i.id=d.input_id WHERE d.queue_id=$1 AND i.input_id=ANY($2) FOR UPDATE OF d",
                &[&request.agent_run_id, &request.input_ids]).map_err(|e| e.to_string())?;
            let native = tx.query("SELECT id,claim_token,claim_lease_owner FROM app_core_agentworkconsumeattempt WHERE coordinator_run_id=$1 AND id=ANY($2) FOR UPDATE",
                &[&request.agent_run_id, &request.input_ids]).map_err(|e| e.to_string())?;
            if rows.len() + native.len() != ids.len() || native.iter().any(|row|
                row.get::<_, Option<String>>(1).as_deref() != Some(&request.claim_token)
                || row.get::<_, Option<String>>(2).as_deref() != Some(&request.lease_owner)) || rows.iter().any(|row|
                row.get::<_, Option<String>>(2).as_deref() != Some(&request.claim_token)
                || row.get::<_, Option<String>>(3).as_deref() != Some(&request.lease_owner)) {
                return Err("agent_input_ack_claim_rejected".into());
            }
            for row in rows {
                tx.execute("UPDATE app_core_agentinputdelivery SET acknowledged_at_ms=COALESCE(acknowledged_at_ms,$1) WHERE input_id=$2",
                    &[&request.acknowledged_at_ms, &row.get::<_, i64>(0)]).map_err(|e| e.to_string())?;
            }
            for row in native {
                tx.execute("UPDATE app_core_agentworkconsumeattempt SET acknowledged_at_ms=COALESCE(acknowledged_at_ms,$1) WHERE id=$2",
                    &[&request.acknowledged_at_ms, &row.get::<_, String>(0)]).map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            Ok(())
        })
    }

    fn close_turn_input_queue(&self, request: CloseTurnInputQueueRequest) -> Result<(), String> {
        self.identity(
            &request.agent_run_id,
            &request.lifecycle_job_id,
            &request.session_id,
            &request.authorization_digest,
        )?;
        if request.reason.trim().is_empty() || request.reason.len() > 64 {
            return Err("agent_input_close_reason_invalid".into());
        }
        self.store.with_client_error(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            self.lock_scope(&mut tx)?;
            self.lease(&mut tx, &request.lifecycle_job_id,
                request.lease_owner.as_deref().map(|owner| (owner, request.closed_at_ms)), false)?;
            tx.execute("UPDATE app_core_agentinputqueue SET accepting=false,closed_reason=$1,closed_at_ms=$2 WHERE agent_run_id=$3",
                &[&request.reason, &request.closed_at_ms, &request.agent_run_id]).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(())
        })
    }
}

fn decode_captures(raw: &str) -> Result<Vec<DeclaredInput>, String> {
    let captures: Vec<DeclaredInput> =
        serde_json::from_str(raw).map_err(|error| error.to_string())?;
    if captures.len() > 50
        || captures
            .windows(2)
            .any(|pair| pair[0].input_ref >= pair[1].input_ref)
    {
        return Err("agent_input_attachment_capture_invalid".into());
    }
    for capture in &captures {
        capture.validate()?;
    }
    Ok(captures)
}
