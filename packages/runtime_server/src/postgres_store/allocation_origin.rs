//! Immutable allocation provenance is independent of the current controller owner.
use crate::agent_run_authorization::{EnvironmentProfile, WorkspaceAgentRunAuthorization};
use crate::environment_authorization::provision::ProvisionAuthorization;
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;
use centaeris_core::execution::EnvironmentTaskOwner;
use centaeris_core::extension::PluginActivationSnapshotV1;
use postgres::GenericClient;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AllocationOrigin {
    pub workspace_id: String,
    pub session_id: String,
    pub user_id: String,
    pub agent_id: String,
    pub environment_profile: EnvironmentProfile,
    pub plugin_activation: PluginActivationSnapshotV1,
}
impl AllocationOrigin {
    pub(crate) fn effective_network(
        &self,
    ) -> Result<crate::agent_run_authorization::ExecutionNetwork, String> {
        self.environment_profile.validate()?;
        Ok(self.environment_profile.network())
    }
}
impl From<WorkspaceAgentRunAuthorization> for AllocationOrigin {
    fn from(a: WorkspaceAgentRunAuthorization) -> Self {
        Self {
            workspace_id: a.workspace_id,
            session_id: a.session_id,
            user_id: a.user_id,
            agent_id: a.agent_id,
            environment_profile: a.environment_profile,
            plugin_activation: a.plugin_activation,
        }
    }
}
pub(super) fn load(
    db: &mut impl GenericClient,
    a: &RetainedInstanceAllocation,
    key: &[u8],
) -> Result<AllocationOrigin, String> {
    let origin = match &a.allocation_owner {
        EnvironmentTaskOwner::AgentRun { run_id } => {
            if a.allocation_agent_run_id.as_deref() != Some(run_id) {
                return Err("allocation_origin_identity_rejected".into());
            }
            let row=db.query_opt("SELECT g.digest,g.payload::text,g.signature,a.environment_id,a.session_id,a.workspace_id,a.user_id::text,a.\"modelConfig_id\",a.\"thinkingMode\" FROM public.app_core_agentrunauthorization g JOIN public.app_core_agentrun a ON a.id=g.agent_run_id WHERE a.id=$1 FOR SHARE OF g,a",&[&run_id]).map_err(|_|"allocation_origin_unavailable")?.ok_or("allocation_origin_missing")?;
            let grant: WorkspaceAgentRunAuthorization =
                serde_json::from_str(&row.get::<_, String>(1))
                    .map_err(|_| "allocation_origin_invalid")?;
            let digest: String = row.get(0);
            grant
                .checked_binding(run_id)?
                .verify(&digest, key, &row.get::<_, String>(2))?;
            if digest != a.authorization_digest
                || row.get::<_, Option<String>>(3).as_deref() != Some(&a.environment_id)
                || grant.session_id != row.get::<_, String>(4)
                || grant.workspace_id != row.get::<_, String>(5)
                || grant.user_id != row.get::<_, String>(6)
                || grant.model_config_ref != row.get::<_, String>(7)
                || grant.thinking_mode != row.get::<_, Option<String>>(8).filter(|v| !v.is_empty())
            {
                return Err("allocation_origin_binding_rejected".into());
            }
            AllocationOrigin::from(grant)
        }
        EnvironmentTaskOwner::MaintenanceOperation { operation_id } => {
            if a.allocation_agent_run_id.is_some() {
                return Err("allocation_origin_identity_rejected".into());
            }
            let rows=db.query("SELECT g.payload::text,g.signature,g.digest,o.environment_id,o.session_id,o.workspace_id,o.operation_kind FROM public.app_core_environmentmaintenancegrant g JOIN public.app_core_environmentmaintenanceoperation o ON o.operation_id=g.operation_id WHERE g.operation_id=$1 AND g.digest=$2 FOR SHARE OF g,o",&[&operation_id,&a.authorization_digest]).map_err(|_|"allocation_origin_unavailable")?;
            if rows.len() != 1 {
                return Err("allocation_origin_missing".into());
            }
            let row = &rows[0];
            let auth = ProvisionAuthorization {
                payload: serde_json::from_str(&row.get::<_, String>(0))
                    .map_err(|_| "allocation_origin_invalid")?,
                signature: row.get(1),
            };
            auth.verify_signature(key)?;
            let b = &auth.payload;
            if auth.digest()? != a.authorization_digest
                || a.binding_digest != a.authorization_digest
                || row.get::<_, String>(2) != a.authorization_digest
                || b.operation_id != *operation_id
                || b.environment_id != a.environment_id
                || row.get::<_, String>(3) != a.environment_id
                || b.session_id != row.get::<_, String>(4)
                || b.workspace_id != row.get::<_, String>(5)
                || row.get::<_, String>(6) != "provision"
            {
                return Err("allocation_origin_binding_rejected".into());
            }
            AllocationOrigin {
                workspace_id: b.workspace_id.clone(),
                session_id: b.session_id.clone(),
                user_id: b.user_id.clone(),
                agent_id: b.agent_id.clone(),
                environment_profile: b.environment_profile.clone(),
                plugin_activation: b.plugin_activation.clone(),
            }
        }
    };
    if origin.environment_profile.legacy()?.image_digest != a.image_digest
        || origin.environment_profile.digest()? != a.profile_digest
    {
        return Err("allocation_origin_profile_rejected".into());
    }
    Ok(origin)
}
