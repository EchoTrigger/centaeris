use centaeris_core::session::RuntimeJobLeaseFence;
use postgres::GenericClient;

// Capacity signals change eligibility, never the resource debit or execution authority.
// The epoch comparison remains true across lost notifications, yield races and
// process restarts. No release transaction scans or locks waiting Job owners.
pub(super) const DUE_AT: &str = "LEAST(j.run_at_ms,COALESCE((SELECT c.changed_at_ms FROM runtime.resident_capacity_clock c JOIN runtime.resident_capacity_waiters w ON w.job_id=j.job_id WHERE c.singleton AND w.observed_generation<c.generation),j.run_at_ms))";
const TABLE_DDL: &str = r#"
CREATE TABLE runtime.resident_capacity_clock(
 singleton boolean PRIMARY KEY DEFAULT true,
 generation bigint NOT NULL,changed_at_ms bigint NOT NULL,
 CONSTRAINT resident_capacity_clock_bounds CHECK(singleton AND generation>=0 AND changed_at_ms>=0));
INSERT INTO runtime.resident_capacity_clock VALUES(true,0,0);
CREATE TABLE runtime.resident_capacity_waiters(
 job_id text PRIMARY KEY REFERENCES runtime.runtime_jobs(job_id) ON DELETE CASCADE,
 lease_owner text NOT NULL,observed_generation bigint NOT NULL,
 CONSTRAINT resident_capacity_waiter_bounds CHECK(lease_owner<>'' AND observed_generation>=0));
"#;
const RELEASE_BODY: &str = r#"
BEGIN
 IF EXISTS(SELECT 1 FROM released_residents) THEN
  UPDATE runtime.resident_capacity_clock SET generation=generation+1,
   changed_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE singleton;
  IF NOT FOUND THEN RAISE EXCEPTION 'resident capacity clock missing'; END IF;
  PERFORM pg_notify('runtime_job_ready_v1','');
 END IF;
 RETURN NULL;
END;
"#;
const JOB_BODY: &str = r#"
DECLARE signal record;
BEGIN
 IF NEW.status='leased' AND NEW.lease_owner IS DISTINCT FROM OLD.lease_owner THEN
  IF NEW.lease_purpose='execute' THEN
   SELECT c.generation,c.changed_at_ms INTO signal FROM runtime.resident_capacity_clock c
    JOIN runtime.resident_capacity_waiters w ON w.job_id=NEW.job_id
    WHERE c.singleton AND w.observed_generation<c.generation;
   IF FOUND THEN
    INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json)
     VALUES('resident_capacity_claim:'||NEW.job_id||':'||NEW.lease_owner,
      COALESCE(NEW.session_id,'runtime_job:'||NEW.job_id),NEW.job_id,
      'runtime.resident_capacity.claimed.v1',NEW.updated_at_ms,'internal',
      json_build_object('schema','runtime.resident_capacity.claimed.v1','jobId',NEW.job_id,
       'leaseOwner',NEW.lease_owner,'generation',signal.generation,
       'releaseRecordedAtMs',signal.changed_at_ms,'claimedAtMs',NEW.updated_at_ms)::text);
   END IF;
  END IF;
  DELETE FROM runtime.resident_capacity_waiters WHERE job_id=NEW.job_id;
 ELSIF NEW.status IN('succeeded','failed','dead_lettered','cancelled') THEN
  DELETE FROM runtime.resident_capacity_waiters WHERE job_id=NEW.job_id;
 END IF;
 RETURN NEW;
END;
"#;

pub(super) fn ddl() -> String {
    format!("{TABLE_DDL} CREATE FUNCTION runtime.resident_capacity_release() RETURNS trigger LANGUAGE plpgsql AS $$ {RELEASE_BODY} $$; CREATE TRIGGER resident_capacity_release AFTER DELETE ON runtime.resident_sandboxes REFERENCING OLD TABLE AS released_residents FOR EACH STATEMENT EXECUTE FUNCTION runtime.resident_capacity_release(); CREATE FUNCTION runtime.resident_capacity_job_change() RETURNS trigger LANGUAGE plpgsql AS $$ {JOB_BODY} $$; CREATE TRIGGER resident_capacity_job_change AFTER UPDATE OF status,lease_owner ON runtime.runtime_jobs FOR EACH ROW EXECUTE FUNCTION runtime.resident_capacity_job_change();")
}

// Lock availability and first physical confirmation are global opportunities
// to resample preparation. They prove neither host headroom nor a refunded holding.
pub(super) const PREPARATION_AVAILABLE_EVENT: &str = "recorded AS (INSERT INTO runtime.runtime_events(event_id,session_id,task_id,event_type,at_ms,visibility,payload_json) SELECT 'host_preparation_available:'||generation,'runtime_host_preparation:'||$1,$1,'runtime.host_preparation.available.v1',changed_at_ms,'internal',json_build_object('schema','runtime.host_preparation.available.v1','hostId',$1,'scope','globalPreparation','generation',generation,'availableAtMs',changed_at_ms)::text FROM changed ON CONFLICT(event_id) DO NOTHING RETURNING event_id) SELECT pg_notify('runtime_job_ready_v1','') FROM recorded";

/// Record one opportunity to observe newly confirmed residents in this transaction.
pub(super) fn resident_confirmed(
    db: &mut postgres::Transaction<'_>,
    host_id: &str,
) -> Result<(), String> {
    let recorded = db.query(
        &format!("WITH changed AS (UPDATE runtime.resident_capacity_clock SET generation=generation+1,changed_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint WHERE singleton RETURNING generation,changed_at_ms), {PREPARATION_AVAILABLE_EVENT}"),
        &[&host_id],
    ).map_err(|error| format!("record resident confirmation availability failed: {error}"))?;
    if recorded.len() != 1 {
        return Err("resident confirmation availability missing".into());
    }
    Ok(())
}

pub(super) fn register(
    db: &mut impl GenericClient,
    fence: &RuntimeJobLeaseFence,
) -> Result<(), String> {
    let changed=db.execute("INSERT INTO runtime.resident_capacity_waiters(job_id,lease_owner,observed_generation) SELECT $1,$2,generation FROM runtime.resident_capacity_clock WHERE singleton ON CONFLICT(job_id) DO UPDATE SET lease_owner=EXCLUDED.lease_owner,observed_generation=EXCLUDED.observed_generation",&[&fence.job_id,&fence.lease_owner]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("resident capacity clock missing".into());
    }
    Ok(())
}

pub(super) fn clear(
    db: &mut impl GenericClient,
    fence: &RuntimeJobLeaseFence,
) -> Result<(), String> {
    db.execute(
        "DELETE FROM runtime.resident_capacity_waiters WHERE job_id=$1 AND lease_owner=$2",
        &[&fence.job_id, &fence.lease_owner],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let clock=db.query_one("SELECT count(*)=1 AND bool_and(singleton AND generation>=0 AND changed_at_ms>=0) FROM runtime.resident_capacity_clock",&[]).map_err(|e|e.to_string())?;
    if !clock.get::<_, bool>(0) {
        return Err("resident capacity clock invalid".into());
    }
    for (name, body) in [
        ("resident_capacity_release", RELEASE_BODY),
        ("resident_capacity_job_change", JOB_BODY),
    ] {
        let actual=db.query_opt("SELECT p.prosrc FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='runtime' AND p.proname=$1 AND p.pronargs=0",&[&name]).map_err(|e|e.to_string())?;
        if actual.is_none_or(|row| row.get::<_, String>(0).trim() != body.trim()) {
            return Err(format!("resident capacity function mismatch: {name}"));
        }
    }
    for (table, name, kind, old_table) in [
        (
            "resident_sandboxes",
            "resident_capacity_release",
            8i16,
            Some("released_residents"),
        ),
        ("runtime_jobs", "resident_capacity_job_change", 17i16, None),
    ] {
        let valid=db.query_one("SELECT EXISTS(SELECT 1 FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace JOIN pg_proc p ON p.oid=t.tgfoid WHERE n.nspname='runtime' AND c.relname=$1 AND t.tgname=$2 AND p.proname=$2 AND p.pronamespace=n.oid AND t.tgenabled='O' AND NOT t.tgisinternal AND t.tgtype=$3 AND t.tgoldtable IS NOT DISTINCT FROM $4 AND t.tgqual IS NULL AND t.tgnargs=0)",&[&table,&name,&kind,&old_table]).map_err(|e|e.to_string())?.get::<_,bool>(0);
        if !valid {
            return Err(format!("resident capacity trigger mismatch: {name}"));
        }
    }
    for (table, name, definition) in [
        (
            "resident_capacity_clock",
            "resident_capacity_clock_bounds",
            "CHECK ((singleton AND (generation >= 0) AND (changed_at_ms >= 0)))",
        ),
        (
            "resident_capacity_waiters",
            "resident_capacity_waiter_bounds",
            "CHECK (((lease_owner <> ''::text) AND (observed_generation >= 0)))",
        ),
    ] {
        let actual = db.query_opt("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid=('runtime.'||$1)::regclass AND conname=$2 AND contype='c' AND convalidated AND NOT condeferrable", &[&table,&name]).map_err(|e|e.to_string())?.map(|r|r.get::<_,String>(0));
        if actual.as_deref() != Some(definition) {
            return Err(format!(
                "resident capacity bounds mismatch: {name}: {actual:?}"
            ));
        }
    }
    for table in ["resident_capacity_clock", "resident_capacity_waiters"] {
        let keys=db.query_one("SELECT count(*) FROM pg_constraint WHERE conrelid=('runtime.'||$1)::regclass AND contype='p' AND conkey=ARRAY[1]::smallint[] AND convalidated AND NOT condeferrable",&[&table]).map_err(|e|e.to_string())?.get::<_,i64>(0);
        if keys != 1 {
            return Err("resident capacity primary key mismatch".into());
        }
    }
    let fk=db.query_one("SELECT count(*) FROM pg_constraint WHERE conrelid='runtime.resident_capacity_waiters'::regclass AND contype='f' AND conkey=ARRAY[1]::smallint[] AND confrelid='runtime.runtime_jobs'::regclass AND confkey=ARRAY[1]::smallint[] AND confdeltype='c' AND convalidated AND NOT condeferrable",&[]).map_err(|e|e.to_string())?.get::<_,i64>(0);
    if fk != 1 {
        return Err("resident capacity waiter binding mismatch".into());
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn restore_previous_fixture(db: &mut postgres::Client) {
    super::runsc_rotation::restore_previous_fixture(db);
    let version: i64 = db
        .query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
        .unwrap()
        .get(0);
    if version < 16 {
        return;
    }
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.resident_capacity_waiters",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0,
        "cannot discard capacity wait evidence"
    );
    db.batch_execute("DROP TRIGGER resident_capacity_job_change ON runtime.runtime_jobs; DROP TRIGGER resident_capacity_release ON runtime.resident_sandboxes; DROP FUNCTION runtime.resident_capacity_job_change(); DROP FUNCTION runtime.resident_capacity_release(); DROP TABLE runtime.resident_capacity_waiters; DROP TABLE runtime.resident_capacity_clock; DELETE FROM runtime.schema_migrations WHERE version=16;").unwrap();
}
