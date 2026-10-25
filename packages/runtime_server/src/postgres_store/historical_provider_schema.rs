//! Preserve released provider histories without enabling provider execution.
//! DDL and validation are copied from released schema versions 17 through 24.
use postgres::GenericClient;

pub(super) mod cleanup;
pub(super) mod maintenance;
pub(super) mod origin;
pub(super) mod retained;

pub(super) const DDL: &str = r#"
CREATE TABLE runtime.provider_creation_operations(
 attempt_id text PRIMARY KEY,execution_id text NOT NULL UNIQUE,instance_id text NOT NULL UNIQUE,
 environment_id text NOT NULL,session_id text NOT NULL,workspace_id text NOT NULL,agent_run_id text NOT NULL,
 authorization_digest text NOT NULL,profile_digest text NOT NULL,provider text NOT NULL,
 provider_config_digest text NOT NULL,request_digest text NOT NULL,idempotency_key text NOT NULL,
 processing_job_id text NOT NULL REFERENCES runtime.runtime_jobs(job_id),
 dispatch_state text NOT NULL,reported_sandbox_id text,recorded_at_ms bigint NOT NULL,
 CONSTRAINT provider_creation_operations_provider_config_key UNIQUE(provider,provider_config_digest,idempotency_key),
 CONSTRAINT provider_creation_operation_facts CHECK(provider='e2b' AND dispatch_state IN('prepared','dispatched','notDispatched') AND (reported_sandbox_id IS NULL OR dispatch_state='dispatched') AND recorded_at_ms>=0));
CREATE UNIQUE INDEX provider_creation_operations_open_key ON runtime.provider_creation_operations(environment_id) WHERE dispatch_state IN('prepared','dispatched');
CREATE FUNCTION runtime.protect_provider_creation_operation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'provider creation responsibility is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.dispatch_state<>'prepared' OR NEW.reported_sandbox_id IS NOT NULL THEN
   RAISE EXCEPTION 'provider creation requires a prepared intent'; END IF;
 ELSE
  IF ROW(NEW.attempt_id,NEW.execution_id,NEW.instance_id,NEW.environment_id,NEW.session_id,NEW.workspace_id,NEW.agent_run_id,NEW.authorization_digest,NEW.profile_digest,NEW.provider,NEW.provider_config_digest,NEW.request_digest,NEW.idempotency_key,NEW.processing_job_id,NEW.recorded_at_ms)
    IS DISTINCT FROM ROW(OLD.attempt_id,OLD.execution_id,OLD.instance_id,OLD.environment_id,OLD.session_id,OLD.workspace_id,OLD.agent_run_id,OLD.authorization_digest,OLD.profile_digest,OLD.provider,OLD.provider_config_digest,OLD.request_digest,OLD.idempotency_key,OLD.processing_job_id,OLD.recorded_at_ms)
   OR NOT ((OLD.dispatch_state='prepared' AND NEW.dispatch_state IN('dispatched','notDispatched') AND NEW.reported_sandbox_id IS NULL)
    OR (OLD.dispatch_state='dispatched' AND NEW.dispatch_state='dispatched' AND OLD.reported_sandbox_id IS NULL AND NEW.reported_sandbox_id IS NOT NULL))
  THEN RAISE EXCEPTION 'provider creation identity or evidence transition rejected'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER provider_creation_operation_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_creation_operations FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_creation_operation();
"#;

pub(super) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let cleanup = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=18)",
            &[],
        )
        .map_err(|e| e.to_string())?
        .get::<_, bool>(0);
    let typed: bool = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=22)",
            &[],
        )
        .map_err(|e| e.to_string())?
        .get(0);
    let guard = if typed {
        origin::schema::creation_guard()
    } else if cleanup {
        cleanup::GUARD_DDL.to_owned()
    } else {
        DDL.to_owned()
    };
    let body = guard
        .split_once("FUNCTION runtime.protect_provider_creation_operation() RETURNS trigger LANGUAGE plpgsql AS $$")
        .ok_or("provider creation function missing")?
        .1
        .split_once("$$;")
        .ok_or("provider creation function body missing")?
        .0;
    let actual = db.query_opt(
        "SELECT prosrc FROM pg_proc WHERE oid=to_regprocedure('runtime.protect_provider_creation_operation()')",
        &[],
    ).map_err(|error| error.to_string())?;
    if actual.is_none_or(|row| row.get::<_, String>(0) != body) {
        return Err("Postgres provider creation function mismatch".into());
    }
    let trigger = db
        .query_one(
            r#"SELECT count(*) FROM pg_trigger
           WHERE tgrelid='runtime.provider_creation_operations'::regclass
            AND tgname='provider_creation_operation_facts' AND tgenabled='O'
            AND NOT tgisinternal AND tgtype=31
            AND tgfoid='runtime.protect_provider_creation_operation()'::regprocedure
            AND tgqual IS NULL AND tgnargs=0"#,
            &[],
        )
        .map_err(|error| error.to_string())?
        .get::<_, i64>(0);
    if trigger != 1 {
        return Err("Postgres provider creation trigger mismatch".into());
    }
    let constraints = db
        .query_one(
            r#"SELECT count(*),bool_and(convalidated AND NOT condeferrable),
            count(*) FILTER(WHERE contype='p' AND conkey=ARRAY[1]::smallint[]),
            count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[2]::smallint[]),
            count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[3]::smallint[]),
            count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[10,11,13]::smallint[]),
            count(*) FILTER(WHERE contype='f' AND conkey=ARRAY[14]::smallint[]
             AND confrelid='runtime.runtime_jobs'::regclass AND confkey=ARRAY[1]::smallint[]
             AND confdeltype='a' AND confupdtype='a'),
            count(*) FILTER(WHERE contype='c')
           FROM pg_constraint
           WHERE conrelid='runtime.provider_creation_operations'::regclass AND contype<>'n'"#,
            &[],
        )
        .map_err(|error| error.to_string())?;
    if constraints.get::<_, i64>(0)
        != if typed {
            9
        } else if cleanup {
            8
        } else {
            6
        }
        || !constraints.get::<_, bool>(1)
        || (2..=6).any(|column| constraints.get::<_, i64>(column) != 1)
        || constraints.get::<_, i64>(7)
            != if typed {
                3
            } else if cleanup {
                2
            } else {
                1
            }
    {
        return Err("Postgres provider creation identity constraints mismatch".into());
    }
    let facts = db
        .query_opt(
            r#"SELECT pg_get_constraintdef(oid) FROM pg_constraint
           WHERE conrelid='runtime.provider_creation_operations'::regclass
            AND conname='provider_creation_operation_facts' AND contype='c'"#,
            &[],
        )
        .map_err(|error| error.to_string())?
        .map(|row| row.get::<_, String>(0));
    let expected_facts = concat!(
        "CHECK (((provider = 'e2b'::text) AND ",
        "(dispatch_state = ANY (ARRAY['prepared'::text, 'dispatched'::text, 'notDispatched'::text])) ",
        "AND ((reported_sandbox_id IS NULL) OR (dispatch_state = 'dispatched'::text)) ",
        "AND (recorded_at_ms >= 0)))",
    );
    if facts.as_deref() != Some(expected_facts) {
        return Err(format!(
            "Postgres provider creation evidence constraint mismatch: {facts:?}"
        ));
    }
    let index = db
        .query_opt(
            r#"SELECT indisunique AND indisvalid AND indisready AND indnkeyatts=1
            AND indnatts=1 AND indkey::text='4' AND indexprs IS NULL,
            pg_get_expr(indpred,indrelid)
           FROM pg_index
           WHERE indexrelid=to_regclass('runtime.provider_creation_operations_open_key')
            AND indrelid='runtime.provider_creation_operations'::regclass"#,
            &[],
        )
        .map_err(|error| error.to_string())?;
    if index.is_none_or(|row| {
        !row.get::<_, bool>(0)
            || row.get::<_, Option<String>>(1).as_deref()
                != Some(if cleanup { "((dispatch_state = ANY (ARRAY['prepared'::text, 'dispatched'::text])) AND (settlement_id IS NULL))" } else { "(dispatch_state = ANY (ARRAY['prepared'::text, 'dispatched'::text]))" })
    }) {
        return Err("Postgres provider creation open intent index mismatch".into());
    }
    if typed {
        origin::schema::validate(db)?;
    }
    Ok(())
}

/// Activation authority is independent of schema installation. Historical rows
/// remain readable and are never settled or removed by native startup.
pub(super) fn require_settled(_db: &mut impl GenericClient) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
pub(super) fn restore_empty_fixture(db: &mut postgres::Client) {
    super::integration_tests::require_owned_archive_fixture(db);
    let present: bool = db
        .query_one(
            "SELECT to_regclass('runtime.provider_creation_operations') IS NOT NULL",
            &[],
        )
        .unwrap()
        .get(0);
    if !present {
        return;
    }
    for table in [
        "provider_creation_operations",
        "provider_run_cleanup_receipts",
        "provider_environment_bindings",
        "provider_environment_run_uses",
        "provider_environment_retirements",
        "provider_environment_power_operations",
        "provider_environment_provision_completions",
        "provider_environment_maintenance_completions",
    ] {
        let any: bool = db
            .query_one(
                &format!("SELECT EXISTS(SELECT 1 FROM runtime.{table})"),
                &[],
            )
            .unwrap()
            .get(0);
        assert!(!any, "cannot discard historical provider evidence: {table}");
    }
    db.batch_execute("DROP VIEW runtime.provider_environment_power_admission;
        DROP TABLE runtime.provider_environment_maintenance_completions, runtime.provider_environment_provision_completions, runtime.provider_environment_power_operations, runtime.provider_environment_run_uses, runtime.provider_environment_retirements, runtime.provider_environment_bindings, runtime.provider_run_cleanup_receipts, runtime.provider_creation_operations;
        DROP FUNCTION runtime.protect_provider_environment_maintenance_completion(), runtime.protect_provider_environment_provision_completion(), runtime.protect_provider_environment_power(), runtime.protect_retained_provider_run_use(), runtime.protect_provider_environment_retirement(), runtime.protect_provider_environment_binding(), runtime.protect_provider_run_cleanup_receipt(), runtime.protect_provider_creation_operation();
        DELETE FROM runtime.schema_migrations WHERE version>=17;").unwrap();
}
