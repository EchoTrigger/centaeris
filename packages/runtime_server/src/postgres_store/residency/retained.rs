use super::*;
use crate::postgres_store::execution_instances::{
    instance_id, RetainedInstanceAllocation, RetainedInstanceRequest,
};

fn verify_controller(
    tx: &mut postgres::Transaction<'_>,
    r: &RetainedInstanceRequest<'_>,
) -> Result<(), String> {
    r.environment_authorization.payload.digest()?;
    if r.execution_id.trim().is_empty()
        || r.host_id.trim().is_empty()
        || !r.profile_digest.strip_prefix("sha256:").is_some_and(|h| {
            h.len() == 64
                && h.bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        })
    {
        return Err("retained controller identity rejected".into());
    }
    let verified = super::super::controller_authority::verify_controller_authority(
        tx,
        &super::super::controller_authority::ControllerAuthorityRequest {
            environment_authorization: r.environment_authorization,
            signing_key: r.signing_key,
            workspace_id: r.workspace_id,
            lease: r.lease,
            purpose: super::super::controller_authority::ControllerPurpose::Execute,
        },
    )?;
    if verified.authorization.environment_profile_digest != r.profile_digest
        || verified.authorization.environment_profile.image_digest != r.image_digest
        || crate::resident_capacity::declared_usage(
            verified
                .authorization
                .environment_profile
                .resources
                .sandbox_resources(),
        ) != r.resources
    {
        return Err("retained original grant binding rejected".into());
    }
    Ok(())
}
fn allocation(
    tx: &mut impl GenericClient,
    execution: &str,
) -> Result<RetainedInstanceAllocation, String> {
    let row=tx.query_opt("SELECT execution_id,agent_run_id,instance_id,environment_id,host_id,image_digest,authorization_digest,binding_digest,profile_digest,allocation_owner_json FROM runtime.execution_instances WHERE execution_id=$1 AND allocation_execution_id=execution_id AND allocation_mode='retained'", &[&execution]).map_err(|e|e.to_string())?.ok_or("retained allocation anchor missing")?;
    Ok(RetainedInstanceAllocation {
        allocation_execution_id: row.get(0),
        allocation_agent_run_id: row.get(1),
        allocation_owner: serde_json::from_str(&row.get::<_, String>(9))
            .map_err(|_| "allocation_owner_invalid")?,
        instance_id: row.get(2),
        environment_id: row.get(3),
        host_id: row.get(4),
        image_digest: row.get(5),
        authorization_digest: row.get(6),
        binding_digest: row.get(7),
        profile_digest: row.get(8),
        container_id: None,
        reused: false,
    })
}
fn associate(
    tx: &mut impl GenericClient,
    r: &RetainedInstanceRequest<'_>,
    a: &RetainedInstanceAllocation,
) -> Result<(), String> {
    let b = &r.environment_authorization.payload;
    tx.execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'retained',$11) ON CONFLICT(execution_id) DO NOTHING", &[&r.execution_id,&a.instance_id,&b.environment_id,&b.session_id,&b.agent_run_id,&r.host_id,&b.agent_run_authorization_digest,&b.digest()?,&r.image_digest,&a.allocation_execution_id,&r.profile_digest]).map_err(|e|e.to_string())?;
    if tx.query_opt("SELECT 1 FROM runtime.execution_instances WHERE execution_id=$1 AND instance_id=$2 AND agent_run_id=$3 AND authorization_digest=$4 AND allocation_execution_id=$5 AND allocation_mode='retained'", &[&r.execution_id,&a.instance_id,&b.agent_run_id,&b.agent_run_authorization_digest,&a.allocation_execution_id]).map_err(|e|e.to_string())?.is_none() { return Err("retained execution reassignment rejected".into()); }
    Ok(())
}
impl PostgresRuntimeStore {
    /// Pin current execute authority and the existing physical incarnation for
    /// one bounded task operation. This cannot allocate or restore a sandbox.
    pub(crate) fn with_retained_instance_operation<T: Send>(
        &self,
        r: &RetainedInstanceRequest<'_>,
        expected: &RetainedInstanceAllocation,
        operation: impl FnOnce(&RetainedInstanceAllocation) -> Result<T, String> + Send,
    ) -> Result<T, String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            verify_controller(&mut tx, r)?;
            let b = &r.environment_authorization.payload;
            let row = tx.query_opt(
                "SELECT i.allocation_execution_id,s.container_id,s.workspace_id,s.memory_bytes,s.cpu_milli,s.pids,s.workspace_bytes FROM runtime.execution_instances i JOIN runtime.resident_sandboxes s ON s.execution_id=i.allocation_execution_id AND s.instance_id=i.instance_id WHERE i.execution_id=$1 AND i.agent_run_id=$2 AND i.authorization_digest=$3 AND i.environment_id=$4 AND i.host_id=$5 AND i.image_digest=$6 AND i.profile_digest=$7 AND i.allocation_mode='retained' AND s.state='resident' AND s.container_id IS NOT NULL FOR SHARE OF i,s",
                &[&r.execution_id, &b.agent_run_id, &b.agent_run_authorization_digest, &b.environment_id, &r.host_id, &r.image_digest, &r.profile_digest],
            ).map_err(|e| e.to_string())?.ok_or("retained operation association rejected")?;
            if row.get::<_, String>(2) != r.workspace_id || usage(&row, 3)? != r.resources {
                return Err("retained operation resource binding rejected".into());
            }
            let mut actual = allocation(&mut tx, &row.get::<_, String>(0))?;
            actual.container_id = row.get(1);
            actual.reused = true;
            if actual.allocation_execution_id != expected.allocation_execution_id
                || actual.allocation_agent_run_id != expected.allocation_agent_run_id
                || actual.instance_id != expected.instance_id
                || actual.authorization_digest != expected.authorization_digest
                || actual.binding_digest != expected.binding_digest
                || actual.container_id != expected.container_id
            {
                return Err("retained operation incarnation rejected".into());
            }
            // Controller/job and association/resident SHARE locks remain held
            // through the real host operation. No host admission lock is needed.
            let result = operation(&actual)?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(result)
        })
    }
    pub(crate) fn prepare_retained_instance(
        &self,
        r: &RetainedInstanceRequest<'_>,
        limits: ResidentCapacity,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
        before_create: impl FnOnce(&mut Client, &RetainedInstanceAllocation) -> Result<(), String>
            + Send,
        prepare: impl FnOnce(&RetainedInstanceAllocation) -> Result<String, String> + Send,
    ) -> Result<RetainedInstanceAllocation, String> {
        self.with_client(|client| {
            let lock=lock_host(client)?; let physical=inventory()?; reconcile(lock.0,r.host_id,&physical)?;
            let mut tx=lock.0.transaction().map_err(|e|e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?; verify_controller(&mut tx,r)?;
            let b=&r.environment_authorization.payload;
            // An orphan has no provable environment; retain its debit and block.
            let blocked=tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.resident_sandboxes s LEFT JOIN runtime.execution_instances i ON i.execution_id=s.execution_id WHERE (s.allocation_mode='unbound' AND i.execution_id IS NULL) OR (i.environment_id=$1 AND i.allocation_mode='perRun'))", &[&b.environment_id]).map_err(|e|e.to_string())?.get::<_,bool>(0);
            if blocked { return Err("retained allocation blocked by unresolved reservation".into()); }
            let holding=tx.query_opt("SELECT execution_id,host_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes FROM runtime.resident_sandboxes WHERE environment_id=$1 AND allocation_mode='retained' FOR UPDATE", &[&b.environment_id]).map_err(|e|e.to_string())?;
            let mut a=if let Some(row)=holding {
                let origin:String=row.get(0); let mut a=allocation(&mut tx,&origin)?;
                if row.get::<_,String>(1)!=r.host_id || row.get::<_,String>(2)!=r.workspace_id || usage(&row,5)?!=r.resources || a.image_digest!=r.image_digest || a.profile_digest!=r.profile_digest { return Err("retained allocation configuration mismatch".into()); }
                a.container_id=row.get(3); a.reused=true;
                if row.get::<_,String>(4)!="resident" || a.container_id.is_none() || !physical.iter().any(|s|s.execution_id==origin && s.allocation_owner==a.allocation_owner && Some(&s.container_id)==a.container_id.as_ref()) { return Err("retained allocation outcome uncertain; creation blocked".into()); } a
            } else {
                // Losing the resident does not erase its allocation history.
                // Cold recovery requires a separately authorized restore path;
                // a new Run/execution cannot impersonate same-instance continuity.
                if tx.query_opt("SELECT 1 FROM runtime.execution_instances i WHERE execution_id=$1 OR (environment_id=$2 AND allocation_mode='retained' AND NOT EXISTS(SELECT 1 FROM runtime.environment_creation_attempts a WHERE a.allocation_execution_id=i.allocation_execution_id AND a.state='notDispatched') AND NOT($3 AND EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts c WHERE c.instance_id=i.instance_id AND c.environment_id=i.environment_id AND c.cleanup_complete AND c.physical_removed)))", &[&r.execution_id,&b.environment_id,&(b.execution_mode=="perRun")]).map_err(|e|e.to_string())?.is_some() { return Err("execution_environment_lost:retained_allocation_missing".into()); }
                let (global,tenant,host)=used(&mut tx,r.host_id,r.workspace_id)?;
                if !admit_resident_resources(global,tenant,ResidentResourceClaim::fully_reserved(r.resources)?,limits.global,limits.tenant)? || !host.checked_add(r.resources)?.fits(limits.host) { return Err(RESIDENT_CAPACITY_WAIT.into()); }
                let a=RetainedInstanceAllocation{allocation_execution_id:r.execution_id.into(),allocation_agent_run_id:Some(b.agent_run_id.clone()),allocation_owner:centaeris_core::execution::EnvironmentTaskOwner::AgentRun{run_id:b.agent_run_id.clone()},instance_id:instance_id(r.execution_id),environment_id:b.environment_id.clone(),host_id:r.host_id.into(),image_digest:r.image_digest.into(),authorization_digest:b.agent_run_authorization_digest.clone(),binding_digest:b.digest()?,profile_digest:r.profile_digest.into(),container_id:None,reused:false};
                associate(&mut tx,r,&a)?; insert(&mut tx,r.host_id,r.execution_id,Some(&b.agent_run_id),r.workspace_id,None,"reserved",r.resources)?; super::super::creation_attempts::reserve(&mut tx,&a)?; a
            };
            associate(&mut tx,r,&a)?;
            // Commit allocation + reservation before Docker. Errors and lost
            // physical acknowledgements leave the durable holding intact.
            tx.commit().map_err(|e|e.to_string())?;
            // Core persists its execution fact using this same Client, outside
            // our transaction. It must finish before authority is pinned again.
            if let Err(error)=before_create(lock.0,&a) {
                if !a.reused {
                    let mut tx=lock.0.transaction().map_err(|e|e.to_string())?;
                    super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
                    verify_controller(&mut tx,r)?;
                    super::super::creation_attempts::close_before_dispatch(&mut tx,&a)?;
                    tx.commit().map_err(|e|e.to_string())?;
                }
                return Err(error);
            }
            let mut tx=lock.0.transaction().map_err(|e|e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?; verify_controller(&mut tx,r)?;
            let current=tx.query_opt("SELECT container_id,state FROM runtime.resident_sandboxes WHERE host_id=$1 AND execution_id=$2 FOR UPDATE", &[&r.host_id,&a.allocation_execution_id]).map_err(|e|e.to_string())?.ok_or("retained reservation missing")?;
            if current.get::<_,String>(1)=="releasing" || current.get::<_,Option<String>>(0)!=a.container_id { return Err("retained reservation changed".into()); }
            if !a.reused {
                // Persist the conservative dispatch boundary before the host call.
                // A crash before the actual send is Unknown, never refund evidence.
                super::super::creation_attempts::dispatch(&mut tx,&a)?;
                tx.commit().map_err(|e|e.to_string())?;
                tx=lock.0.transaction().map_err(|e|e.to_string())?;
                super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
                verify_controller(&mut tx,r)?;
            }
            let container=prepare(&a)?;
            if container.trim().is_empty() || a.container_id.as_ref().is_some_and(|id|id!=&container) { return Err("retained physical instance reassignment rejected".into()); }
            tx.execute("UPDATE runtime.resident_sandboxes SET container_id=$3,state='resident',updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE host_id=$1 AND execution_id=$2", &[&r.host_id,&a.allocation_execution_id,&container]).map_err(|e|e.to_string())?;
            tx.commit().map_err(|e|e.to_string())?; a.container_id=Some(container); Ok(a)
        })
    }
}
