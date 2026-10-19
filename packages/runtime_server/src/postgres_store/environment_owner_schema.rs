//! Forward migration from Run-only allocation provenance to typed environment ownership.
pub(super) const DDL: &str = r#"
ALTER TABLE runtime.agent_run_cleanup_receipts ADD COLUMN physical_removed boolean NOT NULL DEFAULT false, ADD COLUMN removed_container_id text, ADD CONSTRAINT cleanup_physical_removal_facts CHECK((NOT physical_removed AND removed_container_id IS NULL) OR (physical_removed AND allocation_kind='instance' AND removed_container_id IS NOT NULL AND removed_container_id ~ '^[0-9a-f]{64}$'));
CREATE FUNCTION runtime.protect_agent_run_cleanup_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'AgentRun cleanup receipt is immutable'; END;
$$;
CREATE TRIGGER agent_run_cleanup_receipt_facts BEFORE UPDATE OR DELETE ON runtime.agent_run_cleanup_receipts FOR EACH ROW EXECUTE FUNCTION runtime.protect_agent_run_cleanup_receipt();
ALTER TABLE runtime.execution_instances DISABLE TRIGGER execution_allocation_facts;
ALTER TABLE runtime.resident_sandboxes DISABLE TRIGGER resident_allocation_facts;
ALTER TABLE runtime.execution_instances ADD COLUMN allocation_owner_json text;
ALTER TABLE runtime.resident_sandboxes ADD COLUMN allocation_owner_json text;
UPDATE runtime.execution_instances SET allocation_owner_json=jsonb_build_object('kind','agentRun','runId',agent_run_id)::text;
UPDATE runtime.resident_sandboxes SET allocation_owner_json=jsonb_build_object('kind','agentRun','runId',agent_run_id)::text;
ALTER TABLE runtime.execution_instances ALTER COLUMN allocation_owner_json SET NOT NULL;
ALTER TABLE runtime.resident_sandboxes ALTER COLUMN allocation_owner_json SET NOT NULL;
ALTER TABLE runtime.execution_instances ALTER COLUMN agent_run_id DROP NOT NULL;
ALTER TABLE runtime.resident_sandboxes ALTER COLUMN agent_run_id DROP NOT NULL;
ALTER TABLE runtime.execution_instances ENABLE TRIGGER execution_allocation_facts;
ALTER TABLE runtime.resident_sandboxes ENABLE TRIGGER resident_allocation_facts;
CREATE OR REPLACE FUNCTION runtime.verify_execution_allocation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE anchor runtime.execution_instances; owner jsonb; owner_id text;
BEGIN
 IF TG_OP='UPDATE' AND NEW IS DISTINCT FROM OLD THEN RAISE EXCEPTION 'execution allocation facts are immutable'; END IF;
 IF NEW.allocation_execution_id<>NEW.execution_id THEN
  SELECT * INTO anchor FROM runtime.execution_instances WHERE execution_id=NEW.allocation_execution_id FOR KEY SHARE;
  IF NOT FOUND OR anchor.allocation_execution_id<>anchor.execution_id OR anchor.allocation_mode<>'retained'
   OR ROW(NEW.instance_id,NEW.environment_id,NEW.session_id,NEW.host_id,NEW.image_digest,NEW.allocation_mode,NEW.profile_digest)
    IS DISTINCT FROM ROW(anchor.instance_id,anchor.environment_id,anchor.session_id,anchor.host_id,anchor.image_digest,anchor.allocation_mode,anchor.profile_digest)
  THEN RAISE EXCEPTION 'execution allocation anchor mismatch'; END IF;
  IF NEW.allocation_owner_json IS NULL THEN NEW.allocation_owner_json:=anchor.allocation_owner_json; END IF;
  IF NEW.allocation_owner_json::jsonb IS DISTINCT FROM anchor.allocation_owner_json::jsonb OR NEW.agent_run_id IS NULL
  THEN RAISE EXCEPTION 'execution allocation owner mismatch'; END IF;
 ELSE
  IF NEW.allocation_owner_json IS NULL AND NEW.agent_run_id IS NOT NULL THEN
   NEW.allocation_owner_json:=jsonb_build_object('kind','agentRun','runId',NEW.agent_run_id)::text;
  END IF;
 END IF;
 owner:=NEW.allocation_owner_json::jsonb;
 IF owner->>'kind'='agentRun' THEN
  owner_id:=owner->>'runId';
  IF owner IS DISTINCT FROM jsonb_build_object('kind','agentRun','runId',owner_id)
   OR (NEW.allocation_execution_id=NEW.execution_id AND NEW.agent_run_id IS DISTINCT FROM owner_id)
  THEN RAISE EXCEPTION 'execution allocation owner invalid'; END IF;
 ELSIF owner->>'kind'='maintenanceOperation' THEN
  owner_id:=owner->>'operationId';
  IF owner IS DISTINCT FROM jsonb_build_object('kind','maintenanceOperation','operationId',owner_id)
   OR (NEW.allocation_execution_id=NEW.execution_id AND NEW.agent_run_id IS NOT NULL)
  THEN RAISE EXCEPTION 'execution allocation owner invalid'; END IF;
 ELSE RAISE EXCEPTION 'execution allocation owner invalid'; END IF;
 IF owner_id IS NULL OR length(owner_id)=0 OR length(owner_id)>160 OR owner_id ~ '[[:cntrl:]]'
 THEN RAISE EXCEPTION 'execution allocation owner invalid'; END IF;
 NEW.allocation_owner_json:=owner::text;
 RETURN NEW;
END $$;
CREATE OR REPLACE FUNCTION runtime.verify_resident_allocation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE anchor runtime.execution_instances;
BEGIN
 IF NEW.allocation_mode<>'unbound' THEN
  SELECT * INTO anchor FROM runtime.execution_instances WHERE execution_id=NEW.execution_id FOR KEY SHARE;
  IF NOT FOUND OR anchor.allocation_execution_id<>anchor.execution_id
   OR ROW(NEW.instance_id,NEW.environment_id,NEW.host_id,NEW.agent_run_id,NEW.allocation_mode)
    IS DISTINCT FROM ROW(anchor.instance_id,anchor.environment_id,anchor.host_id,anchor.agent_run_id,anchor.allocation_mode)
  THEN RAISE EXCEPTION 'resident allocation anchor mismatch'; END IF;
  IF NEW.allocation_owner_json IS NULL THEN NEW.allocation_owner_json:=anchor.allocation_owner_json; END IF;
  IF NEW.allocation_owner_json::jsonb IS DISTINCT FROM anchor.allocation_owner_json::jsonb
  THEN RAISE EXCEPTION 'resident allocation owner mismatch'; END IF;
 ELSE
  IF NEW.agent_run_id IS NULL THEN RAISE EXCEPTION 'unbound resident owner missing'; END IF;
  IF NEW.allocation_owner_json IS NULL THEN NEW.allocation_owner_json:=jsonb_build_object('kind','agentRun','runId',NEW.agent_run_id)::text; END IF;
  IF NEW.allocation_owner_json::jsonb IS DISTINCT FROM jsonb_build_object('kind','agentRun','runId',NEW.agent_run_id)
  THEN RAISE EXCEPTION 'unbound resident owner invalid'; END IF;
 END IF;
 NEW.allocation_owner_json:=(NEW.allocation_owner_json::jsonb)::text;
 IF TG_OP='UPDATE' AND ROW(NEW.host_id,NEW.execution_id,NEW.agent_run_id,NEW.workspace_id,NEW.memory_bytes,NEW.cpu_milli,NEW.pids,NEW.workspace_bytes,NEW.instance_id,NEW.environment_id,NEW.allocation_mode,NEW.allocation_owner_json)
  IS DISTINCT FROM ROW(OLD.host_id,OLD.execution_id,OLD.agent_run_id,OLD.workspace_id,OLD.memory_bytes,OLD.cpu_milli,OLD.pids,OLD.workspace_bytes,OLD.instance_id,OLD.environment_id,OLD.allocation_mode,OLD.allocation_owner_json)
 THEN RAISE EXCEPTION 'resident allocation declaration is immutable'; END IF;
 RETURN NEW;
END $$;
CREATE TABLE runtime.environment_provision_completions(
 operation_id text PRIMARY KEY,environment_id text NOT NULL,session_id text NOT NULL,
 workspace_id text NOT NULL,host_id text NOT NULL,instance_id text NOT NULL,container_id text NOT NULL,
 profile_digest text NOT NULL,controller_epoch bigint NOT NULL,controller_lease_digest text NOT NULL,
 request_digest text NOT NULL,completion_json text NOT NULL,recorded_at_ms bigint NOT NULL);
CREATE FUNCTION runtime.protect_environment_provision_completion() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'environment provision completion is immutable'; END;
$$;
CREATE TRIGGER environment_provision_completion_facts BEFORE UPDATE OR DELETE ON runtime.environment_provision_completions FOR EACH ROW EXECUTE FUNCTION runtime.protect_environment_provision_completion();
"#;

/// Build the released predecessor shape only in exclusively owned migration fixtures.
#[cfg(test)]
pub(super) fn restore_previous_fixture(db: &mut postgres::Client) {
    let version: i64 = db
        .query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
        .unwrap()
        .get(0);
    if version < 13 {
        return;
    }
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.environment_provision_completions",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0,
        "fixture cannot discard provision history"
    );
    db.batch_execute("DROP TRIGGER agent_run_cleanup_receipt_facts ON runtime.agent_run_cleanup_receipts; DROP FUNCTION runtime.protect_agent_run_cleanup_receipt(); ALTER TABLE runtime.agent_run_cleanup_receipts DROP COLUMN physical_removed, DROP COLUMN removed_container_id; DROP TABLE runtime.environment_provision_completions; DROP FUNCTION runtime.protect_environment_provision_completion();
        ALTER TABLE runtime.execution_instances DROP COLUMN allocation_owner_json; ALTER TABLE runtime.execution_instances ALTER COLUMN agent_run_id SET NOT NULL;
        ALTER TABLE runtime.resident_sandboxes DROP COLUMN allocation_owner_json; ALTER TABLE runtime.resident_sandboxes ALTER COLUMN agent_run_id SET NOT NULL;
        DELETE FROM runtime.schema_migrations WHERE version=13;").unwrap();
    for name in ["verify_execution_allocation", "verify_resident_allocation"] {
        let prefix =
            format!("CREATE FUNCTION runtime.{name}() RETURNS trigger LANGUAGE plpgsql AS $$");
        let body = super::schema::RETAINED_ACCOUNTING_DDL
            .split_once(&prefix)
            .unwrap()
            .1
            .split_once("$$;")
            .unwrap()
            .0;
        db.batch_execute(&format!("CREATE OR REPLACE FUNCTION runtime.{name}() RETURNS trigger LANGUAGE plpgsql AS $${body}$$;")).unwrap();
    }
}
