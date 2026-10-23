//! Fixed workspace helpers and artifact frames for the current Execute owner.
//! Activity exclusion here is scoped to this Runtime process, never a DB fence.
use super::*;
use crate::artifact_publication::ArtifactReadPort;
use crate::e2b_adapter::bulk::MAX_TRANSFER_BYTES;
use crate::e2b_session_workspace::{WorkspaceHelperPort, WorkspaceHelperSession};
use crate::execution_recovery_policy::SnapshotActivityGuard;
use crate::observations::MeasuredWriter;
use crate::session_workspace::snapshot::{copy_exact, WorkspaceSnapshotDescriptor};
use hosted_execution::protocol::{
    SandboxArtifactMetadata, SandboxArtifactRequest, SandboxSnapshotCaptureRequest,
};
use serde::Deserialize;
use std::fs::File;
use std::io::{Seek, Write};

const HELPER_JSON_LIMIT: usize = 1024 * 1024;

struct HeldWorkspace<'a> {
    runner: &'a E2bExecutionHostRunner,
    _activity: SnapshotActivityGuard,
}

impl WorkspaceHelperPort for E2bExecutionHostRunner {
    fn try_exclusive(&self) -> Result<Option<Box<dyn WorkspaceHelperSession + '_>>, E2bHostError> {
        require_known(self)?;
        let Some(activity) = self
            .environment
            .activity
            .try_snapshot_activity()
            .map_err(|_| E2bHostError::Unknown("e2b_workspace_activity_unavailable"))?
        else {
            return Ok(None);
        };
        // A mutator may have finished between the first check and admission.
        // Its latch is set before it releases ordinary activity.
        require_known(self)?;
        Ok(Some(Box::new(HeldWorkspace {
            runner: self,
            _activity: activity,
        })))
    }
}

fn require_known(runner: &E2bExecutionHostRunner) -> Result<(), E2bHostError> {
    if !runner.helper_effects_known() {
        return Err(E2bHostError::Unknown("e2b_prior_helper_effects_unknown"));
    }
    Ok(())
}

impl HeldWorkspace<'_> {
    fn require_profile(&self, profile_digest: &str) -> Result<(), E2bHostError> {
        require_known(self.runner)?;
        if self
            .runner
            .binding
            .metadata
            .get("profileDigest")
            .map(String::as_str)
            != Some(profile_digest)
        {
            return Err(E2bHostError::NotDispatched(
                "e2b_workspace_profile_binding_mismatch",
            ));
        }
        Ok(())
    }
}

impl WorkspaceHelperSession for HeldWorkspace<'_> {
    fn take_capture_baseline(
        &self,
        expected: &WorkspaceSnapshotDescriptor,
        profile_digest: &str,
    ) -> Result<Option<SnapshotCaptureCache>, E2bHostError> {
        self.require_profile(profile_digest)?;
        let previous = self
            .runner
            .environment
            .snapshot_capture_cache
            .lock()
            .map_err(|_| E2bHostError::Unknown("e2b_workspace_cache_unavailable"))?
            .take();
        Ok(previous.filter(|entry| entry.matches_descriptor(expected)))
    }

    fn set_capture_baseline(
        &self,
        entry: Option<SnapshotCaptureCache>,
        profile_digest: &str,
    ) -> Result<(), E2bHostError> {
        self.require_profile(profile_digest)?;
        *self
            .runner
            .environment
            .snapshot_capture_cache
            .lock()
            .map_err(|_| E2bHostError::Unknown("e2b_workspace_cache_unavailable"))? = entry;
        Ok(())
    }

    fn require_quiescent(&self) -> Result<(), E2bHostError> {
        require_known(self.runner)?;
        let output = self.runner.invoke_authorized_helper(
            TrustedHelperKind::AgentProcessState,
            &[],
            HELPER_JSON_LIMIT,
        )?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct State {
            active_process_count: u64,
        }
        let state: State = serde_json::from_slice(&output)
            .map_err(|_| E2bHostError::Unknown("e2b_process_state_invalid"))?;
        if state.active_process_count != 0 {
            return Err(E2bHostError::NotDispatched("e2b_workspace_not_quiescent"));
        }
        Ok(())
    }

    fn quiesce(&self) -> Result<(), E2bHostError> {
        require_known(self.runner)?;
        let output = self.runner.invoke_authorized_helper(
            TrustedHelperKind::QuiesceAgentProcesses,
            &[],
            HELPER_JSON_LIMIT,
        )?;
        if !output.is_empty() {
            return Err(E2bHostError::Unknown("e2b_quiescence_output_invalid"));
        }
        self.require_quiescent()
    }

    fn restore_snapshot(
        &self,
        input: &mut File,
        descriptor: &WorkspaceSnapshotDescriptor,
    ) -> Result<(), E2bHostError> {
        require_known(self.runner)?;
        if descriptor.size_bytes == 0 || descriptor.size_bytes > MAX_TRANSFER_BYTES {
            return Err(E2bHostError::NotDispatched("e2b_restore_transfer_limit"));
        }
        let mut bytes = Vec::new();
        copy_exact(input, &mut bytes, descriptor.size_bytes, &descriptor.sha256)
            .map_err(|_| E2bHostError::NotDispatched("e2b_restore_input_invalid"))?;
        let output = self.runner.invoke_authorized_helper(
            TrustedHelperKind::SnapshotRestore,
            &bytes,
            HELPER_JSON_LIMIT,
        )?;
        if !output.is_empty() {
            self.runner.latch_unknown_helper_effects();
            return Err(E2bHostError::Unknown("e2b_restore_output_invalid"));
        }
        Ok(())
    }

    fn capture_snapshot(
        &self,
        request: &SandboxSnapshotCaptureRequest,
        output_limit: u64,
        observation: &WorkspaceProtection,
    ) -> Result<File, E2bHostError> {
        require_known(self.runner)?;
        if output_limit == 0 || output_limit > MAX_TRANSFER_BYTES {
            return Err(E2bHostError::NotDispatched("e2b_capture_transfer_limit"));
        }
        let input = serde_json::to_vec(request)
            .map_err(|_| E2bHostError::NotDispatched("e2b_capture_request_invalid"))?;
        let output = self.runner.invoke_authorized_helper_observed(
            TrustedHelperKind::SnapshotCapture,
            &input,
            output_limit as usize,
            Some(observation),
        )?;
        // The Session workspace service validates this exact frame with the
        // shared read_capture codec before committing any snapshot.
        let mut file = tempfile::tempfile().map_err(|_| {
            observation.count(ProtectionMetric::RawCaptureSpoolWrittenBytes, 0, false);
            E2bHostError::Unknown("e2b_capture_spool_unavailable")
        })?;
        write_raw_capture_frame(&mut file, &output, observation)
            .and_then(|()| file.rewind())
            .map_err(|_| E2bHostError::Unknown("e2b_capture_spool_unavailable"))?;
        Ok(file)
    }
}

/// Count only successful writes, including those preceding a later I/O error.
fn write_raw_capture_frame(
    target: &mut impl Write,
    bytes: &[u8],
    observation: &WorkspaceProtection,
) -> std::io::Result<()> {
    let mut measured = MeasuredWriter {
        inner: target,
        written: 0,
    };
    let result = measured.write_all(bytes);
    observation.count(
        ProtectionMetric::RawCaptureSpoolWrittenBytes,
        measured.written,
        result.is_ok(),
    );
    result
}

impl ArtifactReadPort for E2bExecutionHostRunner {
    fn read_artifact(&self, path: &str) -> Result<(SandboxArtifactMetadata, Vec<u8>), String> {
        let request = serde_json::to_vec(&SandboxArtifactRequest {
            path: path.into(),
            max_bytes: MAX_PUBLISHED_ARTIFACT_BYTES,
        })
        .map_err(|_| "e2b_artifact_request_invalid")?;
        let output = self
            .invoke_helper(
                TrustedHelperKind::ReadArtifact,
                &request,
                MAX_PUBLISHED_ARTIFACT_BYTES as usize + HELPER_JSON_LIMIT + 4,
            )
            .map_err(|_| "e2b_artifact_helper_unavailable")?;
        let length: [u8; 4] = output
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or("e2b_artifact_frame_invalid")?;
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > HELPER_JSON_LIMIT || output.len() < 4 + length {
            return Err("e2b_artifact_frame_invalid".into());
        }
        let metadata: SandboxArtifactMetadata = serde_json::from_slice(&output[4..4 + length])
            .map_err(|_| "e2b_artifact_frame_invalid")?;
        let bytes = &output[4 + length..];
        if metadata.size_bytes != bytes.len() as u64
            || metadata.size_bytes > MAX_PUBLISHED_ARTIFACT_BYTES
            || metadata.sha256 != format!("sha256:{:x}", Sha256::digest(bytes))
        {
            return Err("e2b_artifact_integrity_mismatch".into());
        }
        Ok((metadata, bytes.to_vec()))
    }
}

#[cfg(test)]
mod tests;
