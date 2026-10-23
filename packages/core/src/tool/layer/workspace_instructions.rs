//! Host-owned read access to the workspace instruction file used by Core prompts.
use std::path::Path;

use crate::execution::ExecutionFileSystemError;

/// Reads exactly AGENTS.md relative to the supplied workspace, without preparing
/// a prompt. The host must preserve authorization and version scope; it may use
/// an immutable startup source until execution is materialized, then live files.
/// Return NotFound only when that source proves the file is absent. Other errors
/// must remain errors, and successful bytes must be complete rather than truncated.
pub trait WorkspaceInstructionReadPort: Send + Sync {
    fn read_agents_file(
        &self,
        cwd: &Path,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ExecutionFileSystemError>;
}
