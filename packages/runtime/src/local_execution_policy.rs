use centaeris_core::execution::{ExecutionPolicy, NetworkPolicy};
use std::path::{Path, PathBuf};

pub(crate) fn for_workspace(workspace: &Path, readonly: &[PathBuf]) -> ExecutionPolicy {
    let mut policy = ExecutionPolicy::workspace_write_public_internet(workspace);
    policy
        .filesystem
        .read_only_roots
        .extend_from_slice(readonly);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        // Actual product-owned state, never a reserved workspace directory.
        let private = crate::user_data_layout::execution_private_paths();
        policy.filesystem.denied_read_paths = private.clone();
        policy.filesystem.denied_write_paths = private;
    }
    policy.network = NetworkPolicy::PublicInternet;
    policy
}
