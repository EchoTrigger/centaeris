use centaeris_core::session::reliability::RenewRuntimeJobLeaseRequest;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::PostgresRuntimeStore;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerLeaseRenewalError {
    FenceRejected,
    InvalidRequest,
    Unavailable {
        stage: &'static str,
        sqlstate: Option<String>,
    },
}

impl From<String> for WorkerLeaseRenewalError {
    fn from(_: String) -> Self {
        Self::Unavailable {
            stage: "checkout",
            sqlstate: None,
        }
    }
}

impl PostgresRuntimeStore {
    pub(crate) fn renew_worker_job_lease(
        &self,
        request: RenewRuntimeJobLeaseRequest,
    ) -> Result<(), WorkerLeaseRenewalError> {
        if !(1000..=300000).contains(&request.lease_ms) {
            return Err(WorkerLeaseRenewalError::InvalidRequest);
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let attempt = NEXT.fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let lease_ms = request.lease_ms as i64;
        let result = self.with_execution_control_client_error(|client| {
            observe(&request, attempt, started, "checkout", "ready", None, None);
            let mut tx = client.transaction().map_err(|error| storage_failure("begin", error))?;
            // Take the row lock first. Client timestamps and a predicate evaluated
            // before a lock wait cannot revive expired execution authority.
            let row=tx.query_opt("SELECT job_id FROM runtime.runtime_jobs WHERE job_id=$1 FOR UPDATE", &[&request.job_id])
                .map_err(|error| storage_failure("lock", error))?;
            observe(&request, attempt, started, "lock", "ready", None, None);
            if row.is_none() { return Err(WorkerLeaseRenewalError::FenceRejected); }
            let renewed=tx.query_opt(
                "WITH observed AS MATERIALIZED (SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint AS now_ms) UPDATE runtime.runtime_jobs j SET heartbeat_at_ms=o.now_ms,lease_expires_at_ms=o.now_ms+$3,updated_at_ms=o.now_ms FROM observed o WHERE j.job_id=$1 AND j.status IN('leased','running') AND j.lease_owner=$2 AND j.lease_expires_at_ms>o.now_ms RETURNING j.lease_expires_at_ms",
                &[&request.job_id,&request.lease_owner,&lease_ms],
            ).map_err(|error| storage_failure("renew",error))?
                .ok_or(WorkerLeaseRenewalError::FenceRejected)?;
            let expires:i64=renewed.get(0);
            tx.commit().map_err(|error| storage_failure("commit",error))?;
            observe(&request,attempt,started,"commit","renewed",None,Some(expires));
            Ok(())
        });
        if let Err(error) = &result {
            let (stage, outcome, code) = match error {
                WorkerLeaseRenewalError::FenceRejected => ("verify", "fenceRejected", None),
                WorkerLeaseRenewalError::InvalidRequest => ("decode", "invalidRequest", None),
                WorkerLeaseRenewalError::Unavailable { stage, sqlstate } => {
                    (*stage, "unavailable", sqlstate.as_deref())
                }
            };
            observe(&request, attempt, started, stage, outcome, code, None);
        }
        result
    }
}

fn storage_failure(stage: &'static str, error: postgres::Error) -> WorkerLeaseRenewalError {
    WorkerLeaseRenewalError::Unavailable {
        stage,
        sqlstate: error.code().map(|code| code.code().to_owned()),
    }
}

fn observe(
    request: &RenewRuntimeJobLeaseRequest,
    attempt: u64,
    started: Instant,
    stage: &str,
    outcome: &str,
    sqlstate: Option<&str>,
    expires: Option<i64>,
) {
    if !crate::observations::enabled() {
        return;
    }
    let event = serde_json::json!({"schema":"workspace.perf.v1","source":"runtime","phase":"leaseHeartbeat","attemptId":attempt,"atUnixMs":crate::now_ms().ok(),"jobId":request.job_id,"leaseOwnerSha256":format!("{:x}",Sha256::digest(request.lease_owner.as_bytes())),"stage":stage,"outcome":outcome,"sqlstate":sqlstate,"elapsedMs":started.elapsed().as_millis(),"leaseExpiresAtMs":expires});
    let _ = writeln!(std::io::stderr().lock(), "{event}");
}
