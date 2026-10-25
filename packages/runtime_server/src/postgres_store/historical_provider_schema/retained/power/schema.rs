//! Released schema metadata only; this module cannot dispatch provider operations.
use postgres::GenericClient;

pub(crate) const DDL: &str = r#"
CREATE TABLE runtime.provider_environment_power_operations(
 operation_id text PRIMARY KEY,
 environment_id text NOT NULL REFERENCES runtime.provider_environment_bindings(environment_id),
 operation_sequence bigint NOT NULL,
 predecessor_operation_id text UNIQUE,
 request_digest text NOT NULL,request_json text NOT NULL,action text NOT NULL,dispatch_state text NOT NULL,
 recorded_at_ms bigint NOT NULL,dispatched_at_ms bigint,acknowledged_at_ms bigint,ack_json text,
 UNIQUE(environment_id,operation_sequence),UNIQUE(environment_id,operation_id),
 FOREIGN KEY(environment_id,predecessor_operation_id) REFERENCES runtime.provider_environment_power_operations(environment_id,operation_id),
 CONSTRAINT provider_environment_power_bounds CHECK(operation_sequence>0 AND recorded_at_ms>=0 AND request_digest ~ '^sha256:[0-9a-f]{64}$' AND request_json<>'' AND action IN('pause','resume')
 AND ((dispatch_state='prepared' AND dispatched_at_ms IS NULL AND acknowledged_at_ms IS NULL AND ack_json IS NULL)
 OR (dispatch_state='dispatched' AND dispatched_at_ms IS NOT NULL AND dispatched_at_ms>=recorded_at_ms
 AND ((acknowledged_at_ms IS NULL AND ack_json IS NULL) OR (acknowledged_at_ms IS NOT NULL AND acknowledged_at_ms>=dispatched_at_ms AND ack_json IS NOT NULL AND ack_json<>''))))));
CREATE FUNCTION runtime.protect_provider_environment_power() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE previous runtime.provider_environment_power_operations;
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'provider power responsibility is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.dispatch_state<>'prepared' OR NEW.dispatched_at_ms IS NOT NULL OR NEW.acknowledged_at_ms IS NOT NULL OR NEW.ack_json IS NOT NULL
  THEN RAISE EXCEPTION 'provider power starts without dispatch or acknowledgement'; END IF;
  SELECT * INTO previous FROM runtime.provider_environment_power_operations WHERE environment_id=NEW.environment_id ORDER BY operation_sequence DESC LIMIT 1;
  IF NOT FOUND THEN
   IF NEW.operation_sequence<>1 OR NEW.predecessor_operation_id IS NOT NULL OR NEW.action<>'pause'
   THEN RAISE EXCEPTION 'provider power initial operation rejected'; END IF;
  ELSIF previous.ack_json IS NULL OR previous.action=NEW.action OR NEW.predecessor_operation_id IS DISTINCT FROM previous.operation_id OR NEW.operation_sequence<>previous.operation_sequence+1
  THEN RAISE EXCEPTION 'provider power predecessor rejected'; END IF;
 ELSE
  IF ROW(NEW.operation_id,NEW.environment_id,NEW.operation_sequence,NEW.predecessor_operation_id,NEW.request_digest,NEW.request_json,NEW.action,NEW.recorded_at_ms)
   IS DISTINCT FROM ROW(OLD.operation_id,OLD.environment_id,OLD.operation_sequence,OLD.predecessor_operation_id,OLD.request_digest,OLD.request_json,OLD.action,OLD.recorded_at_ms)
  THEN RAISE EXCEPTION 'provider power identity is immutable'; END IF;
  IF OLD.dispatch_state='prepared' THEN
   IF NEW.dispatch_state<>'dispatched' OR NEW.dispatched_at_ms IS NULL OR NEW.acknowledged_at_ms IS NOT NULL OR NEW.ack_json IS NOT NULL
   THEN RAISE EXCEPTION 'provider power dispatch transition rejected'; END IF;
  ELSIF OLD.dispatch_state='dispatched' AND OLD.ack_json IS NULL THEN
   IF NEW.dispatch_state<>'dispatched' OR NEW.dispatched_at_ms IS DISTINCT FROM OLD.dispatched_at_ms OR NEW.acknowledged_at_ms IS NULL OR NEW.ack_json IS NULL
   THEN RAISE EXCEPTION 'provider power acknowledgement transition rejected'; END IF;
  ELSE RAISE EXCEPTION 'provider power acknowledgement is immutable'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER provider_environment_power_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_environment_power_operations FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_environment_power();
CREATE VIEW runtime.provider_environment_power_admission AS
 SELECT DISTINCT ON (p.environment_id) p.environment_id,(p.action<>'resume' OR p.ack_json IS NULL) AS blocked
 FROM runtime.provider_environment_power_operations p ORDER BY p.environment_id,p.operation_sequence DESC;
"#;

const VIEW_DEFINITION:&str="SELECT DISTINCT ON (environment_id) environment_id, action <> 'resume'::text OR ack_json IS NULL AS blocked FROM provider_environment_power_operations p ORDER BY environment_id, operation_sequence DESC;";

pub(crate) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let valid:bool=db.query_one("SELECT EXISTS(SELECT 1 FROM pg_trigger WHERE tgrelid='runtime.provider_environment_power_operations'::regclass AND tgname='provider_environment_power_facts' AND tgenabled='O' AND NOT tgisinternal AND tgtype=31 AND tgfoid='runtime.protect_provider_environment_power()'::regprocedure AND tgqual IS NULL AND tgnargs=0)",&[]).map_err(|e|e.to_string())?.get(0);
    if !valid {
        return Err("provider_environment_power_schema_trigger_rejected".into());
    }
    let expected=DDL.split_once("CREATE FUNCTION runtime.protect_provider_environment_power() RETURNS trigger LANGUAGE plpgsql AS $$").ok_or("provider_environment_power_guard_missing")?.1.split_once("$$;").ok_or("provider_environment_power_guard_missing")?.0;
    let actual:String=db.query_one("SELECT prosrc FROM pg_proc WHERE oid='runtime.protect_provider_environment_power()'::regprocedure",&[]).map_err(|e|e.to_string())?.get(0);
    let automatic: bool = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=24)",
            &[],
        )
        .map_err(|e| e.to_string())?
        .get(0);
    if automatic {
        super::automatic_resume::validate(db)?;
    } else if actual != expected {
        return Err("provider_environment_power_schema_guard_rejected".into());
    }
    let constraints=db.query_one("SELECT count(*),bool_and(convalidated AND NOT condeferrable) FROM pg_constraint WHERE conrelid='runtime.provider_environment_power_operations'::regclass AND contype<>'n'",&[]).map_err(|e|e.to_string())?;
    if constraints.get::<_, i64>(0) != 7 || constraints.get::<_, Option<bool>>(1) != Some(true) {
        return Err("provider_environment_power_schema_constraints_rejected".into());
    }
    for (kind, columns, reference, reference_columns) in [
        ("p", vec![1i16], None, None),
        ("u", vec![4], None, None),
        ("u", vec![2, 3], None, None),
        ("u", vec![2, 1], None, None),
        (
            "f",
            vec![2],
            Some("runtime.provider_environment_bindings"),
            Some(vec![1i16]),
        ),
        (
            "f",
            vec![2, 4],
            Some("runtime.provider_environment_power_operations"),
            Some(vec![2, 1]),
        ),
    ] {
        let count:i64=db.query_one("SELECT count(*) FROM pg_constraint WHERE conrelid='runtime.provider_environment_power_operations'::regclass AND contype::text=$1 AND conkey=$2 AND ($3::text IS NULL OR (confrelid=to_regclass($3) AND confkey=$4 AND confdeltype='a')) AND convalidated AND NOT condeferrable",&[&kind,&columns,&reference,&reference_columns]).map_err(|e|e.to_string())?.get(0);
        if count != 1 {
            return Err("provider_environment_power_schema_key_rejected".into());
        }
    }
    let columns:Option<String>=db.query_one("SELECT string_agg(column_name||':'||data_type||':'||is_nullable,',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_schema='runtime' AND table_name='provider_environment_power_admission'",&[]).map_err(|e|e.to_string())?.get(0);
    let view=db.query_opt("SELECT pg_get_viewdef(oid,true) FROM pg_class WHERE oid=to_regclass('runtime.provider_environment_power_admission') AND relkind='v'",&[]).map_err(|e|e.to_string())?.map(|r|r.get::<_,String>(0));
    if columns.as_deref() != Some("environment_id:text:YES,blocked:boolean:YES")
        || view
            .as_ref()
            .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
            .as_deref()
            != Some(VIEW_DEFINITION)
    {
        return Err(format!(
            "provider_environment_power_admission_view_rejected: {view:?}"
        ));
    }
    Ok(())
}
