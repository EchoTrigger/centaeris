//! Exact contended Run attempts reuse the durable eligibility clock, never resource refunds.
use super::*;
use crate::resident_capacity::HOST_PREPARATION_WAIT;

fn event_id(fence: &RuntimeJobLeaseFence) -> String {
    format!(
        "host_preparation_wait:{}:{}",
        fence.job_id, fence.lease_owner
    )
}

pub(super) fn register(
    client: &mut Client,
    host_id: &str,
    fence: &RuntimeJobLeaseFence,
) -> Result<(), String> {
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    let changed = tx.execute(
        "UPDATE runtime.runtime_jobs j SET last_error=$3 WHERE j.job_id=$1 AND j.lease_owner=$2 AND j.status IN('leased','running') AND j.lease_purpose='execute' AND j.lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint AND EXISTS(SELECT 1 FROM runtime.resident_capacity_waiters w WHERE w.job_id=j.job_id AND w.lease_owner=$2)",
        &[&fence.job_id, &fence.lease_owner, &HOST_PREPARATION_WAIT],
    ).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("host_preparation_wait_lease_rejected".into());
    }
    let payload = serde_json::json!({
        "schema":"runtime.host_preparation.waiting.v1",
        "hostId":host_id,"jobId":fence.job_id,"leaseOwner":fence.lease_owner,"scope":"globalPreparation",
    })
    .to_string();
    tx.execute(
        "INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) SELECT $1,COALESCE(session_id,'runtime_job:'||job_id),job_id,'runtime.host_preparation.waiting.v1',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,'internal',$4 FROM runtime.runtime_jobs WHERE job_id=$2 AND lease_owner=$3 ON CONFLICT(event_id) DO NOTHING",
        &[&event_id(fence), &fence.job_id, &fence.lease_owner, &payload],
    ).map_err(|e|e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

pub(super) fn clear(client: &mut Client, fence: &RuntimeJobLeaseFence) -> Result<(), String> {
    client.execute(
        "UPDATE runtime.runtime_jobs SET last_error=NULL WHERE job_id=$1 AND lease_owner=$2 AND status IN('leased','running') AND last_error=$3",
        &[&fence.job_id,&fence.lease_owner,&HOST_PREPARATION_WAIT],
    ).map(|_|()).map_err(|e|e.to_string())
}

pub(super) fn signal(client: &mut Client, host_id: &str) -> Result<(), String> {
    client.query(
        "WITH changed AS (UPDATE runtime.resident_capacity_clock SET generation=generation+1,changed_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE singleton AND EXISTS(SELECT 1 FROM runtime.resident_capacity_waiters w JOIN runtime.runtime_jobs j ON j.job_id=w.job_id JOIN runtime.runtime_events e ON e.event_id='host_preparation_wait:'||w.job_id||':'||w.lease_owner WHERE e.event_type='runtime.host_preparation.waiting.v1' AND COALESCE(e.payload_json::jsonb->>'hostId','')<>'' AND e.payload_json::jsonb->>'scope'='globalPreparation' AND e.payload_json::jsonb->>'jobId'=w.job_id AND e.payload_json::jsonb->>'leaseOwner'=w.lease_owner AND j.last_error=$2 AND ((j.status='queued' AND j.lease_owner IS NULL) OR (j.status IN('leased','running') AND j.lease_owner=w.lease_owner AND j.lease_purpose='execute' AND j.lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint))) RETURNING generation,changed_at_ms), recorded AS (INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) SELECT 'host_preparation_available:'||generation,'runtime_host_preparation:'||$1,$1,'runtime.host_preparation.available.v1',changed_at_ms,'internal',json_build_object('schema','runtime.host_preparation.available.v1','hostId',$1,'scope','globalPreparation','generation',generation,'availableAtMs',changed_at_ms)::text FROM changed ON CONFLICT(event_id) DO NOTHING RETURNING event_id) SELECT pg_notify('runtime_job_ready_v1','') FROM recorded",
        &[&host_id,&HOST_PREPARATION_WAIT],
    ).map(|_|()).map_err(|e|e.to_string())
}
