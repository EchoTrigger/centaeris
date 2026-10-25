//! Durable running-slot accounting for retained native environments.
//! Paused residents retain every memory, PID, storage and residency debit.
use super::execution_instances::RetainedInstanceAllocation;
use super::PostgresRuntimeStore;
use crate::agent_run_authorization::EnvironmentProfile;
use postgres::{GenericClient, Row};

pub(crate) const RUNSC_CAPACITY_WAIT: &str = "runsc_environment_capacity_wait";
const PAGE_SIZE: usize = 64;
const RUNNING_LOCK: &str = "SELECT pg_advisory_xact_lock(731946,101)";
pub(super) const COLUMNS: &str = "environment_id:text:NO,host_id:text:NO,allocation_execution_id:text:NO,instance_id:text:NO,container_id:text:YES,state:text:NO,controller_epoch:bigint:NO,idle_since_ms:bigint:YES,updated_at_ms:bigint:NO";
pub(super) const DDL: &str = r#"
CREATE TABLE runtime.runsc_environment_power(
 environment_id text PRIMARY KEY,host_id text NOT NULL,allocation_execution_id text NOT NULL,
 instance_id text NOT NULL,container_id text,state text NOT NULL,
 controller_epoch bigint NOT NULL DEFAULT 0,idle_since_ms bigint,updated_at_ms bigint NOT NULL,
 CONSTRAINT runsc_power_state CHECK(state IN('running','resumePrepared','pauseDispatched','paused')),
 CONSTRAINT runsc_power_bounds CHECK(environment_id<>'' AND host_id<>'' AND allocation_execution_id<>'' AND instance_id<>'' AND controller_epoch>=0 AND updated_at_ms>=0 AND (idle_since_ms IS NULL OR idle_since_ms>=0)),
 CONSTRAINT runsc_power_paused_identity CHECK(state NOT IN('paused','pauseDispatched') OR container_id IS NOT NULL),
 CONSTRAINT runsc_power_resident FOREIGN KEY(host_id,allocation_execution_id) REFERENCES runtime.resident_sandboxes(host_id,execution_id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED);
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunscBinding {
    pub environment_id: String,
    pub host_id: String,
    pub allocation_execution_id: String,
    pub instance_id: String,
    pub container_id: Option<String>,
}
impl From<&RetainedInstanceAllocation> for RunscBinding {
    fn from(value: &RetainedInstanceAllocation) -> Self {
        Self {
            environment_id: value.environment_id.clone(),
            host_id: value.host_id.clone(),
            allocation_execution_id: value.allocation_execution_id.clone(),
            instance_id: value.instance_id.clone(),
            container_id: value.container_id.clone(),
        }
    }
}
impl RunscBinding {
    fn validate(&self) -> Result<(), String> {
        if [
            &self.environment_id,
            &self.host_id,
            &self.allocation_execution_id,
            &self.instance_id,
        ]
        .into_iter()
        .any(|v| v.trim().is_empty())
            || self
                .container_id
                .as_ref()
                .is_some_and(|v| v.trim().is_empty())
        {
            return Err("runsc_power_binding_invalid".into());
        }
        Ok(())
    }
}

fn exact_binding() -> &'static str {
    "p.environment_id=s.environment_id AND p.host_id=s.host_id AND p.allocation_execution_id=s.execution_id AND p.instance_id=s.instance_id AND p.container_id IS NOT DISTINCT FROM s.container_id"
}
/// Unregistered and uncertain native residents remain charged across restart.
pub(super) fn running_count(db: &mut impl GenericClient, host: &str) -> Result<i64, String> {
    db.query_one(&format!("SELECT count(*) FROM runtime.resident_sandboxes s WHERE s.host_id=$1 AND s.allocation_mode='retained' AND NOT EXISTS(SELECT 1 FROM runtime.runsc_environment_power p WHERE {} AND p.state='paused')",exact_binding()),&[&host]).map(|r|r.get(0)).map_err(|e|e.to_string())
}

fn power_state(db: &mut impl GenericClient, b: &RunscBinding) -> Result<Option<String>, String> {
    let row=db.query_opt("SELECT host_id,allocation_execution_id,instance_id,container_id,state FROM runtime.runsc_environment_power WHERE environment_id=$1 FOR UPDATE",&[&b.environment_id]).map_err(|e|e.to_string())?;
    row.map(|r| {
        if r.get::<_, String>(0) != b.host_id
            || r.get::<_, String>(1) != b.allocation_execution_id
            || r.get::<_, String>(2) != b.instance_id
            || (r.get::<_, Option<String>>(3).is_some()
                && r.get::<_, Option<String>>(3) != b.container_id)
        {
            return Err("runsc_power_incarnation_changed".into());
        }
        Ok(r.get(4))
    })
    .transpose()
}

/// Call inside the existing verified controller/resident reservation transaction.
/// The caller must insert a new resident before commit (the FK is deferred).
pub(super) fn reserve_running(
    db: &mut impl GenericClient,
    b: &RunscBinding,
    limit: u32,
) -> Result<(), String> {
    b.validate()?;
    if limit == 0 {
        return Err("runsc_running_limit_invalid".into());
    }
    db.query_one(RUNNING_LOCK, &[]).map_err(|e| e.to_string())?;
    let state = power_state(db, b)?;
    let existing=db.query_opt("SELECT environment_id,instance_id,container_id FROM runtime.resident_sandboxes WHERE host_id=$1 AND execution_id=$2 FOR SHARE",&[&b.host_id,&b.allocation_execution_id]).map_err(|e|e.to_string())?;
    if existing.as_ref().is_some_and(|r| {
        r.get::<_, Option<String>>(0).as_deref() != Some(&b.environment_id)
            || r.get::<_, Option<String>>(1).as_deref() != Some(&b.instance_id)
            || r.get::<_, Option<String>>(2) != b.container_id
    }) {
        return Err("runsc_power_resident_binding_changed".into());
    }
    let already_charged = existing.is_some() && state.as_deref() != Some("paused");
    let used = running_count(db, &b.host_id)?;
    let prior_running_reservation = matches!(state.as_deref(), Some("running" | "resumePrepared"));
    if (!already_charged && used >= i64::from(limit))
        || (already_charged && !prior_running_reservation && used > i64::from(limit))
    {
        return Err(RUNSC_CAPACITY_WAIT.into());
    }
    db.execute("INSERT INTO runtime.runsc_environment_power(environment_id,host_id,allocation_execution_id,instance_id,container_id,state,controller_epoch,idle_since_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,'resumePrepared',0,NULL,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint) ON CONFLICT(environment_id) DO UPDATE SET state=CASE WHEN runsc_environment_power.state='running' THEN 'running' ELSE 'resumePrepared' END,idle_since_ms=NULL,updated_at_ms=EXCLUDED.updated_at_ms",&[&b.environment_id,&b.host_id,&b.allocation_execution_id,&b.instance_id,&b.container_id]).map_err(|e|e.to_string())?;
    Ok(())
}

/// Record only the original exact container after its physical running observation.
pub(super) fn observe_running(
    db: &mut impl GenericClient,
    b: &RunscBinding,
    container_id: &str,
) -> Result<(), String> {
    b.validate()?;
    if container_id.trim().is_empty() || b.container_id.as_ref().is_some_and(|v| v != container_id)
    {
        return Err("runsc_power_incarnation_changed".into());
    }
    db.query_one(RUNNING_LOCK, &[]).map_err(|e| e.to_string())?;
    power_state(db, b)?.ok_or("runsc_running_reservation_missing")?;
    let changed=db.execute("UPDATE runtime.runsc_environment_power p SET container_id=$5,state='running',idle_since_ms=NULL,updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE environment_id=$1 AND host_id=$2 AND allocation_execution_id=$3 AND instance_id=$4 AND (container_id IS NULL OR container_id=$5) AND EXISTS(SELECT 1 FROM runtime.resident_sandboxes s WHERE s.host_id=$2 AND s.execution_id=$3 AND s.environment_id=$1 AND s.instance_id=$4 AND s.container_id=$5 AND s.state='resident')",&[&b.environment_id,&b.host_id,&b.allocation_execution_id,&b.instance_id,&container_id]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("runsc_running_observation_binding_rejected".into());
    }
    Ok(())
}

// Accepted work blocks idle selection even before its lifecycle Job is created.
fn demand() -> &'static str {
    "EXISTS(SELECT 1 FROM public.app_core_agentrun r WHERE r.environment_id=e.id AND r.status IN('queued','running') AND r.\"preAdmissionCancelledAt\" IS NULL AND NOT EXISTS(SELECT 1 FROM public.app_core_sessionevent t WHERE t.agent_run_id=r.id AND t.session_id=r.session_id AND NOT t.session_level AND t.payload->>'type' IN('agent_run_completed','agent_run_failed','agent_run_interrupted')))"
}
fn eligible() -> String {
    format!("e.controller_released AND NOT ({}) AND s.state='resident' AND s.allocation_mode='retained' AND s.container_id IS NOT NULL AND e.environment_profile->>'schema'='runtime.environment_profile.v1' AND NOT EXISTS(SELECT 1 FROM runtime.environment_retirements d WHERE d.environment_id=e.id)",demand())
}
fn allocation_columns() -> &'static str {
    "i.execution_id,i.agent_run_id,i.allocation_owner_json,i.instance_id,i.environment_id,i.host_id,i.image_digest,i.authorization_digest,i.binding_digest,i.profile_digest,s.container_id,e.environment_profile::text,e.controller_epoch"
}
fn decode(row: &Row) -> Result<(RetainedInstanceAllocation, EnvironmentProfile, i64), String> {
    let profile: EnvironmentProfile = serde_json::from_str(&row.get::<_, String>(11))
        .map_err(|_| "runsc_idle_profile_invalid")?;
    if profile.digest()? != row.get::<_, String>(9)
        || profile.image_digest != row.get::<_, String>(6)
    {
        return Err("runsc_idle_profile_binding_changed".into());
    }
    Ok((
        RetainedInstanceAllocation {
            allocation_execution_id: row.get(0),
            allocation_agent_run_id: row.get(1),
            allocation_owner: serde_json::from_str(&row.get::<_, String>(2))
                .map_err(|_| "runsc_idle_owner_invalid")?,
            instance_id: row.get(3),
            environment_id: row.get(4),
            host_id: row.get(5),
            image_digest: row.get(6),
            authorization_digest: row.get(7),
            binding_digest: row.get(8),
            profile_digest: row.get(9),
            container_id: row.get(10),
            reused: true,
        },
        profile,
        row.get(12),
    ))
}
fn targets_from() -> &'static str {
    "runtime.resident_sandboxes s JOIN runtime.execution_instances i ON i.execution_id=s.execution_id AND i.instance_id=s.instance_id AND i.allocation_execution_id=i.execution_id JOIN public.app_core_sessionenvironment e ON e.id=s.environment_id AND e.session_id=i.session_id"
}
#[derive(Debug, Clone)]
pub(crate) struct RunscRotationCandidate {
    pub allocation: RetainedInstanceAllocation,
    pub profile: EnvironmentProfile,
    pub controller_epoch: i64,
    pub idle_since_ms: i64,
}
pub(crate) struct RunscRotationPage {
    pub candidates: Vec<RunscRotationCandidate>,
    pub next: Option<String>,
}
impl PostgresRuntimeStore {
    /// Establish continuous idle observations in a bounded cursor page.
    pub(crate) fn runsc_idle_candidates(
        &self,
        host: &str,
        after: Option<&str>,
        idle_timeout_ms: u64,
    ) -> Result<RunscRotationPage, String> {
        let timeout = i64::try_from(idle_timeout_ms).map_err(|_| "runsc_idle_pause_invalid")?;
        if timeout <= 0 {
            return Err("runsc_idle_pause_invalid".into());
        }
        self.cleanup_view().with_client(|db| {
            let mut tx=db.transaction().map_err(|e|e.to_string())?;
            bound_rotation_transaction(&mut tx)?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let rows=tx.query(&format!("SELECT {},{} AS eligible FROM {} WHERE s.host_id=$1 AND s.allocation_mode='retained' AND s.state='resident' AND s.container_id IS NOT NULL AND ($2::text IS NULL OR e.id>$2) ORDER BY e.id LIMIT $3",allocation_columns(),eligible(),targets_from()),&[&host,&after,&((PAGE_SIZE+1) as i64)]).map_err(|e|e.to_string())?;
            let more=rows.len()>PAGE_SIZE;
            let now:i64=tx.query_one("SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",&[]).map_err(|e|e.to_string())?.get(0);
            let mut candidates=Vec::new();let mut last=None;
            for row in rows.into_iter().take(PAGE_SIZE) {
                let (allocation,profile,epoch)=decode(&row)?;
                let b=RunscBinding::from(&allocation);last=Some(b.environment_id.clone());
                if !row.get::<_,bool>(13) {
                    tx.execute("UPDATE runtime.runsc_environment_power SET idle_since_ms=NULL WHERE environment_id=$1",&[&b.environment_id]).map_err(|e|e.to_string())?;
                    continue;
                }
                let old=power_state(&mut tx,&b)?;
                if old.as_deref()==Some("paused") {continue;}
                tx.execute("INSERT INTO runtime.runsc_environment_power(environment_id,host_id,allocation_execution_id,instance_id,container_id,state,controller_epoch,idle_since_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,'running',$6,$7,$7) ON CONFLICT(environment_id) DO UPDATE SET controller_epoch=$6,idle_since_ms=CASE WHEN runsc_environment_power.controller_epoch<>$6 OR runsc_environment_power.idle_since_ms IS NULL OR runsc_environment_power.idle_since_ms>$7 THEN $7 ELSE runsc_environment_power.idle_since_ms END",&[&b.environment_id,&b.host_id,&b.allocation_execution_id,&b.instance_id,&b.container_id,&epoch,&now]).map_err(|e|e.to_string())?;
                let since:i64=tx.query_one("SELECT idle_since_ms FROM runtime.runsc_environment_power WHERE environment_id=$1",&[&b.environment_id]).map_err(|e|e.to_string())?.get(0);
                if now.saturating_sub(since)>=timeout {candidates.push(RunscRotationCandidate {allocation,profile,controller_epoch:epoch,idle_since_ms:since});}
            }
            tx.commit().map_err(|e|e.to_string())?;
            Ok(RunscRotationPage {candidates,next:if more {last}else{None}})
        })
    }

    /// Persist conservative dispatch, then re-pin authority through the host call.
    /// Callback success must mean the original container is observed paused.
    pub(crate) fn with_runsc_idle_pause(
        &self,
        candidate: &RunscRotationCandidate,
        pause: impl FnOnce(&RetainedInstanceAllocation, &EnvironmentProfile) -> Result<(), String>
            + Send,
    ) -> Result<(), String> {
        self.cleanup_view().with_client(|db| {
            let b=RunscBinding::from(&candidate.allocation);
            let mut tx=db.transaction().map_err(|e|e.to_string())?;
            bound_rotation_transaction(&mut tx)?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            pin_idle(&mut tx,candidate)?;
            let changed=tx.execute("UPDATE runtime.runsc_environment_power SET state='pauseDispatched',updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE environment_id=$1 AND state IN('running','resumePrepared','pauseDispatched') AND controller_epoch=$2 AND idle_since_ms=$3",&[&b.environment_id,&candidate.controller_epoch,&candidate.idle_since_ms]).map_err(|e|e.to_string())?;
            if changed!=1 {return Err("runsc_pause_observation_changed".into());}
            tx.commit().map_err(|e|e.to_string())?;
            let mut tx=db.transaction().map_err(|e|e.to_string())?;
            bound_rotation_transaction(&mut tx)?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            pin_idle(&mut tx,candidate)?;
            if power_state(&mut tx,&b)?.as_deref()!=Some("pauseDispatched") {return Err("runsc_pause_dispatch_changed".into());}
            pause(&candidate.allocation,&candidate.profile)?;
            tx.execute("UPDATE runtime.runsc_environment_power SET state='paused',updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE environment_id=$1",&[&b.environment_id]).map_err(|e|e.to_string())?;
            tx.execute("UPDATE runtime.resident_capacity_clock SET generation=generation+1,changed_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE singleton",&[]).map_err(|e|e.to_string())?;
            tx.query_one("SELECT pg_notify('runtime_job_ready_v1','')",&[]).map_err(|e|e.to_string())?;
            tx.commit().map_err(|e|e.to_string())
        })
    }
}
/// A bounded physical reconciliation page; unknown observations stay charged.
pub(crate) struct RunscReconcilePage {
    pub next: Option<String>,
    pub unconfirmed: usize,
}
impl PostgresRuntimeStore {
    /// Revalidate the complete startup page without dispatching physical mutations.
    pub(crate) fn reconcile_runsc_paused(
        &self,
        host: &str,
        after: Option<&str>,
        observe: impl FnMut(&RetainedInstanceAllocation, &EnvironmentProfile) -> Result<bool, String>
            + Send,
    ) -> Result<RunscReconcilePage, String> {
        self.reconcile_runsc_paused_until(host, after, || false, observe)
    }

    /// Stop admits no further physical observation. Already observed facts may
    /// commit; unobserved rows and their conservative debits remain untouched.
    pub(crate) fn reconcile_runsc_paused_until(
        &self,
        host: &str,
        after: Option<&str>,
        mut should_stop: impl FnMut() -> bool + Send,
        mut observe: impl FnMut(&RetainedInstanceAllocation, &EnvironmentProfile) -> Result<bool, String>
            + Send,
    ) -> Result<RunscReconcilePage, String> {
        self.cleanup_view().with_client(|db| {
            if should_stop() {
                return Ok(RunscReconcilePage { next: after.map(str::to_owned), unconfirmed: 0 });
            }
            let rows = {
                let mut tx = db.transaction().map_err(|e| e.to_string())?;
                bound_rotation_transaction(&mut tx)?;
                let rows = tx.query(&format!("SELECT {} FROM {} JOIN runtime.runsc_environment_power p ON {} WHERE s.host_id=$1 AND s.state='resident' AND p.state IN('paused','pauseDispatched') AND ($2::text IS NULL OR e.id>$2) ORDER BY e.id LIMIT $3", allocation_columns(), targets_from(), exact_binding()), &[&host, &after, &((PAGE_SIZE+1) as i64)]).map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                rows
            };
            let more = rows.len() > PAGE_SIZE;
            let mut last = None;
            let mut unconfirmed = 0;
            let mut stopped = false;
            for row in rows.into_iter().take(PAGE_SIZE) {
                if should_stop() { stopped = true; break; }
                let (allocation, profile, _) = decode(&row)?;
                let b = RunscBinding::from(&allocation);
                let mut tx = db.transaction().map_err(|e| e.to_string())?;
                bound_rotation_transaction(&mut tx)?;
                super::resource_commit::require_durable_resource_commit(&mut tx)?;
                if tx.query_opt("SELECT id FROM public.app_core_sessionenvironment WHERE id=$1 FOR UPDATE", &[&b.environment_id]).map_err(|e| e.to_string())?.is_none() {
                    return Err("runsc_reconcile_environment_missing".into());
                }
                tx.query_one(RUNNING_LOCK, &[]).map_err(|e| e.to_string())?;
                let current = tx.query_opt(&format!("SELECT {} FROM {} WHERE e.id=$1 AND s.host_id=$2 AND s.execution_id=$3 AND s.state='resident' FOR UPDATE OF s", allocation_columns(), targets_from()), &[&b.environment_id, &b.host_id, &b.allocation_execution_id]).map_err(|e| e.to_string())?;
                let Some(current) = current else { last=Some(b.environment_id); continue; };
                let (actual, actual_profile, _) = decode(&current)?;
                if actual != allocation || actual_profile != profile { return Err("runsc_reconcile_incarnation_changed".into()); }
                let state = power_state(&mut tx, &b)?;
                if !matches!(state.as_deref(), Some("paused" | "pauseDispatched")) { last=Some(b.environment_id); continue; }
                // A stop that arrived during a database lock must also prevent
                // the next host request, not merely the next cursor page.
                if should_stop() { stopped = true; break; }
                let physical = observe(&allocation, &profile);
                let next = match physical {
                    Ok(true) => "paused",
                    Ok(false) => "resumePrepared",
                    Err(_) => { unconfirmed += 1; "pauseDispatched" },
                };
                tx.execute("UPDATE runtime.runsc_environment_power SET state=$2,updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE environment_id=$1", &[&b.environment_id, &next]).map_err(|e| e.to_string())?;
                if next == "paused" && state.as_deref() != Some("paused") {
                    tx.execute("UPDATE runtime.resident_capacity_clock SET generation=generation+1,changed_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE singleton", &[]).map_err(|e| e.to_string())?;
                    tx.query_one("SELECT pg_notify('runtime_job_ready_v1','')", &[]).map_err(|e| e.to_string())?;
                }
                tx.commit().map_err(|e| e.to_string())?;
                last = Some(b.environment_id);
            }
            Ok(RunscReconcilePage { next: if more || stopped { last } else { None }, unconfirmed })
        })
    }
}

// Bound background database waits independently of HTTP request deadlines.
// These limits do not cancel an already dispatched Docker operation.
fn bound_rotation_transaction(tx: &mut postgres::Transaction<'_>) -> Result<(), String> {
    tx.batch_execute("SET LOCAL lock_timeout='1000ms'; SET LOCAL statement_timeout='5000ms'")
        .map_err(|e| e.to_string())
}

fn pin_idle(tx: &mut postgres::Transaction<'_>, c: &RunscRotationCandidate) -> Result<(), String> {
    let b = RunscBinding::from(&c.allocation);
    // These are the same authoritative rows that controller acquisition and retirement lock.
    let env=tx.query_opt("SELECT controller_epoch,controller_released FROM public.app_core_sessionenvironment WHERE id=$1 FOR UPDATE",&[&b.environment_id]).map_err(|e|e.to_string())?.ok_or("runsc_idle_environment_missing")?;
    if env.get::<_, i64>(0) != c.controller_epoch || !env.get::<_, bool>(1) {
        return Err("runsc_idle_controller_changed".into());
    }
    let row=tx.query_opt(&format!("SELECT {} FROM {} WHERE e.id=$1 AND s.host_id=$2 AND s.execution_id=$3 AND {} FOR UPDATE OF s",allocation_columns(),targets_from(),eligible()),&[&b.environment_id,&b.host_id,&b.allocation_execution_id]).map_err(|e|e.to_string())?.ok_or("runsc_idle_demand_or_retirement_changed")?;
    let (a, p, epoch) = decode(&row)?;
    if a != c.allocation || p != c.profile || epoch != c.controller_epoch {
        return Err("runsc_idle_incarnation_changed".into());
    }
    Ok(())
}

pub(super) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let columns=db.query("SELECT column_name,data_type,is_nullable FROM information_schema.columns WHERE table_schema='runtime' AND table_name='runsc_environment_power' ORDER BY ordinal_position",&[]).map_err(|e|e.to_string())?.into_iter().map(|r|format!("{}:{}:{}",r.get::<_,String>(0),r.get::<_,String>(1),r.get::<_,String>(2))).collect::<Vec<_>>().join(",");
    if columns != COLUMNS {
        return Err("runsc power schema columns mismatch".into());
    }
    for (name,expected) in [
        ("runsc_power_state", "CHECK ((state = ANY (ARRAY['running'::text, 'resumePrepared'::text, 'pauseDispatched'::text, 'paused'::text])))"),
        ("runsc_power_bounds", "CHECK (((environment_id <> ''::text) AND (host_id <> ''::text) AND (allocation_execution_id <> ''::text) AND (instance_id <> ''::text) AND (controller_epoch >= 0) AND (updated_at_ms >= 0) AND ((idle_since_ms IS NULL) OR (idle_since_ms >= 0))))"),
        ("runsc_power_paused_identity", "CHECK (((state <> ALL (ARRAY['paused'::text, 'pauseDispatched'::text])) OR (container_id IS NOT NULL)))"),
    ] {
        let actual=db.query_opt("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.runsc_environment_power'::regclass AND conname=$1 AND contype='c' AND convalidated",&[&name]).map_err(|e|e.to_string())?.map(|r|r.get::<_,String>(0));
        if actual.as_deref()!=Some(expected) {return Err(format!("runsc power constraint mismatch: {name}: {actual:?}"));}
    }
    let keys:i64=db.query_one("SELECT count(*) FROM pg_constraint WHERE conrelid='runtime.runsc_environment_power'::regclass AND contype='p' AND conkey=ARRAY[1]::smallint[] AND convalidated",&[]).map_err(|e|e.to_string())?.get(0);
    let foreign:i64=db.query_one("SELECT count(*) FROM pg_constraint WHERE conrelid='runtime.runsc_environment_power'::regclass AND contype='f' AND conkey=ARRAY[2,3]::smallint[] AND confrelid='runtime.resident_sandboxes'::regclass AND confkey=ARRAY[1,2]::smallint[] AND confdeltype='c' AND condeferrable AND condeferred AND convalidated",&[]).map_err(|e|e.to_string())?.get(0);
    let checks:i64=db.query_one("SELECT count(*) FROM pg_constraint WHERE conrelid='runtime.runsc_environment_power'::regclass AND contype='c' AND conname IN('runsc_power_state','runsc_power_bounds','runsc_power_paused_identity') AND convalidated",&[]).map_err(|e|e.to_string())?.get(0);
    if keys != 1 || foreign != 1 || checks != 3 {
        return Err("runsc power schema constraints mismatch".into());
    }
    Ok(())
}
#[cfg(test)]
pub(in crate::postgres_store) fn restore_previous_fixture(db: &mut postgres::Client) {
    let exists: bool = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=25)",
            &[],
        )
        .expect("inspect runsc fixture schema")
        .get(0);
    if !exists {
        return;
    }
    assert_eq!(
        db.query_one("SELECT count(*) FROM runtime.runsc_environment_power", &[])
            .expect("inspect runsc power evidence")
            .get::<_, i64>(0),
        0,
        "cannot discard runsc power responsibility"
    );
    let mut tx = db.transaction().expect("begin runsc fixture downgrade");
    tx.batch_execute("LOCK TABLE runtime.schema_migrations,runtime.runsc_environment_power IN ACCESS EXCLUSIVE MODE; DROP TABLE runtime.runsc_environment_power; DELETE FROM runtime.schema_migrations WHERE version=25;").expect("restore empty released schema fixture");
    tx.commit().expect("commit empty fixture restoration");
}
