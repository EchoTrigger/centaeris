use centaeris_core::execution::{admit_resident_resources, ResidentResourceUsage};
use centaeris_core::session::reliability::agent_run_lifecycle_job_id;
use centaeris_core::session::RuntimeJobLeaseFence;
use postgres::{Client, GenericClient};

use super::PostgresRuntimeStore;
mod retained;
pub(crate) mod retirement;
use crate::resident_capacity::{ObservedResidentSandbox, ResidentCapacity, RESIDENT_CAPACITY_WAIT};

// This session lock spans a committed reservation and the bounded Docker
// mutation. A SQL transaction alone could roll back after Docker committed.
// All managed create/remove paths use this lock; contention yields admission.
struct ResidentHostLock<'a>(&'a mut Client, String);
mod preparation_wait;

impl Drop for ResidentHostLock<'_> {
    fn drop(&mut self) {
        match self.0.query_one("SELECT pg_advisory_unlock(731946,3)", &[]) {
            Ok(row) if row.get::<_, bool>(0) => {
                // Eligibility changes without refunding any resident or unknown creation.
                // A failed notification leaves the existing durable timed retry intact.
                if let Err(error) = preparation_wait::signal(self.0, &self.1) {
                    eprintln!("host preparation wake unavailable: {error}");
                }
            }
            _ => eprintln!("resident host admission unlock unavailable"),
        }
    }
}

fn try_lock_host(client: &mut Client) -> Result<bool, String> {
    client
        .query_one("SELECT pg_try_advisory_lock(731946,3)", &[])
        .map(|row| row.get(0))
        .map_err(|error| format!("resident host lock failed: {error}"))
}

fn lock_host<'a>(client: &'a mut Client, host_id: &str) -> Result<ResidentHostLock<'a>, String> {
    if !try_lock_host(client)? {
        return Err(RESIDENT_CAPACITY_WAIT.into());
    }
    Ok(ResidentHostLock(client, host_id.into()))
}

#[cfg(test)]
thread_local! {
    static BEFORE_PREPARATION_WAIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// A bounded test seam for release between the first refusal and durable registration.
#[cfg(test)]
pub(super) fn before_preparation_wait_registration(hook: impl FnOnce() + 'static) {
    BEFORE_PREPARATION_WAIT.with(|pending| {
        assert!(pending.borrow().is_none());
        *pending.borrow_mut() = Some(Box::new(hook));
    });
}

fn lock_host_for_run<'a>(
    client: &'a mut Client,
    host_id: &str,
    fence: &RuntimeJobLeaseFence,
) -> Result<ResidentHostLock<'a>, String> {
    if !try_lock_host(client)? {
        #[cfg(test)]
        BEFORE_PREPARATION_WAIT.with(|pending| {
            if let Some(hook) = pending.borrow_mut().take() {
                hook();
            }
        });
        preparation_wait::register(client, host_id, fence)?;
        // The previous owner may have unlocked before the wait fact committed.
        // Recheck after registration instead of relying on a lost notification.
        if !try_lock_host(client)? {
            return Err(crate::resident_capacity::HOST_PREPARATION_WAIT.into());
        }
    }
    let lock = ResidentHostLock(client, host_id.into());
    preparation_wait::clear(lock.0, fence)?;
    Ok(lock)
}

pub(super) fn with_host_inventory<T>(
    client: &mut Client,
    host_id: &str,
    inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String>,
    operation: impl FnOnce(&mut Client, &[ObservedResidentSandbox]) -> Result<T, String>,
) -> Result<T, String> {
    let lock = lock_host(client, host_id)?;
    let physical = inventory()?;
    reconcile(lock.0, host_id, &physical)?;
    operation(lock.0, &physical)
}

impl PostgresRuntimeStore {
    pub(crate) fn reconcile_resident_sandboxes(
        &self,
        host_id: &str,
        inventory: impl FnOnce() -> Result<Vec<ObservedResidentSandbox>, String> + Send,
    ) -> Result<(), String> {
        self.with_client(|client| {
            let lock = lock_host(client, host_id)?;
            reconcile(lock.0, host_id, &inventory()?)
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
            let lock = lock_host(client, host_id)?;
            let mut tx = lock.0.transaction().map_err(|error| error.to_string())?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            reject_retained_legacy(&mut tx, agent_run_id, execution_id)?;
            // Pin the validated owner until the bounded physical mutation and
            // confirmed budget release finish. Reclaimers skip this locked row.
            verify_removal_fence(&mut tx, agent_run_id, fence)?;
            tx.execute("UPDATE resident_sandboxes SET state='releasing',updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE host_id=$1 AND agent_run_id=$2 AND ($3::text IS NULL OR execution_id=$3)", &[&host_id, &agent_run_id, &execution_id])
                .map_err(|error| error.to_string())?;
            remove(&mut tx)?;
            if inventory()?.iter().any(|sandbox| sandbox.allocation_owner == (centaeris_core::execution::EnvironmentTaskOwner::AgentRun{run_id:agent_run_id.into()}) && execution_id.is_none_or(|id| sandbox.execution_id == id)) {
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

fn reject_retained_legacy(
    client: &mut impl GenericClient,
    run: &str,
    execution: Option<&str>,
) -> Result<(), String> {
    if client.query_one("SELECT EXISTS(SELECT 1 FROM runtime.execution_instances WHERE allocation_mode='retained' AND (agent_run_id=$1 OR execution_id=$2))", &[&run,&execution]).map_err(|e|e.to_string())?.get::<_,bool>(0) {
        return Err("retained allocation requires explicit controller operation; legacy restore/remove rejected".into());
    }
    Ok(())
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

pub(super) fn used(
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

/// Recompute an admission view without rewriting any durable resource declaration.
fn used_shared(
    client: &mut impl GenericClient,
    host_id: &str,
    tenant: &str,
    sample: &crate::resident_capacity::SharedHostInventory,
) -> Result<
    (
        ResidentResourceUsage,
        ResidentResourceUsage,
        ResidentResourceUsage,
        u64,
    ),
    String,
> {
    sample.ensure_fresh(host_id)?;
    let rows = client.query("SELECT host_id,workspace_id,memory_bytes,cpu_milli,pids,workspace_bytes,container_id,state FROM runtime.resident_sandboxes", &[])
        .map_err(|error| format!("load shared resident declarations failed: {error}"))?;
    let mut global = ResidentResourceUsage::default();
    let mut tenant_used = ResidentResourceUsage::default();
    let mut host_used = ResidentResourceUsage::default();
    let mut unobserved_host_memory = 0u64;
    for row in rows {
        let declared = usage(&row, 2)?;
        let on_host = row.get::<_, String>(0) == host_id;
        let confirmed = row.get::<_, String>(7) == "resident";
        let observed = if on_host && confirmed {
            row.get::<_, Option<String>>(6)
                .as_ref()
                .and_then(|id| sample.measured_by_container.get(id))
                .copied()
        } else {
            None
        };
        let resources = crate::shared_resident_admission::charge(declared, observed)?;
        global = global.checked_add(resources)?;
        if on_host {
            host_used = host_used.checked_add(resources)?;
            if observed.is_none() {
                unobserved_host_memory = unobserved_host_memory
                    .checked_add(declared.memory_bytes)
                    .ok_or("shared unknown creation memory overflow")?;
            }
        }
        if row.get::<_, String>(1) == tenant {
            tenant_used = tenant_used.checked_add(resources)?;
        }
    }
    sample.ensure_fresh(host_id)?;
    Ok((global, tenant_used, host_used, unobserved_host_memory))
}

// Persist the complete ownership and resource declaration in one reservation.
#[allow(clippy::too_many_arguments)]
pub(super) fn insert(
    client: &mut impl GenericClient,
    host_id: &str,
    execution_id: &str,
    agent_run_id: Option<&str>,
    workspace_id: &str,
    container_id: Option<&str>,
    state: &str,
    resources: ResidentResourceUsage,
) -> Result<(), String> {
    let value = |number: u64| i64::try_from(number).map_err(|error| error.to_string());
    client.execute("INSERT INTO resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,instance_id,environment_id,allocation_mode) SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,i.instance_id,i.environment_id,COALESCE(i.allocation_mode,'unbound') FROM (SELECT 1) seed LEFT JOIN runtime.execution_instances i ON i.execution_id=$2 AND i.allocation_execution_id=i.execution_id",
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
        let run_id = match &sandbox.allocation_owner {
            centaeris_core::execution::EnvironmentTaskOwner::AgentRun { run_id } => {
                Some(run_id.as_str())
            }
            centaeris_core::execution::EnvironmentTaskOwner::MaintenanceOperation { .. } => None,
        };
        let tenant = if let Some(run) = run_id {
            tx.query_opt(
                "SELECT workspace_id FROM execution_job_tenants WHERE job_id=$1",
                &[&agent_run_lifecycle_job_id(run)?],
            )
            .map_err(|e| e.to_string())?
            .ok_or("managed resident sandbox has unknown tenant; admission blocked")?
            .get::<_, String>(0)
        } else {
            tx.query_opt("SELECT s.workspace_id FROM runtime.execution_instances i JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id WHERE i.execution_id=$1 AND i.agent_run_id IS NULL AND i.allocation_owner_json::jsonb=$2::text::jsonb AND i.allocation_execution_id=i.execution_id FOR SHARE OF i,s",&[&sandbox.execution_id,&serde_json::to_string(&sandbox.allocation_owner).map_err(|_|"allocation_owner_invalid")?]).map_err(|e|e.to_string())?.ok_or("managed maintenance sandbox has no durable reservation")?.get(0)
        };
        let existing = tx.query_opt("SELECT agent_run_id,workspace_id,container_id,memory_bytes,cpu_milli,pids,workspace_bytes,allocation_owner_json FROM resident_sandboxes WHERE host_id=$1 AND execution_id=$2", &[&host_id, &sandbox.execution_id]).map_err(|error| error.to_string())?;
        if let Some(row) = existing {
            if row.get::<_, Option<String>>(0).as_deref() != run_id
                || serde_json::from_str::<centaeris_core::execution::EnvironmentTaskOwner>(
                    &row.get::<_, String>(7),
                )
                .map_err(|_| "allocation_owner_invalid")?
                    != sandbox.allocation_owner
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
                run_id,
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
