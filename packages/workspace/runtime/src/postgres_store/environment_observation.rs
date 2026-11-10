use super::PostgresRuntimeStore;
use crate::agent_run_authorization::EnvironmentProfile;
use crate::postgres_store::execution_instances::RetainedInstanceAllocation;
use centaeris_core::execution::EnvironmentTaskOwner;

pub(crate) struct EnvironmentObservation {
    pub session_id: String,
    pub environment_id: String,
    pub profile: EnvironmentProfile,
    pub profile_digest: String,
    pub controller_epoch: i64,
    pub controller_owner: Option<EnvironmentTaskOwner>,
    pub allocation: Option<RetainedInstanceAllocation>,
    pub holding_state: Option<String>,
    pub resource_holding: bool,
    pub destroyed: bool,
    pub not_materialized: bool,
}
impl PostgresRuntimeStore {
    pub(crate) fn with_environment_observation<T: Send>(
        &self,
        session: &str,
        environment: &str,
        observe: impl FnOnce(&EnvironmentObservation) -> Result<T, String> + Send,
    ) -> Result<T, String> {
        self.with_client(|client|{
            let mut tx=client.transaction().map_err(|_|"environment_observation_unavailable")?;
            tx.batch_execute("SET LOCAL lock_timeout='1s'; SET LOCAL statement_timeout='5s';SELECT pg_advisory_xact_lock(731946,3)").map_err(|_|"environment_observation_unavailable")?;
            let session_row=tx.query_opt("SELECT current_environment_id FROM public.app_core_session WHERE id=$1 FOR SHARE",&[&session]).map_err(|_|"environment_observation_unavailable")?.ok_or("environment_session_missing")?;
            let env=tx.query_opt("SELECT session_id,environment_profile::text,environment_profile_digest,controller_epoch,controller_run_id,controller_maintenance_operation_id,controller_released FROM public.app_core_sessionenvironment WHERE id=$1 FOR SHARE",&[&environment]).map_err(|_|"environment_observation_unavailable")?.ok_or("environment_missing")?;
            if env.get::<_,String>(0)!=session||session_row.get::<_,Option<String>>(0).as_deref()!=Some(environment){return Err("environment_session_binding_rejected".into());}
            let profile:EnvironmentProfile=serde_json::from_str(&env.get::<_,Option<String>>(1).ok_or("environment_profile_missing")?).map_err(|_|"environment_profile_invalid")?;
            let digest:String=env.get::<_,Option<String>>(2).ok_or("environment_profile_missing")?;
            if profile.digest()?!=digest{return Err("environment_profile_binding_rejected".into());}
            let run:Option<String>=env.get(4);let operation:Option<String>=env.get(5);
            let owner=if env.get::<_,bool>(6){None}else{match(run,operation){(Some(run_id),None)=>Some(EnvironmentTaskOwner::AgentRun{run_id}),(None,Some(operation_id))=>Some(EnvironmentTaskOwner::MaintenanceOperation{operation_id}),_=>return Err("environment_controller_invalid".into())}};
            let rows=tx.query("SELECT i.execution_id,i.agent_run_id,i.allocation_owner_json,i.instance_id,i.host_id,i.image_digest,i.authorization_digest,i.binding_digest,i.profile_digest,COALESCE(s.container_id,(SELECT c.removed_container_id FROM runtime.agent_run_cleanup_receipts c WHERE c.instance_id=i.instance_id AND c.environment_id=i.environment_id AND c.physical_removed AND c.cleanup_complete ORDER BY c.recorded_at_ms DESC LIMIT 1),(SELECT r.container_id FROM runtime.environment_retirements r WHERE r.environment_id=i.environment_id AND r.instance_id=i.instance_id)),s.state FROM runtime.execution_instances i LEFT JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id AND s.instance_id=i.instance_id WHERE i.environment_id=$1 AND i.session_id=$2 AND i.allocation_execution_id=i.execution_id ORDER BY (s.execution_id IS NOT NULL) DESC,EXISTS(SELECT 1 FROM runtime.environment_creation_attempts a WHERE a.allocation_execution_id=i.execution_id AND a.state='notDispatched'),i.execution_id LIMIT 2",&[&environment,&session]).map_err(|_|"environment_observation_unavailable")?;
            // Each retained Environment has one incarnation. perRun old incarnations
            // are excluded only by a proven cleanup receipt and absent holding.
            let mut allocation=None;let mut state=None;
            for row in rows{
                let held:Option<String>=row.get(10);
                if allocation.is_some()&&held.is_some(){return Err("environment_allocation_ambiguous".into());}
                if allocation.is_some()&&held.is_none(){continue;}
                if row.get::<_,String>(5)!=profile.image_digest||row.get::<_,Option<String>>(8).as_deref()!=Some(&digest){return Err("environment_allocation_binding_rejected".into());}
                state=held;
                allocation=Some(RetainedInstanceAllocation{allocation_execution_id:row.get(0),allocation_agent_run_id:row.get(1),allocation_owner:serde_json::from_str(&row.get::<_,String>(2)).map_err(|_|"allocation_owner_invalid")?,instance_id:row.get(3),environment_id:environment.into(),host_id:row.get(4),image_digest:row.get(5),authorization_digest:row.get(6),binding_digest:row.get(7),profile_digest:digest.clone(),container_id:row.get(9),reused:true});
            }
            let retired=tx.query_opt("SELECT state FROM runtime.environment_retirements WHERE environment_id=$1",&[&environment]).map_err(|_|"environment_observation_unavailable")?.is_some_and(|r|r.get::<_,String>(0)=="destroyed");
            let removed=if let Some(a)=&allocation{tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts WHERE environment_id=$1 AND instance_id=$2 AND physical_removed AND cleanup_complete AND removed_container_id=$3)",&[&environment,&a.instance_id,&a.container_id]).map_err(|_|"environment_observation_unavailable")?.get::<_,bool>(0)}else{false};
            let not_materialized=if let Some(a)=&allocation { super::creation_attempts::not_materialized(&mut tx,&a.allocation_execution_id,&a.instance_id)? } else { false };
            let destroyed=retired||removed;
            let target=EnvironmentObservation{session_id:session.into(),environment_id:environment.into(),profile,profile_digest:digest,controller_epoch:env.get(3),controller_owner:owner,resource_holding:state.is_some(),holding_state:state,allocation,destroyed,not_materialized};
            let result=observe(&target)?;tx.commit().map_err(|_|"environment_observation_unavailable")?;Ok(result)
        })
    }
}
