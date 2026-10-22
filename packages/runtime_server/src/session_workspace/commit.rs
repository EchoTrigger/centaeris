//! Existing immutable snapshot upload and uncertain-commit reconciliation.
use super::snapshot::{
    snapshot_matches_frozen_workspace, ImmutableWorkspaceSnapshot, SessionWorkspaceUploadBody,
};
use super::{SessionWorkspaceApi, SessionWorkspaceCommitOutcome, SessionWorkspaceLease};
use crate::agent_run_authorization::SessionWorkspace;
use crate::observations::ProtectionMetric;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::time::Duration;
const SESSION_WORKSPACE_COMMIT_SCHEMA: &str = "runtime.session_workspace.commit.v1";
const SESSION_WORKSPACE_COMMIT_RESULT_SCHEMA: &str = "runtime.session_workspace.commit.result.v1";
#[cfg(test)]
mod tests;

impl SessionWorkspaceApi<'_> {
    pub(crate) fn commit_snapshot(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        artifact: ImmutableWorkspaceSnapshot,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        let descriptor = &artifact.descriptor;
        if snapshot_matches_frozen_workspace(descriptor, frozen) {
            artifact
                .observation
                .count(ProtectionMetric::UploadAttempts, 0, true);
            artifact
                .observation
                .count(ProtectionMetric::UploadedBytes, 0, true);
            return Ok(SessionWorkspaceCommitOutcome::Unchanged);
        }
        let candidate = SessionWorkspace {
            generation: frozen
                .generation
                .checked_add(1)
                .ok_or_else(|| "session workspace generation overflow".to_string())?,
            snapshot_sha256: descriptor.sha256.clone(),
            snapshot_size_bytes: descriptor.size_bytes,
            expanded_size_bytes: descriptor.expanded_size_bytes,
            file_count: descriptor.file_count,
        };
        candidate.validate()?;
        let metadata = SessionWorkspaceCommitRequest {
            schema: SESSION_WORKSPACE_COMMIT_SCHEMA,
            job_id: lease.job_id.as_str(),
            lease_owner: lease.lease_owner.as_str(),
            agent_run_id: self.agent_run_id,
            authorization_digest: self.authorization_digest,
            snapshot_sha256: candidate.snapshot_sha256.as_str(),
            snapshot_size_bytes: candidate.snapshot_size_bytes,
            expanded_size_bytes: candidate.expanded_size_bytes,
            file_count: candidate.file_count,
        };
        let metadata = serde_json::to_vec(&metadata)
            .map_err(|error| format!("encode session workspace commit failed: {error}"))?;
        let metadata_length = u32::try_from(metadata.len())
            .map_err(|_| "session workspace commit metadata is too large".to_string())?;
        let mut prefix = Vec::with_capacity(4 + metadata.len());
        prefix.extend_from_slice(&metadata_length.to_be_bytes());
        prefix.extend_from_slice(metadata.as_slice());
        let observation = &artifact.observation;
        let length = u64::try_from(prefix.len())
            .map_err(|_| "session workspace commit length overflow".to_string())?
            .checked_add(descriptor.size_bytes)
            .ok_or_else(|| "session workspace commit length overflow".to_string())?;
        let response = observation.timed(ProtectionMetric::UploadElapsedMs, || {
            observation.count(ProtectionMetric::UploadAttempts, 1, true);
            if descriptor.size_bytes == 0 {
                self.client
                    .post(format!(
                        "{}/internal/agent-runs/session-workspace/commit",
                        self.api_url
                    ))
                    .header("X-Internal-Token", self.api_token)
                    .header("Content-Length", prefix.len().to_string())
                    .header("Content-Type", "application/octet-stream")
                    .body(reqwest::blocking::Body::sized(
                        observation.upload_reader(Cursor::new(prefix), length),
                        length,
                    ))
                    .timeout(Duration::from_secs(30))
                    .send()
            } else {
                let snapshot = artifact.file;
                let body = SessionWorkspaceUploadBody {
                    prefix: Cursor::new(prefix),
                    snapshot,
                };
                self.client
                    .post(format!(
                        "{}/internal/agent-runs/session-workspace/commit",
                        self.api_url
                    ))
                    .header("X-Internal-Token", self.api_token)
                    .header("Content-Length", length.to_string())
                    .header("Content-Type", "application/octet-stream")
                    .body(reqwest::blocking::Body::sized(
                        observation.upload_reader(body, length),
                        length,
                    ))
                    .timeout(Duration::from_secs(300))
                    .send()
            }
        });
        match response {
            Ok(response) if response.status().is_success() => {
                let committed = response
                    .json::<SessionWorkspaceCommitResponse>()
                    .map_err(|error| format!("decode session workspace commit failed: {error}"))?;
                if committed.schema != SESSION_WORKSPACE_COMMIT_RESULT_SCHEMA
                    || !matches!(committed.disposition.as_str(), "committed" | "idempotent")
                    || committed.session_workspace != candidate
                {
                    return Ok(SessionWorkspaceCommitOutcome::Rejected(
                        "session workspace commit response binding mismatch".to_string(),
                    ));
                }
                Ok(SessionWorkspaceCommitOutcome::Accepted)
            }
            Ok(response) if response.status().is_server_error() => {
                Ok(self.reconcile_commit(lease, frozen)?)
            }
            Ok(response) => Ok(SessionWorkspaceCommitOutcome::Rejected(format!(
                "session workspace commit returned {}",
                response.status().as_u16()
            ))),
            Err(_) => Ok(self.reconcile_commit(lease, frozen)?),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionWorkspaceCommitRequest<'a> {
    schema: &'a str,
    job_id: &'a str,
    lease_owner: &'a str,
    agent_run_id: &'a str,
    authorization_digest: &'a str,
    snapshot_sha256: &'a str,
    snapshot_size_bytes: u64,
    expanded_size_bytes: u64,
    file_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionWorkspaceCommitResponse {
    schema: String,
    disposition: String,
    session_workspace: SessionWorkspace,
}
