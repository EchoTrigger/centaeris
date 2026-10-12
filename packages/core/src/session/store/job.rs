//! Storage capabilities used by the complete asynchronous Subagent job chain.
//! Requests and decisions remain Core-owned; adapters own execution and SQL.
use super::{
    UpsertExternalContextAndScheduleJobRequest, UpsertExternalContextLinkAndCompleteJobRequest,
};
use crate::runtime::contracts::TimestampMs;
use crate::session::external_context::ExternalContextObject;
use crate::session::reliability::{
    CancelRuntimeJobRequest, ClaimDueRuntimeJobsRequest, CompleteRuntimeJobRequest,
    FailRuntimeJobRequest, ListRuntimeJobsRequest, RenewRuntimeJobLeaseRequest, RuntimeJobRecord,
    ScheduleRuntimeJobResult, StartRuntimeJobRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobStoreOperation {
    Read,
    Create,
    Claim,
    Start,
    Renew,
    Reclaim,
    Complete,
    Fail,
    Cancel,
    Acknowledge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobWriteOutcome {
    ReadOnly,
    NotExecuted,
    RolledBack,
    Committed,
    Unknown,
}

/// Storage uncertainty remains distinct from a Core business rejection.
/// A caller must establish actual work completion before reconciling an unknown
/// write through a healthy connection; a reply timeout is not that barrier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "kind",
    content = "detail",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum JobStoreError {
    #[error("{0}")]
    Rejected(String),
    #[error("{message}")]
    Storage {
        operation: JobStoreOperation,
        outcome: JobWriteOutcome,
        message: String,
    },
}

impl From<String> for JobStoreError {
    fn from(message: String) -> Self {
        Self::Rejected(message)
    }
}

impl JobStoreError {
    pub fn not_executed(operation: JobStoreOperation, message: String) -> Self {
        Self::Storage {
            operation,
            outcome: JobWriteOutcome::NotExecuted,
            message,
        }
    }
    /// String-based host boundaries retain the operation and write outcome.
    /// Display remains compatible with the original business error text.
    pub fn diagnostic_message(&self) -> String {
        match self {
            Self::Rejected(message) => message.clone(),
            Self::Storage {
                operation,
                outcome,
                message,
            } => {
                format!("{message} (operation={operation:?}, writeOutcome={outcome:?})")
            }
        }
    }
    pub fn requires_settlement(&self) -> bool {
        matches!(
            self,
            Self::Storage {
                outcome: JobWriteOutcome::Unknown,
                ..
            }
        )
    }
    pub fn committed(operation: JobStoreOperation, message: String) -> Self {
        Self::Storage {
            operation,
            outcome: JobWriteOutcome::Committed,
            message,
        }
    }
    pub fn with_message(self, replacement: String) -> Self {
        match self {
            Self::Rejected(_) => Self::Rejected(replacement),
            Self::Storage {
                operation, outcome, ..
            } => Self::Storage {
                operation,
                outcome,
                message: replacement,
            },
        }
    }
    pub fn unconfirmed(operation: JobStoreOperation, message: String) -> Self {
        Self::Storage {
            operation,
            outcome: if operation == JobStoreOperation::Read {
                JobWriteOutcome::ReadOnly
            } else {
                JobWriteOutcome::Unknown
            },
            message,
        }
    }
}

/// Accepted work may outlive the reply waiter. Implementations must not interpret
/// a lost response as permission to replay a write, or duplicate Core job policy.
#[async_trait::async_trait]
pub trait AsyncJobStorePort: Send + Sync {
    /// Complete previously accepted work and establish a healthy source for
    /// reconciliation. This is a completion barrier, never a write retry.
    async fn settle_job_operations(&self) -> Result<(), JobStoreError>;
    async fn get_runtime_job(
        &self,
        job_id: &str,
    ) -> Result<Option<RuntimeJobRecord>, JobStoreError>;
    async fn list_runtime_jobs(
        &self,
        req: ListRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, JobStoreError>;
    async fn claim_due_runtime_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
    ) -> Result<Vec<RuntimeJobRecord>, JobStoreError>;
    async fn start_runtime_job(&self, req: StartRuntimeJobRequest) -> Result<(), JobStoreError>;
    async fn renew_runtime_job_lease(
        &self,
        req: RenewRuntimeJobLeaseRequest,
    ) -> Result<(), JobStoreError>;
    async fn complete_runtime_job(
        &self,
        req: CompleteRuntimeJobRequest,
    ) -> Result<(), JobStoreError>;
    async fn fail_runtime_job(&self, req: FailRuntimeJobRequest) -> Result<(), JobStoreError>;
    async fn cancel_runtime_job(&self, req: CancelRuntimeJobRequest) -> Result<(), JobStoreError>;
    async fn reclaim_expired_runtime_job_leases(
        &self,
        now_ms: TimestampMs,
    ) -> Result<usize, JobStoreError>;
    async fn load_external_context_object(
        &self,
        object_id: &str,
    ) -> Result<Option<ExternalContextObject>, JobStoreError>;
    async fn upsert_external_context_and_schedule_job(
        &self,
        req: UpsertExternalContextAndScheduleJobRequest,
    ) -> Result<ScheduleRuntimeJobResult, JobStoreError>;
    async fn upsert_external_context_link_and_complete_job(
        &self,
        req: UpsertExternalContextLinkAndCompleteJobRequest,
    ) -> Result<(), JobStoreError>;
}
