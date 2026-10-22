//! Durable no-Run allocation: current authority and resource holding precede host mutation.
use super::PostgresRuntimeStore;
use crate::environment_authorization::provision::{ProvisionAuthorization, ProvisionBinding};
use crate::postgres_store::execution_instances::{instance_id, RetainedInstanceAllocation};
use crate::resident_capacity::{
    declared_usage, ObservedResidentSandbox, ResidentCapacity, RESIDENT_CAPACITY_WAIT,
};
use centaeris_core::execution::{
    admit_resident_resources, EnvironmentTaskOwner, ResidentResourceClaim,
};
use sha2::{Digest, Sha256};

fn locked(
    tx: &mut postgres::Transaction<'_>,
    auth: &ProvisionAuthorization,
    key: &[u8],
) -> Result<(), String> {
    let b = &auth.payload;
    b.validate_request(&b.request())?;
    tx.batch_execute("SET LOCAL lock_timeout='1s'; SET LOCAL statement_timeout='5s'")
        .map_err(|_| "provision_store_unavailable")?;
    let ws = tx
        .query_opt(
            "SELECT status FROM public.app_core_workspace WHERE id=$1 FOR SHARE",
            &[&b.workspace_id],
        )
        .map_err(|_| "provision_store_unavailable")?
        .ok_or("provision_workspace_missing")?;
    let session=tx.query_opt("SELECT workspace_id,current_environment_id,owner_id::text,agent_id,\"deletedAt\" IS NULL FROM public.app_core_session WHERE id=$1 FOR SHARE",&[&b.session_id]).map_err(|_|"provision_store_unavailable")?.ok_or("provision_session_missing")?;
    if ws.get::<_, String>(0) != "active"
        || session.get::<_, String>(0) != b.workspace_id
        || session.get::<_, Option<String>>(1).as_deref() != Some(&b.environment_id)
        || session.get::<_, String>(2) != b.user_id
        || session.get::<_, String>(3) != b.agent_id
        || !session.get::<_, bool>(4)
    {
        return Err("provision_session_rejected".into());
    }
    let env=tx.query_opt("SELECT session_id,controller_run_id,controller_maintenance_operation_id,controller_epoch,controller_lease_owner,controller_purpose,controller_released,environment_profile::text,environment_profile_digest FROM public.app_core_sessionenvironment WHERE id=$1 FOR SHARE",&[&b.environment_id]).map_err(|_|"provision_store_unavailable")?.ok_or("provision_environment_missing")?;
    let lease = format!(
        "sha256:{:x}",
        Sha256::digest(
            [
                b"workspace:environment-controller-lease:v1\0".as_slice(),
                env.get::<_, String>(4).as_bytes()
            ]
            .concat()
        )
    );
    let profile: crate::agent_run_authorization::EnvironmentProfile = serde_json::from_str(
        &env.get::<_, Option<String>>(7)
            .ok_or("provision_profile_missing")?,
    )
    .map_err(|_| "provision_profile_invalid")?;
    if env.get::<_, String>(0) != b.session_id
        || env.get::<_, Option<String>>(1).is_some()
        || env.get::<_, Option<String>>(2).as_deref() != Some(&b.operation_id)
        || env.get::<_, i64>(3) != b.controller_epoch
        || lease != b.controller_lease_digest
        || env.get::<_, String>(5) != "execute"
        || env.get::<_, bool>(6)
        || profile != b.environment_profile
        || env.get::<_, Option<String>>(8).as_deref() != Some(&b.environment_profile_digest)
    {
        return Err("provision_controller_rejected".into());
    }
    let op=tx.query_opt("SELECT workspace_id,session_id,environment_id,operation_kind,request::text,request_digest,profile_digest,current_grant_id,state,instance_id,container_id,host_id,allocation_execution_id FROM public.app_core_environmentmaintenanceoperation WHERE operation_id=$1 FOR SHARE",&[&b.operation_id]).map_err(|_|"provision_store_unavailable")?.ok_or("provision_operation_missing")?;
    let request: crate::environment_authorization::provision::ProvisionRequest =
        serde_json::from_str(&op.get::<_, String>(4)).map_err(|_| "provision_request_invalid")?;
    if op.get::<_, String>(0) != b.workspace_id
        || op.get::<_, String>(1) != b.session_id
        || op.get::<_, String>(2) != b.environment_id
        || op.get::<_, String>(3) != "provision"
        || request != b.request()
        || op.get::<_, String>(5) != b.request_digest
        || op.get::<_, String>(6) != b.environment_profile_digest
        || op.get::<_, String>(8) != "active"
        || (9..13).any(|i| op.get::<_, Option<String>>(i).is_some())
    {
        return Err("provision_operation_rejected".into());
    }
    let grant_id: String = op
        .get::<_, Option<String>>(7)
        .ok_or("provision_grant_missing")?;
    let grant=tx.query_opt("SELECT operation_id,payload::text,signature,digest FROM public.app_core_environmentmaintenancegrant WHERE id=$1 FOR SHARE",&[&grant_id]).map_err(|_|"provision_store_unavailable")?.ok_or("provision_grant_missing")?;
    let current: ProvisionBinding =
        serde_json::from_str(&grant.get::<_, String>(1)).map_err(|_| "provision_grant_invalid")?;
    if grant.get::<_, String>(0) != b.operation_id
        || grant.get::<_, String>(2) != auth.signature
        || grant.get::<_, String>(3) != auth.digest()?
    {
        return Err("provision_grant_rejected".into());
    }
    let now = tx
        .query_one(
            "SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .map_err(|_| "provision_store_unavailable")?
        .get(0);
    auth.verify_current(&current, now, key)
}

impl PostgresRuntimeStore {
    pub(crate) fn provision_environment(
        &self,
        auth: &ProvisionAuthorization,
        key: &[u8],
        host: &str,
        limits: ResidentCapacity,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
        prepare: impl FnOnce(&RetainedInstanceAllocation) -> Result<String, String> + Send,
    ) -> Result<RetainedInstanceAllocation, String> {
        if host.is_empty() {
            return Err("provision_host_invalid".into());
        }
        self.with_client(|client|super::residency::with_host_inventory(client,host,inventory,|client,_physical|{
            let mut tx=client.transaction().map_err(|_|"provision_store_unavailable")?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;locked(&mut tx,auth,key)?;
            let b=&auth.payload;let execution=format!("provision:{}",b.operation_id);let owner=EnvironmentTaskOwner::MaintenanceOperation{operation_id:b.operation_id.clone()};
            let mut a=RetainedInstanceAllocation{allocation_execution_id:execution.clone(),allocation_agent_run_id:None,allocation_owner:owner.clone(),instance_id:instance_id(&execution),environment_id:b.environment_id.clone(),host_id:host.into(),image_digest:b.environment_profile.legacy()?.image_digest.clone(),authorization_digest:auth.digest()?,binding_digest:auth.digest()?,profile_digest:b.environment_profile_digest.clone(),container_id:None,reused:false};
            let usage=declared_usage(b.environment_profile.legacy()?.resources.sandbox_resources());
            if let Some(row)=tx.query_opt("SELECT i.execution_id,i.instance_id,i.host_id,i.authorization_digest,i.binding_digest,i.profile_digest,i.allocation_owner_json,s.container_id,s.state,s.workspace_id,s.memory_bytes,s.cpu_milli,s.pids,s.workspace_bytes FROM runtime.execution_instances i LEFT JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id WHERE i.environment_id=$1 AND i.allocation_execution_id=i.execution_id FOR SHARE OF i",&[&b.environment_id]).map_err(|_|"provision_store_unavailable")?{
                if row.get::<_,String>(0)!=execution||row.get::<_,String>(1)!=a.instance_id||row.get::<_,String>(2)!=host||row.get::<_,String>(3)!=a.authorization_digest||row.get::<_,String>(4)!=a.binding_digest||row.get::<_,Option<String>>(5).as_deref()!=Some(&a.profile_digest)||serde_json::from_str::<EnvironmentTaskOwner>(&row.get::<_,String>(6)).map_err(|_|"allocation_owner_invalid")?!=owner||row.get::<_,Option<String>>(8).is_none_or(|s|s!="reserved"&&s!="resident")||row.get::<_,Option<String>>(9).as_deref()!=Some(&b.workspace_id){return Err("provision_allocation_conflict".into());}
                for(i,n)in [(10,usage.memory_bytes),(11,usage.cpu_milli),(12,usage.pids),(13,usage.workspace_bytes)]{if row.get::<_,Option<i64>>(i).and_then(|n|u64::try_from(n).ok())!=Some(n){return Err("provision_resource_binding_rejected".into());}}
                a.container_id=row.get(7);a.reused=a.container_id.is_some();
            }else{
                if tx.query_opt("SELECT operation_id FROM runtime.environment_retirements WHERE environment_id=$1",&[&b.environment_id]).map_err(|_|"provision_store_unavailable")?.is_some(){return Err("provision_environment_retired".into());}
                let(global,tenant,used)=super::residency::used(&mut tx,host,&b.workspace_id)?;
                if !admit_resident_resources(global,tenant,ResidentResourceClaim::fully_reserved(usage)?,limits.global,limits.tenant)?||!used.checked_add(usage)?.fits(limits.host){return Err(RESIDENT_CAPACITY_WAIT.into());}
                let owner_json=serde_json::to_string(&owner).map_err(|_|"allocation_owner_invalid")?;
                tx.execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest,allocation_owner_json) VALUES($1,$2,$3,$4,NULL,$5,$6,$7,$8,$1,'retained',$9,$10)",&[&execution,&a.instance_id,&b.environment_id,&b.session_id,&host,&a.authorization_digest,&a.binding_digest,&a.image_digest,&a.profile_digest,&owner_json]).map_err(|_|"provision_allocation_unavailable")?;
                super::residency::insert(&mut tx,host,&execution,None,&b.workspace_id,None,"reserved",usage)?;
            }
            // This commit must survive a lost host acknowledgement.
            tx.commit().map_err(|_|"provision_reservation_unconfirmed")?;
            let mut tx=client.transaction().map_err(|_|"provision_store_unavailable")?;super::resource_commit::require_durable_resource_commit(&mut tx)?;locked(&mut tx,auth,key)?;
            let held=tx.query_opt("SELECT container_id,state FROM runtime.resident_sandboxes WHERE host_id=$1 AND execution_id=$2 FOR UPDATE",&[&host,&execution]).map_err(|_|"provision_store_unavailable")?.ok_or("provision_reservation_missing")?;
            if held.get::<_,Option<String>>(0)!=a.container_id||!matches!(held.get::<_,String>(1).as_str(),"reserved"|"resident"){return Err("provision_reservation_changed".into());}
            let container=prepare(&a)?;
            if container.len()!=64||!container.bytes().all(|v|v.is_ascii_digit()||(b'a'..=b'f').contains(&v))||a.container_id.as_ref().is_some_and(|id|id!=&container){return Err("provision_physical_identity_rejected".into());}
            let completion=serde_json::to_string(&serde_json::json!({"kind":"provisioned","instanceId":a.instance_id,"containerId":container,"profileDigest":a.profile_digest})).map_err(|_|"provision_completion_invalid")?;
            let values:&[&(dyn postgres::types::ToSql+Sync)]=&[&b.operation_id,&b.environment_id,&b.session_id,&b.workspace_id,&host,&a.instance_id,&container,&a.profile_digest,&b.controller_epoch,&b.controller_lease_digest,&b.request_digest,&completion];
            tx.execute("INSERT INTO runtime.environment_provision_completions(operation_id,environment_id,session_id,workspace_id,host_id,instance_id,container_id,profile_digest,controller_epoch,controller_lease_digest,request_digest,completion_json,recorded_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint) ON CONFLICT(operation_id) DO NOTHING",values).map_err(|_|"provision_completion_unavailable")?;
            if !tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.environment_provision_completions WHERE operation_id=$1 AND environment_id=$2 AND session_id=$3 AND workspace_id=$4 AND host_id=$5 AND instance_id=$6 AND container_id=$7 AND profile_digest=$8 AND controller_epoch=$9 AND controller_lease_digest=$10 AND request_digest=$11 AND completion_json=$12)",values).map_err(|_|"provision_completion_unavailable")?.get::<_,bool>(0){return Err("provision_completion_conflict".into());}
            tx.execute("UPDATE runtime.resident_sandboxes SET container_id=$3,state='resident',updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE host_id=$1 AND execution_id=$2",&[&host,&execution,&container]).map_err(|_|"provision_completion_unavailable")?;
            tx.commit().map_err(|_|"provision_completion_unconfirmed")?;a.container_id=Some(container);Ok(a)
        }))
    }
}
