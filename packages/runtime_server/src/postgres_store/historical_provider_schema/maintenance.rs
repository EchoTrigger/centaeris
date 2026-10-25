//! Released schema metadata only; this module cannot dispatch provider operations.
use postgres::GenericClient;

pub(crate) const TABLE: &str = "provider_environment_maintenance_completions";

pub(crate) const COLUMNS: &str = "operation_id:text:NO,environment_id:text:NO,creation_attempt_id:text:NO,instance_id:text:NO,sandbox_id:text:NO,profile_digest:text:NO,provider_config_digest:text:NO,verification_digest:text:NO,controller_epoch:bigint:NO,controller_lease_digest:text:NO,request_digest:text:NO,completion_json:text:NO,recorded_at_ms:bigint:NO";

pub(crate) const DDL: &str = r#"
CREATE TABLE runtime.provider_environment_maintenance_completions(
 operation_id text PRIMARY KEY,environment_id text NOT NULL REFERENCES runtime.provider_environment_bindings(environment_id),
 creation_attempt_id text NOT NULL REFERENCES runtime.provider_creation_operations(attempt_id),
 instance_id text NOT NULL,sandbox_id text NOT NULL,profile_digest text NOT NULL,provider_config_digest text NOT NULL,
 verification_digest text NOT NULL,controller_epoch bigint NOT NULL,controller_lease_digest text NOT NULL,
 request_digest text NOT NULL,completion_json text NOT NULL,recorded_at_ms bigint NOT NULL,
 CONSTRAINT provider_environment_maintenance_completion_facts CHECK(controller_epoch>0 AND recorded_at_ms>=0
 AND operation_id ~ '^[A-Za-z0-9_.:-]+$' AND length(operation_id)<=160
 AND environment_id ~ '^[A-Za-z0-9_.:-]+$' AND length(environment_id)<=160
 AND creation_attempt_id ~ '^[A-Za-z0-9_.:-]+$' AND length(creation_attempt_id)<=256
 AND instance_id ~ '^instance_[0-9a-f]{64}$' AND sandbox_id ~ '^[A-Za-z0-9_.:-]+$' AND length(sandbox_id)<=256
 AND profile_digest ~ '^sha256:[0-9a-f]{64}$' AND provider_config_digest ~ '^sha256:[0-9a-f]{64}$'
 AND verification_digest ~ '^sha256:[0-9a-f]{64}$' AND controller_lease_digest ~ '^sha256:[0-9a-f]{64}$'
 AND request_digest ~ '^sha256:[0-9a-f]{64}$' AND completion_json<>''));
CREATE FUNCTION runtime.protect_provider_environment_maintenance_completion() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE completion jsonb;
BEGIN
 IF TG_OP<>'INSERT' THEN RAISE EXCEPTION 'provider maintenance completion is immutable'; END IF;
 completion:=NEW.completion_json::jsonb;
 IF jsonb_typeof(completion)<>'object' OR NOT(completion ?& ARRAY['kind','taskIds','cleanupComplete'])
  OR (completion-ARRAY['kind','taskIds','cleanupComplete'])<>'{}'::jsonb
  OR completion->'kind'<>'"operationEnded"'::jsonb OR completion->'cleanupComplete'<>'true'::jsonb
  OR jsonb_typeof(completion->'taskIds')<>'array'
 THEN RAISE EXCEPTION 'provider maintenance completion unconfirmed'; END IF;
 IF EXISTS(SELECT 1 FROM jsonb_array_elements(completion->'taskIds') id WHERE jsonb_typeof(id)<>'string')
 THEN RAISE EXCEPTION 'provider maintenance completion invalid'; END IF;
 IF NOT EXISTS(SELECT 1 FROM runtime.provider_creation_operations p
  JOIN runtime.provider_environment_bindings b ON b.environment_id=p.environment_id AND b.creation_attempt_id=p.attempt_id
  JOIN public.app_core_environmentmaintenanceoperation o ON o.environment_id=p.environment_id
  JOIN public.app_core_environmentmaintenancegrant g ON g.id=o.current_grant_id AND g.operation_id=o.operation_id
  JOIN public.app_core_sessionenvironment e ON e.id=o.environment_id AND e.session_id=o.session_id
  WHERE o.operation_id=NEW.operation_id AND o.operation_kind='task' AND o.state='active'
   AND o.instance_id IS NULL AND o.container_id IS NULL AND o.host_id IS NULL AND o.allocation_execution_id IS NULL
   AND p.attempt_id=NEW.creation_attempt_id AND p.environment_id=NEW.environment_id
   AND p.session_id=o.session_id AND p.workspace_id=o.workspace_id AND p.provider='e2b'
   AND p.instance_id=NEW.instance_id AND p.verified_sandbox_id=NEW.sandbox_id AND p.verification_digest=NEW.verification_digest
   AND p.verified_at_ms<=NEW.recorded_at_ms AND p.dispatch_state='dispatched' AND p.settlement_id IS NULL
   AND p.profile_digest=NEW.profile_digest AND p.provider_config_digest=NEW.provider_config_digest
   AND o.profile_digest=NEW.profile_digest AND o.request_digest=NEW.request_digest
   AND o.provider_target=jsonb_build_object('provider','e2b','creationAttemptId',NEW.creation_attempt_id,
    'instanceId',NEW.instance_id,'sandboxId',NEW.sandbox_id,'verificationDigest',NEW.verification_digest,'providerConfigDigest',NEW.provider_config_digest)
   AND e.controller_run_id IS NULL AND e.controller_maintenance_operation_id=o.operation_id
   AND NOT e.controller_released AND e.controller_epoch=NEW.controller_epoch
   AND e.environment_profile_digest=NEW.profile_digest
   AND e.environment_profile->>'schema'='runtime.e2b_environment_profile.v1'
   AND e.environment_profile->>'lifetime'='retained'
   AND e.environment_profile->>'providerConfigDigest'=NEW.provider_config_digest
   AND g.controller_epoch=NEW.controller_epoch AND g.controller_lease_digest=NEW.controller_lease_digest
   AND g.controller_purpose=e.controller_purpose AND NEW.recorded_at_ms<g.expires_at_ms
   AND 'sha256:'||encode(sha256(convert_to('workspace:environment-controller-lease:v1','UTF8')||decode('00','hex')||convert_to(e.controller_lease_owner,'UTF8')),'hex')=NEW.controller_lease_digest
   AND g.payload->>'schema'='workspace.provider_environment_maintenance_authorization.v1'
   AND g.payload->>'operationKind'='task' AND g.payload->>'workspaceId'=o.workspace_id
   AND g.payload->>'sessionId'=o.session_id AND g.payload->>'environmentId'=o.environment_id
   AND g.payload->'providerTarget'=o.provider_target AND g.payload->>'profileDigest'=NEW.profile_digest
   AND g.payload->'controllerOwner'=jsonb_build_object('kind','maintenanceOperation','operationId',o.operation_id)
   AND (g.payload->>'controllerEpoch')::bigint=NEW.controller_epoch
   AND g.payload->>'controllerLeaseDigest'=NEW.controller_lease_digest
   AND g.payload->>'controllerPurpose'=e.controller_purpose
   AND g.payload->>'requestDigest'=NEW.request_digest
   AND (g.payload->>'expiresAtMs')::bigint=g.expires_at_ms)
 THEN RAISE EXCEPTION 'provider maintenance completion target rejected'; END IF;
 IF EXISTS(SELECT 1 FROM runtime.provider_environment_run_uses u WHERE u.environment_id=NEW.environment_id
  AND (u.creation_attempt_id<>NEW.creation_attempt_id OR u.cleanup_receipt_id IS NULL))
 THEN RAISE EXCEPTION 'provider maintenance consumer cleanup pending'; END IF;
 IF EXISTS(SELECT 1 FROM runtime.provider_environment_run_uses WHERE environment_id=NEW.environment_id) THEN
  IF EXISTS(SELECT 1 FROM runtime.provider_environment_run_uses u WHERE u.environment_id=NEW.environment_id
   AND NOT EXISTS(SELECT 1 FROM public.app_core_agentrunenvironmentrelease r WHERE r.agent_run_id=u.agent_run_id
    AND r.environment_id=u.environment_id AND r.authorization_digest=u.authorization_digest AND r.controller_epoch>=u.cleanup_controller_epoch))
  THEN RAISE EXCEPTION 'provider maintenance consumer release pending'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER provider_environment_maintenance_completion_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_environment_maintenance_completions FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_environment_maintenance_completion();
"#;

pub(crate) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let columns=db.query("SELECT column_name,data_type,is_nullable FROM information_schema.columns WHERE table_schema='runtime' AND table_name=$1 ORDER BY ordinal_position",&[&TABLE]).map_err(|e|e.to_string())?.iter()
        .map(|r|format!("{}:{}:{}",r.get::<_,String>(0),r.get::<_,String>(1),r.get::<_,String>(2))).collect::<Vec<_>>().join(",");
    if columns != COLUMNS {
        return Err("provider_maintenance_completion_schema_rejected".into());
    }
    let constraints:bool=db.query_one("SELECT count(*)=4 AND bool_and(convalidated AND NOT condeferrable AND NOT condeferred)
      AND count(*) FILTER(WHERE contype='p' AND conkey=ARRAY[1]::smallint[])=1
      AND count(*) FILTER(WHERE contype='f' AND conkey=ARRAY[2]::smallint[] AND confrelid='runtime.provider_environment_bindings'::regclass AND confkey=ARRAY[1]::smallint[] AND confdeltype='a' AND confupdtype='a')=1
      AND count(*) FILTER(WHERE contype='f' AND conkey=ARRAY[3]::smallint[] AND confrelid='runtime.provider_creation_operations'::regclass AND confkey=ARRAY[1]::smallint[] AND confdeltype='a' AND confupdtype='a')=1
      AND count(*) FILTER(WHERE contype='c')=1 FROM pg_constraint WHERE conrelid='runtime.provider_environment_maintenance_completions'::regclass AND contype<>'n'",&[]).map_err(|e|e.to_string())?.get(0);
    if !constraints {
        return Err("provider_maintenance_completion_constraints_rejected".into());
    }
    let expected=DDL.split_once("CREATE FUNCTION runtime.protect_provider_environment_maintenance_completion() RETURNS trigger LANGUAGE plpgsql AS $$")
        .ok_or("provider_maintenance_completion_guard_missing")?.1.split_once("$$;").ok_or("provider_maintenance_completion_guard_missing")?.0;
    let actual:String=db.query_one("SELECT prosrc FROM pg_proc WHERE oid=to_regprocedure('runtime.protect_provider_environment_maintenance_completion()')",&[]).map_err(|e|e.to_string())?.get(0);
    let trigger:bool=db.query_one("SELECT count(*)=1 FROM pg_trigger WHERE tgrelid='runtime.provider_environment_maintenance_completions'::regclass AND tgname='provider_environment_maintenance_completion_facts' AND tgenabled='O' AND NOT tgisinternal AND tgtype=31 AND tgfoid='runtime.protect_provider_environment_maintenance_completion()'::regprocedure AND tgqual IS NULL AND tgnargs=0",&[]).map_err(|e|e.to_string())?.get(0);
    if actual != expected || !trigger {
        return Err("provider_maintenance_completion_guard_rejected".into());
    }
    Ok(())
}
