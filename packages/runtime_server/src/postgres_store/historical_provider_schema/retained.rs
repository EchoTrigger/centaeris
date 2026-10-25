//! Released schema metadata only; this module cannot dispatch provider operations.
use postgres::GenericClient;

pub(in crate::postgres_store) mod power;
pub(in crate::postgres_store) mod schema;

pub(crate) const DDL: &str = r#"
CREATE TABLE runtime.provider_environment_bindings(
 environment_id text PRIMARY KEY,creation_attempt_id text NOT NULL UNIQUE REFERENCES runtime.provider_creation_operations(attempt_id),
 recorded_at_ms bigint NOT NULL CHECK(recorded_at_ms>=0));
CREATE TABLE runtime.provider_environment_run_uses(
 provider_use_id text PRIMARY KEY,agent_run_id text NOT NULL UNIQUE,
 environment_id text NOT NULL REFERENCES runtime.provider_environment_bindings(environment_id),
 authorization_digest text NOT NULL,execution_id text NOT NULL UNIQUE,provider_use_digest text NOT NULL,
 recorded_at_ms bigint NOT NULL,cleanup_receipt_id text UNIQUE,cleanup_controller_epoch bigint,quiescence_digest text,cleanup_recorded_at_ms bigint,
 CONSTRAINT retained_provider_run_use_facts CHECK(recorded_at_ms>=0 AND authorization_digest ~ '^sha256:[0-9a-f]{64}$' AND provider_use_digest ~ '^sha256:[0-9a-f]{64}$'
 AND ((cleanup_receipt_id IS NULL AND cleanup_controller_epoch IS NULL AND quiescence_digest IS NULL AND cleanup_recorded_at_ms IS NULL)
 OR (cleanup_receipt_id IS NOT NULL AND cleanup_controller_epoch IS NOT NULL AND cleanup_controller_epoch>0 AND quiescence_digest IS NOT NULL AND quiescence_digest ~ '^sha256:[0-9a-f]{64}$' AND cleanup_recorded_at_ms IS NOT NULL AND cleanup_recorded_at_ms>=recorded_at_ms))));
CREATE TABLE runtime.provider_environment_retirements(
 operation_id text PRIMARY KEY,environment_id text NOT NULL UNIQUE REFERENCES runtime.provider_environment_bindings(environment_id),
 request_digest text NOT NULL,request_json text NOT NULL,expected_controller_epoch bigint NOT NULL,recorded_at_ms bigint NOT NULL,
 CONSTRAINT provider_environment_retirement_facts CHECK(request_digest ~ '^sha256:[0-9a-f]{64}$' AND expected_controller_epoch>=0 AND recorded_at_ms>=0 AND request_json<>''));
CREATE FUNCTION runtime.protect_provider_environment_binding() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP<>'INSERT' THEN RAISE EXCEPTION 'provider environment origin is immutable'; END IF;
 IF NOT EXISTS(SELECT 1 FROM runtime.provider_creation_operations WHERE attempt_id=NEW.creation_attempt_id AND environment_id=NEW.environment_id)
 THEN RAISE EXCEPTION 'provider environment origin mismatch'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER provider_environment_binding_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_environment_bindings FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_environment_binding();
CREATE FUNCTION runtime.protect_retained_provider_run_use() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'provider consumer responsibility is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.cleanup_receipt_id IS NOT NULL OR NEW.cleanup_controller_epoch IS NOT NULL OR NEW.quiescence_digest IS NOT NULL OR NEW.cleanup_recorded_at_ms IS NOT NULL
  THEN RAISE EXCEPTION 'provider consumer starts without cleanup evidence'; END IF;
 ELSE
  IF ROW(NEW.provider_use_id,NEW.agent_run_id,NEW.environment_id,NEW.authorization_digest,NEW.execution_id,NEW.provider_use_digest,NEW.recorded_at_ms)
   IS DISTINCT FROM ROW(OLD.provider_use_id,OLD.agent_run_id,OLD.environment_id,OLD.authorization_digest,OLD.execution_id,OLD.provider_use_digest,OLD.recorded_at_ms)
   OR OLD.cleanup_receipt_id IS NOT NULL OR NEW.cleanup_receipt_id IS NULL
  THEN RAISE EXCEPTION 'provider consumer identity or evidence transition rejected'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER retained_provider_run_use_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_environment_run_uses FOR EACH ROW EXECUTE FUNCTION runtime.protect_retained_provider_run_use();
CREATE FUNCTION runtime.protect_provider_environment_retirement() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 RAISE EXCEPTION 'provider environment retirement is immutable';
END $$;
CREATE TRIGGER provider_environment_retirement_facts BEFORE UPDATE OR DELETE ON runtime.provider_environment_retirements FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_environment_retirement();
"#;

pub(crate) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let history: bool = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=21)",
            &[],
        )
        .map_err(|e| e.to_string())?
        .get(0);
    for (table, name, kind) in [
        (
            "provider_environment_bindings",
            "provider_environment_binding_facts",
            31i16,
        ),
        (
            "provider_environment_run_uses",
            "retained_provider_run_use_facts",
            31i16,
        ),
        (
            "provider_environment_retirements",
            "provider_environment_retirement_facts",
            27i16,
        ),
    ] {
        let function = match table {
            "provider_environment_bindings" => "protect_provider_environment_binding",
            "provider_environment_run_uses" => "protect_retained_provider_run_use",
            _ => "protect_provider_environment_retirement",
        };
        let signature = format!("runtime.{function}()");
        let found:bool=db.query_one("SELECT EXISTS(SELECT 1 FROM pg_trigger WHERE tgrelid=to_regclass($1) AND tgname=$2 AND tgenabled='O' AND NOT tgisinternal AND tgtype=$3 AND tgfoid=to_regprocedure($4) AND tgqual IS NULL AND tgnargs=0)",&[&format!("runtime.{table}"),&name,&kind,&signature]).map_err(|e|e.to_string())?.get(0);
        if !found {
            return Err("retained_provider_schema_trigger_rejected".into());
        }
        let guard = if history && table == "provider_environment_run_uses" {
            schema::DDL
        } else {
            DDL
        };
        let expected = guard
            .split_once(&format!(
                "CREATE FUNCTION runtime.{function}() RETURNS trigger LANGUAGE plpgsql AS $$"
            ))
            .ok_or("retained_provider_guard_missing")?
            .1
            .split_once("$$;")
            .ok_or("retained_provider_guard_missing")?
            .0;
        let actual: String = db
            .query_one(
                "SELECT prosrc FROM pg_proc WHERE oid=to_regprocedure($1)",
                &[&signature],
            )
            .map_err(|e| e.to_string())?
            .get(0);
        if actual != expected {
            return Err("retained_provider_schema_guard_rejected".into());
        }
    }
    for (table, expected) in [
        ("provider_environment_bindings", 4i64),
        (
            "provider_environment_run_uses",
            if history { 7i64 } else { 6i64 },
        ),
        ("provider_environment_retirements", 4i64),
    ] {
        let row=db.query_one("SELECT count(*),bool_and(convalidated AND NOT condeferrable) FROM pg_constraint WHERE conrelid=to_regclass($1) AND contype<>'n'",&[&format!("runtime.{table}")]).map_err(|e|e.to_string())?;
        if row.get::<_, i64>(0) != expected || row.get::<_, Option<bool>>(1) != Some(true) {
            return Err("retained_provider_schema_constraints_rejected".into());
        }
    }
    if history {
        schema::validate(db)?;
    }
    Ok(())
}
