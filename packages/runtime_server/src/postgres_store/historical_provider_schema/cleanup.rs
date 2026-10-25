//! Released schema metadata only; this module cannot dispatch provider operations.
use postgres::GenericClient;

pub(crate) const DDL: &str = r#"
ALTER TABLE runtime.provider_creation_operations
 ADD COLUMN verified_sandbox_id text,
 ADD COLUMN verification_digest text,
 ADD COLUMN verified_at_ms bigint,
 ADD COLUMN settlement_id text UNIQUE,
 ADD COLUMN settlement_kind text,
 ADD COLUMN settlement_sandbox_id text,
 ADD COLUMN settlement_controller_epoch bigint,
 ADD COLUMN settled_at_ms bigint;
ALTER TABLE runtime.provider_creation_operations ADD CONSTRAINT provider_creation_cleanup_facts CHECK(
 ((verified_sandbox_id IS NULL AND verification_digest IS NULL AND verified_at_ms IS NULL)
  OR (dispatch_state='dispatched' AND verified_sandbox_id=reported_sandbox_id AND verified_sandbox_id IS NOT NULL
   AND verification_digest ~ '^sha256:[0-9a-f]{64}$' AND verification_digest IS NOT NULL AND verified_at_ms>=0 AND verified_at_ms IS NOT NULL))
 AND ((settlement_id IS NULL AND settlement_kind IS NULL AND settlement_sandbox_id IS NULL AND settlement_controller_epoch IS NULL AND settled_at_ms IS NULL)
  OR (settlement_id IS NOT NULL AND settlement_kind IS NOT NULL AND settlement_controller_epoch>0 AND settlement_controller_epoch IS NOT NULL AND settled_at_ms>=0 AND settled_at_ms IS NOT NULL
   AND ((settlement_kind='notDispatched' AND dispatch_state='notDispatched' AND verified_sandbox_id IS NULL AND settlement_sandbox_id IS NULL)
    OR (settlement_kind='removed' AND dispatch_state='dispatched' AND verified_sandbox_id IS NOT NULL AND settlement_sandbox_id=verified_sandbox_id AND settlement_sandbox_id IS NOT NULL AND settled_at_ms>=verified_at_ms)))));
DROP INDEX runtime.provider_creation_operations_open_key;
CREATE UNIQUE INDEX provider_creation_operations_open_key ON runtime.provider_creation_operations(environment_id) WHERE dispatch_state IN('prepared','dispatched') AND settlement_id IS NULL;
CREATE TABLE runtime.provider_run_cleanup_receipts(
 receipt_id text PRIMARY KEY,agent_run_id text NOT NULL UNIQUE,environment_id text NOT NULL,
 authorization_digest text NOT NULL,profile_digest text NOT NULL,provider_config_digest text NOT NULL,
 controller_epoch bigint NOT NULL,settlement_digest text NOT NULL,operation_count bigint NOT NULL,recorded_at_ms bigint NOT NULL,
 CONSTRAINT provider_run_cleanup_receipt_facts CHECK(controller_epoch>0 AND operation_count>0 AND recorded_at_ms>=0
  AND authorization_digest ~ '^sha256:[0-9a-f]{64}$' AND profile_digest ~ '^sha256:[0-9a-f]{64}$'
  AND provider_config_digest ~ '^sha256:[0-9a-f]{64}$' AND settlement_digest ~ '^sha256:[0-9a-f]{64}$'));
CREATE FUNCTION runtime.protect_provider_run_cleanup_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 RAISE EXCEPTION 'provider cleanup receipts are immutable';
END $$;
CREATE TRIGGER provider_run_cleanup_receipt_facts BEFORE UPDATE OR DELETE ON runtime.provider_run_cleanup_receipts FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_run_cleanup_receipt();
"#;

pub(crate) const GUARD_DDL: &str = r#"
CREATE OR REPLACE FUNCTION runtime.protect_provider_creation_operation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'provider creation responsibility is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.dispatch_state<>'prepared' OR NEW.reported_sandbox_id IS NOT NULL OR NEW.verified_sandbox_id IS NOT NULL OR NEW.settlement_id IS NOT NULL THEN
   RAISE EXCEPTION 'provider creation requires a prepared intent'; END IF;
 ELSE
  IF ROW(NEW.attempt_id,NEW.execution_id,NEW.instance_id,NEW.environment_id,NEW.session_id,NEW.workspace_id,NEW.agent_run_id,NEW.authorization_digest,NEW.profile_digest,NEW.provider,NEW.provider_config_digest,NEW.request_digest,NEW.idempotency_key,NEW.processing_job_id,NEW.recorded_at_ms)
    IS DISTINCT FROM ROW(OLD.attempt_id,OLD.execution_id,OLD.instance_id,OLD.environment_id,OLD.session_id,OLD.workspace_id,OLD.agent_run_id,OLD.authorization_digest,OLD.profile_digest,OLD.provider,OLD.provider_config_digest,OLD.request_digest,OLD.idempotency_key,OLD.processing_job_id,OLD.recorded_at_ms)
   OR OLD.settlement_id IS NOT NULL
   OR NOT (
    (OLD.dispatch_state='prepared' AND NEW.dispatch_state IN('dispatched','notDispatched') AND NEW.reported_sandbox_id IS NULL AND NEW.verified_sandbox_id IS NULL AND NEW.settlement_id IS NULL)
    OR (OLD.dispatch_state='dispatched' AND NEW.dispatch_state='dispatched' AND OLD.reported_sandbox_id IS NULL AND NEW.reported_sandbox_id IS NOT NULL AND NEW.verified_sandbox_id IS NULL AND NEW.settlement_id IS NULL)
    OR (OLD.dispatch_state='dispatched' AND NEW.dispatch_state='dispatched' AND OLD.verified_sandbox_id IS NULL AND NEW.verified_sandbox_id IS NOT NULL AND NEW.reported_sandbox_id=OLD.reported_sandbox_id AND NEW.settlement_id IS NULL)
    OR (NEW.dispatch_state=OLD.dispatch_state AND NEW.reported_sandbox_id IS NOT DISTINCT FROM OLD.reported_sandbox_id
     AND ROW(NEW.verified_sandbox_id,NEW.verification_digest,NEW.verified_at_ms) IS NOT DISTINCT FROM ROW(OLD.verified_sandbox_id,OLD.verification_digest,OLD.verified_at_ms)
     AND NEW.settlement_id IS NOT NULL))
  THEN RAISE EXCEPTION 'provider creation identity or evidence transition rejected'; END IF;
 END IF;
 RETURN NEW;
END $$;
"#;

pub(crate) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let body = DDL.split_once("CREATE FUNCTION runtime.protect_provider_run_cleanup_receipt() RETURNS trigger LANGUAGE plpgsql AS $$").ok_or("provider cleanup function missing")?.1.split_once("$$;").ok_or("provider cleanup body missing")?.0;
    let actual = db.query_opt("SELECT prosrc FROM pg_proc WHERE oid=to_regprocedure('runtime.protect_provider_run_cleanup_receipt()')", &[]).map_err(|e| e.to_string())?;
    if actual.is_none_or(|row| row.get::<_, String>(0) != body) {
        return Err("Postgres provider cleanup function mismatch".into());
    }
    let valid = db.query_one("SELECT count(*)=1 FROM pg_trigger WHERE tgrelid='runtime.provider_run_cleanup_receipts'::regclass AND tgname='provider_run_cleanup_receipt_facts' AND tgenabled='O' AND NOT tgisinternal AND tgtype=27 AND tgfoid='runtime.protect_provider_run_cleanup_receipt()'::regprocedure AND tgqual IS NULL AND tgnargs=0", &[]).map_err(|e| e.to_string())?.get::<_, bool>(0);
    if !valid {
        return Err("Postgres provider cleanup trigger mismatch".into());
    }
    let constraints = db.query_one("SELECT count(*)=3 AND bool_and(convalidated AND NOT condeferrable) AND count(*) FILTER(WHERE contype='p' AND conkey=ARRAY[1]::smallint[])=1 AND count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[2]::smallint[])=1 AND count(*) FILTER(WHERE contype='c')=1 FROM pg_constraint WHERE conrelid='runtime.provider_run_cleanup_receipts'::regclass AND contype<>'n'", &[]).map_err(|e| e.to_string())?.get::<_, bool>(0);
    if !constraints {
        return Err("Postgres provider cleanup constraints mismatch".into());
    }
    Ok(())
}
