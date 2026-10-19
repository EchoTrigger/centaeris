use sha2::{Digest, Sha256};

pub(crate) struct RetainedInstanceRequest<'a> {
    pub environment_authorization: &'a crate::environment_authorization::EnvironmentAuthorization,
    pub signing_key: &'a [u8],
    pub execution_id: &'a str,
    pub workspace_id: &'a str,
    pub host_id: &'a str,
    pub image_digest: &'a str,
    pub profile_digest: &'a str,
    pub resources: centaeris_core::execution::ResidentResourceUsage,
    pub lease: &'a centaeris_core::session::RuntimeJobLeaseFence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetainedInstanceAllocation {
    pub allocation_execution_id: String,
    pub allocation_agent_run_id: Option<String>,
    pub allocation_owner: centaeris_core::execution::EnvironmentTaskOwner,
    pub instance_id: String,
    pub environment_id: String,
    pub host_id: String,
    pub image_digest: String,
    pub authorization_digest: String,
    pub binding_digest: String,
    pub profile_digest: String,
    pub container_id: Option<String>,
    pub reused: bool,
}

// One Core execution has one allocation identity, including lost receipts.
// Recovery creates a new execution; this is never cross-Run reuse.
pub(crate) fn instance_id(execution_id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"workspace:execution-instance:v1\0");
    hash.update(execution_id.as_bytes());
    format!("instance_{:x}", hash.finalize())
}
