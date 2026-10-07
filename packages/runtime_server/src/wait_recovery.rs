//! Immutable hosted storage attachment for Core's completed wait handoff.
use centaeris_core::runtime::contracts::{
    CheckpointKindV1, CheckpointRecord, RuntimeAwaitJobCheckpointV1, RuntimeRecoveryCheckpointV1,
};
use centaeris_core::session::state::SessionStateSnapshot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SCHEMA: &str = "workspace.runtime_job_wait_handoff.v1";
const PREFIX: &str = "waitcp:";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WaitHandoff {
    pub schema: String,
    pub handoff_key: String,
    pub source_session_sequence: u64,
    pub model_request_id: String,
    pub wait_checkpoint: CheckpointRecord,
    // Preserve Core's complete state verbatim; its private metadata is not interpreted here.
    pub snapshot_json: String,
}

pub(crate) struct WaitHandoffPublisher<'a> {
    pub store: &'a crate::postgres_store::PostgresRuntimeStore,
    pub log: &'a crate::postgres_store::PostgresSessionLog,
    pub host: &'a crate::docker_execution_host::DockerExecutionHostRunner,
    pub start: &'a crate::contract::AgentRunStart,
    pub fence: &'a centaeris_core::session::RuntimeJobLeaseFence,
    pub workspace_lease: &'a crate::docker_execution_host::SessionWorkspaceLease,
    pub input_upper_bound_bytes: u64,
}

impl WaitHandoffPublisher<'_> {
    pub fn publish(
        &self,
        turn: &centaeris_core::runtime::TurnStepResult,
        sequence: &mut centaeris_core::session::AgentRunSessionState,
        stream: &mut Option<crate::transient_stream::TransientAgentRunStream>,
    ) -> Result<(), String> {
        let wait = turn
            .checkpoint
            .as_ref()
            .ok_or("runtime job wait has no checkpoint")?;
        if !sequence.open_tool_call_ids().is_empty() {
            return Err("wait handoff requires an empty in-flight tool set".into());
        }
        let now = crate::now_ms()?;
        let mut payload = RuntimeRecoveryCheckpointV1 {
            schema: centaeris_core::runtime::contracts::RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1
                .into(),
            checkpoint_id: String::new(),
            session_id: self.start.authorization.session_id.clone(),
            agent_run_id: self.start.agent_run_id.clone(),
            execution_id: self.host.execution_id().into(),
            authorization_digest: self.start.authorization_digest.clone(),
            session_sequence: sequence
                .committed_session_sequence()
                .checked_add(1)
                .ok_or("wait handoff position overflow")?,
            model_request_id: self.store.latest_model_request_id(
                &wait.session_id,
                &self.start.agent_run_id,
                &wait.turn_id,
            )?,
            workspace_snapshot: crate::recovery_snapshot_from_session_workspace(
                &self.start.authorization.session_workspace,
            )?,
            workspace_generation: crate::observed_workspace_generation(self.host),
            created_at_ms: now,
        };
        let handoff = WaitHandoff::new(&payload, wait.clone(), &turn.session_snapshot)?;
        if let Some(committed) = self.store.wait_handoff_checkpoint(&handoff.handoff_key)? {
            let sealed = self
                .store
                .load_wait_handoff(&committed.checkpoint_id)?
                .ok_or("wait recovery attachment missing")?;
            let committed_payload: RuntimeRecoveryCheckpointV1 =
                serde_json::from_str(&committed.payload_json).map_err(|e| e.to_string())?;
            sealed.validate(&committed_payload)?;
            if sealed.wait_checkpoint != *wait || sealed.snapshot_json != handoff.snapshot_json {
                return Err("wait handoff replay changed Core state".into());
            }
            if sequence.has_checkpoint(&committed.checkpoint_id) {
                if sequence.latest_recovery_checkpoint_id()
                    != Some(committed.checkpoint_id.as_str())
                    || !sequence.tool_ledger_is_checkpointed()
                {
                    return Err("wait handoff replay has a different durable suffix".into());
                }
                return Ok(());
            }
            let mut next = sequence.clone();
            let event = next.checkpoint_ref(&committed)?;
            let receipt = self.log.append_wait_handoff(
                &self.start.agent_run_id,
                &[event],
                &committed,
                self.fence,
                &sealed,
            )?;
            crate::accept_session_commit(&mut next, stream, &receipt)?;
            *sequence = next;
            return Ok(());
        }
        // Attest the live owned host after all accepted calls, never the old pre-tool witness.
        if payload.workspace_generation.token().is_none() {
            return Ok(());
        }
        payload.checkpoint_id = handoff.checkpoint_id()?;
        let Some(snapshot) = self.host.stage_recovery_workspace(
            self.workspace_lease,
            &payload.checkpoint_id,
            &payload.workspace_snapshot,
            self.input_upper_bound_bytes,
        )?
        else {
            return Ok(());
        };
        let after = crate::observed_workspace_generation(self.host);
        let witness = self.host.recovery_workspace_evidence()?;
        if !crate::workspace_generations_match(&payload.workspace_generation, &after)
            || !witness.is_some_and(|w| {
                w.snapshot_activity_epoch == w.current_activity_epoch
                    && w.snapshot_host_instance == w.current_host_instance
            })
        {
            return Ok(());
        }
        payload.workspace_snapshot = snapshot;
        payload.workspace_generation = after;
        handoff.validate(&payload)?;
        let checkpoint = CheckpointRecord {
            checkpoint_id: payload.checkpoint_id.clone(),
            kind: CheckpointKindV1::Recovery,
            session_id: payload.session_id.clone(),
            turn_id: wait.turn_id.clone(),
            status: "committed".into(),
            done_reason: None,
            updated_at_ms: now,
            payload_json: serde_json::to_string(&payload).map_err(|e| e.to_string())?,
        };
        let mut next = sequence.clone();
        let event = next.checkpoint_ref(&checkpoint)?;
        let receipt = self.log.append_wait_handoff(
            &self.start.agent_run_id,
            &[event],
            &checkpoint,
            self.fence,
            &handoff,
        )?;
        crate::accept_session_commit(&mut next, stream, &receipt)?;
        *sequence = next;
        Ok(())
    }
}

fn digest(value: &impl Serialize) -> Result<String, String> {
    let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

impl WaitHandoff {
    pub fn key(
        payload: &RuntimeRecoveryCheckpointV1,
        wait: &CheckpointRecord,
    ) -> Result<String, String> {
        digest(&(
            SCHEMA,
            &payload.session_id,
            &payload.agent_run_id,
            &payload.execution_id,
            &payload.authorization_digest,
            &wait.checkpoint_id,
        ))
    }

    pub fn new(
        payload: &RuntimeRecoveryCheckpointV1,
        wait: CheckpointRecord,
        snapshot: &SessionStateSnapshot,
    ) -> Result<Self, String> {
        Ok(Self {
            schema: SCHEMA.into(),
            handoff_key: Self::key(payload, &wait)?,
            source_session_sequence: payload
                .session_sequence
                .checked_sub(1)
                .ok_or("wait recovery position invalid")?,
            model_request_id: payload.model_request_id.clone(),
            wait_checkpoint: wait,
            snapshot_json: serde_json::to_string(
                &serde_json::to_value(snapshot).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
        })
    }

    pub fn checkpoint_id(&self) -> Result<String, String> {
        Ok(format!("{PREFIX}{}", digest(self)?))
    }

    pub fn is_wait_checkpoint(id: &str) -> bool {
        id.starts_with(PREFIX)
    }

    pub fn validate_reference(
        checkpoint: &CheckpointRecord,
        reference: &centaeris_core::session::SessionLogRecord,
    ) -> Result<(), String> {
        let hash = format!(
            "sha256:{:x}",
            Sha256::digest(checkpoint.payload_json.as_bytes())
        );
        if reference.event_type != centaeris_core::session::SessionRecordType::CheckpointRef
            || reference.session_id != checkpoint.session_id
            || reference.turn_id.as_deref() != Some(checkpoint.turn_id.as_str())
            || reference.payload["checkpointId"].as_str() != Some(checkpoint.checkpoint_id.as_str())
            || reference.payload["kind"].as_str() != Some(checkpoint.kind.as_str())
            || reference.payload["status"].as_str() != Some(checkpoint.status.as_str())
            || reference.payload["payloadSha256"].as_str() != Some(hash.as_str())
            || reference.payload["objectRef"].as_str()
                != Some(format!("checkpoint-object:{hash}").as_str())
            || reference.payload["payloadByteLength"].as_u64()
                != Some(checkpoint.payload_json.len() as u64)
            || reference.payload["updatedAtMs"].as_i64() != Some(checkpoint.updated_at_ms)
        {
            return Err("wait recovery checkpoint reference binding mismatch".into());
        }
        Ok(())
    }

    pub fn validate(
        &self,
        payload: &RuntimeRecoveryCheckpointV1,
    ) -> Result<SessionStateSnapshot, String> {
        payload.validate()?;
        let wait: RuntimeAwaitJobCheckpointV1 =
            serde_json::from_str(&self.wait_checkpoint.payload_json).map_err(|e| e.to_string())?;
        wait.validate()?;
        let snapshot: SessionStateSnapshot =
            serde_json::from_str(&self.snapshot_json).map_err(|e| e.to_string())?;
        if self.schema != SCHEMA
            || self.handoff_key != Self::key(payload, &self.wait_checkpoint)?
            || payload.checkpoint_id != self.checkpoint_id()?
            || self.source_session_sequence.checked_add(1) != Some(payload.session_sequence)
            || self.model_request_id != payload.model_request_id
            || snapshot.session_id != payload.session_id
            || self.wait_checkpoint.kind != CheckpointKindV1::Wait
            || self.wait_checkpoint.status != "waiting"
            || self.wait_checkpoint.done_reason.as_deref() != Some("runtime_job")
            || self.wait_checkpoint.session_id != payload.session_id
            || self.wait_checkpoint.turn_id != wait.turn_id
            || wait.agent_run_id != payload.agent_run_id
            || wait.authorization_digest != payload.authorization_digest
            || payload.workspace_generation.token().is_none()
        {
            return Err("wait recovery attachment binding mismatch".into());
        }
        Ok(snapshot)
    }
}
