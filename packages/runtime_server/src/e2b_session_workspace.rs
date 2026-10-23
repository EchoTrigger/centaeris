//! Per-Run workspace transfer through the existing Session workspace protocol.
//! The injected host port owns dispatch exclusion and current helper authority.
use crate::agent_run_authorization::{EnvironmentProfile, SessionWorkspace};
use crate::e2b_execution_host::E2bHostError;
use crate::observations::{ProtectionMetric, ProtectionTrigger, WorkspaceProtection};
use crate::session_workspace::snapshot::capture_cache::SnapshotCaptureCache;
use crate::session_workspace::snapshot::{
    capture_snapshot_artifact_observed, read_capture, snapshot_matches_frozen_workspace,
    SnapshotCaptureResult, WorkspaceSnapshotDescriptor, SESSION_WORKSPACE_MANIFEST_LIMIT,
};
use crate::session_workspace::{
    SessionWorkspaceApi, SessionWorkspaceApiError, SessionWorkspaceCommitOutcome,
    SessionWorkspaceLease, SessionWorkspaceLeaseRequest, SessionWorkspaceResolution,
    SESSION_WORKSPACE_DOWNLOAD_SCHEMA,
};
use hosted_execution::protocol::{SandboxSnapshotCaptureRequest, SANDBOX_SNAPSHOT_CAPTURE_SCHEMA};
use std::fs::File;
use std::io::Seek;
use std::sync::Arc;
use std::time::{Duration, Instant};

// API transfer admission reserves this overhead independently of any guest
// filesystem quota. Snapshot framing is validated by the shared wire reader.
const RESTORE_OVERHEAD_BYTES: u64 = 64 * 1024;
const CAPTURE_RESPONSE_LIMIT: u64 = 1024 + 4;

/// Acquiring a session must exclude new ordinary dispatch and reject unresolved
/// mutating helper effects. The guard retains that exclusion until it is dropped.
pub(crate) trait WorkspaceHelperPort: Send + Sync {
    fn try_exclusive(&self) -> Result<Option<Box<dyn WorkspaceHelperSession + '_>>, E2bHostError>;
}

/// Only fixed, authorized helper operations are available under this guard.
/// Success means exit zero and a fully received helper result. This is neither
/// VM cleanup proof nor permission to resume an environment.
pub(crate) trait WorkspaceHelperSession {
    /// Consume before any capture side effect; failures cannot retain a stale entry.
    fn take_capture_baseline(
        &self,
        expected: &WorkspaceSnapshotDescriptor,
        profile_digest: &str,
    ) -> Result<Option<SnapshotCaptureCache>, E2bHostError>;
    fn set_capture_baseline(
        &self,
        entry: Option<SnapshotCaptureCache>,
        profile_digest: &str,
    ) -> Result<(), E2bHostError>;
    fn require_quiescent(&self) -> Result<(), E2bHostError>;
    fn quiesce(&self) -> Result<(), E2bHostError>;
    fn restore_snapshot(
        &self,
        input: &mut File,
        descriptor: &WorkspaceSnapshotDescriptor,
    ) -> Result<(), E2bHostError>;
    fn capture_snapshot(
        &self,
        request: &SandboxSnapshotCaptureRequest,
        output_limit: u64,
        observation: &WorkspaceProtection,
    ) -> Result<File, E2bHostError>;
}

pub(crate) struct E2bSessionWorkspace {
    profile: EnvironmentProfile,
    client: reqwest::blocking::Client,
    api_url: String,
    api_token: String,
    agent_run_id: String,
    authorization_digest: String,
    helpers: Arc<dyn WorkspaceHelperPort>,
}

impl E2bSessionWorkspace {
    pub(crate) fn new(
        profile: EnvironmentProfile,
        api: SessionWorkspaceApi<'_>,
        helpers: Arc<dyn WorkspaceHelperPort>,
    ) -> Result<Self, String> {
        profile.validate()?;
        let EnvironmentProfile::E2b(value) = &profile else {
            return Err("e2b_workspace_profile_unsupported".into());
        };
        if !matches!(value.lifetime.as_str(), "perRun" | "retained")
            || value.workspace_root != "/mnt/data"
            || value.user != "agent"
        {
            return Err("e2b_workspace_profile_unsupported".into());
        }
        Ok(Self {
            profile,
            client: api.client.clone(),
            api_url: api.api_url.into(),
            api_token: api.api_token.into(),
            agent_run_id: api.agent_run_id.into(),
            authorization_digest: api.authorization_digest.into(),
            helpers,
        })
    }

    fn api(&self) -> SessionWorkspaceApi<'_> {
        SessionWorkspaceApi {
            client: &self.client,
            api_url: &self.api_url,
            api_token: &self.api_token,
            agent_run_id: &self.agent_run_id,
            authorization_digest: &self.authorization_digest,
        }
    }

    fn transfer_limit(&self) -> u64 {
        let EnvironmentProfile::E2b(profile) = &self.profile else {
            unreachable!("constructor validates the provider profile")
        };
        profile.input_budget_bytes
    }

    fn require_transfer_budget(&self, expanded: u64, inputs: u64) -> Result<(), String> {
        if expanded
            .checked_add(inputs)
            .and_then(|size| size.checked_add(RESTORE_OVERHEAD_BYTES))
            .is_none_or(|size| size > self.transfer_limit())
        {
            return Err("session workspace exceeds inputBudgetBytes transfer admission".into());
        }
        Ok(())
    }

    fn snapshot_limit(&self) -> u64 {
        // The validated profile is at most Core's 64 MiB input budget.
        self.transfer_limit() + SESSION_WORKSPACE_MANIFEST_LIMIT as u64 + 4
    }

    pub(crate) fn resolve(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        self.api().resolve(lease, frozen)
    }

    pub(crate) fn restore(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        input_upper_bound_bytes: u64,
    ) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
        let Some(session) = self.helpers.try_exclusive().map_err(restore_helper_error)? else {
            return Err(SessionWorkspaceApiError::Unavailable(
                "e2b_workspace_exclusion_busy".into(),
            ));
        };
        let profile_digest = self
            .profile
            .digest()
            .map_err(SessionWorkspaceApiError::Rejected)?;
        // Invalidate before validating or applying a new restore. Only a fully
        // verified frame and successful fixed helper application seed a baseline.
        session
            .set_capture_baseline(None, &profile_digest)
            .map_err(restore_helper_error)?;
        frozen
            .validate()
            .map_err(SessionWorkspaceApiError::Rejected)?;
        let resolution = self.api().resolve(lease, frozen)?;
        if resolution == SessionWorkspaceResolution::Advanced {
            return Ok(resolution);
        }
        self.require_transfer_budget(frozen.expanded_size_bytes, input_upper_bound_bytes)
            .map_err(SessionWorkspaceApiError::Rejected)?;
        match resolution {
            SessionWorkspaceResolution::Empty => return Ok(resolution),
            SessionWorkspaceResolution::Download => {}
            _ => {
                return Err(SessionWorkspaceApiError::Rejected(
                    "e2b_workspace_resolution_unsupported".into(),
                ))
            }
        }
        if frozen.snapshot_size_bytes > self.snapshot_limit() {
            return Err(SessionWorkspaceApiError::Rejected(
                "e2b_workspace_snapshot_transfer_limit_exceeded".into(),
            ));
        }
        let mut response = self
            .client
            .post(format!(
                "{}/internal/agent-runs/session-workspace/download",
                self.api_url
            ))
            .header("X-Internal-Token", &self.api_token)
            .json(&SessionWorkspaceLeaseRequest::new(
                SESSION_WORKSPACE_DOWNLOAD_SCHEMA,
                &self.agent_run_id,
                &self.authorization_digest,
                lease,
            ))
            .timeout(Duration::from_secs(30))
            .send()
            .map_err(|_| {
                SessionWorkspaceApiError::Unavailable("e2b_workspace_download_unavailable".into())
            })?;
        if response.status().is_server_error() {
            return Err(SessionWorkspaceApiError::Unavailable(
                "e2b_workspace_download_unavailable".into(),
            ));
        }
        if !response.status().is_success()
            || response.content_length() != Some(frozen.snapshot_size_bytes)
            || response
                .headers()
                .get("x-content-sha256")
                .and_then(|value| value.to_str().ok())
                != Some(frozen.snapshot_sha256.as_str())
        {
            return Err(SessionWorkspaceApiError::Rejected(
                "e2b_workspace_download_binding_mismatch".into(),
            ));
        }
        let observation = WorkspaceProtection::new().with_context(
            ProtectionTrigger::SessionRestore,
            &self.agent_run_id,
            Some(&lease.job_id),
        );
        // No helper receives data until the entire canonical snapshot, all file
        // hashes and the frozen outer descriptor have been verified in this spool.
        let mut artifact = observation
            .download(&mut response, |source| {
                capture_snapshot_artifact_observed(
                    source,
                    self.snapshot_limit(),
                    observation.clone(),
                )
            })
            .map_err(SessionWorkspaceApiError::Rejected)?;
        if !snapshot_matches_frozen_workspace(&artifact.descriptor, frozen) {
            return Err(SessionWorkspaceApiError::Rejected(
                "e2b_workspace_snapshot_binding_mismatch".into(),
            ));
        }
        session.require_quiescent().map_err(restore_helper_error)?;
        observation
            .timed(ProtectionMetric::RestoreApplyElapsedMs, || {
                session.restore_snapshot(&mut artifact.file, &artifact.descriptor)
            })
            .map_err(restore_helper_error)?;
        session
            .set_capture_baseline(
                SnapshotCaptureCache::from_verified_snapshot(&artifact),
                &profile_digest,
            )
            .map_err(restore_helper_error)?;
        Ok(resolution)
    }

    pub(crate) fn collect_and_commit(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        input_upper_bound_bytes: u64,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        let observation = WorkspaceProtection::new().with_context(
            ProtectionTrigger::TerminalCommit,
            &self.agent_run_id,
            Some(&lease.job_id),
        );
        let started = Instant::now();
        let mut already_advanced = false;
        let result = self.collect_and_commit_observed(
            lease,
            frozen,
            input_upper_bound_bytes,
            &observation,
            &mut already_advanced,
        );
        let disposition = match &result {
            Ok(SessionWorkspaceCommitOutcome::Accepted) if already_advanced => "alreadyAdvanced",
            Ok(SessionWorkspaceCommitOutcome::Accepted) => "full",
            Ok(SessionWorkspaceCommitOutcome::Unchanged) => "unchanged",
            Ok(
                SessionWorkspaceCommitOutcome::Pending
                | SessionWorkspaceCommitOutcome::ProtectionDeferred,
            ) => "pending",
            Ok(SessionWorkspaceCommitOutcome::Rejected(_)) | Err(_) => "rejected",
        };
        observation.elapsed(
            ProtectionMetric::WorkspaceCommitElapsedMs,
            started.elapsed(),
            matches!(
                &result,
                Ok(SessionWorkspaceCommitOutcome::Accepted
                    | SessionWorkspaceCommitOutcome::Unchanged)
            ),
        );
        observation.workspace_commit_disposition(disposition);
        result
    }

    fn collect_and_commit_observed(
        &self,
        lease: &SessionWorkspaceLease,
        frozen: &SessionWorkspace,
        input_upper_bound_bytes: u64,
        observation: &WorkspaceProtection,
        already_advanced: &mut bool,
    ) -> Result<SessionWorkspaceCommitOutcome, String> {
        if let Err(reason) = frozen.validate() {
            return Ok(SessionWorkspaceCommitOutcome::Rejected(reason));
        }
        match self.api().resolve(lease, frozen) {
            Ok(SessionWorkspaceResolution::Advanced) => {
                *already_advanced = true;
                return Ok(SessionWorkspaceCommitOutcome::Accepted);
            }
            Ok(SessionWorkspaceResolution::Empty | SessionWorkspaceResolution::Download) => {}
            Ok(SessionWorkspaceResolution::RetainedLive) => {
                return Ok(SessionWorkspaceCommitOutcome::Rejected(
                    "e2b_workspace_resolution_unsupported".into(),
                ))
            }
            Err(SessionWorkspaceApiError::Unavailable(_)) => {
                eprintln!("e2b_workspace_protection: stage=resolve; code=api_unavailable");
                return Ok(SessionWorkspaceCommitOutcome::Pending);
            }
            Err(SessionWorkspaceApiError::Rejected(reason)) => {
                return Ok(SessionWorkspaceCommitOutcome::Rejected(reason))
            }
        }
        if let Err(reason) = self.require_transfer_budget(0, input_upper_bound_bytes) {
            return Ok(SessionWorkspaceCommitOutcome::Rejected(reason));
        }
        let session = match self.helpers.try_exclusive() {
            Ok(Some(session)) => session,
            Ok(None) => {
                eprintln!("e2b_workspace_protection: stage=exclusive; code=activity_busy");
                return Ok(SessionWorkspaceCommitOutcome::Pending);
            }
            Err(error) => return Ok(capture_helper_error(error, "exclusive")),
        };
        let profile_digest = self.profile.digest()?;
        let expected = WorkspaceSnapshotDescriptor::from(frozen);
        let previous = match session.take_capture_baseline(&expected, &profile_digest) {
            Ok(previous) => previous,
            Err(error) => return Ok(capture_helper_error(error, "capture")),
        };
        if let Err(error) = session.quiesce() {
            return Ok(capture_helper_error(error, "quiesce"));
        }
        let request = SandboxSnapshotCaptureRequest {
            schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
            capture_id: format!("capture-{}", uuid::Uuid::new_v4()),
            baseline: previous
                .as_ref()
                .and_then(|entry| entry.baseline_for(&expected)),
        };
        let output_limit = self.snapshot_limit() + CAPTURE_RESPONSE_LIMIT;
        let mut output = match observation.timed(ProtectionMetric::CaptureElapsedMs, || {
            session.capture_snapshot(&request, output_limit, observation)
        }) {
            Ok(output) => output,
            Err(error) => return Ok(capture_helper_error(error, "capture")),
        };
        if output
            .metadata()
            .map_err(|_| "e2b_workspace_capture_spool_unavailable")?
            .len()
            > output_limit
        {
            return Ok(SessionWorkspaceCommitOutcome::Rejected(
                "e2b_workspace_capture_transfer_limit_exceeded".into(),
            ));
        }
        output
            .rewind()
            .map_err(|_| "e2b_workspace_capture_spool_unavailable")?;
        let artifact = match read_capture(&mut output, &request, |source| {
            capture_snapshot_artifact_observed(source, self.snapshot_limit(), observation.clone())
        }) {
            Ok(SnapshotCaptureResult::Snapshot(artifact)) => artifact,
            Ok(SnapshotCaptureResult::Unchanged) => {
                if let Err(reason) = self
                    .require_transfer_budget(frozen.expanded_size_bytes, input_upper_bound_bytes)
                {
                    return Ok(SessionWorkspaceCommitOutcome::Rejected(reason));
                }
                observation.count(ProtectionMetric::SpoolWrittenBytes, 0, true);
                observation.count(
                    ProtectionMetric::SnapshotLogicalBytes,
                    frozen.expanded_size_bytes,
                    true,
                );
                observation.count(ProtectionMetric::UploadAttempts, 0, true);
                observation.count(ProtectionMetric::UploadedBytes, 0, true);
                if let Err(error) = session.set_capture_baseline(previous, &profile_digest) {
                    return Ok(capture_helper_error(error, "capture"));
                }
                return Ok(SessionWorkspaceCommitOutcome::Unchanged);
            }
            Err(reason) => return Ok(SessionWorkspaceCommitOutcome::Rejected(reason)),
        };
        if let Err(reason) = self.require_transfer_budget(
            artifact.descriptor.expanded_size_bytes,
            input_upper_bound_bytes,
        ) {
            return Ok(SessionWorkspaceCommitOutcome::Rejected(reason));
        }
        observation.count(
            ProtectionMetric::SnapshotLogicalBytes,
            artifact.descriptor.expanded_size_bytes,
            true,
        );
        let next = SnapshotCaptureCache::from_verified_snapshot(&artifact);
        let result = self.api().commit_snapshot(lease, frozen, artifact)?;
        if matches!(
            result,
            SessionWorkspaceCommitOutcome::Accepted | SessionWorkspaceCommitOutcome::Unchanged
        ) {
            if let Err(error) = session.set_capture_baseline(next, &profile_digest) {
                return Ok(capture_helper_error(error, "capture"));
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;

fn restore_helper_error(error: E2bHostError) -> SessionWorkspaceApiError {
    match error {
        E2bHostError::NotDispatched(reason) => SessionWorkspaceApiError::Rejected(reason.into()),
        E2bHostError::Unknown(reason) => SessionWorkspaceApiError::Unavailable(reason.into()),
    }
}

fn capture_helper_error(error: E2bHostError, stage: &'static str) -> SessionWorkspaceCommitOutcome {
    let code = match &error {
        E2bHostError::NotDispatched(code) | E2bHostError::Unknown(code) => *code,
    };
    eprintln!("e2b_workspace_protection: stage={stage}; code={code}");
    match error {
        E2bHostError::NotDispatched(reason) => {
            SessionWorkspaceCommitOutcome::Rejected(reason.into())
        }
        E2bHostError::Unknown(_) => SessionWorkspaceCommitOutcome::Pending,
    }
}
