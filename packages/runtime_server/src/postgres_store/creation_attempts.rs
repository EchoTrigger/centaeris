//! A dispatch journal is evidence about a physical attempt, never a second resource ledger.
use super::execution_instances::RetainedInstanceAllocation;
use postgres::GenericClient;

pub(super) fn reserve(
    db: &mut impl GenericClient,
    a: &RetainedInstanceAllocation,
) -> Result<(), String> {
    db.execute("INSERT INTO runtime.environment_creation_attempts(allocation_execution_id,instance_id,attempt_id,state,recorded_at_ms) VALUES($1,$2,$3,'prepared',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint)", &[&a.allocation_execution_id,&a.instance_id,&format!("create:{}",a.instance_id)]).map_err(|e|e.to_string())?;
    Ok(())
}

pub(super) fn dispatch(
    db: &mut impl GenericClient,
    a: &RetainedInstanceAllocation,
) -> Result<(), String> {
    let changed=db.execute("UPDATE runtime.environment_creation_attempts SET state='dispatched' WHERE allocation_execution_id=$1 AND instance_id=$2 AND state='prepared'", &[&a.allocation_execution_id,&a.instance_id]).map_err(|e|e.to_string())?;
    if changed != 1 {
        return Err("creation_dispatch_not_authorized".into());
    }
    Ok(())
}

/// Only the fresh attempt's caller may close this branch before its physical callback.
/// The caller holds host serialization and current controller/lease pins in this transaction.
pub(super) fn close_before_dispatch(
    db: &mut impl GenericClient,
    a: &RetainedInstanceAllocation,
) -> Result<(), String> {
    if a.reused || a.container_id.is_some() {
        return Err("creation_non_dispatch_evidence_rejected".into());
    }
    let closed=db.execute("UPDATE runtime.environment_creation_attempts SET state='notDispatched' WHERE allocation_execution_id=$1 AND instance_id=$2 AND state='prepared'", &[&a.allocation_execution_id,&a.instance_id]).map_err(|e|e.to_string())?;
    if closed != 1 {
        return Err("creation_non_dispatch_evidence_rejected".into());
    }
    let released=db.execute("DELETE FROM runtime.resident_sandboxes WHERE execution_id=$1 AND instance_id=$2 AND host_id=$3 AND environment_id=$4 AND state='reserved' AND container_id IS NULL", &[&a.allocation_execution_id,&a.instance_id,&a.host_id,&a.environment_id]).map_err(|e|e.to_string())?;
    if released != 1 {
        return Err("creation_non_dispatch_settlement_unconfirmed".into());
    }
    Ok(())
}

pub(super) fn not_materialized(
    db: &mut impl GenericClient,
    execution: &str,
    instance: &str,
) -> Result<bool, String> {
    db.query_one("SELECT EXISTS(SELECT 1 FROM runtime.environment_creation_attempts a JOIN runtime.execution_instances i ON i.execution_id=a.allocation_execution_id AND i.instance_id=a.instance_id WHERE a.allocation_execution_id=$1 AND a.instance_id=$2 AND a.state='notDispatched' AND i.allocation_execution_id=i.execution_id AND NOT EXISTS(SELECT 1 FROM runtime.resident_sandboxes s WHERE s.execution_id=i.execution_id))", &[&execution,&instance]).map(|r|r.get(0)).map_err(|e|e.to_string())
}

pub(super) const DDL: &str = r#"
CREATE TABLE runtime.environment_creation_attempts(
 allocation_execution_id text PRIMARY KEY,instance_id text NOT NULL,attempt_id text NOT NULL UNIQUE,
 state text NOT NULL CHECK(state IN('prepared','dispatched','notDispatched')),recorded_at_ms bigint NOT NULL,
 FOREIGN KEY(allocation_execution_id,instance_id) REFERENCES runtime.execution_instances(execution_id,instance_id));
CREATE FUNCTION runtime.protect_environment_creation_attempt() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'creation attempt evidence is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.state<>'prepared' OR NOT EXISTS(
   SELECT 1 FROM runtime.execution_instances i JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id AND s.instance_id=i.instance_id
   WHERE i.execution_id=NEW.allocation_execution_id AND i.instance_id=NEW.instance_id AND i.allocation_execution_id=i.execution_id
    AND s.state='reserved' AND s.container_id IS NULL)
  THEN RAISE EXCEPTION 'creation attempt requires a fresh reservation'; END IF;
 ELSE
  IF ROW(NEW.allocation_execution_id,NEW.instance_id,NEW.attempt_id,NEW.recorded_at_ms)
    IS DISTINCT FROM ROW(OLD.allocation_execution_id,OLD.instance_id,OLD.attempt_id,OLD.recorded_at_ms)
   OR OLD.state<>'prepared' OR NEW.state NOT IN('dispatched','notDispatched')
  THEN RAISE EXCEPTION 'creation attempt transition rejected'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER environment_creation_attempt_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.environment_creation_attempts FOR EACH ROW EXECUTE FUNCTION runtime.protect_environment_creation_attempt();
"#;

#[cfg(test)]
pub(super) fn restore_previous_fixture(db: &mut postgres::Client) {
    db.batch_execute("DROP TABLE IF EXISTS runtime.environment_creation_attempts; DROP FUNCTION IF EXISTS runtime.protect_environment_creation_attempt(); DELETE FROM runtime.schema_migrations WHERE version=14;").unwrap();
}
