use centaeris_core::execution::{ExecutionPolicy, NetworkPolicy};
use std::path::{Path, PathBuf};

pub(crate) fn for_workspace(workspace: &Path, readonly: &[PathBuf]) -> ExecutionPolicy {
    let mut policy = ExecutionPolicy::workspace_write_public_internet(workspace);
    policy
        .filesystem
        .read_only_roots
        .extend_from_slice(readonly);
    policy.network = NetworkPolicy::PublicInternet;
    policy
}
