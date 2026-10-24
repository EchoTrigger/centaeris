//! A cancelled, explicitly bound automatic resume can end only before dispatch.
use super::*;
use postgres::GenericClient;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AutomaticResumeDemand {
    pub schema: String,
    pub job_id: String,
    pub agent_run_id: String,
    pub authorization_digest: String,
}
impl AutomaticResumeDemand {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.schema != "runtime.providerEnvironment.automaticResumeDemand.v1"
            || self.job_id != format!("agent_run.lifecycle:{}", self.agent_run_id)
        {
            return Err("provider_automatic_resume_demand_rejected".into());
        }
        super::super::super::validate_identity(&self.agent_run_id)?;
        super::super::super::validate_digest(&self.authorization_digest)
    }
}
#[derive(PartialEq, Eq)]
pub(super) enum DemandState {
    Current,
    Cancelled,
    Other,
}

pub(super) fn demand_state(
    tx: &mut postgres::Transaction<'_>,
    environment: &str,
    demand: &AutomaticResumeDemand,
) -> Result<DemandState, String> {
    demand.validate()?;
    let row = tx
        .query_opt(
            "SELECT idempotency_key FROM runtime.runtime_jobs WHERE job_id=$1 FOR SHARE",
            &[&demand.job_id],
        )
        .map_err(|e| e.to_string())?
        .ok_or("provider_automatic_resume_job_missing")?;
    if row.get::<_, String>(0) != format!("{}:{}", demand.job_id, demand.authorization_digest) {
        return Err("provider_automatic_resume_job_binding_rejected".into());
    }
    let current:bool=tx.query_one(&format!("SELECT EXISTS(SELECT 1 FROM {} JOIN public.app_core_sessionenvironment e ON e.id=r.environment_id WHERE r.environment_id=$1 AND j.job_id=$2 AND g.digest=$3 AND {})",super::super::idle::demand_from(),super::super::idle::demand_predicate()),&[&environment,&demand.job_id,&demand.authorization_digest]).map_err(|e|e.to_string())?.get(0);
    if current {
        return Ok(DemandState::Current);
    }
    let cancelled:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM public.app_core_agentrun r JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id WHERE r.id=$1 AND r.environment_id=$2 AND g.digest=$3 AND (r.\"preAdmissionCancelledAt\" IS NOT NULL OR EXISTS(SELECT 1 FROM runtime.runtime_events c WHERE c.event_id='agent_run_cancel_requested:'||r.id AND c.task_id=r.id AND c.session_id=r.session_id AND c.payload_json::jsonb->>'authorizationDigest'=g.digest)))",&[&demand.agent_run_id,&environment,&demand.authorization_digest]).map_err(|e|e.to_string())?.get(0);
    Ok(if cancelled {
        DemandState::Cancelled
    } else {
        DemandState::Other
    })
}

pub(super) fn end_unsent(
    tx: &mut postgres::Transaction<'_>,
    operation: &ProviderEnvironmentPowerOperation,
) -> Result<(), String> {
    if operation.dispatch_state != ProviderEnvironmentPowerDispatchState::Prepared
        || operation.request.action != ProviderEnvironmentPowerAction::Resume
        || operation.automatic_demand.is_none()
    {
        return Err("provider_automatic_resume_not_unsent".into());
    }
    let changed=tx.execute("UPDATE runtime.provider_environment_power_operations SET dispatch_state='notDispatched',not_dispatched_at_ms=GREATEST(recorded_at_ms,(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint) WHERE operation_id=$1 AND dispatch_state='prepared' AND automatic_demand_json IS NOT NULL",&[&operation.request.operation_id]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("provider_automatic_resume_end_conflict".into());
    }
    Ok(())
}

const BOUNDS: &str = r#"CHECK(operation_sequence>0 AND recorded_at_ms>=0 AND request_digest ~ '^sha256:[0-9a-f]{64}$' AND request_json<>'' AND action IN('pause','resume')
 AND (automatic_demand_json IS NULL OR (action='resume' AND jsonb_typeof(automatic_demand_json::jsonb)='object' AND automatic_demand_json::jsonb->>'schema'='runtime.providerEnvironment.automaticResumeDemand.v1' AND automatic_demand_json::jsonb->>'jobId'='agent_run.lifecycle:'||(automatic_demand_json::jsonb->>'agentRunId') AND automatic_demand_json::jsonb->>'authorizationDigest' ~ '^sha256:[0-9a-f]{64}$' AND automatic_demand_json::jsonb-ARRAY['schema','jobId','agentRunId','authorizationDigest']='{}'::jsonb))
 AND ((dispatch_state='prepared' AND dispatched_at_ms IS NULL AND acknowledged_at_ms IS NULL AND ack_json IS NULL AND not_dispatched_at_ms IS NULL)
 OR (dispatch_state='dispatched' AND dispatched_at_ms IS NOT NULL AND dispatched_at_ms>=recorded_at_ms AND not_dispatched_at_ms IS NULL AND ((acknowledged_at_ms IS NULL AND ack_json IS NULL) OR (acknowledged_at_ms IS NOT NULL AND acknowledged_at_ms>=dispatched_at_ms AND ack_json IS NOT NULL AND ack_json<>'')))
 OR (dispatch_state='notDispatched' AND action='resume' AND automatic_demand_json IS NOT NULL AND not_dispatched_at_ms IS NOT NULL AND not_dispatched_at_ms>=recorded_at_ms AND dispatched_at_ms IS NULL AND acknowledged_at_ms IS NULL AND ack_json IS NULL)))"#;

fn guard() -> String {
    let original=super::schema::DDL.split_once("CREATE FUNCTION runtime.protect_provider_environment_power() RETURNS trigger LANGUAGE plpgsql AS $$").expect("released power guard").1.split_once("$$;").expect("released power guard end").0;
    let mut guard=original.replace(" ELSIF previous.ack_json IS NULL OR previous.action=NEW.action OR", " ELSIF ((previous.dispatch_state<>'notDispatched' AND (previous.ack_json IS NULL OR previous.action=NEW.action)) OR (previous.dispatch_state='notDispatched' AND NEW.action<>'resume')) OR");
    guard = guard
        .replace(
            "NEW.action,NEW.recorded_at_ms)",
            "NEW.action,NEW.recorded_at_ms,NEW.automatic_demand_json)",
        )
        .replace(
            "OLD.action,OLD.recorded_at_ms)",
            "OLD.action,OLD.recorded_at_ms,OLD.automatic_demand_json)",
        );
    guard=guard.replace(" IF TG_OP='INSERT' THEN",r#" IF TG_OP='INSERT' THEN
  IF NEW.not_dispatched_at_ms IS NOT NULL THEN RAISE EXCEPTION 'provider power starts without unsent terminal fact'; END IF;
  IF NEW.automatic_demand_json IS NOT NULL AND NOT EXISTS(SELECT 1 FROM runtime.runtime_jobs j JOIN public.app_core_agentrun r ON r.id=NEW.automatic_demand_json::jsonb->>'agentRunId' JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id WHERE j.job_id=NEW.automatic_demand_json::jsonb->>'jobId' AND j.job_kind='agent_run.lifecycle' AND j.session_id=r.session_id AND r.environment_id=NEW.environment_id AND g.digest=NEW.automatic_demand_json::jsonb->>'authorizationDigest' AND j.idempotency_key=j.job_id||':'||g.digest)
  THEN RAISE EXCEPTION 'automatic resume demand binding rejected'; END IF;"#);
    let old = r#"   IF NEW.dispatch_state<>'dispatched' OR NEW.dispatched_at_ms IS NULL OR NEW.acknowledged_at_ms IS NOT NULL OR NEW.ack_json IS NOT NULL
   THEN RAISE EXCEPTION 'provider power dispatch transition rejected'; END IF;"#;
    let new = r#"   IF NEW.dispatch_state='notDispatched' THEN
    IF OLD.action<>'resume' OR OLD.automatic_demand_json IS NULL OR OLD.dispatched_at_ms IS NOT NULL OR NOT EXISTS(SELECT 1 FROM public.app_core_agentrun r JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id WHERE r.id=OLD.automatic_demand_json::jsonb->>'agentRunId' AND r.environment_id=OLD.environment_id AND g.digest=OLD.automatic_demand_json::jsonb->>'authorizationDigest' AND (r."preAdmissionCancelledAt" IS NOT NULL OR EXISTS(SELECT 1 FROM runtime.runtime_events c WHERE c.event_id='agent_run_cancel_requested:'||r.id AND c.task_id=r.id AND c.session_id=r.session_id AND c.payload_json::jsonb->>'authorizationDigest'=g.digest)))
    THEN RAISE EXCEPTION 'automatic resume unsent termination rejected'; END IF;
   ELSIF NEW.dispatch_state<>'dispatched' OR NEW.dispatched_at_ms IS NULL OR NEW.acknowledged_at_ms IS NOT NULL OR NEW.ack_json IS NOT NULL
   THEN RAISE EXCEPTION 'provider power dispatch transition rejected'; END IF;"#;
    assert!(guard.contains(old), "released power dispatch guard");
    guard.replace(old, new)
}
pub(crate) fn ddl() -> String {
    format!("LOCK TABLE runtime.provider_environment_power_operations IN ACCESS EXCLUSIVE MODE; ALTER TABLE runtime.provider_environment_power_operations ADD COLUMN automatic_demand_json text; ALTER TABLE runtime.provider_environment_power_operations ADD COLUMN not_dispatched_at_ms bigint; ALTER TABLE runtime.provider_environment_power_operations DROP CONSTRAINT provider_environment_power_bounds; ALTER TABLE runtime.provider_environment_power_operations ADD CONSTRAINT provider_environment_power_bounds {BOUNDS}; CREATE OR REPLACE FUNCTION runtime.protect_provider_environment_power() RETURNS trigger LANGUAGE plpgsql AS $$ {} $$;",guard())
}
pub(crate) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let actual:String=db.query_one("SELECT prosrc FROM pg_proc WHERE oid='runtime.protect_provider_environment_power()'::regprocedure",&[]).map_err(|e|e.to_string())?.get(0);
    if actual != format!(" {} ", guard()) {
        return Err("provider_automatic_resume_schema_guard_rejected".into());
    }
    Ok(())
}
#[cfg(test)]
pub(in crate::postgres_store) fn restore_previous_fixture(db: &mut postgres::Client) {
    let exists: bool = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=24)",
            &[],
        )
        .unwrap()
        .get(0);
    if !exists {
        return;
    }
    assert_eq!(db.query_one("SELECT count(*) FROM runtime.provider_environment_power_operations WHERE automatic_demand_json IS NOT NULL OR not_dispatched_at_ms IS NOT NULL",&[]).unwrap().get::<_,i64>(0),0,"cannot discard automatic resume facts");
    let mut tx = db.transaction().unwrap();
    tx.batch_execute("LOCK TABLE runtime.schema_migrations,runtime.provider_environment_power_operations IN ACCESS EXCLUSIVE MODE; ALTER TABLE runtime.provider_environment_power_operations DROP CONSTRAINT provider_environment_power_bounds; ALTER TABLE runtime.provider_environment_power_operations DROP COLUMN automatic_demand_json; ALTER TABLE runtime.provider_environment_power_operations DROP COLUMN not_dispatched_at_ms;").unwrap();
    let old = super::schema::DDL
        .split_once(" CONSTRAINT provider_environment_power_bounds CHECK")
        .unwrap()
        .1
        .split_once("CREATE FUNCTION")
        .unwrap()
        .0
        .trim()
        .trim_end_matches(';');
    let old = old.strip_suffix(')').unwrap();
    tx.batch_execute(&format!("ALTER TABLE runtime.provider_environment_power_operations ADD CONSTRAINT provider_environment_power_bounds CHECK{old};")).unwrap();
    let old_guard=super::schema::DDL.split_once("CREATE FUNCTION runtime.protect_provider_environment_power() RETURNS trigger LANGUAGE plpgsql AS $$").unwrap().1.split_once("$$;").unwrap().0;
    tx.batch_execute(&format!("CREATE OR REPLACE FUNCTION runtime.protect_provider_environment_power() RETURNS trigger LANGUAGE plpgsql AS $${old_guard}$$; DELETE FROM runtime.schema_migrations WHERE version=24;")).unwrap();
    tx.commit().unwrap();
}
