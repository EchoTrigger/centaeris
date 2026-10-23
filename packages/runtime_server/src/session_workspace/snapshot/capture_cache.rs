//! One bounded, verified manifest for conditional capture, never authority to skip validation.
use super::{ImmutableWorkspaceSnapshot, WorkspaceSnapshotDescriptor};
use hosted_execution::protocol::{SandboxSnapshotBaseline, SandboxWorkspaceSnapshotManifest};

#[derive(Clone)]
pub(crate) struct SnapshotCaptureCache {
    descriptor: WorkspaceSnapshotDescriptor,
    manifest: SandboxWorkspaceSnapshotManifest,
}

impl SnapshotCaptureCache {
    pub(crate) fn from_verified_snapshot(snapshot: &ImmutableWorkspaceSnapshot) -> Option<Self> {
        snapshot.manifest.as_ref().map(|manifest| Self {
            descriptor: snapshot.descriptor.clone(),
            manifest: manifest.clone(),
        })
    }

    pub(crate) fn matches_descriptor(&self, expected: &WorkspaceSnapshotDescriptor) -> bool {
        &self.descriptor == expected
    }

    pub(crate) fn baseline_for(
        &self,
        expected: &WorkspaceSnapshotDescriptor,
    ) -> Option<SandboxSnapshotBaseline> {
        self.matches_descriptor(expected)
            .then(|| SandboxSnapshotBaseline {
                snapshot_sha256: self.descriptor.sha256.clone(),
                manifest: self.manifest.clone(),
            })
    }
}
