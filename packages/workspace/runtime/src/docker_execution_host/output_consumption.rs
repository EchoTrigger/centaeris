//! Durable result-consumer links; retained output bodies stay on their original host.
use super::*;
use centaeris_core::execution::{
    EnvironmentTaskFence, EnvironmentTaskOwner, EnvironmentTaskRequest, EnvironmentTaskResult,
    EnvironmentTaskScope, EnvironmentTaskState,
};
use centaeris_core::runtime::contracts::{EventVisibility, RuntimeEvent};
use centaeris_core::session::store::RuntimeStore;
use serde::{Deserialize, Serialize};

const TICKET: &str = "runtime.execution_output.ticket.v1";
const CONFIRMED: &str = "runtime.execution_output.confirmed.v1";
const RELEASED: &str = "runtime.execution_output.released.v1";
const SNAPSHOT_PUBLISHED: &str = "runtime.execution_output.snapshot_published.v1";
const SNAPSHOT_REUSED: &str = "runtime.execution_output.snapshot_reused.v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Ticket {
    schema: String,
    task_id: String,
    operation_id: String,
    instance_id: String,
    owner: EnvironmentTaskOwner,
    consumer_id: String,
    confirmation_event_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Confirmation {
    schema: String,
    task_id: String,
    consumer_id: String,
    receipt_id: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum SnapshotOutputSource {
    Published,
    Reused,
}

impl SnapshotOutputSource {
    fn event_type(self) -> &'static str {
        match self {
            Self::Published => SNAPSHOT_PUBLISHED,
            Self::Reused => SNAPSHOT_REUSED,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotOutputBinding {
    schema: String,
    checkpoint_id: String,
    snapshot: RecoveryWorkspaceSnapshotV1,
    consumer_ids: Vec<String>,
    source: SnapshotOutputSource,
}

fn snapshot_binding_id(run: &str, binding: &SnapshotOutputBinding) -> String {
    let mut digest = Sha256::new();
    for field in std::iter::once(binding.source.event_type())
        .chain(std::iter::once(binding.checkpoint_id.as_str()))
        .chain(binding.consumer_ids.iter().map(String::as_str))
    {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    format!("execution-output-snapshot:{run}:{:x}", digest.finalize())
}

fn decode_snapshot_binding(raw: &str) -> Result<SnapshotOutputBinding, String> {
    let binding: SnapshotOutputBinding =
        serde_json::from_str(raw).map_err(|_| "execution_output_snapshot_binding_invalid")?;
    if binding.schema != binding.source.event_type() {
        return Err("execution_output_snapshot_source_mismatch".into());
    }
    binding.snapshot.validate()?;
    Ok(binding)
}

// A published or reused source alone is insufficient. Its full snapshot must
// also occur in this original Run's newly committed Core recovery checkpoint.
fn committed_snapshot_outputs(
    store: &PostgresRuntimeStore,
    session: &str,
    run: &str,
    checkpoint: &str,
) -> Result<Vec<SnapshotOutputBinding>, String> {
    let rows = store.with_client(|db| db.query(
        "SELECT p.payload_json FROM runtime.runtime_events p JOIN runtime.checkpoints c ON c.checkpoint_id=(p.payload_json::jsonb->>'checkpointId') AND c.session_id=p.session_id AND c.kind='recovery' AND c.status='committed' WHERE p.session_id=$1 AND p.task_id=$2 AND p.event_type IN ($3,$4) AND p.payload_json::jsonb->>'checkpointId'=$5 AND c.payload_json::jsonb->>'agentRunId'=$2 AND c.payload_json::jsonb->>'sessionId'=$1 AND c.payload_json::jsonb->'workspaceSnapshot'=(p.payload_json::jsonb->'snapshot')",
        &[&session,&run,&SNAPSHOT_PUBLISHED,&SNAPSHOT_REUSED,&checkpoint]).map_err(|e|e.to_string()))?;
    rows.into_iter()
        .map(|row| {
            let binding = decode_snapshot_binding(&row.get::<_, String>(0))?;
            if binding.checkpoint_id != checkpoint {
                return Err("execution_output_snapshot_binding_mismatch".into());
            }
            Ok(binding)
        })
        .collect()
}

#[derive(Default)]
pub(super) struct OutputConsumption {
    loaded: bool,
    tickets: BTreeMap<String, Ticket>,
    confirmed: BTreeMap<String, Confirmation>,
}

fn confirmation_id(run: &str, task: &str) -> String {
    format!(
        "execution-output-confirmed:{run}:{:x}",
        Sha256::digest(task.as_bytes())
    )
}
fn ticket_id(task: &str) -> String {
    format!("execution-output-ticket:{task}")
}
fn released_id(task: &str) -> String {
    format!("execution-output-released:{task}")
}
fn wire_receipt(receipt: &str) -> String {
    format!("output-consumer:{:x}", Sha256::digest(receipt.as_bytes()))
}
fn event<T: Serialize>(
    run: &str,
    session: &str,
    id: String,
    kind: &str,
    value: &T,
) -> Result<RuntimeEvent, String> {
    Ok(RuntimeEvent {
        event_id: id,
        session_id: session.into(),
        task_id: Some(run.into()),
        event_type: kind.into(),
        // Identity metadata is timestamp-independent so replay is byte-identical.
        // This is not an observation of process activity at the Unix epoch.
        at_ms: 0,
        visibility: EventVisibility::Internal,
        payload_json: serde_json::to_string(value)
            .map_err(|_| "execution_output_ticket_invalid")?,
    })
}
fn append<T: Serialize>(
    store: &PostgresRuntimeStore,
    run: &str,
    session: &str,
    id: String,
    kind: &str,
    value: &T,
) -> Result<(), String> {
    store
        .append_event_idempotent(event(run, session, id, kind, value)?)
        .map_err(|e| e.to_string())
}

impl OutputConsumption {
    fn load(
        &mut self,
        store: &PostgresRuntimeStore,
        session: &str,
        run: &str,
        instance: &str,
    ) -> Result<(), String> {
        if self.loaded {
            return Ok(());
        }
        let mut after = String::new();
        loop {
            let rows = store.with_client(|db| db.query(
                "SELECT t.event_id,t.payload_json,c.payload_json FROM runtime.runtime_events t LEFT JOIN runtime.runtime_events c ON c.event_id=(t.payload_json::jsonb->>'confirmationEventId') AND c.session_id=t.session_id AND c.task_id=t.task_id AND c.event_type=$4 WHERE t.session_id=$1 AND t.task_id=$2 AND t.event_type=$3 AND t.event_id>$5 AND t.payload_json::jsonb->>'instanceId'=$6 AND NOT EXISTS(SELECT 1 FROM runtime.runtime_events a WHERE a.event_id='execution-output-released:'||(t.payload_json::jsonb->>'taskId') AND a.session_id=t.session_id AND a.task_id=t.task_id AND a.event_type=$7) ORDER BY t.event_id LIMIT 64",
                &[&session,&run,&TICKET,&CONFIRMED,&after,&instance,&RELEASED]).map_err(|e|e.to_string()))?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                after = row.get(0);
                let ticket: Ticket = serde_json::from_str(&row.get::<_, String>(1))
                    .map_err(|_| "execution_output_ticket_invalid")?;
                if ticket.schema != TICKET
                    || ticket.instance_id != instance
                    || ticket.owner != (EnvironmentTaskOwner::AgentRun { run_id: run.into() })
                {
                    return Err("execution_output_ticket_binding_mismatch".into());
                }
                let confirmed: Option<String> = row.get(2);
                if let Some(raw) = confirmed {
                    let confirmation: Confirmation = serde_json::from_str(&raw)
                        .map_err(|_| "execution_output_confirmation_invalid")?;
                    if confirmation.schema != CONFIRMED
                        || confirmation.consumer_id != ticket.consumer_id
                        || confirmation.task_id != ticket.task_id
                    {
                        return Err("execution_output_confirmation_binding_mismatch".into());
                    }
                    self.confirmed.insert(ticket.task_id.clone(), confirmation);
                }
                self.tickets.insert(ticket.task_id.clone(), ticket);
            }
        }
        // A crash can occur after Core's receipt commit and before our callback.
        // Recover only exact same-Run tool receipts; image/snapshot consumers need
        // their own successful validation/publication confirmation.
        let consumers: HashSet<_> = self
            .tickets
            .values()
            .filter_map(|t| t.consumer_id.strip_prefix("tool:").map(str::to_owned))
            .collect();
        for call_id in consumers {
            let consumer = format!("tool:{call_id}");
            if self
                .tickets
                .values()
                .filter(|ticket| ticket.consumer_id == consumer)
                .all(|ticket| self.confirmed.contains_key(&ticket.task_id))
            {
                continue;
            }
            let receipt = store.with_client(|db| {
                let rows = db.query("SELECT e.event_id FROM runtime.runtime_events e WHERE e.session_id=$1 AND e.event_type='tool_execution.receipt.v1' AND e.payload_json::jsonb->>'toolCallId'=$2 AND EXISTS(SELECT 1 FROM runtime.runtime_events i WHERE i.session_id=e.session_id AND i.task_id=e.task_id AND i.event_type='tool_execution.intent.v1' AND i.payload_json::jsonb->>'toolCallId'=$2 AND i.payload_json::jsonb->'agentRunIdentity'->>'agentRunId'=$3) LIMIT 2", &[&session,&call_id,&run]).map_err(|e|e.to_string())?;
                Ok((rows.len()==1).then(||rows[0].get::<_,String>(0)))
            })?;
            if let Some(receipt) = receipt {
                self.confirm(store, session, run, &consumer, &receipt)?;
            }
        }
        // Recover the commit-to-callback gap using the exact durable source
        // and its committed Core checkpoint, never observation or stage alone.
        let snapshots: HashSet<_> = self
            .tickets
            .values()
            .filter(|ticket| {
                ticket.consumer_id.starts_with("snapshot:")
                    && !self.confirmed.contains_key(&ticket.task_id)
            })
            .map(|ticket| ticket.consumer_id.clone())
            .collect();
        for consumer in snapshots {
            let raw = store.with_client(|db| {
                let rows=db.query("SELECT p.payload_json FROM runtime.runtime_events p JOIN runtime.checkpoints c ON c.checkpoint_id=(p.payload_json::jsonb->>'checkpointId') AND c.session_id=p.session_id AND c.kind='recovery' AND c.status='committed' WHERE p.session_id=$1 AND p.task_id=$2 AND p.event_type IN ($3,$4) AND p.payload_json::jsonb->'consumerIds' ? $5 AND c.payload_json::jsonb->>'agentRunId'=$2 AND c.payload_json::jsonb->>'sessionId'=$1 AND c.payload_json::jsonb->'workspaceSnapshot'=(p.payload_json::jsonb->'snapshot') LIMIT 2",&[&session,&run,&SNAPSHOT_PUBLISHED,&SNAPSHOT_REUSED,&consumer]).map_err(|e|e.to_string())?;
                Ok((rows.len()==1).then(||rows[0].get::<_,String>(0)))
            })?;
            if let Some(raw) = raw {
                let binding = decode_snapshot_binding(&raw)?;
                if !binding.consumer_ids.contains(&consumer) {
                    return Err("execution_output_snapshot_binding_mismatch".into());
                }
                self.confirm(
                    store,
                    session,
                    run,
                    &consumer,
                    &format!("checkpoint:{}", binding.checkpoint_id),
                )?;
            }
        }
        self.loaded = true;
        Ok(())
    }
    fn confirm(
        &mut self,
        store: &PostgresRuntimeStore,
        session: &str,
        run: &str,
        consumer: &str,
        receipt: &str,
    ) -> Result<(), String> {
        // Confirmation is per physical task. An earlier successful read of an
        // image reference cannot acknowledge a later, still unvalidated helper.
        let tasks: Vec<_> = self
            .tickets
            .values()
            .filter(|ticket| ticket.consumer_id == consumer)
            .map(|ticket| ticket.task_id.clone())
            .collect();
        for task_id in tasks {
            let confirmation = Confirmation {
                schema: CONFIRMED.into(),
                task_id: task_id.clone(),
                consumer_id: consumer.into(),
                receipt_id: wire_receipt(receipt),
            };
            if let Some(previous) = self.confirmed.get(&task_id) {
                if previous != &confirmation {
                    return Err("execution_output_confirmation_conflict".into());
                }
                continue;
            }
            append(
                store,
                run,
                session,
                confirmation_id(run, &task_id),
                CONFIRMED,
                &confirmation,
            )?;
            self.confirmed.insert(task_id, confirmation);
        }
        Ok(())
    }
    fn record(
        &mut self,
        store: &PostgresRuntimeStore,
        session: &str,
        run: &str,
        fence: &EnvironmentTaskFence,
        operation: &str,
        consumer: &str,
    ) -> Result<(), String> {
        self.load(store, session, run, &fence.environment_instance_id)?;
        let task_id = fence.task_id(operation)?;
        let ticket = Ticket {
            schema: TICKET.into(),
            task_id: task_id.clone(),
            operation_id: operation
                .split_once(':')
                .map(|(_, id)| id)
                .ok_or("execution_output_operation_invalid")?
                .into(),
            instance_id: fence.environment_instance_id.clone(),
            owner: fence.controller_owner.clone(),
            consumer_id: consumer.into(),
            confirmation_event_id: confirmation_id(run, &task_id),
        };
        if let Some(previous) = self.tickets.get(&task_id) {
            return if previous == &ticket {
                Ok(())
            } else {
                Err("execution_output_ticket_conflict".into())
            };
        }
        append(store, run, session, ticket_id(&task_id), TICKET, &ticket)?;
        self.tickets.insert(task_id, ticket);
        Ok(())
    }
}

fn release_confirmed(
    state: &mut OutputConsumption,
    fence: &EnvironmentTaskFence,
    mut rpc: impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
    mut record_released: impl FnMut(&Ticket, &str) -> Result<(), String>,
) -> Result<(), String> {
    let EnvironmentTaskOwner::AgentRun { run_id: run } = &fence.controller_owner else {
        return Err("execution_output_owner_not_run".into());
    };
    let pending: Vec<_> = state
        .tickets
        .values()
        .filter_map(|ticket| {
            state
                .confirmed
                .get(&ticket.task_id)
                .map(|confirmation| (ticket.clone(), confirmation.clone()))
        })
        .collect();
    for (ticket, confirmation) in pending {
        if ticket.instance_id != fence.environment_instance_id
            || ticket.owner != fence.controller_owner
        {
            continue;
        }
        let response = rpc(EnvironmentTaskRequest::ConsumeOutput {
            fence: fence.clone(),
            task_id: ticket.task_id.clone(),
            consumer_receipt_id: confirmation.receipt_id.clone(),
        });
        match response {
            Ok(EnvironmentTaskResult::OutputConsumed {
                task,
                consumer_receipt_id,
            }) if task.task_id == ticket.task_id
                && consumer_receipt_id == confirmation.receipt_id
                && task.state == EnvironmentTaskState::Exited
                && task.scope
                    == (EnvironmentTaskScope::Run {
                        run_id: run.clone(),
                    })
                && task.cleanup_complete
                && task.output_complete
                && fence.matches_original_operation_id(
                    &task.operation_id,
                    &fence.operation_id(&ticket.operation_id),
                ) =>
            {
                record_released(&ticket, &consumer_receipt_id)?;
                state.tickets.remove(&task.task_id);
            }
            // A failure only keeps the original body charged. Never turn a
            // consumption retry into another Start, or a durable tool failure.
            _ => {}
        }
    }
    Ok(())
}

fn append_released(
    store: &PostgresRuntimeStore,
    session: &str,
    run: &str,
    ticket: &Ticket,
    receipt: &str,
) -> Result<(), String> {
    append(
        store,
        run,
        session,
        released_id(&ticket.task_id),
        RELEASED,
        &serde_json::json!({"schema":RELEASED,"taskId":ticket.task_id,"consumerReceiptId":receipt}),
    )
}

pub(super) fn recover_before_prepare(
    store: &PostgresRuntimeStore,
    session: &str,
    run: &str,
    fence: &EnvironmentTaskFence,
    rpc: impl FnMut(EnvironmentTaskRequest) -> Result<EnvironmentTaskResult, String>,
) -> Result<(), String> {
    let mut state = OutputConsumption::default();
    state.load(store, session, run, &fence.environment_instance_id)?;
    release_confirmed(&mut state, fence, rpc, |ticket, receipt| {
        append_released(store, session, run, ticket, receipt)
    })
}

impl DockerExecutionHostRunner {
    pub(super) fn record_published_snapshot_outputs(
        &self,
        checkpoint: &str,
        snapshot: &RecoveryWorkspaceSnapshotV1,
        consumers: &[String],
    ) -> Result<(), String> {
        self.record_snapshot_output_binding(
            checkpoint,
            snapshot,
            consumers,
            SnapshotOutputSource::Published,
        )
    }

    pub(super) fn record_reused_snapshot_outputs(
        &self,
        checkpoint: &str,
        snapshot: &RecoveryWorkspaceSnapshotV1,
        consumers: &[String],
    ) -> Result<(), String> {
        self.record_snapshot_output_binding(
            checkpoint,
            snapshot,
            consumers,
            SnapshotOutputSource::Reused,
        )
    }

    fn record_snapshot_output_binding(
        &self,
        checkpoint: &str,
        snapshot: &RecoveryWorkspaceSnapshotV1,
        consumers: &[String],
        source: SnapshotOutputSource,
    ) -> Result<(), String> {
        if consumers.is_empty() {
            return Ok(());
        }
        let Some((store, session)) = self.consumption_binding() else {
            return Ok(());
        };
        snapshot.validate()?;
        let binding = SnapshotOutputBinding {
            schema: source.event_type().into(),
            checkpoint_id: checkpoint.into(),
            snapshot: snapshot.clone(),
            consumer_ids: consumers.to_vec(),
            source,
        };
        append(
            store,
            &self.agent_run_id,
            session,
            snapshot_binding_id(&self.agent_run_id, &binding),
            source.event_type(),
            &binding,
        )
    }

    pub(crate) fn confirm_recovery_checkpoint_outputs(&self, checkpoint: &str) {
        let proof = (|| {
            let Some((store, session)) = self.consumption_binding() else {
                return Ok(Vec::new());
            };
            committed_snapshot_outputs(store, session, &self.agent_run_id, checkpoint)
        })();
        if let Ok(bindings) = proof {
            for binding in bindings {
                for consumer in binding.consumer_ids {
                    self.consume_output_group(&consumer, &format!("checkpoint:{checkpoint}"));
                }
            }
        }
    }

    fn consumption_binding(&self) -> Option<(&PostgresRuntimeStore, &str)> {
        Some((
            self.resident_store.as_deref()?,
            self.environment_authorization
                .as_ref()?
                .payload
                .session_id
                .as_str(),
        ))
    }
    pub(super) fn record_output_consumer(
        &self,
        fence: &EnvironmentTaskFence,
        operation: &str,
        consumer: Option<&str>,
    ) -> Result<(), String> {
        let Some(consumer) = consumer else {
            return Ok(());
        };
        let Some((store, session)) = self.consumption_binding() else {
            return Ok(());
        };
        self.output_consumption
            .lock()
            .map_err(|_| "execution_output_consumer_lock_failed")?
            .record(
                store,
                session,
                &self.agent_run_id,
                fence,
                operation,
                consumer,
            )
    }
    pub(super) fn consume_output_group(&self, consumer: &str, receipt: &str) {
        let result = (|| {
            let Some((store, session)) = self.consumption_binding() else {
                return Ok(());
            };
            let fence = self.environment_task_fence()?;
            {
                let mut state = self
                    .output_consumption
                    .lock()
                    .map_err(|_| "execution_output_consumer_lock_failed")?;
                state.load(
                    store,
                    session,
                    &self.agent_run_id,
                    &fence.environment_instance_id,
                )?;
                state.confirm(store, session, &self.agent_run_id, consumer, receipt)?;
            }
            self.flush_consumed_outputs()
        })();
        if result.is_err() {
            eprintln!("execution_output_reclamation_pending");
        }
    }
    pub(super) fn flush_consumed_outputs(&self) -> Result<(), String> {
        let Some((store, session)) = self.consumption_binding() else {
            return Ok(());
        };
        let fence = self.environment_task_fence()?;
        // Move only metadata out while performing Host I/O; never hold this lock
        // across an RPC that may acquire the controller's authorization pin.
        let mut pending = {
            let mut state = self
                .output_consumption
                .lock()
                .map_err(|_| "execution_output_consumer_lock_failed")?;
            state.load(
                store,
                session,
                &self.agent_run_id,
                &fence.environment_instance_id,
            )?;
            OutputConsumption {
                loaded: true,
                tickets: state.tickets.clone(),
                confirmed: state.confirmed.clone(),
            }
        };
        let observed_ids: HashSet<_> = pending.tickets.keys().cloned().collect();
        release_confirmed(
            &mut pending,
            &fence,
            |request| self.environment_task_rpc(request),
            |ticket, receipt| append_released(store, session, &self.agent_run_id, ticket, receipt),
        )?;
        let mut state = self
            .output_consumption
            .lock()
            .map_err(|_| "execution_output_consumer_lock_failed")?;
        state
            .tickets
            .retain(|id, _| !observed_ids.contains(id) || pending.tickets.contains_key(id));
        let active: HashSet<_> = state.tickets.keys().cloned().collect();
        state.confirmed.retain(|id, _| active.contains(id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::{EnvironmentTaskControllerPurpose, EnvironmentTaskSnapshot};

    fn fence() -> EnvironmentTaskFence {
        EnvironmentTaskFence {
            environment_instance_id: "instance".into(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "run".into(),
            },
            controller_epoch: 2,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        }
    }
    fn ticket(fence: &EnvironmentTaskFence, operation: &str, consumer: &str) -> Ticket {
        Ticket {
            schema: TICKET.into(),
            task_id: fence.task_id(&fence.operation_id(operation)).unwrap(),
            operation_id: operation.into(),
            instance_id: fence.environment_instance_id.clone(),
            owner: fence.controller_owner.clone(),
            consumer_id: consumer.into(),
            confirmation_event_id: String::new(),
        }
    }
    fn state(ticket: &Ticket, confirmed: bool) -> OutputConsumption {
        OutputConsumption {
            loaded: true,
            tickets: BTreeMap::from([(ticket.task_id.clone(), ticket.clone())]),
            confirmed: if confirmed {
                BTreeMap::from([(
                    ticket.task_id.clone(),
                    Confirmation {
                        schema: CONFIRMED.into(),
                        task_id: ticket.task_id.clone(),
                        consumer_id: ticket.consumer_id.clone(),
                        receipt_id: wire_receipt("durable-receipt"),
                    },
                )])
            } else {
                BTreeMap::new()
            },
        }
    }
    fn acknowledged(
        ticket: &Ticket,
        fence: &EnvironmentTaskFence,
        receipt: String,
    ) -> EnvironmentTaskResult {
        EnvironmentTaskResult::OutputConsumed {
            task: EnvironmentTaskSnapshot {
                task_id: ticket.task_id.clone(),
                operation_id: fence.operation_id(&ticket.operation_id),
                scope: EnvironmentTaskScope::Run {
                    run_id: "run".into(),
                },
                state: EnvironmentTaskState::Exited,
                exit_code: Some(0),
                timed_out: None,
                cleanup_complete: true,
                output_complete: true,
            },
            consumer_receipt_id: receipt,
        }
    }
    fn receipt(request: &EnvironmentTaskRequest) -> String {
        match request {
            EnvironmentTaskRequest::ConsumeOutput {
                consumer_receipt_id,
                ..
            } => consumer_receipt_id.clone(),
            _ => panic!("reclamation must never dispatch another task"),
        }
    }

    #[test]
    fn unconfirmed_output_is_not_consumed() {
        let f = fence();
        let t = ticket(&f, "original", "tool:call");
        let mut pending = state(&t, false);
        release_confirmed(
            &mut pending,
            &f,
            |_| panic!("uncommitted result must retain its output"),
            |_, _| panic!("no release fact before confirmation"),
        )
        .unwrap();
        assert!(pending.tickets.contains_key(&t.task_id));
    }

    #[test]
    fn lost_ack_repeats_only_the_original_consumption_identity() {
        let f = fence();
        let t = ticket(&f, "original", "tool:call");
        let mut pending = state(&t, true);
        let mut requests = Vec::new();
        let mut released = Vec::new();
        release_confirmed(
            &mut pending,
            &f,
            |request| {
                requests.push(request);
                Err("transport_lost".into())
            },
            |_, _| panic!("lost acknowledgement must retain the pending ticket"),
        )
        .unwrap();
        assert!(pending.tickets.contains_key(&t.task_id));
        release_confirmed(
            &mut pending,
            &f,
            |request| {
                let id = receipt(&request);
                requests.push(request);
                Ok(acknowledged(&t, &f, id))
            },
            |ticket, _| {
                released.push(ticket.task_id.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);
        assert_eq!(released, vec![t.task_id]);
        assert!(pending.tickets.is_empty());
    }

    #[test]
    fn prior_image_confirmation_does_not_cover_a_later_read_of_the_same_source() {
        let f = fence();
        let first = ticket(&f, "image-first", "image:source-hash");
        let second = ticket(&f, "image-second", "image:source-hash");
        let mut pending = state(&first, true);
        pending
            .tickets
            .insert(second.task_id.clone(), second.clone());
        let mut requests = Vec::new();
        release_confirmed(
            &mut pending,
            &f,
            |request| {
                let id = receipt(&request);
                requests.push(request);
                Ok(acknowledged(&first, &f, id))
            },
            |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(requests.len(), 1);
        assert!(!pending.tickets.contains_key(&first.task_id));
        assert!(pending.tickets.contains_key(&second.task_id));
    }

    #[test]
    fn nonterminal_or_foreign_scope_acknowledgements_do_not_release_a_ticket() {
        for case in 0..4 {
            let f = fence();
            let t = ticket(&f, "original", "tool:call");
            let mut pending = state(&t, true);
            release_confirmed(
                &mut pending,
                &f,
                |request| {
                    let mut result = acknowledged(&t, &f, receipt(&request));
                    let EnvironmentTaskResult::OutputConsumed { task, .. } = &mut result else {
                        unreachable!()
                    };
                    match case {
                        0 => task.state = EnvironmentTaskState::Running,
                        1 => task.scope = EnvironmentTaskScope::Environment,
                        2 => {
                            task.scope = EnvironmentTaskScope::Run {
                                run_id: "other".into(),
                            }
                        }
                        _ => task.cleanup_complete = false,
                    }
                    Ok(result)
                },
                |_, _| panic!("invalid acknowledgement must keep original recovery responsibility"),
            )
            .unwrap();
            assert!(pending.tickets.contains_key(&t.task_id), "case {case}");
        }
    }

    #[test]
    fn failed_release_fact_commit_keeps_idempotent_host_confirmation_recoverable() {
        let f = fence();
        let t = ticket(&f, "original", "tool:call");
        let mut pending = state(&t, true);
        let error = release_confirmed(
            &mut pending,
            &f,
            |request| Ok(acknowledged(&t, &f, receipt(&request))),
            |_, _| Err("release_fact_unavailable".into()),
        )
        .unwrap_err();
        assert_eq!(error, "release_fact_unavailable");
        assert!(pending.tickets.contains_key(&t.task_id));
        release_confirmed(
            &mut pending,
            &f,
            |request| Ok(acknowledged(&t, &f, receipt(&request))),
            |_, _| Ok(()),
        )
        .unwrap();
        assert!(pending.tickets.is_empty());
    }

    #[test]
    #[ignore = "requires gate-owned Postgres database"]
    fn postgres_output_consumption_recovery_requires_committed_authority() {
        use centaeris_core::runtime::contracts::{
            CheckpointKindV1, CheckpointRecord, RuntimeRecoveryCheckpointV1,
            RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1,
        };
        use postgres::{Client, NoTls};
        let url = std::env::var("CENTAERIS_TEST_POSTGRES_URL").expect("gate-owned Postgres URL");
        let mut db = Client::connect(&url, NoTls).unwrap();
        crate::postgres_store::integration_tests::owned_database::require_owned_database(&mut db);
        drop(db);
        let store = PostgresRuntimeStore::new(&url).unwrap();
        let unique = format!(
            "output-consumption-{}-{}",
            std::process::id(),
            crate::now_ms().unwrap()
        );
        let session = format!("session-{unique}");
        let run = format!("run-{unique}");
        let turn = format!("turn-{unique}");
        let mut original = fence();
        original.environment_instance_id = format!("instance-{unique}");
        original.controller_owner = EnvironmentTaskOwner::AgentRun {
            run_id: run.clone(),
        };
        let mut state = OutputConsumption::default();
        state
            .record(
                &store,
                &session,
                &run,
                &original,
                &original.operation_id("bash-original"),
                "tool:call",
            )
            .unwrap();
        let task = state.tickets.values().next().unwrap().clone();
        let stored_result = serde_json::json!({"tool_call_id":"call","tool_name":"bash","status":"ok","content":"durably saved output","details":{},"error":null,"started_at_ms":1,"completed_at_ms":2,"latency_ms":1,"parallel_group":null,"transition_reason":"local_tool_exec"});
        for (id, kind, payload) in [
            (
                format!("intent-{unique}"),
                "tool_execution.intent.v1",
                serde_json::json!({"schema":"tool_execution.intent.v1","sessionId":session,"turnId":turn,"toolCallId":"call","agentRunIdentity":{"agentRunId":run,"authorizationDigest":format!("sha256:{}","a".repeat(64))},"sourceToolName":"bash","sessionToolCallEventId":"test-call","providerId":"native","toolContractDigest":"test","modelArgsDigest":"test","argsDigest":"test","effectiveArgsJson":"{}","recordedAtMs":1}),
            ),
            (
                format!("receipt-{unique}"),
                "tool_execution.receipt.v1",
                serde_json::json!({"schema":"tool_execution.receipt.v1","sessionId":session,"turnId":turn,"toolCallId":"call","sourceToolName":"bash","argsDigest":"test","effectiveArgsJson":"{}","preHookContexts":[],"runPostHook":false,"resultJson":stored_result.to_string()}),
            ),
        ] {
            let mut fact = event(&run, &session, id, kind, &payload).unwrap();
            fact.task_id = Some(turn.clone());
            store.append_event_idempotent(fact).unwrap();
        }
        drop(state); // Receipt committed, but Runtime died before its consumption callback.
        let mut resumed = OutputConsumption::default();
        resumed
            .load(&store, &session, &run, &original.environment_instance_id)
            .unwrap();
        assert!(resumed.confirmed.contains_key(&task.task_id));
        let mut foreign = original.clone();
        foreign.controller_owner = EnvironmentTaskOwner::AgentRun {
            run_id: "foreign-run".into(),
        };
        release_confirmed(
            &mut resumed,
            &foreign,
            |_| panic!("foreign owner must not reclaim original output"),
            |_, _| panic!("foreign owner must not record release"),
        )
        .unwrap();
        let mut renewed = original.clone();
        renewed.controller_epoch += 1;
        release_confirmed(
            &mut resumed,
            &renewed,
            |request| {
                let EnvironmentTaskRequest::ConsumeOutput {
                    fence,
                    consumer_receipt_id,
                    ..
                } = request
                else {
                    panic!("must not redispatch original command")
                };
                assert_eq!(fence, renewed);
                let mut result = acknowledged(&task, &original, consumer_receipt_id);
                if let EnvironmentTaskResult::OutputConsumed { task, .. } = &mut result {
                    task.scope = EnvironmentTaskScope::Run {
                        run_id: run.clone(),
                    };
                }
                Ok(result)
            },
            |ticket, receipt| append_released(&store, &session, &run, ticket, receipt),
        )
        .unwrap();
        assert!(resumed.tickets.is_empty());
        let mut reloaded = OutputConsumption::default();
        reloaded
            .load(&store, &session, &run, &original.environment_instance_id)
            .unwrap();
        assert!(reloaded.tickets.is_empty());

        let snapshot = RecoveryWorkspaceSnapshotV1 {
            object_ref: Some(format!("object-{unique}")),
            snapshot_sha256: format!("sha256:{}", "b".repeat(64)),
            snapshot_size_bytes: 3,
            expanded_size_bytes: 3,
            file_count: 1,
        };
        let mut expected = HashSet::new();
        for source in [
            SnapshotOutputSource::Published,
            SnapshotOutputSource::Reused,
        ] {
            for case in [
                "uncommitted",
                "wrong-baseline",
                "wrong-ref",
                "foreign-checkpoint",
                "committed",
            ] {
                let label = format!("{}-{case}", source.event_type());
                let consumer = format!("snapshot:{unique}:{label}");
                let checkpoint = format!("checkpoint-{unique}-{label}");
                let mut observed = OutputConsumption::default();
                observed
                    .record(
                        &store,
                        &session,
                        &run,
                        &original,
                        &original.operation_id(&label),
                        &consumer,
                    )
                    .unwrap();
                let task = observed
                    .tickets
                    .values()
                    .find(|ticket| ticket.consumer_id == consumer)
                    .unwrap()
                    .clone();
                let binding = SnapshotOutputBinding {
                    schema: source.event_type().into(),
                    checkpoint_id: checkpoint.clone(),
                    snapshot: snapshot.clone(),
                    consumer_ids: vec![consumer],
                    source,
                };
                append(
                    &store,
                    &run,
                    &session,
                    snapshot_binding_id(&run, &binding),
                    source.event_type(),
                    &binding,
                )
                .unwrap();
                // A second successful observation for the same planned checkpoint
                // has its own physical task; neither may disappear before commit.
                if case == "committed" {
                    let repeated_consumer = format!("snapshot:{unique}:{label}-again");
                    observed
                        .record(
                            &store,
                            &session,
                            &run,
                            &original,
                            &original.operation_id(&format!("{label}-again")),
                            &repeated_consumer,
                        )
                        .unwrap();
                    let repeated_task = observed
                        .tickets
                        .values()
                        .find(|ticket| ticket.consumer_id == repeated_consumer)
                        .unwrap();
                    expected.insert(repeated_task.task_id.clone());
                    let repeated = SnapshotOutputBinding {
                        consumer_ids: vec![repeated_consumer],
                        ..binding.clone()
                    };
                    append(
                        &store,
                        &run,
                        &session,
                        snapshot_binding_id(&run, &repeated),
                        source.event_type(),
                        &repeated,
                    )
                    .unwrap();
                    assert!(
                        committed_snapshot_outputs(&store, &session, &run, &checkpoint)
                            .unwrap()
                            .is_empty()
                    );
                }
                if case != "uncommitted" {
                    let mut bound_snapshot = snapshot.clone();
                    if case == "wrong-ref" {
                        bound_snapshot.object_ref = Some("a-different-durable-object".into());
                    }
                    if case == "wrong-baseline" {
                        bound_snapshot.snapshot_sha256 = format!("sha256:{}", "c".repeat(64));
                    }
                    let payload = RuntimeRecoveryCheckpointV1 {
                        schema: RUNTIME_RECOVERY_CHECKPOINT_SCHEMA_V1.into(),
                        checkpoint_id: checkpoint.clone(),
                        session_id: session.clone(),
                        agent_run_id: if case == "foreign-checkpoint" {
                            "foreign-run".into()
                        } else {
                            run.clone()
                        },
                        execution_id: format!("execution-{unique}"),
                        authorization_digest: format!("sha256:{}", "a".repeat(64)),
                        session_sequence: 1,
                        model_request_id: format!("request-{label}"),
                        workspace_snapshot: bound_snapshot,
                        workspace_generation: ExecutionWorkspaceGeneration::Unknown {
                            reason: "fixture".into(),
                        },
                        created_at_ms: 1,
                    };
                    payload.validate().unwrap();
                    store
                        .save_checkpoint(CheckpointRecord {
                            checkpoint_id: checkpoint.clone(),
                            kind: CheckpointKindV1::Recovery,
                            session_id: session.clone(),
                            turn_id: turn.clone(),
                            status: "committed".into(),
                            done_reason: None,
                            updated_at_ms: 1,
                            payload_json: serde_json::to_string(&payload).unwrap(),
                        })
                        .unwrap();
                }
                let proof =
                    committed_snapshot_outputs(&store, &session, &run, &checkpoint).unwrap();
                assert_eq!(
                    proof.len(),
                    if case == "committed" { 2 } else { 0 },
                    "case {label}"
                );
                if case == "committed" {
                    expected.insert(task.task_id);
                }
            }
        }
        let mut recovered = OutputConsumption::default();
        recovered
            .load(&store, &session, &run, &original.environment_instance_id)
            .unwrap();
        assert_eq!(recovered.confirmed.len(), expected.len());
        for task in expected {
            assert!(recovered.confirmed.contains_key(&task));
        }
        assert_eq!(
            recovered.tickets.len(),
            12,
            "unconfirmed snapshot evidence stays retained"
        );
    }
}
