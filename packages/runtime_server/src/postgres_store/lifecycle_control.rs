//! Bounded control leases on the canonical lifecycle Job.
use super::*;
use centaeris_core::session::reliability::{ClaimDueRuntimeJobsRequest, RuntimeJobRecord};
use postgres::GenericClient;

pub(super) const DDL: &str = r#"
ALTER TABLE runtime.runtime_jobs ADD COLUMN lease_purpose text NOT NULL DEFAULT 'execute' CHECK(lease_purpose IN('execute','control'));
ALTER TABLE runtime.runtime_jobs ADD COLUMN lease_control_stage text;
ALTER TABLE runtime.runtime_jobs ADD CONSTRAINT runtime_jobs_control_stage_check CHECK((lease_purpose='execute' AND lease_control_stage IS NULL) OR (lease_purpose='control' AND lease_control_stage IN('advance','cleanup') AND lease_control_stage IS NOT NULL));
CREATE FUNCTION runtime.protect_lifecycle_lease_purpose() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.lease_owner IS NOT NULL AND NEW.lease_owner=OLD.lease_owner AND
 (NEW.lease_purpose IS DISTINCT FROM OLD.lease_purpose OR NEW.lease_control_stage IS DISTINCT FROM OLD.lease_control_stage)
 THEN RAISE EXCEPTION 'lifecycle lease purpose is immutable'; END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER lifecycle_lease_purpose_facts BEFORE UPDATE ON runtime.runtime_jobs FOR EACH ROW EXECUTE FUNCTION runtime.protect_lifecycle_lease_purpose();
"#;

// The ordinary and control schedulers share durable eligibility.
pub(super) const PENDING: &str = "EXISTS(SELECT 1 FROM runtime.runtime_events c WHERE c.session_id=j.session_id AND c.task_id=substring(j.job_id FROM 21) AND c.event_id IN('agent_run_cancel_requested:'||c.task_id,'agent_run_control_pending:'||c.task_id) AND j.idempotency_key=j.job_id||':'||(c.payload_json::jsonb->>'authorizationDigest'))";
const TERMINAL: &str = r#"(EXISTS(SELECT 1 FROM public.app_core_agentrun r WHERE r.id=substring(j.job_id FROM 21) AND r.session_id=j.session_id AND r."preAdmissionCancelledAt" IS NOT NULL) OR EXISTS(SELECT 1 FROM public.app_core_sessionevent e WHERE e.agent_run_id=substring(j.job_id FROM 21) AND e.session_id=j.session_id AND NOT e.session_level AND e.payload->>'type' IN('agent_run_completed','agent_run_failed','agent_run_interrupted')) OR EXISTS(SELECT 1 FROM runtime.runtime_events e WHERE e.event_id='agent_run_pre_admission_failure:'||substring(j.job_id FROM 21) AND e.session_id=j.session_id))"#;

fn control_candidates() -> String {
    format!("j.job_kind='agent_run.lifecycle' AND j.status='queued' AND j.run_at_ms<=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint AND COALESCE(j.lease_reclaim_not_before_ms,j.run_at_ms)<=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint AND ($1::text IS NULL OR j.job_id=$1) AND {PENDING} AND ({TERMINAL})=$2")
}

pub(super) fn mark_terminal_pending(tx: &mut impl GenericClient, run: &str) -> Result<(), String> {
    tx.execute("INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) SELECT 'agent_run_control_pending:'||$1,session_id,$1,'runtime.agent_run.control_pending.v1',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,'internal',json_build_object('schema','runtime.agent_run.control_pending.v1','agentRunId',$1,'authorizationDigest',substring(idempotency_key FROM length(job_id)+2))::text FROM runtime.runtime_jobs WHERE job_id='agent_run.lifecycle:'||$1 AND job_kind='agent_run.lifecycle' AND session_id IS NOT NULL ON CONFLICT(event_id) DO NOTHING", &[&run]).map_err(|e|e.to_string())?;
    Ok(())
}

impl PostgresRuntimeStore {
    pub(crate) fn verify_lifecycle_completion(&self, id: &str) -> Result<(), String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|e| e.to_string())?;
            require_lifecycle_completion(&mut tx, id)
        })
    }
    pub(crate) fn control_ready(&self, stage: &str) -> Result<bool, String> {
        if !matches!(stage, "advance" | "cleanup") {
            return Err("control stage invalid".into());
        }
        self.with_execution_control_client(|client| {
            client.query_one(&format!("SELECT (SELECT count(*) FROM runtime_jobs WHERE job_kind='agent_run.lifecycle' AND status IN('leased','running') AND lease_purpose='control' AND lease_control_stage=$3)<2 AND EXISTS(SELECT 1 FROM runtime_jobs j WHERE {} )",control_candidates()),&[&None::<String>,&(stage=="cleanup"),&stage]).map(|row|row.get(0)).map_err(|e|e.to_string())
        })
    }
    pub(crate) fn claim_control_jobs(
        &self,
        req: ClaimDueRuntimeJobsRequest,
        stage: &str,
    ) -> Result<Vec<RuntimeJobRecord>, String> {
        if !matches!(stage, "advance" | "cleanup")
            || req.job_kind.as_deref() != Some("agent_run.lifecycle")
        {
            return Err("control claim invalid".into());
        }
        self.with_execution_control_client(|client| {
            let mut tx=client.transaction().map_err(|e|e.to_string())?;
            if !tx.query_one("SELECT pg_try_advisory_xact_lock(731946,2)",&[]).map_err(|e|e.to_string())?.get::<_,bool>(0) { return Err("execution claim busy".into()); }
            let used=tx.query_one("SELECT count(*) FROM runtime_jobs WHERE job_kind='agent_run.lifecycle' AND status IN('leased','running') AND lease_purpose='control' AND lease_control_stage=$1", &[&stage]).map_err(|e|e.to_string())?.get::<_,i64>(0);
            let available=(2i64-used).max(0).min(req.limit as i64);
            if available==0 { return Ok(vec![]); }
            let rows=tx.query(&format!("SELECT j.job_id FROM runtime_jobs j WHERE {} ORDER BY j.run_at_ms,j.created_at_ms,j.job_id FOR UPDATE OF j SKIP LOCKED LIMIT $3",control_candidates()), &[&req.job_id,&(stage=="cleanup"),&available]).map_err(|e|e.to_string())?;
            let now=tx.query_one("SELECT (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint",&[]).map_err(|e|e.to_string())?.get::<_,i64>(0);
            let until=now.saturating_add(i64::try_from(req.lease_ms).map_err(|_|"lease overflow")?);
            let mut jobs=vec![];
            for row in rows {
                let id:String=row.get(0);
                tx.execute("UPDATE runtime_jobs SET status='leased',lease_owner=gen_random_uuid()::text,lease_expires_at_ms=$2,heartbeat_at_ms=$3,updated_at_ms=$3,lease_purpose='control',lease_control_stage=$4 WHERE job_id=$1", &[&id,&until,&now,&stage]).map_err(|e|e.to_string())?;
                jobs.push(super::reliability::load_job(&mut tx,&id)?.ok_or("claimed control Job missing")?);
            }
            tx.commit().map_err(|e|e.to_string())?; Ok(jobs)
        })
    }
    pub(crate) fn start_control_job(&self, id: &str, owner: &str) -> Result<(), String> {
        self.with_execution_control_client(|client| {
            let changed=client.execute("UPDATE runtime_jobs SET status='running',updated_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE job_id=$1 AND job_kind='agent_run.lifecycle' AND status='leased' AND lease_owner=$2 AND lease_purpose='control' AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint", &[&id,&owner]).map_err(|e|e.to_string())?;
            if changed!=1 {return Err("control start rejected".into());} Ok(())
        })
    }
    pub(crate) fn require_execute_lease(&self, id: &str, owner: &str) -> Result<(), String> {
        self.with_client(|client| {
            let valid=client.query_opt("SELECT 1 FROM runtime_jobs WHERE job_id=$1 AND lease_owner=$2 AND status IN('leased','running') AND lease_purpose='execute' AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint", &[&id,&owner]).map_err(|e|e.to_string())?;
            if valid.is_none() {return Err("ordinary execution lease rejected".into());} Ok(())
        })
    }
    pub(crate) fn require_control_lease(
        &self,
        control: &crate::contract::AgentRunControl,
        id: &str,
        owner: &str,
    ) -> Result<(), String> {
        self.with_execution_control_client(|client| {
            let valid=client.query_opt("SELECT 1 FROM runtime_jobs WHERE job_id=$1 AND job_id='agent_run.lifecycle:'||$3 AND job_kind='agent_run.lifecycle' AND lease_owner=$2 AND status='running' AND lease_purpose='control' AND session_id=$4 AND idempotency_key=job_id||':'||$5 AND payload_ref='record:agent_run:'||$3 AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint", &[&id,&owner,&control.agent_run_id,&control.authorization.session_id,&control.authorization_digest]).map_err(|e|e.to_string())?;
            if valid.is_none() { return Err("control lease rejected".into()); } Ok(())
        })
    }
}

pub(super) fn require_control_records(
    tx: &mut impl GenericClient,
    fence: &centaeris_core::session::RuntimeJobLeaseFence,
    events: &[centaeris_core::session::SequencedSessionRecord],
) -> Result<(), String> {
    let row = tx
        .query_opt(
            "SELECT lease_purpose FROM runtime_jobs WHERE job_id=$1 AND lease_owner=$2",
            &[&fence.job_id, &fence.lease_owner],
        )
        .map_err(|e| e.to_string())?
        .ok_or("append lease missing")?;
    if row.get::<_, String>(0) == "control" {
        use centaeris_core::session::SessionRecordType;
        if events.is_empty()
            || events.iter().any(|item| {
                !matches!(
                    item.event.event_type,
                    SessionRecordType::AgentRunExecutionEnded
                        | SessionRecordType::AgentRunInterrupted
                )
            })
        {
            return Err("control lease cannot append execution records".into());
        }
        let pending = tx
            .query_one(
                &format!("SELECT {PENDING} FROM runtime_jobs j WHERE j.job_id=$1"),
                &[&fence.job_id],
            )
            .map_err(|e| e.to_string())?
            .get::<_, bool>(0);
        if !pending {
            return Err("control terminal evidence missing".into());
        }
    }
    Ok(())
}

pub(super) fn require_lifecycle_completion(
    tx: &mut impl GenericClient,
    id: &str,
) -> Result<(), String> {
    let row = tx
        .query_one(
            "SELECT job_kind,session_id,idempotency_key FROM runtime_jobs WHERE job_id=$1",
            &[&id],
        )
        .map_err(|e| e.to_string())?;
    if row.get::<_, String>(0) != "agent_run.lifecycle" {
        return Ok(());
    }
    let run = id
        .strip_prefix("agent_run.lifecycle:")
        .ok_or("lifecycle Job binding invalid")?;
    let session: Option<String> = row.get(1);
    let key: String = row.get(2);
    let digest = key
        .strip_prefix(&format!("{id}:"))
        .ok_or("lifecycle digest missing")?;
    let terminal=tx.query_one(r#"SELECT EXISTS(SELECT 1 FROM public.app_core_agentrun r WHERE r.id=$1 AND r.session_id=$2 AND r."preAdmissionCancelledAt" IS NOT NULL) OR EXISTS(SELECT 1 FROM public.app_core_sessionevent e WHERE e.agent_run_id=$1 AND e.session_id=$2 AND NOT e.session_level AND e.payload->>'type' IN('agent_run_completed','agent_run_failed','agent_run_interrupted')) OR EXISTS(SELECT 1 FROM runtime_events e WHERE e.event_id='agent_run_pre_admission_failure:'||$1 AND e.session_id=$2)"#, &[&run,&session]).map_err(|e|e.to_string())?.get::<_,bool>(0);
    if !terminal {
        return Err("lifecycle terminal receipt required".into());
    }
    let no_effects=tx.query_opt("SELECT payload_json FROM runtime_events WHERE event_id='agent_run_no_effects:'||$1 AND session_id=$2 AND task_id=$1", &[&run,&session]).map_err(|e|e.to_string())?;
    if let Some(row) = no_effects {
        let value: serde_json::Value =
            serde_json::from_str(&row.get::<_, String>(0)).map_err(|e| e.to_string())?;
        if value["schema"] == "runtime.agent_run.no_effects.v1"
            && value["authorizationDigest"] == digest
            && value["agentRunId"] == run
            && value["jobId"] == id
            && value["scope"] == "noEffectsForRun"
            && value["sessionId"].as_str() == session.as_deref()
            && value["cleanupReceiptId"] == format!("cleanup:noEffects:{run}")
            && value["releaseReceiptId"] == format!("release:noOperation:{run}")
        {
            return Ok(());
        }
        return Err("lifecycle no-effects receipt invalid".into());
    }
    let complete=tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts c JOIN public.app_core_agentrunenvironmentrelease r ON r.agent_run_id=c.agent_run_id AND r.environment_id=c.environment_id AND r.authorization_digest=c.authorization_digest WHERE c.agent_run_id=$1 AND c.authorization_digest=$2 AND c.cleanup_complete AND r.controller_epoch>=c.controller_epoch)", &[&run,&digest]).map_err(|e|e.to_string())?.get::<_,bool>(0);
    if !complete {
        return Err("lifecycle cleanup and release receipts required".into());
    }
    Ok(())
}

impl PostgresRuntimeStore {
    /// Prove this Run owns no physical or Session effects; never acquire another controller.
    pub(crate) fn confirm_no_effects(
        &self,
        control: &crate::contract::AgentRunControl,
        job: &str,
        owner: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let run = &control.agent_run_id;
        self.with_execution_control_client(|client| {
            let mut tx=client.transaction().map_err(|e|e.to_string())?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            tx.query_opt("SELECT id FROM public.app_core_session WHERE id=$1 AND workspace_id=$2 FOR UPDATE", &[&control.authorization.session_id,&control.authorization.workspace_id]).map_err(|e|e.to_string())?.ok_or("no-effects Session missing")?;
            // Session serialization prevents admission or controller changes while proving absence.
            let rows=tx.query_opt("SELECT r.environment_id,e.controller_run_id,e.controller_released FROM public.app_core_agentrun r JOIN public.app_core_sessionenvironment e ON e.id=r.environment_id WHERE r.id=$1 AND r.session_id=$2 FOR SHARE OF r,e", &[&run,&control.authorization.session_id]).map_err(|e|e.to_string())?.ok_or("no-effects Run missing")?;
            let current=tx.query_opt("SELECT 1 FROM runtime_jobs WHERE job_id=$1 AND lease_owner=$2 AND job_kind='agent_run.lifecycle' AND lease_purpose='control' AND status='running' AND lease_expires_at_ms>(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint FOR SHARE", &[&job,&owner]).map_err(|e|e.to_string())?;
            if current.is_none() {return Err("no-effects lease rejected".into());}
            let has_effects=tx.query_one("SELECT EXISTS(SELECT 1 FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND NOT session_level) OR EXISTS(SELECT 1 FROM runtime.execution_instances WHERE agent_run_id=$1)", &[&run]).map_err(|e|e.to_string())?.get::<_,bool>(0);
            let owns_controller=rows.get::<_,Option<String>>(1).as_deref()==Some(run.as_str()) && !rows.get::<_,bool>(2);
            if has_effects || owns_controller {return Ok(None);}
            let payload=serde_json::json!({"schema":"runtime.agent_run.no_effects.v1","agentRunId":run,"sessionId":control.authorization.session_id,"environmentId":rows.get::<_,String>(0),"authorizationDigest":control.authorization_digest,"jobId":job,"scope":"noEffectsForRun","cleanupReceiptId":format!("cleanup:noEffects:{run}"),"releaseReceiptId":format!("release:noOperation:{run}")});
            let event=format!("agent_run_no_effects:{run}");
            tx.execute("INSERT INTO runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) VALUES($1,$2,$3,'runtime.agent_run.no_effects.v1',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,'internal',$4) ON CONFLICT(event_id) DO NOTHING", &[&event,&control.authorization.session_id,&run,&payload.to_string()]).map_err(|e|e.to_string())?;
            let stored:serde_json::Value=serde_json::from_str(&tx.query_one("SELECT payload_json FROM runtime_events WHERE event_id=$1", &[&event]).map_err(|e|e.to_string())?.get::<_,String>(0)).map_err(|e|e.to_string())?;
            if stored!=payload {return Err("no-effects receipt conflict".into());}
            tx.commit().map_err(|e|e.to_string())?; Ok(Some(stored))
        })
    }
}

#[cfg(test)]
pub(super) fn restore_previous_fixture(db: &mut postgres::Client) {
    let version: i64 = db
        .query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
        .unwrap()
        .get(0);
    if version < 15 {
        return;
    }
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.runtime_jobs WHERE lease_purpose='control'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0,
        "cannot discard control lease evidence"
    );
    db.batch_execute("DROP TRIGGER lifecycle_lease_purpose_facts ON runtime.runtime_jobs; DROP FUNCTION runtime.protect_lifecycle_lease_purpose(); ALTER TABLE runtime.runtime_jobs DROP COLUMN lease_purpose, DROP COLUMN lease_control_stage; DELETE FROM runtime.schema_migrations WHERE version=15;").unwrap();
}
