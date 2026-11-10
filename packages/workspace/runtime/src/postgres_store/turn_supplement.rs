use postgres::{GenericClient, Row, Transaction};

use centaeris_core::session::reliability::{
    agent_run_lifecycle_job_id, AGENT_RUN_LIFECYCLE_JOB_KIND,
};
use centaeris_core::session::supplement::{
    turn_supplement_message_digest, validate_turn_supplement_id, validate_turn_supplement_message,
    AcknowledgeTurnSupplementsRequest, ClaimTurnSupplementsRequest,
    CloseTurnSupplementQueueRequest, DurableTurnSupplement, EnqueueTurnSupplementDisposition,
    EnqueueTurnSupplementRequest, EnqueueTurnSupplementResult, TurnSupplementStoreError,
    TurnSupplementStorePort, MAX_PENDING_TURN_SUPPLEMENTS,
};

use super::PostgresRuntimeStore;

impl TurnSupplementStorePort for PostgresRuntimeStore {
    fn enqueue_turn_supplement(
        &self,
        mut request: EnqueueTurnSupplementRequest,
    ) -> Result<EnqueueTurnSupplementResult, TurnSupplementStoreError> {
        request.message = validate_turn_supplement_message(request.message.as_str())?;
        validate_turn_supplement_id(request.supplement_id.as_str())?;
        validate_common_identity(
            &request.agent_run_id,
            &request.lifecycle_job_id,
            &request.session_id,
            &request.authorization_digest,
        )?;
        self.with_client_error(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin Postgres turn supplement enqueue failed: {error}"))?;
            validate_active_job(
                &mut tx,
                &request.agent_run_id,
                &request.lifecycle_job_id,
                &request.session_id,
                &request.authorization_digest,
                None,
                true,
            )?;
            let queue = load_queue(&mut tx, &request.agent_run_id, true)?;
            let (revision, next_sequence, accepting, mut entries, mut dedupe) = match queue {
                Some(queue) => {
                    validate_queue_identity(
                        &queue,
                        &request.lifecycle_job_id,
                        &request.session_id,
                        &request.authorization_digest,
                    )?;
                    (
                        queue.revision,
                        queue.next_sequence,
                        queue.accepting,
                        queue.entries,
                        queue.dedupe,
                    )
                }
                None => (0, 1, true, Vec::new(), std::collections::BTreeMap::new()),
            };
            let message_digest = turn_supplement_message_digest(request.message.as_str());
            if let Some(existing_digest) = dedupe.get(request.supplement_id.as_str()) {
                if existing_digest != &message_digest {
                    return Err(TurnSupplementStoreError::IdempotencyConflict);
                }
                tx.commit().map_err(|error| {
                    format!("commit duplicate Postgres turn supplement failed: {error}")
                })?;
                return Ok(EnqueueTurnSupplementResult {
                    disposition: EnqueueTurnSupplementDisposition::Duplicate,
                    queued_count: entries.len(),
                    revision,
                });
            }
            if !accepting {
                return Err(TurnSupplementStoreError::AdmissionClosed);
            }
            if entries.len() >= MAX_PENDING_TURN_SUPPLEMENTS {
                return Err(TurnSupplementStoreError::QueueFull);
            }
            entries.push(DurableTurnSupplement {
                supplement_id: request.supplement_id.clone(),
                sequence: next_sequence,
                message: request.message.clone(),
                created_at_ms: request.created_at_ms,
                claim_token: None,
                claim_lease_owner: None,
            });
            dedupe.insert(request.supplement_id.clone(), message_digest);
            let next_revision = revision.saturating_add(1);
            let encoded = encode_entries(entries.as_slice())?;
            let dedupe_json = encode_dedupe(&dedupe)?;
            if revision == 0 {
                tx.execute(
                    "INSERT INTO runtime_turn_supplement_queues(agent_run_id,lifecycle_job_id,session_id,authorization_digest,revision,next_sequence,accepting,entries_json,dedupe_json,closed_reason,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,1,$7,$8,NULL,$9)",
                    &[&request.agent_run_id, &request.lifecycle_job_id, &request.session_id, &request.authorization_digest, &to_i64(next_revision)?, &to_i64(next_sequence.saturating_add(1))?, &encoded, &dedupe_json, &request.created_at_ms],
                ).map_err(|error| format!("insert Postgres turn supplement queue failed: {error}"))?;
            } else {
                let updated = tx.execute(
                    "UPDATE runtime_turn_supplement_queues SET revision=$1,next_sequence=$2,entries_json=$3,dedupe_json=$4,updated_at_ms=$5 WHERE agent_run_id=$6 AND revision=$7",
                    &[&to_i64(next_revision)?, &to_i64(next_sequence.saturating_add(1))?, &encoded, &dedupe_json, &request.created_at_ms, &request.agent_run_id, &to_i64(revision)?],
                ).map_err(|error| format!("update Postgres turn supplement queue failed: {error}"))?;
                if updated != 1 {
                    return Err(TurnSupplementStoreError::QueueCasConflict);
                }
            }
            tx.execute(
                "UPDATE runtime_jobs SET run_at_ms=LEAST(run_at_ms,$1),updated_at_ms=$1 WHERE job_id=$2 AND status='queued' AND NOT EXISTS(SELECT 1 FROM checkpoints WHERE session_id=$3 AND done_reason IN('approval','question','runtime_job'))",
                &[&request.created_at_ms, &request.lifecycle_job_id, &request.session_id],
            ).map_err(|error| format!("wake Postgres turn supplement lifecycle job failed: {error}"))?;
            tx.commit().map_err(|error| {
                format!("commit Postgres turn supplement enqueue failed: {error}")
            })?;
            Ok(EnqueueTurnSupplementResult {
                disposition: EnqueueTurnSupplementDisposition::Accepted,
                queued_count: entries.len(),
                revision: next_revision,
            })
        })
    }

    fn claim_turn_supplements(
        &self,
        request: ClaimTurnSupplementsRequest,
    ) -> Result<Vec<DurableTurnSupplement>, TurnSupplementStoreError> {
        validate_common_identity(
            &request.agent_run_id,
            &request.lifecycle_job_id,
            &request.session_id,
            &request.authorization_digest,
        )?;
        if request.lease_owner.trim().is_empty()
            || request.claim_token.trim().is_empty()
            || request.limit == 0
            || request.limit > MAX_PENDING_TURN_SUPPLEMENTS
        {
            return Err(TurnSupplementStoreError::ClaimIdentityInvalid);
        }
        self.with_client_error(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin Postgres turn supplement claim failed: {error}"))?;
            validate_active_job(
                &mut tx,
                &request.agent_run_id,
                &request.lifecycle_job_id,
                &request.session_id,
                &request.authorization_digest,
                Some((&request.lease_owner, request.now_ms)),
                true,
            )?;
            let Some(mut queue) = load_queue(&mut tx, &request.agent_run_id, true)? else {
                if request.close_if_empty {
                    insert_closed_queue(&mut tx, &request)?;
                }
                tx.commit().map_err(|error| {
                    format!("commit empty Postgres turn supplement claim failed: {error}")
                })?;
                return Ok(Vec::new());
            };
            validate_queue_identity(
                &queue,
                &request.lifecycle_job_id,
                &request.session_id,
                &request.authorization_digest,
            )?;
            let mut claimed = Vec::new();
            for entry in &mut queue.entries {
                if entry.claim_token.as_deref() == Some(request.claim_token.as_str()) {
                    continue;
                }
                if entry.claim_lease_owner.as_deref() == Some(request.lease_owner.as_str()) {
                    return Err(TurnSupplementStoreError::ClaimInProgress);
                }
                entry.claim_token = Some(request.claim_token.clone());
                entry.claim_lease_owner = Some(request.lease_owner.clone());
                claimed.push(entry.clone());
                if claimed.len() == request.limit {
                    break;
                }
            }
            let close = request.close_if_empty && queue.entries.is_empty();
            if !claimed.is_empty() || close {
                queue.accepting = !close && queue.accepting;
                queue.closed_reason = close.then(|| "safe_point_closed".to_string());
                update_queue(&mut tx, &request.agent_run_id, &queue, request.now_ms)?;
            }
            tx.commit().map_err(|error| {
                format!("commit Postgres turn supplement claim failed: {error}")
            })?;
            Ok(claimed)
        })
    }

    fn acknowledge_turn_supplements(
        &self,
        request: AcknowledgeTurnSupplementsRequest,
    ) -> Result<(), TurnSupplementStoreError> {
        validate_common_identity(
            &request.agent_run_id,
            &request.lifecycle_job_id,
            &request.session_id,
            &request.authorization_digest,
        )?;
        if request.lease_owner.trim().is_empty()
            || request.claim_token.trim().is_empty()
            || request.supplement_ids.is_empty()
        {
            return Err(TurnSupplementStoreError::AcknowledgeIdentityInvalid);
        }
        self.with_client_error(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin Postgres turn supplement ack failed: {error}"))?;
            validate_active_job(
                &mut tx,
                &request.agent_run_id,
                &request.lifecycle_job_id,
                &request.session_id,
                &request.authorization_digest,
                Some((&request.lease_owner, request.acknowledged_at_ms)),
                true,
            )?;
            let mut queue = load_queue(&mut tx, &request.agent_run_id, true)?
                .ok_or(TurnSupplementStoreError::QueueMissing)?;
            let requested = request
                .supplement_ids
                .iter()
                .collect::<std::collections::HashSet<_>>();
            let matched = queue
                .entries
                .iter()
                .filter(|entry| requested.contains(&entry.supplement_id))
                .collect::<Vec<_>>();
            if matched.is_empty() {
                tx.commit().map_err(|error| {
                    format!("commit duplicate Postgres turn supplement ack failed: {error}")
                })?;
                return Ok(());
            }
            if matched.len() != requested.len()
                || matched.iter().any(|entry| {
                    entry.claim_token.as_deref() != Some(request.claim_token.as_str())
                        || entry.claim_lease_owner.as_deref() != Some(request.lease_owner.as_str())
                })
            {
                return Err(TurnSupplementStoreError::AcknowledgeIdentityMismatch);
            }
            queue
                .entries
                .retain(|entry| !requested.contains(&entry.supplement_id));
            update_queue(
                &mut tx,
                &request.agent_run_id,
                &queue,
                request.acknowledged_at_ms,
            )?;
            tx.commit()
                .map_err(|error| format!("commit Postgres turn supplement ack failed: {error}"))?;
            Ok(())
        })
    }

    fn close_turn_supplement_queue(
        &self,
        request: CloseTurnSupplementQueueRequest,
    ) -> Result<(), TurnSupplementStoreError> {
        validate_common_identity(
            &request.agent_run_id,
            &request.lifecycle_job_id,
            &request.session_id,
            &request.authorization_digest,
        )?;
        if request.reason.trim().is_empty() {
            return Err(TurnSupplementStoreError::CloseReasonRequired);
        }
        self.with_client_error(|client| {
            let mut tx = client
                .transaction()
                .map_err(|error| format!("begin Postgres turn supplement close failed: {error}"))?;
            validate_active_job(
                &mut tx,
                &request.agent_run_id,
                &request.lifecycle_job_id,
                &request.session_id,
                &request.authorization_digest,
                request
                    .lease_owner
                    .as_deref()
                    .map(|owner| (owner, request.closed_at_ms)),
                false,
            )?;
            match load_queue(&mut tx, &request.agent_run_id, true)? {
                Some(mut queue) => {
                    validate_queue_identity(
                        &queue,
                        &request.lifecycle_job_id,
                        &request.session_id,
                        &request.authorization_digest,
                    )?;
                    queue.accepting = false;
                    queue.entries.clear();
                    queue.closed_reason = Some(request.reason.clone());
                    update_queue(
                        &mut tx,
                        &request.agent_run_id,
                        &queue,
                        request.closed_at_ms,
                    )?;
                }
                None => {
                    tx.execute(
                        "INSERT INTO runtime_turn_supplement_queues(agent_run_id,lifecycle_job_id,session_id,authorization_digest,revision,next_sequence,accepting,entries_json,dedupe_json,closed_reason,updated_at_ms) VALUES($1,$2,$3,$4,1,1,0,'[]','{}',$5,$6)",
                        &[&request.agent_run_id, &request.lifecycle_job_id, &request.session_id, &request.authorization_digest, &request.reason, &request.closed_at_ms],
                    ).map_err(|error| format!("insert closed Postgres turn supplement queue failed: {error}"))?;
                }
            }
            tx.commit().map_err(|error| {
                format!("commit Postgres turn supplement close failed: {error}")
            })?;
            Ok(())
        })
    }
}

#[derive(Debug)]
struct StoredQueue {
    lifecycle_job_id: String,
    session_id: String,
    authorization_digest: String,
    revision: u64,
    next_sequence: u64,
    accepting: bool,
    entries: Vec<DurableTurnSupplement>,
    dedupe: std::collections::BTreeMap<String, String>,
    closed_reason: Option<String>,
}

fn load_queue<C: GenericClient>(
    client: &mut C,
    agent_run_id: &str,
    lock: bool,
) -> Result<Option<StoredQueue>, String> {
    let suffix = if lock { " FOR UPDATE" } else { "" };
    client
        .query_opt(
            format!("SELECT lifecycle_job_id,session_id,authorization_digest,revision,next_sequence,accepting,entries_json,dedupe_json,closed_reason FROM runtime_turn_supplement_queues WHERE agent_run_id=$1{suffix}").as_str(),
            &[&agent_run_id],
        )
        .map_err(|error| format!("load Postgres turn supplement queue failed: {error}"))?
        .map(row_queue)
        .transpose()
}

fn row_queue(row: Row) -> Result<StoredQueue, String> {
    let revision = row.get::<_, i64>(3);
    let next_sequence = row.get::<_, i64>(4);
    Ok(StoredQueue {
        lifecycle_job_id: row.get(0),
        session_id: row.get(1),
        authorization_digest: row.get(2),
        revision: u64::try_from(revision)
            .map_err(|_| "turn supplement revision is negative".to_string())?,
        next_sequence: u64::try_from(next_sequence)
            .map_err(|_| "turn supplement sequence is negative".to_string())?,
        accepting: row.get::<_, i64>(5) == 1,
        entries: serde_json::from_str(row.get::<_, String>(6).as_str())
            .map_err(|error| format!("decode Postgres turn supplement queue failed: {error}"))?,
        dedupe: serde_json::from_str(row.get::<_, String>(7).as_str())
            .map_err(|error| format!("decode Postgres turn supplement dedupe failed: {error}"))?,
        closed_reason: row.get(8),
    })
}

fn update_queue(
    tx: &mut Transaction<'_>,
    agent_run_id: &str,
    queue: &StoredQueue,
    updated_at_ms: i64,
) -> Result<(), TurnSupplementStoreError> {
    let updated = tx.execute(
        "UPDATE runtime_turn_supplement_queues SET revision=$1,accepting=$2,entries_json=$3,dedupe_json=$4,closed_reason=$5,updated_at_ms=$6 WHERE agent_run_id=$7 AND revision=$8",
        &[&to_i64(queue.revision.saturating_add(1))?, &i64::from(queue.accepting), &encode_entries(&queue.entries)?, &encode_dedupe(&queue.dedupe)?, &queue.closed_reason.as_deref(), &updated_at_ms, &agent_run_id, &to_i64(queue.revision)?],
    ).map_err(|error| format!("CAS Postgres turn supplement queue failed: {error}"))?;
    if updated != 1 {
        return Err(TurnSupplementStoreError::QueueCasConflict);
    }
    Ok(())
}

fn insert_closed_queue(
    tx: &mut Transaction<'_>,
    request: &ClaimTurnSupplementsRequest,
) -> Result<(), TurnSupplementStoreError> {
    tx.execute(
        "INSERT INTO runtime_turn_supplement_queues(agent_run_id,lifecycle_job_id,session_id,authorization_digest,revision,next_sequence,accepting,entries_json,dedupe_json,closed_reason,updated_at_ms) VALUES($1,$2,$3,$4,1,1,0,'[]','{}','safe_point_closed',$5)",
        &[&request.agent_run_id, &request.lifecycle_job_id, &request.session_id, &request.authorization_digest, &request.now_ms],
    ).map_err(|error| format!("insert empty closed Postgres turn supplement queue failed: {error}"))?;
    Ok(())
}

pub(crate) fn validate_active_job<C: GenericClient>(
    client: &mut C,
    agent_run_id: &str,
    job_id: &str,
    session_id: &str,
    authorization_digest: &str,
    lease: Option<(&str, i64)>,
    reject_cancelled: bool,
) -> Result<(), TurnSupplementStoreError> {
    if reject_cancelled {
        let cancelled = client
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM runtime_events WHERE event_id=$1 AND event_type='runtime.agent_run.cancel_requested.v1')",
                &[&format!("agent_run_cancel_requested:{agent_run_id}")],
            )
            .map_err(|error| format!("load Postgres turn supplement cancellation state failed: {error}"))?
            .get::<_, bool>(0);
        if cancelled {
            return Err(TurnSupplementStoreError::AdmissionClosed);
        }
    }
    let row = client
        .query_opt(
            "SELECT job_kind,status,session_id,payload_ref,idempotency_key,lease_owner,lease_expires_at_ms FROM runtime_jobs WHERE job_id=$1 FOR UPDATE",
            &[&job_id],
        )
        .map_err(|error| format!("load Postgres turn supplement lifecycle job failed: {error}"))?
        .ok_or(TurnSupplementStoreError::AgentRunNotActive)?;
    let status = row.get::<_, String>(1);
    if row.get::<_, String>(0) != AGENT_RUN_LIFECYCLE_JOB_KIND
        || !matches!(status.as_str(), "queued" | "leased" | "running")
        || row.get::<_, Option<String>>(2).as_deref() != Some(session_id)
        || row.get::<_, Option<String>>(3).as_deref()
            != Some(format!("record:agent_run:{agent_run_id}").as_str())
        || row.get::<_, String>(4)
            != format!("agent_run.lifecycle:{agent_run_id}:{authorization_digest}")
    {
        return Err(TurnSupplementStoreError::IdentityMismatch);
    }
    if let Some((owner, now_ms)) = lease {
        if status != "running"
            || row.get::<_, Option<String>>(5).as_deref() != Some(owner)
            || row
                .get::<_, Option<i64>>(6)
                .is_none_or(|expires| expires <= now_ms)
        {
            return Err(TurnSupplementStoreError::LeaseFenceRejected);
        }
    }
    Ok(())
}

pub(crate) fn validate_common_identity(
    agent_run_id: &str,
    job_id: &str,
    session_id: &str,
    digest: &str,
) -> Result<(), TurnSupplementStoreError> {
    if [agent_run_id, job_id, session_id, digest]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        return Err(TurnSupplementStoreError::IdentityRequired);
    }
    if job_id != agent_run_lifecycle_job_id(agent_run_id)? {
        return Err(TurnSupplementStoreError::JobIdMismatch);
    }
    Ok(())
}

fn validate_queue_identity(
    queue: &StoredQueue,
    job_id: &str,
    session_id: &str,
    digest: &str,
) -> Result<(), TurnSupplementStoreError> {
    if queue.lifecycle_job_id != job_id
        || queue.session_id != session_id
        || queue.authorization_digest != digest
    {
        return Err(TurnSupplementStoreError::QueueIdentityMismatch);
    }
    Ok(())
}

fn encode_entries(entries: &[DurableTurnSupplement]) -> Result<String, String> {
    serde_json::to_string(entries)
        .map_err(|error| format!("encode Postgres turn supplement queue failed: {error}"))
}

fn encode_dedupe(dedupe: &std::collections::BTreeMap<String, String>) -> Result<String, String> {
    serde_json::to_string(dedupe)
        .map_err(|error| format!("encode Postgres turn supplement dedupe ledger failed: {error}"))
}

fn to_i64(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "turn supplement counter overflow".to_string())
}
