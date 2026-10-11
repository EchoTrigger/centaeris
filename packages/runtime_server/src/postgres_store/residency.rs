use centaeris_core::execution::{admit_resident_resources, ResidentResourceUsage};
use centaeris_core::session::reliability::agent_run_lifecycle_job_id;
use centaeris_core::session::RuntimeJobLeaseFence;
use postgres::{Client, GenericClient};

use super::PostgresRuntimeStore;
use crate::resident_capacity::{
    ObservedResidentSandbox, ResidentCapacity, ResidentSandboxRequest, RESIDENT_CAPACITY_WAIT,
};

// This session lock spans a committed reservation and the bounded Docker
// mutation. A SQL transaction alone could roll back after Docker committed.
// All managed create/remove paths use this lock; contention yields admission.
struct ResidentHostLock<'a>(&'a mut Client);

impl Drop for ResidentHostLock<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.0.query_one("SELECT pg_advisory_unlock(731946,3)", &[]) {
            eprintln!("resident host admission unlock failed: {error}");
        }
    }
}

fn lock_host(client: &mut Client) -> Result<ResidentHostLock<'_>, String> {
    if !client
        .query_one("SELECT pg_try_advisory_lock(731946,3)", &[])
        .map_err(|error| format!("resident host lock failed: {error}"))?
        .get::<_, bool>(0)
    {
        return Err(RESIDENT_CAPACITY_WAIT.into());
    }
    Ok(ResidentHostLock(client))
}

impl PostgresRuntimeStore {
    pub(crate) fn with_resident_execution_restore<T: Send>(
        &self,
        host_id: &str,
        agent_run_id: &str,
        execution_id: &str,
        fence: &RuntimeJobLeaseFence,
        restore: impl FnOnce() -> T + Send,
    ) -> Result<T, String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            // Pin the current owner throughout the bounded physical restore.
            // Lease reclaimers skip this row until the operation has finished.
            verify_removal_fence(&mut tx, agent_run_id, fence)?;
            let resident = tx.query_opt(
                "SELECT 1 FROM resident_sandboxes s JOIN execution_job_tenants t ON t.job_id=$4 JOIN app_core_agentrun a ON a.id=s.agent_run_id WHERE s.host_id=$1 AND s.agent_run_id=$2 AND s.execution_id=$3 AND s.state='resident' AND s.container_id IS NOT NULL AND s.workspace_id=t.workspace_id AND a.workspace_id=t.workspace_id AND a.status IN('queued','running') FOR UPDATE OF s",
                &[&host_id, &agent_run_id, &execution_id, &fence.job_id],
            ).map_err(|error| format!("verify resident restore binding failed: {error}"))?;
            if resident.is_none() {
                return Err("resident restore binding rejected".into());
            }
            let restored = restore();
            tx.commit().map_err(|error| format!("release resident restore lease fence failed: {error}"))?;
            Ok(restored)
        })
    }

    pub(crate) fn reconcile_resident_sandboxes(
        &self,
        host_id: &str,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
    ) -> Result<(), String> {
        self.with_client(|client| {
            let lock = lock_host(client)?;
            reconcile(lock.0, host_id, &inventory()?)
        })
    }

    #[cfg(test)]
    pub(crate) fn prepare_resident_sandbox(
        &self,
        host_id: &str,
        request: &ResidentSandboxRequest<'_>,
        limits: ResidentCapacity,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
        prepare: impl FnOnce() -> Result<String, String> + Send,
    ) -> Result<(), String> {
        self.prepare_resident_sandbox_with_client(host_id, request, limits, inventory, |_| {
            prepare()
        })
    }

    pub(crate) fn prepare_resident_sandbox_with_client(
        &self,
        host_id: &str,
        request: &ResidentSandboxRequest<'_>,
        limits: ResidentCapacity,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
        prepare: impl FnOnce(&mut Client) -> Result<String, String> + Send,
    ) -> Result<(), String> {
        if host_id.trim().is_empty()
            || request.execution_id.trim().is_empty()
            || request.lifecycle_job_id != agent_run_lifecycle_job_id(request.agent_run_id)?
        {
            return Err("resident sandbox identity invalid".into());
        }
        self.with_client(|client| {
            let lock = lock_host(client)?;
            let inventory = inventory()?;
            reconcile(lock.0, host_id, &inventory)?;
            if request.must_exist && !inventory.iter().any(|sandbox| sandbox.execution_id == request.execution_id && sandbox.agent_run_id == request.agent_run_id) {
                return Err("execution_environment_lost:container_missing".into());
            }
            let mut tx = lock.0.transaction().map_err(|error| error.to_string())?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            // Signed authorization supplies the workspace. Its durable job
            // binding and live lease must agree before physical creation.
            let admitted = tx.query_opt("SELECT t.workspace_id FROM runtime_jobs j JOIN execution_job_tenants t ON t.job_id=j.job_id JOIN app_core_agentrun a ON a.id=$4 WHERE j.job_id=$1 AND j.job_kind='agent_run.lifecycle' AND j.status IN('leased','running') AND j.lease_owner=$2 AND j.lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint AND t.workspace_id=$3 AND a.workspace_id=t.workspace_id AND a.status IN('queued','running')", &[&request.lifecycle_job_id, &request.lifecycle_lease_owner, &request.workspace_id, &request.agent_run_id])
                .map_err(|error| format!("verify resident admission owner failed: {error}"))?;
            if admitted.is_none_or(|row| row.get::<_, String>(0) != request.workspace_id) {
                return Err("resident sandbox lease or tenant binding rejected".into());
            }
            let existing = tx.query_opt("SELECT workspace_id,agent_run_id,state,memory_bytes,cpu_milli,pids,workspace_bytes FROM resident_sandboxes WHERE host_id=$1 AND execution_id=$2 FOR UPDATE", &[&host_id, &request.execution_id])
                .map_err(|error| error.to_string())?;
            if let Some(row) = existing {
                if row.get::<_, String>(0) != request.workspace_id || row.get::<_, String>(1) != request.agent_run_id
                    || usage(&row, 3)? != request.resources {
                    return Err("resident reservation identity or resources mismatch".into());
                }
                if row.get::<_, String>(2) == "releasing" { return Err(RESIDENT_CAPACITY_WAIT.into()); }
            } else {
                if !request.resources.fits(limits.global) || !request.resources.fits(limits.tenant) || !request.resources.fits(limits.host) {
                    return Err(RESIDENT_CAPACITY_WAIT.into());
                }
                let (global_used, tenant_used, host_used) = used(&mut tx, host_id, request.workspace_id)?;
                if !admit_resident_resources(global_used, tenant_used, request.resources, limits.global, limits.tenant)?
                    || !host_used.checked_add(request.resources)?.fits(limits.host) {
                    return Err(RESIDENT_CAPACITY_WAIT.into());
                }
                insert(&mut tx, host_id, request.execution_id, request.agent_run_id, request.workspace_id, None, "reserved", request.resources)?;
            }
            tx.commit().map_err(|error| error.to_string())?;
            // Unknown creation outcomes keep the reservation across restart.
            let container_id = prepare(lock.0)?;
            lock.0.execute("UPDATE resident_sandboxes SET state='resident',container_id=$3,updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE host_id=$1 AND execution_id=$2", &[&host_id, &request.execution_id, &container_id])
                .map_err(|error| format!("confirm resident sandbox failed: {error}"))?;
            Ok(())
        })
    }

    pub(crate) fn remove_resident_sandboxes(
        &self,
        host_id: &str,
        agent_run_id: &str,
        fence: &RuntimeJobLeaseFence,
        remove: impl FnOnce() -> Result<(), String> + Send,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
    ) -> Result<(), String> {
        self.remove_resident_scope(host_id, agent_run_id, None, fence, |_| remove(), inventory)
    }

    pub(crate) fn remove_resident_execution(
        &self,
        host_id: &str,
        agent_run_id: &str,
        execution_id: &str,
        fence: &RuntimeJobLeaseFence,
        remove: impl FnOnce() -> Result<(), String> + Send,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
    ) -> Result<(), String> {
        if execution_id.trim().is_empty() {
            return Err("resident removal execution identity missing".into());
        }
        self.remove_resident_scope(
            host_id,
            agent_run_id,
            Some(execution_id),
            fence,
            |_| remove(),
            inventory,
        )
    }

    // Keep authorization, physical removal and confirmation explicit at this boundary.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn discard_unstarted_resident_execution(
        &self,
        host_id: &str,
        agent_run_id: &str,
        execution_id: &str,
        fence: &RuntimeJobLeaseFence,
        authorize: impl FnOnce(&mut postgres::Transaction<'_>) -> Result<(), String> + Send,
        remove: impl FnOnce(Option<&str>) -> Result<(), String> + Send,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
    ) -> Result<(), String> {
        if execution_id.trim().is_empty() {
            return Err("pending recovery execution identity missing".into());
        }
        self.remove_resident_scope(host_id, agent_run_id, Some(execution_id), fence,
            |tx| {
                authorize(tx)?;
                let Some(row) = tx.query_opt("SELECT container_id FROM resident_sandboxes WHERE host_id=$1 AND agent_run_id=$2 AND execution_id=$3 FOR UPDATE",
                    &[&host_id, &agent_run_id, &execution_id]).map_err(|error| error.to_string())? else {
                    // A prior discard may have committed before the next intent.
                    // Do not mutate a physical object without a holding; the
                    // surrounding inventory must still confirm exact absence.
                    return Ok(());
                };
                let known_container_id = row.get::<_, Option<String>>(0);
                remove(known_container_id.as_deref())
            }, inventory)
    }

    fn remove_resident_scope(
        &self,
        host_id: &str,
        agent_run_id: &str,
        execution_id: Option<&str>,
        fence: &RuntimeJobLeaseFence,
        remove: impl FnOnce(&mut postgres::Transaction<'_>) -> Result<(), String> + Send,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
    ) -> Result<(), String> {
        self.with_client(|client| {
            let lock = lock_host(client)?;
            let mut tx = lock.0.transaction().map_err(|error| error.to_string())?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            // Pin the validated owner until the bounded physical mutation and
            // confirmed budget release finish. Reclaimers skip this locked row.
            verify_removal_fence(&mut tx, agent_run_id, fence)?;
            tx.execute("UPDATE resident_sandboxes SET state='releasing',updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE host_id=$1 AND agent_run_id=$2 AND ($3::text IS NULL OR execution_id=$3)", &[&host_id, &agent_run_id, &execution_id])
                .map_err(|error| error.to_string())?;
            remove(&mut tx)?;
            if inventory()?.iter().any(|sandbox| sandbox.agent_run_id == agent_run_id && execution_id.is_none_or(|id| sandbox.execution_id == id)) {
                return Err("resident sandbox removal was not confirmed".into());
            }
            tx.execute("DELETE FROM resident_sandboxes WHERE host_id=$1 AND agent_run_id=$2 AND ($3::text IS NULL OR execution_id=$3)", &[&host_id, &agent_run_id, &execution_id])
                .map_err(|error| format!("release resident sandbox budget failed: {error}"))?;
            let holdings_remain = tx.query_one("SELECT EXISTS(SELECT 1 FROM resident_sandboxes WHERE agent_run_id=$1)", &[&agent_run_id])
                .map_err(|error| error.to_string())?.get::<_, bool>(0);
            tx.commit().map_err(|error| format!("commit resident removal failed: {error}"))?;
            if holdings_remain && execution_id.is_none() {
                return Err("resident holdings remain on another host; budget retained".into());
            }
            Ok(())
        })
    }
}

fn verify_removal_fence(
    client: &mut impl GenericClient,
    agent_run_id: &str,
    fence: &RuntimeJobLeaseFence,
) -> Result<(), String> {
    if fence.job_id != agent_run_lifecycle_job_id(agent_run_id)?
        || fence.job_kind != centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND
    {
        return Err("resident removal job identity mismatch".into());
    }
    let valid = client.query_opt("SELECT 1 FROM runtime_jobs WHERE job_id=$1 AND job_kind='agent_run.lifecycle' AND status IN('leased','running') AND lease_owner=$2 AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint FOR UPDATE", &[&fence.job_id, &fence.lease_owner])
        .map_err(|error| format!("verify resident removal lease failed: {error}"))?.is_some();
    if !valid {
        return Err("resident removal lease rejected".into());
    }
    Ok(())
}

fn usage(row: &postgres::Row, offset: usize) -> Result<ResidentResourceUsage, String> {
    let value = |index| u64::try_from(row.get::<_, i64>(index)).map_err(|error| error.to_string());
    Ok(ResidentResourceUsage {
        sandbox_count: 1,
        memory_bytes: value(offset)?,
        cpu_milli: value(offset + 1)?,
        pids: value(offset + 2)?,
        workspace_bytes: value(offset + 3)?,
    })
}

fn used(
    client: &mut impl GenericClient,
    host_id: &str,
    tenant: &str,
) -> Result<
    (
        ResidentResourceUsage,
        ResidentResourceUsage,
        ResidentResourceUsage,
    ),
    String,
> {
    let rows = client.query("SELECT host_id,workspace_id,memory_bytes,cpu_milli,pids,workspace_bytes FROM resident_sandboxes", &[]).map_err(|error| error.to_string())?;
    let mut global = ResidentResourceUsage::default();
    let mut tenant_used = ResidentResourceUsage::default();
    let mut host_used = ResidentResourceUsage::default();
    for row in rows {
        let resources = usage(&row, 2)?;
        global = global.checked_add(resources)?;
        if row.get::<_, String>(0) == host_id {
            host_used = host_used.checked_add(resources)?;
        }
        if row.get::<_, String>(1) == tenant {
            tenant_used = tenant_used.checked_add(resources)?;
        }
    }
    Ok((global, tenant_used, host_used))
}

// Persist the complete ownership and resource declaration in one reservation.
#[allow(clippy::too_many_arguments)]
fn insert(
    client: &mut impl GenericClient,
    host_id: &str,
    execution_id: &str,
    agent_run_id: &str,
    workspace_id: &str,
    container_id: Option<&str>,
    state: &str,
    resources: ResidentResourceUsage,
) -> Result<(), String> {
    let value = |number: u64| i64::try_from(number).map_err(|error| error.to_string());
    client.execute("INSERT INTO resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint)",
        &[&host_id, &execution_id, &agent_run_id, &workspace_id, &container_id, &state, &value(resources.memory_bytes)?, &value(resources.cpu_milli)?, &value(resources.pids)?, &value(resources.workspace_bytes)?])
        .map(|_| ()).map_err(|error| format!("reserve resident sandbox failed: {error}"))
}

fn reconcile(
    client: &mut Client,
    host_id: &str,
    inventory: &[ObservedResidentSandbox],
) -> Result<(), String> {
    if host_id.trim().is_empty() {
        return Err("resident host identity missing".into());
    }
    let mut tx = client.transaction().map_err(|error| error.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    for sandbox in inventory {
        let tenant = tx
            .query_opt(
                "SELECT workspace_id FROM execution_job_tenants WHERE job_id=$1",
                &[&agent_run_lifecycle_job_id(&sandbox.agent_run_id)?],
            )
            .map_err(|error| error.to_string())?
            .ok_or("managed resident sandbox has unknown tenant; admission blocked")?
            .get::<_, String>(0);
        let existing = tx.query_opt("SELECT agent_run_id,workspace_id,container_id,memory_bytes,cpu_milli,pids,workspace_bytes FROM resident_sandboxes WHERE host_id=$1 AND execution_id=$2", &[&host_id, &sandbox.execution_id])
            .map_err(|error| error.to_string())?;
        if let Some(row) = existing {
            if row.get::<_, String>(0) != sandbox.agent_run_id
                || row.get::<_, String>(1) != tenant
                || row
                    .get::<_, Option<String>>(2)
                    .is_some_and(|id| id != sandbox.container_id)
                || usage(&row, 3)? != sandbox.resources
            {
                return Err(
                    "managed resident sandbox disagrees with reservation; admission blocked".into(),
                );
            }
            tx.execute("UPDATE resident_sandboxes SET container_id=$3,state=CASE WHEN state='releasing' THEN state ELSE 'resident' END,updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE host_id=$1 AND execution_id=$2", &[&host_id, &sandbox.execution_id, &sandbox.container_id]).map_err(|error| error.to_string())?;
        } else {
            insert(
                &mut tx,
                host_id,
                &sandbox.execution_id,
                &sandbox.agent_run_id,
                &tenant,
                Some(&sandbox.container_id),
                "resident",
                sandbox.resources,
            )?;
        }
    }
    // No timeout releases reserved/unknown creations. A confirmed resident can
    // be reconciled absent while this lock excludes every managed creation.
    let present = inventory
        .iter()
        .map(|sandbox| sandbox.execution_id.clone())
        .collect::<Vec<_>>();
    tx.execute("DELETE FROM resident_sandboxes WHERE host_id=$1 AND state='resident' AND NOT(execution_id=ANY($2))", &[&host_id, &present]).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}
