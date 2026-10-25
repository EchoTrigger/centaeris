//! Released schema metadata only; this module cannot dispatch provider operations.
use postgres::GenericClient;

pub(crate) const COMPLETION_TABLE: &str = "provider_environment_provision_completions";

pub(crate) const COMPLETION_COLUMNS: &str = "operation_id:text:NO,creation_attempt_id:text:NO,environment_id:text:NO,session_id:text:NO,workspace_id:text:NO,instance_id:text:NO,sandbox_id:text:NO,profile_digest:text:NO,provider_config_digest:text:NO,authorization_digest:text:NO,controller_epoch:bigint:NO,controller_lease_digest:text:NO,request_digest:text:NO,preparation_digest:text:NO,recorded_at_ms:bigint:NO";

pub(crate) fn creation_guard() -> String {
    let old = super::super::cleanup::GUARD_DDL;
    old.replace("NEW.recorded_at_ms)", "NEW.recorded_at_ms,NEW.creation_owner_json,NEW.maintenance_operation_id)")
       .replace("OLD.recorded_at_ms)", "OLD.recorded_at_ms,OLD.creation_owner_json,OLD.maintenance_operation_id)")
       .replace(" RETURN NEW;", r#" IF TG_OP='INSERT' AND NEW.maintenance_operation_id IS NOT NULL THEN
 IF NOT EXISTS(
 SELECT 1 FROM public.app_core_environmentmaintenanceoperation o
 JOIN public.app_core_environmentmaintenancegrant g ON g.operation_id=o.operation_id
 WHERE o.operation_id=NEW.maintenance_operation_id AND o.operation_kind='provision'
 AND o.environment_id=NEW.environment_id AND o.session_id=NEW.session_id AND o.workspace_id=NEW.workspace_id
 AND o.profile_digest=NEW.profile_digest AND g.digest=NEW.authorization_digest
 AND g.payload->>'operationId'=o.operation_id AND g.payload->>'environmentId'=NEW.environment_id
 AND g.payload->>'environmentProfileDigest'=NEW.profile_digest
 AND g.payload->'environmentProfile'->>'providerConfigDigest'=NEW.provider_config_digest
 AND g.payload->'environmentProfile'->>'lifetime'='retained')
 THEN RAISE EXCEPTION 'provider maintenance creation origin rejected'; END IF;
 END IF;
 RETURN NEW;"#)
}

pub(crate) fn ddl() -> String {
    format!(
        r#"
LOCK TABLE runtime.provider_creation_operations IN ACCESS EXCLUSIVE MODE;
DROP TRIGGER provider_creation_operation_facts ON runtime.provider_creation_operations;
ALTER TABLE runtime.provider_creation_operations ADD COLUMN creation_owner_json text;
ALTER TABLE runtime.provider_creation_operations ADD COLUMN maintenance_operation_id text;
UPDATE runtime.provider_creation_operations SET creation_owner_json=jsonb_build_object('kind','agentRun','runId',agent_run_id)::text;
ALTER TABLE runtime.provider_creation_operations ALTER COLUMN creation_owner_json SET NOT NULL;
ALTER TABLE runtime.provider_creation_operations ALTER COLUMN agent_run_id DROP NOT NULL;
ALTER TABLE runtime.provider_creation_operations ALTER COLUMN processing_job_id DROP NOT NULL;
ALTER TABLE runtime.provider_creation_operations ADD CONSTRAINT provider_creation_owner_facts CHECK(
 (agent_run_id IS NOT NULL AND agent_run_id<>'' AND processing_job_id IS NOT NULL AND processing_job_id<>''
  AND maintenance_operation_id IS NULL AND creation_owner_json::jsonb=jsonb_build_object('kind','agentRun','runId',agent_run_id))
 OR (agent_run_id IS NULL AND processing_job_id IS NULL AND maintenance_operation_id IS NOT NULL AND maintenance_operation_id<>''
  AND creation_owner_json::jsonb=jsonb_build_object('kind','maintenanceOperation','operationId',maintenance_operation_id)));
{guard}
CREATE TRIGGER provider_creation_operation_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_creation_operations FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_creation_operation();
{completion}
"#,
        guard = creation_guard(),
        completion = COMPLETION_DDL
    )
}

pub(crate) const COMPLETION_DDL: &str = r#"
CREATE TABLE runtime.provider_environment_provision_completions(
 operation_id text PRIMARY KEY,creation_attempt_id text NOT NULL UNIQUE REFERENCES runtime.provider_creation_operations(attempt_id),
 environment_id text NOT NULL,session_id text NOT NULL,workspace_id text NOT NULL,instance_id text NOT NULL,sandbox_id text NOT NULL,
 profile_digest text NOT NULL,provider_config_digest text NOT NULL,authorization_digest text NOT NULL,
 controller_epoch bigint NOT NULL,controller_lease_digest text NOT NULL,request_digest text NOT NULL,preparation_digest text NOT NULL,
 recorded_at_ms bigint NOT NULL,
 CONSTRAINT provider_environment_provision_completion_facts CHECK(controller_epoch>0 AND recorded_at_ms>=0
 AND operation_id<>'' AND environment_id<>'' AND session_id<>'' AND workspace_id<>'' AND instance_id<>'' AND sandbox_id<>''
 AND profile_digest ~ '^sha256:[0-9a-f]{64}$' AND provider_config_digest ~ '^sha256:[0-9a-f]{64}$'
 AND authorization_digest ~ '^sha256:[0-9a-f]{64}$' AND controller_lease_digest ~ '^sha256:[0-9a-f]{64}$'
 AND request_digest ~ '^sha256:[0-9a-f]{64}$' AND preparation_digest ~ '^sha256:[0-9a-f]{64}$'));
CREATE FUNCTION runtime.protect_provider_environment_provision_completion() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP<>'INSERT' THEN RAISE EXCEPTION 'provider provision completion is immutable'; END IF;
 IF NOT EXISTS(SELECT 1 FROM runtime.provider_creation_operations p
 JOIN runtime.provider_environment_bindings b ON b.environment_id=p.environment_id AND b.creation_attempt_id=p.attempt_id
 JOIN public.app_core_environmentmaintenanceoperation o ON o.operation_id=p.maintenance_operation_id
 JOIN public.app_core_environmentmaintenancegrant g ON g.operation_id=o.operation_id AND g.digest=p.authorization_digest
 WHERE p.attempt_id=NEW.creation_attempt_id AND p.maintenance_operation_id=NEW.operation_id
 AND p.environment_id=NEW.environment_id AND p.session_id=NEW.session_id AND p.workspace_id=NEW.workspace_id
 AND p.instance_id=NEW.instance_id AND p.verified_sandbox_id=NEW.sandbox_id AND p.verification_digest IS NOT NULL AND p.verified_at_ms<=NEW.recorded_at_ms
 AND p.profile_digest=NEW.profile_digest AND p.provider_config_digest=NEW.provider_config_digest
 AND p.authorization_digest=NEW.authorization_digest AND p.dispatch_state='dispatched' AND p.settlement_id IS NULL
 AND o.operation_kind='provision' AND o.environment_id=NEW.environment_id AND o.session_id=NEW.session_id AND o.workspace_id=NEW.workspace_id
 AND o.profile_digest=NEW.profile_digest AND o.request_digest=NEW.request_digest
 AND (g.payload->>'controllerEpoch')::bigint=NEW.controller_epoch
 AND g.payload->>'controllerLeaseDigest'=NEW.controller_lease_digest)
 THEN RAISE EXCEPTION 'provider provision completion target rejected'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER provider_environment_provision_completion_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_environment_provision_completions FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_environment_provision_completion();
"#;

pub(crate) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let owner: bool=db.query_one("SELECT count(*)=1 FROM pg_constraint WHERE conrelid='runtime.provider_creation_operations'::regclass AND conname='provider_creation_owner_facts' AND contype='c' AND convalidated AND NOT condeferrable",&[]).map_err(|e|e.to_string())?.get(0);
    if !owner {
        return Err("provider_creation_owner_schema_rejected".into());
    }
    let definition:String=db.query_one("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.provider_creation_operations'::regclass AND conname='provider_creation_owner_facts'",&[]).map_err(|e|e.to_string())?.get(0);
    if definition
        != r#"CHECK ((((agent_run_id IS NOT NULL) AND (agent_run_id <> ''::text) AND (processing_job_id IS NOT NULL) AND (processing_job_id <> ''::text) AND (maintenance_operation_id IS NULL) AND ((creation_owner_json)::jsonb = jsonb_build_object('kind', 'agentRun', 'runId', agent_run_id))) OR ((agent_run_id IS NULL) AND (processing_job_id IS NULL) AND (maintenance_operation_id IS NOT NULL) AND (maintenance_operation_id <> ''::text) AND ((creation_owner_json)::jsonb = jsonb_build_object('kind', 'maintenanceOperation', 'operationId', maintenance_operation_id)))))"#
    {
        return Err("provider_creation_owner_constraint_rejected".into());
    }

    let columns=db.query("SELECT column_name,data_type,is_nullable FROM information_schema.columns WHERE table_schema='runtime' AND table_name=$1 ORDER BY ordinal_position",&[&COMPLETION_TABLE]).map_err(|e|e.to_string())?.iter().map(|r|format!("{}:{}:{}",r.get::<_,String>(0),r.get::<_,String>(1),r.get::<_,String>(2))).collect::<Vec<_>>().join(",");
    if columns != COMPLETION_COLUMNS {
        return Err("provider_provision_completion_schema_rejected".into());
    }
    let valid: bool=db.query_one("SELECT count(*)=4 AND bool_and(convalidated AND NOT condeferrable) AND count(*) FILTER(WHERE contype='p' AND conkey=ARRAY[1]::smallint[])=1 AND count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[2]::smallint[])=1 AND count(*) FILTER(WHERE contype='f' AND conkey=ARRAY[2]::smallint[] AND confrelid='runtime.provider_creation_operations'::regclass AND confkey=ARRAY[1]::smallint[] AND confdeltype='a' AND confupdtype='a')=1 AND count(*) FILTER(WHERE contype='c')=1 FROM pg_constraint WHERE conrelid='runtime.provider_environment_provision_completions'::regclass AND contype<>'n'",&[]).map_err(|e|e.to_string())?.get(0);
    if !valid {
        return Err("provider_provision_completion_constraints_rejected".into());
    }
    let body = COMPLETION_DDL
        .split_once("RETURNS trigger LANGUAGE plpgsql AS $$")
        .ok_or("provider_provision_guard_missing")?
        .1
        .split_once("$$;")
        .ok_or("provider_provision_guard_missing")?
        .0;
    let actual:String=db.query_one("SELECT prosrc FROM pg_proc WHERE oid=to_regprocedure('runtime.protect_provider_environment_provision_completion()')",&[]).map_err(|e|e.to_string())?.get(0);
    let trigger:bool=db.query_one("SELECT count(*)=1 FROM pg_trigger WHERE tgrelid='runtime.provider_environment_provision_completions'::regclass AND tgname='provider_environment_provision_completion_facts' AND tgenabled='O' AND NOT tgisinternal AND tgtype=31 AND tgfoid='runtime.protect_provider_environment_provision_completion()'::regprocedure AND tgqual IS NULL AND tgnargs=0",&[]).map_err(|e|e.to_string())?.get(0);
    if actual != body || !trigger {
        return Err("provider_provision_completion_guard_rejected".into());
    }
    Ok(())
}
