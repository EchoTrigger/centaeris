use crate::environment_authorization::EnvironmentBinding;
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
    pub allocation_agent_run_id: String,
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

pub(crate) fn bind(
    client: &mut postgres::Client,
    binding: &EnvironmentBinding,
    execution_id: &str,
    host_id: &str,
    image_digest: &str,
) -> Result<String, String> {
    if binding.controller.is_some() {
        return Err("retained execution requires controller admission".into());
    }
    let binding_digest = binding.digest()?;
    if execution_id.trim().is_empty()
        || host_id.trim().is_empty()
        || !image_digest.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err("execution instance identity invalid".into());
    }
    let mut tx = client.transaction().map_err(|e| format!("{e:?}"))?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    // Share residency::lock_host's database-wide admission key before taking
    // authority row locks. Its session lock spans retained reservation + Docker;
    // this transaction lock also reenters on the legacy prepare callback's Client.
    tx.query_one("SELECT pg_advisory_xact_lock(731946,3)", &[])
        .map_err(|e| format!("execution instance admission lock failed: {e}"))?;
    let original = tx.query_opt("SELECT a.environment_id,a.session_id,g.digest FROM public.app_core_agentrun a JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=a.id WHERE a.id=$1 FOR SHARE OF a,g", &[&binding.agent_run_id]).map_err(|e| format!("verify execution instance authority failed: {e}"))?;
    let Some(row) = original else {
        return Err("execution instance authority missing".into());
    };
    if row.get::<_, Option<String>>(0).as_deref() != Some(&binding.environment_id)
        || row.get::<_, String>(1) != binding.session_id
        || row.get::<_, String>(2) != binding.agent_run_authorization_digest
    {
        return Err("execution instance authority mismatch".into());
    }
    require_per_run_environment(&mut tx, &binding.environment_id)?;
    let instance_id = instance_id(execution_id);
    // Both execution_id and deterministic instance_id are unique. A concurrent
    // first insert can conflict with either index; compare the committed binding
    // after either conflict instead of treating the secondary index as a failure.
    tx.execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$1) ON CONFLICT DO NOTHING", &[&execution_id,&instance_id,&binding.environment_id,&binding.session_id,&binding.agent_run_id,&host_id,&binding.agent_run_authorization_digest,&binding_digest,&image_digest]).map_err(|e| format!("{e:?}"))?;
    let row = tx.query_opt("SELECT instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest FROM runtime.execution_instances WHERE execution_id=$1", &[&execution_id]).map_err(|e| format!("{e:?}"))?.ok_or("execution instance allocation identity collision")?;
    for (i, value) in [
        instance_id.as_str(),
        binding.environment_id.as_str(),
        binding.session_id.as_str(),
        binding.agent_run_id.as_str(),
        host_id,
        binding.agent_run_authorization_digest.as_str(),
        binding_digest.as_str(),
        image_digest,
    ]
    .iter()
    .enumerate()
    {
        if row.get::<_, String>(i) != *value {
            return Err("execution instance reassignment rejected".into());
        }
    }
    tx.commit().map_err(|e| format!("{e:?}"))?;
    Ok(instance_id)
}

// Callers hold the shared admission lock through this check and any reservation
// or binding insert, so a concurrent retained admission cannot change the mode.
pub(super) fn require_per_run_environment(
    client: &mut impl postgres::GenericClient,
    environment_id: &str,
) -> Result<(), String> {
    if client.query_one("SELECT EXISTS(SELECT 1 FROM runtime.execution_instances WHERE environment_id=$1 AND allocation_mode='retained')", &[&environment_id])
        .map_err(|e| format!("verify execution environment mode failed: {e}"))?.get::<_,bool>(0) {
        return Err("retained environment requires controller admission".into());
    }
    Ok(())
}
