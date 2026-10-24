//! Session workspace API contract shared by execution providers.
//! Provider adapters own capture/quiescence; this boundary only transfers the
//! existing authorization, resolution and uncertain-commit protocol.
use crate::agent_run_authorization::SessionWorkspace;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(crate) const SESSION_WORKSPACE_RESOLVE_SCHEMA: &str = "runtime.session_workspace.resolve.v1";
pub(crate) const SESSION_WORKSPACE_RESOLVED_SCHEMA: &str = "runtime.session_workspace.resolved.v1";
pub(crate) const SESSION_WORKSPACE_DOWNLOAD_SCHEMA: &str = "runtime.session_workspace.download.v1";

#[derive(Debug, Clone)]
pub(crate) struct SessionWorkspaceLease {
    pub job_id: String,
    pub lease_owner: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionWorkspaceResolution {
    RetainedLive,
    Empty,
    Download,
    Advanced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionWorkspaceApiError {
    Rejected(String),
    Unavailable(String),
}

/// Files remain referenced by the durable Environment binding and original Run
/// authorization. This receipt is not an archive generation or snapshot commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetainedProviderWorkspaceBinding {
    pub environment_id: String,
    pub instance_id: String,
    pub sandbox_id: String,
    pub profile_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionWorkspaceCommitOutcome {
    ProtectionDeferred,
    RetainedProvider(RetainedProviderWorkspaceBinding),
    Accepted,
    Unchanged,
    Rejected(String),
    Pending,
}

pub(crate) struct SessionWorkspaceApi<'a> {
    pub client: &'a reqwest::blocking::Client,
    pub api_url: &'a str,
    pub api_token: &'a str,
    pub agent_run_id: &'a str,
    pub authorization_digest: &'a str,
}

impl SessionWorkspaceApi<'_> {
    pub(crate) fn resolve(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        let response = self
            .client
            .post(format!(
                "{}/internal/agent-runs/session-workspace/resolve",
                self.api_url
            ))
            .header("X-Internal-Token", self.api_token)
            .json(&SessionWorkspaceLeaseRequest::new(
                SESSION_WORKSPACE_RESOLVE_SCHEMA,
                self.agent_run_id,
                self.authorization_digest,
                lease,
            ))
            .timeout(Duration::from_secs(10))
            .send()
            .map_err(|error| {
                SessionWorkspaceApiError::Unavailable(format!(
                    "resolve session workspace failed: {error}"
                ))
            })?;
        if response.status().is_server_error() {
            return Err(SessionWorkspaceApiError::Unavailable(format!(
                "resolve session workspace returned {}",
                response.status().as_u16()
            )));
        }
        if !response.status().is_success() {
            return Err(SessionWorkspaceApiError::Rejected(format!(
                "resolve session workspace returned {}",
                response.status().as_u16()
            )));
        }
        let resolved = response
            .json::<SessionWorkspaceResolveResponse>()
            .map_err(|error| {
                SessionWorkspaceApiError::Rejected(format!(
                    "decode session workspace resolve failed: {error}"
                ))
            })?;
        resolved_resolution(&resolved, frozen)
    }

    pub(crate) fn reconcile_commit(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        match self.resolve(lease, frozen) {
            Ok(SessionWorkspaceResolution::RetainedLive) => {
                Ok(SessionWorkspaceCommitOutcome::ProtectionDeferred)
            }
            Ok(SessionWorkspaceResolution::Advanced) => Ok(SessionWorkspaceCommitOutcome::Accepted),
            Ok(SessionWorkspaceResolution::Empty | SessionWorkspaceResolution::Download) => {
                Ok(SessionWorkspaceCommitOutcome::Rejected(
                    "session workspace commit was not accepted".to_string(),
                ))
            }
            Err(SessionWorkspaceApiError::Unavailable(_)) => {
                Ok(SessionWorkspaceCommitOutcome::Pending)
            }
            Err(SessionWorkspaceApiError::Rejected(reason)) => {
                Ok(SessionWorkspaceCommitOutcome::Rejected(reason))
            }
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionWorkspaceLeaseRequest<'a> {
    pub(crate) schema: &'a str,
    pub(crate) job_id: &'a str,
    pub(crate) lease_owner: &'a str,
    pub(crate) agent_run_id: &'a str,
    pub(crate) authorization_digest: &'a str,
}

impl<'a> SessionWorkspaceLeaseRequest<'a> {
    pub(crate) fn new(
        schema: &'a str,
        agent_run_id: &'a str,
        authorization_digest: &'a str,
        lease: &'a SessionWorkspaceLease,
    ) -> Self {
        Self {
            schema,
            job_id: lease.job_id.as_str(),
            lease_owner: lease.lease_owner.as_str(),
            agent_run_id,
            authorization_digest,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SessionWorkspaceResolveResponse {
    pub(crate) schema: String,
    pub(crate) disposition: String,
    pub(crate) session_workspace: SessionWorkspace,
}

pub(crate) fn resolved_resolution(
    resolved: &SessionWorkspaceResolveResponse,
    frozen: &SessionWorkspace,
) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
    if resolved.schema != SESSION_WORKSPACE_RESOLVED_SCHEMA {
        return Err(SessionWorkspaceApiError::Rejected(
            "session workspace resolve schema mismatch".into(),
        ));
    }
    match resolved.disposition.as_str() {
        "empty" if &resolved.session_workspace == frozen && frozen.snapshot_size_bytes == 0 => {
            Ok(SessionWorkspaceResolution::Empty)
        }
        "download" if &resolved.session_workspace == frozen && frozen.snapshot_size_bytes != 0 => {
            Ok(SessionWorkspaceResolution::Download)
        }
        "advanced"
            if Some(resolved.session_workspace.generation) == frozen.generation.checked_add(1)
                && resolved.session_workspace.validate().is_ok() =>
        {
            Ok(SessionWorkspaceResolution::Advanced)
        }
        _ => Err(SessionWorkspaceApiError::Rejected(
            "session workspace resolve binding mismatch".into(),
        )),
    }
}

pub(crate) mod snapshot;

mod commit;
