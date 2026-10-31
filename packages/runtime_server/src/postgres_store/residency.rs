use centaeris_core::execution::{admit_resident_resources, ResidentResourceUsage};
use centaeris_core::session::reliability::agent_run_lifecycle_job_id;
use centaeris_core::session::RuntimeJobLeaseFence;
use postgres::{Client, GenericClient};
use std::collections::{HashMap, HashSet};

use super::PostgresRuntimeStore;
mod retained;
pub(crate) mod retirement;
use crate::resident_capacity::{
    ObservedResidentSandbox, PreparingResidentSandbox, ResidentCapacity, ResidentInventorySource,
    RESIDENT_CAPACITY_WAIT,
};

// Serialize inventory reconciliation and admission. Retained preparation releases
// this gate after its durable reservation; maintenance keeps its existing scope.
struct ResidentHostLock<'a>(&'a mut Client, String, Option<crate::observations::Span>);
#[cfg(test)]
mod cost_tests;
mod preparation_wait;

impl Drop for ResidentHostLock<'_> {
    fn drop(&mut self) {
        match self.0.query_one("SELECT pg_advisory_unlock(731946,3)", &[]) {
            Ok(row) if row.get::<_, bool>(0) => {
                if let Some(mut held) = self.2.take() {
                    held.finished(true);
                    drop(held);
                }
                // Eligibility changes without refunding any resident or unknown creation.
                // A failed notification leaves the existing durable timed retry intact.
                if let Err(error) = preparation_wait::signal(self.0, &self.1) {
                    eprintln!("host preparation wake unavailable: {error}");
                }
                #[cfg(test)]
                AFTER_GLOBAL_UNLOCK.with(|pending| {
                    if let Some(hook) = pending.borrow_mut().take() {
                        hook();
                    }
                });
            }
            _ => {
                if let Some(mut held) = self.2.take() {
                    held.finished(false);
                    drop(held);
                }
                eprintln!("resident host admission unlock unavailable");
            }
        }
    }
}

/// One environment's complete physical preparation, across committed SQL phases.
/// This session gate is acquired before global admission and never waits for it.
pub(super) struct EnvironmentPreparationLock<'a> {
    pub(super) client: &'a mut Client,
    environment_id: String,
    host_id: String,
    pub(super) acquired_global: bool,
}
impl Drop for EnvironmentPreparationLock<'_> {
    fn drop(&mut self) {
        match self.client.query_one(
            "SELECT pg_advisory_unlock(731947,hashtext($1))",
            &[&self.environment_id],
        ) {
            Ok(row) if row.get::<_, bool>(0) => {
                // Release the environment gate after its actual global owner,
                // including failed preparation and maintenance. The second hint
                // closes the global-unlock/environment-unlock registration race.
                // A failed global try must never broadcast temporary gate drops.
                if self.acquired_global {
                    if let Err(error) = preparation_wait::signal(self.client, &self.host_id) {
                        eprintln!("environment preparation wake unavailable: {error}");
                    }
                }
            }
            _ => eprintln!("environment preparation unlock unavailable"),
        }
    }
}
fn try_lock_environment(client: &mut Client, environment_id: &str) -> Result<bool, String> {
    client
        .query_one(
            "SELECT pg_try_advisory_lock(731947,hashtext($1))",
            &[&environment_id],
        )
        .map(|row| row.get(0))
        .map_err(|error| format!("environment preparation lock failed: {error}"))
}
pub(super) fn lock_environment<'a>(
    client: &'a mut Client,
    host_id: &str,
    environment_id: &str,
) -> Result<EnvironmentPreparationLock<'a>, String> {
    if !try_lock_environment(client, environment_id)? {
        return Err(RESIDENT_CAPACITY_WAIT.into());
    }
    Ok(EnvironmentPreparationLock {
        client,
        environment_id: environment_id.into(),
        host_id: host_id.into(),
        acquired_global: false,
    })
}
fn lock_environment_for_run<'a>(
    client: &'a mut Client,
    host_id: &str,
    environment_id: &str,
    fence: &RuntimeJobLeaseFence,
) -> Result<EnvironmentPreparationLock<'a>, String> {
    if !try_lock_environment(client, environment_id)? {
        // A pinned same-Job replay may be unable to write a marker. Keep its
        // existing capacity waiter and the normal timed retry in that case.
        let _registered = preparation_wait::register_environment(client, host_id, fence)?;
        // Registration cannot miss an unlock that occurred before it committed.
        if !try_lock_environment(client, environment_id)? {
            return Err(crate::resident_capacity::HOST_PREPARATION_WAIT.into());
        }
    }
    Ok(EnvironmentPreparationLock {
        client,
        environment_id: environment_id.into(),
        host_id: host_id.into(),
        acquired_global: false,
    })
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
    Ok(ResidentHostLock(
        client,
        host_id.into(),
        Some(crate::observations::Span::new("globalHold")),
    ))
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
    acquired_global: &mut bool,
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
    let lock = ResidentHostLock(
        client,
        host_id.into(),
        Some(crate::observations::Span::new("globalHold")),
    );
    *acquired_global = true;
    preparation_wait::clear(lock.0, fence)?;
    Ok(lock)
}

#[cfg(test)]
thread_local! {
    static AFTER_GLOBAL_UNLOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn after_global_preparation_unlock(hook: impl FnOnce() + 'static) {
    AFTER_GLOBAL_UNLOCK.with(|pending| {
        assert!(pending.borrow().is_none());
        *pending.borrow_mut() = Some(Box::new(hook));
    });
}

/// Freeze pending declarations before the physical sample can become stale.
struct ResidentInventorySnapshot {
    pending_executions: Vec<String>,
    preparing: Vec<PreparingResidentSandbox>,
}
impl ResidentInventorySnapshot {
    fn freeze(
        client: &mut Client,
        host_id: &str,
        caller_environment: Option<&str>,
    ) -> Result<Self, String> {
        let rows = client.query("SELECT s.execution_id,s.host_id,s.container_id,s.memory_bytes,s.cpu_milli,s.pids,s.workspace_bytes,i.instance_id,i.environment_id,i.image_digest,i.profile_digest,i.allocation_owner_json FROM runtime.resident_sandboxes s LEFT JOIN runtime.execution_instances i ON i.execution_id=s.execution_id AND i.allocation_execution_id=i.execution_id AND i.instance_id=s.instance_id AND i.environment_id=s.environment_id AND i.allocation_mode='retained' AND i.allocation_owner_json::jsonb=s.allocation_owner_json::jsonb WHERE s.state='reserved'", &[]).map_err(|error| format!("freeze pending resident inventory failed: {error}"))?;
        let mut snapshot = Self {
            pending_executions: Vec::new(),
            preparing: Vec::new(),
        };
        for row in rows {
            let execution_id: String = row.get(0);
            snapshot.pending_executions.push(execution_id.clone());
            if row.get::<_, String>(1) != host_id {
                continue;
            }
            let Some(environment_id) = row.get::<_, Option<String>>(8) else {
                continue;
            };
            if caller_environment == Some(environment_id.as_str()) {
                continue;
            }
            // Probing another gate never waits or sends a release notification.
            // Exclude our own gate because advisory locks are session-reentrant.
            if try_lock_environment(client, &environment_id)? {
                let unlocked = client
                    .query_one(
                        "SELECT pg_advisory_unlock(731947,hashtext($1))",
                        &[&environment_id],
                    )
                    .map_err(|error| format!("pending environment probe unlock failed: {error}"))?
                    .get::<_, bool>(0);
                if !unlocked {
                    return Err("pending environment probe unlock rejected".into());
                }
                continue;
            }
            snapshot.preparing.push(PreparingResidentSandbox {
                execution_id,
                allocation_owner: serde_json::from_str(&row.get::<_, String>(11))
                    .map_err(|_| "allocation_owner_invalid")?,
                instance_id: row.get(7),
                environment_id,
                image_digest: row.get(9),
                profile_digest: row
                    .get::<_, Option<String>>(10)
                    .ok_or("preparing resident profile missing; admission blocked")?,
                container_id: row.get(2),
                resources: usage(&row, 3)?,
            });
        }
        Ok(snapshot)
    }
}

#[cfg(test)]
thread_local! {
    static AFTER_RESIDENT_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn after_resident_preparation_commit(hook: impl FnOnce() + 'static) {
    AFTER_RESIDENT_COMMIT.with(|pending| {
        assert!(pending.borrow().is_none());
        *pending.borrow_mut() = Some(Box::new(hook));
    });
}
#[cfg(test)]
fn after_resident_preparation_commit_hook() {
    AFTER_RESIDENT_COMMIT.with(|pending| {
        if let Some(hook) = pending.borrow_mut().take() {
            hook();
        }
    });
}

pub(super) fn with_environment_inventory<T>(
    environment: &mut EnvironmentPreparationLock<'_>,
    inventory: impl ResidentInventorySource<Vec<ObservedResidentSandbox>>,
    operation: impl FnOnce(&mut Client, &[ObservedResidentSandbox]) -> Result<T, String>,
) -> Result<T, String> {
    let lock = lock_host(environment.client, &environment.host_id)?;
    environment.acquired_global = true;
    let snapshot = ResidentInventorySnapshot::freeze(
        lock.0,
        &environment.host_id,
        Some(&environment.environment_id),
    )?;
    let physical = inventory.collect(&snapshot.preparing)?;
    reconcile(lock.0, &environment.host_id, &physical, &snapshot)?;
    operation(lock.0, &physical)
}

pub(super) fn with_host_inventory<T>(
    client: &mut Client,
    host_id: &str,
    inventory: impl ResidentInventorySource<Vec<ObservedResidentSandbox>>,
    operation: impl FnOnce(&mut Client, &[ObservedResidentSandbox]) -> Result<T, String>,
) -> Result<T, String> {
    let lock = lock_host(client, host_id)?;
    let snapshot = ResidentInventorySnapshot::freeze(lock.0, host_id, None)?;
    let physical = inventory.collect(&snapshot.preparing)?;
    reconcile(lock.0, host_id, &physical, &snapshot)?;
    operation(lock.0, &physical)
}

impl PostgresRuntimeStore {
    pub(crate) fn reconcile_resident_sandboxes(
        &self,
        host_id: &str,
        inventory: impl ResidentInventorySource<Vec<ObservedResidentSandbox>>,
    ) -> Result<(), String> {
        self.with_client(|client| {
            let lock = lock_host(client, host_id)?;
            let snapshot = ResidentInventorySnapshot::freeze(lock.0, host_id, None)?;
            reconcile(
                lock.0,
                host_id,
                &inventory.collect(&snapshot.preparing)?,
                &snapshot,
            )
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
    snapshot: &ResidentInventorySnapshot,
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
    let rows = client.query("SELECT host_id,workspace_id,memory_bytes,cpu_milli,pids,workspace_bytes,container_id,state,execution_id FROM runtime.resident_sandboxes", &[])
        .map_err(|error| format!("load shared resident declarations failed: {error}"))?;
    let mut global = ResidentResourceUsage::default();
    let mut tenant_used = ResidentResourceUsage::default();
    let mut host_used = ResidentResourceUsage::default();
    let mut unobserved_host_memory = 0u64;
    let pending = snapshot
        .pending_executions
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    for row in rows {
        let declared = usage(&row, 2)?;
        let on_host = row.get::<_, String>(0) == host_id;
        let confirmed = row.get::<_, String>(7) == "resident";
        let observed =
            if on_host && confirmed && !pending.contains(row.get::<_, String>(8).as_str()) {
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

/// Confirm only the original reservation's first known physical incarnation.
fn confirm_reserved_resident(
    client: &mut impl GenericClient,
    host_id: &str,
    execution_id: &str,
    container_id: &str,
) -> Result<bool, String> {
    client.execute("UPDATE runtime.resident_sandboxes SET container_id=$3,state='resident',updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE host_id=$1 AND execution_id=$2 AND state='reserved' AND (container_id IS NULL OR container_id=$3)", &[&host_id, &execution_id, &container_id])
        .map(|changed| changed == 1)
        .map_err(|error| format!("confirm resident reservation failed: {error}"))
}

struct ResidentDeclaration {
    run_id: Option<String>,
    workspace_id: String,
    container_id: Option<String>,
    resources: ResidentResourceUsage,
    allocation_owner: centaeris_core::execution::EnvironmentTaskOwner,
    confirmed: bool,
}

impl ResidentDeclaration {
    fn from_row(row: &postgres::Row) -> Result<Self, String> {
        Ok(Self {
            run_id: row.get(0),
            workspace_id: row.get(1),
            container_id: row.get(2),
            resources: usage(row, 3)?,
            allocation_owner: serde_json::from_str(&row.get::<_, String>(7))
                .map_err(|_| "allocation_owner_invalid")?,
            confirmed: matches!(row.get::<_, String>(8).as_str(), "resident" | "releasing"),
        })
    }
}

fn reconcile(
    client: &mut Client,
    host_id: &str,
    inventory: &[ObservedResidentSandbox],
    snapshot: &ResidentInventorySnapshot,
) -> Result<(), String> {
    if host_id.trim().is_empty() {
        return Err("resident host identity missing".into());
    }
    let mut tx = client.transaction().map_err(|error| error.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    let mut newly_confirmed = false;
    let preparing = snapshot
        .preparing
        .iter()
        .map(|pending| (pending.execution_id.as_str(), pending))
        .collect::<HashMap<_, _>>();
    // Run declarations share the existing admission gate. Maintenance retains
    // its locking read below, before reading the corresponding declaration.
    let executions = inventory
        .iter()
        .filter(|sandbox| !preparing.contains_key(sandbox.execution_id.as_str()))
        .filter(|sandbox| {
            matches!(
                sandbox.allocation_owner,
                centaeris_core::execution::EnvironmentTaskOwner::AgentRun { .. }
            )
        })
        .map(|sandbox| sandbox.execution_id.as_str())
        .collect::<Vec<_>>();
    let jobs = inventory
        .iter()
        .filter(|sandbox| !preparing.contains_key(sandbox.execution_id.as_str()))
        .filter_map(|sandbox| match &sandbox.allocation_owner {
            centaeris_core::execution::EnvironmentTaskOwner::AgentRun { run_id } => {
                Some(agent_run_lifecycle_job_id(run_id))
            }
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tenants = if jobs.is_empty() {
        HashMap::new()
    } else {
        tx.query(
            "SELECT job_id,workspace_id FROM execution_job_tenants WHERE job_id=ANY($1)",
            &[&jobs],
        )
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect::<HashMap<_, _>>()
    };
    let mut declarations = if executions.is_empty() {
        HashMap::new()
    } else {
        tx.query("SELECT agent_run_id,workspace_id,container_id,memory_bytes,cpu_milli,pids,workspace_bytes,allocation_owner_json,state,execution_id FROM resident_sandboxes WHERE host_id=$1 AND execution_id=ANY($2)", &[&host_id,&executions])
            .map_err(|error| error.to_string())?
            .iter()
            .map(|row| Ok((row.get::<_, String>(9), ResidentDeclaration::from_row(row)?)))
            .collect::<Result<HashMap<_, _>, String>>()?
    };
    for sandbox in inventory {
        if let Some(preparing) = preparing.get(sandbox.execution_id.as_str()) {
            if preparing.allocation_owner != sandbox.allocation_owner
                || preparing.resources != sandbox.resources
                || preparing
                    .container_id
                    .as_ref()
                    .is_some_and(|id| id != &sandbox.container_id)
            {
                return Err(
                    "managed preparing sandbox disagrees with reservation; admission blocked"
                        .into(),
                );
            }
            // Its creator owns the final resident commit. Updating this row here
            // could wait for its physical transaction while holding global admission.
            continue;
        }
        let run_id = match &sandbox.allocation_owner {
            centaeris_core::execution::EnvironmentTaskOwner::AgentRun { run_id } => {
                Some(run_id.as_str())
            }
            centaeris_core::execution::EnvironmentTaskOwner::MaintenanceOperation { .. } => None,
        };
        let tenant = if let Some(run) = run_id {
            tenants
                .get(&agent_run_lifecycle_job_id(run)?)
                .ok_or("managed resident sandbox has unknown tenant; admission blocked")?
                .clone()
        } else {
            tx.query_opt("SELECT s.workspace_id FROM runtime.execution_instances i JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id WHERE i.execution_id=$1 AND i.agent_run_id IS NULL AND i.allocation_owner_json::jsonb=$2::text::jsonb AND i.allocation_execution_id=i.execution_id FOR SHARE OF i,s",&[&sandbox.execution_id,&serde_json::to_string(&sandbox.allocation_owner).map_err(|_|"allocation_owner_invalid")?]).map_err(|e|e.to_string())?.ok_or("managed maintenance sandbox has no durable reservation")?.get(0)
        };
        let existing = if run_id.is_some() {
            declarations.remove(&sandbox.execution_id)
        } else {
            tx.query_opt("SELECT agent_run_id,workspace_id,container_id,memory_bytes,cpu_milli,pids,workspace_bytes,allocation_owner_json,state FROM resident_sandboxes WHERE host_id=$1 AND execution_id=$2", &[&host_id, &sandbox.execution_id])
                .map_err(|error| error.to_string())?
                .as_ref().map(ResidentDeclaration::from_row).transpose()?
        };
        let declaration = if let Some(mut row) = existing {
            if row.run_id.as_deref() != run_id
                || row.allocation_owner != sandbox.allocation_owner
                || row.workspace_id != tenant
                || row
                    .container_id
                    .as_ref()
                    .is_some_and(|id| id != &sandbox.container_id)
                || row.resources != sandbox.resources
            {
                return Err(
                    "managed resident sandbox disagrees with reservation; admission blocked".into(),
                );
            }
            // A verified unchanged fact needs no write. In particular, ordinary
            // task/controller pins must not make another environment hold the
            // global preparation gate while refreshing an unused timestamp.
            if row.container_id.as_deref() != Some(sandbox.container_id.as_str()) || !row.confirmed
            {
                if confirm_reserved_resident(
                    &mut tx,
                    host_id,
                    &sandbox.execution_id,
                    &sandbox.container_id,
                )? {
                    newly_confirmed = true;
                } else {
                    let updated = tx.execute("UPDATE resident_sandboxes SET container_id=$3,updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE host_id=$1 AND execution_id=$2 AND state IN('resident','releasing') AND (container_id IS NULL OR container_id=$3)", &[&host_id, &sandbox.execution_id, &sandbox.container_id]).map_err(|error| error.to_string())?;
                    if updated != 1 {
                        return Err("resident reservation changed during confirmation".into());
                    }
                }
                row.container_id = Some(sandbox.container_id.clone());
                row.confirmed = true;
            }
            row
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
            ResidentDeclaration {
                run_id: run_id.map(str::to_owned),
                workspace_id: tenant,
                container_id: Some(sandbox.container_id.clone()),
                resources: sandbox.resources,
                allocation_owner: sandbox.allocation_owner.clone(),
                confirmed: true,
            }
        };
        if run_id.is_some() {
            declarations.insert(sandbox.execution_id.clone(), declaration);
        }
    }
    if newly_confirmed {
        super::capacity_wait::resident_confirmed(&mut tx, host_id)?;
    }
    // No timeout releases reserved/unknown creations. A confirmed resident can
    // be reconciled absent only if it was already confirmed before this sample.
    let present = inventory
        .iter()
        .map(|sandbox| sandbox.execution_id.clone())
        .collect::<Vec<_>>();
    // A busy resident remains charged. Skipping its row is a deferred cleanup
    // decision, never proof that its physical instance is absent or refundable.
    tx.execute("WITH absent AS (SELECT host_id,execution_id FROM resident_sandboxes WHERE host_id=$1 AND state='resident' AND NOT(execution_id=ANY($2)) AND NOT(execution_id=ANY($3)) FOR UPDATE SKIP LOCKED) DELETE FROM resident_sandboxes s USING absent a WHERE s.host_id=a.host_id AND s.execution_id=a.execution_id", &[&host_id, &present, &snapshot.pending_executions]).map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}
