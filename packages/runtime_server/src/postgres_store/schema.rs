use std::collections::BTreeSet;

use postgres::{Client, GenericClient};

const STORE_SCHEMA_VERSION: i64 = 24;
const MAINTENANCE_COMPLETION_DDL: &str = r#"
CREATE TABLE runtime.environment_maintenance_completions(
 operation_id text PRIMARY KEY,environment_id text NOT NULL,instance_id text NOT NULL,
 container_id text NOT NULL,profile_digest text NOT NULL,controller_epoch bigint NOT NULL,
 controller_lease_digest text NOT NULL,request_digest text NOT NULL,completion_json text NOT NULL,
 recorded_at_ms bigint NOT NULL);
CREATE FUNCTION runtime.protect_environment_maintenance_completion() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'environment maintenance completion is immutable'; END;
$$;
CREATE TRIGGER environment_maintenance_completion_facts BEFORE UPDATE OR DELETE ON runtime.environment_maintenance_completions FOR EACH ROW EXECUTE FUNCTION runtime.protect_environment_maintenance_completion();
"#;
const LEASE_RECLAIM_COLUMNS: &str =
    ",lease_reclaim_count:bigint:NO,lease_reclaim_not_before_ms:bigint:YES";
const PREVIOUS_OBSERVATION_KINDS: &str = "'system_prompt'::text, 'message'::text, 'input_image'::text, 'tool_catalog'::text, 'compaction_prompt'::text, 'input_uptake'::text";
const CURRENT_OBSERVATION_KINDS: &str = "'system_prompt'::text, 'message'::text, 'input_image'::text, 'tool_catalog'::text, 'compaction_prompt'::text, 'input_uptake'::text, 'required_completion_delivery'::text";

pub(super) const RUNTIME_TABLES: &[&str] = &[
    "provider_environment_provision_completions",
    "provider_environment_maintenance_completions",
    "provider_environment_power_operations",
    "provider_environment_bindings",
    "provider_environment_run_uses",
    "provider_environment_retirements",
    "provider_creation_operations",
    "provider_run_cleanup_receipts",
    "environment_creation_attempts",
    "environment_provision_completions",
    "environment_maintenance_completions",
    "environment_retirements",
    "agent_run_cleanup_receipts",
    "execution_instances",
    "execution_job_tenants",
    "checkpoints",
    "dead_letters",
    "external_context_links",
    "external_context_objects",
    "model_observation_contents",
    "model_observation_manifests",
    "resource_claims",
    "resident_capacity_clock",
    "resident_capacity_waiters",
    "resident_sandboxes",
    "runtime_events",
    "runtime_jobs",
    "runtime_job_outbox",
    "runtime_job_waiters",
    "runtime_turn_supplement_queues",
    "schema_migrations",
    "session_runtime_snapshots",
    "transcript_block_identities",
    "transcript_block_versions",
    "transcript_projection_current_recoveries",
    "transcript_projection_commits",
    "transcript_projection_current_generations",
    "transcript_projection_heads",
    "transcript_resume_cursors",
];

const RUNTIME_INDEXES: &[&str] = &[
    "idx_runtime_job_waiters_owner",
    "idx_runtime_job_waiters_source",
    "idx_checkpoints_session_updated",
    "idx_checkpoints_wait_handoff",
    "idx_dead_letters_session_job_kind",
    "idx_dead_letters_status_failed_at",
    "idx_external_context_links_object",
    "idx_external_context_links_session_linked",
    "idx_external_context_objects_kind_updated",
    "idx_external_context_objects_provider_updated",
    "idx_resource_claims_expiry",
    "idx_resource_claims_owner",
    "idx_runtime_events_session_at",
    "idx_runtime_jobs_lease_expiry",
    "idx_runtime_jobs_session_branch_run_at",
    "idx_runtime_jobs_status_run_at",
    "idx_runtime_job_outbox_pending",
    "idx_session_runtime_snapshots_updated",
    "idx_transcript_block_identities_page",
    "idx_transcript_block_versions_latest",
    "idx_transcript_resume_cursors_latest",
];

const TABLE_SHAPES: &[(&str, &str)] = &[
    ("provider_environment_power_operations", "operation_id:text:NO,environment_id:text:NO,operation_sequence:bigint:NO,predecessor_operation_id:text:YES,request_digest:text:NO,request_json:text:NO,action:text:NO,dispatch_state:text:NO,recorded_at_ms:bigint:NO,dispatched_at_ms:bigint:YES,acknowledged_at_ms:bigint:YES,ack_json:text:YES"),
    ("provider_environment_bindings", "environment_id:text:NO,creation_attempt_id:text:NO,recorded_at_ms:bigint:NO"),
    ("provider_environment_run_uses", "provider_use_id:text:NO,agent_run_id:text:NO,environment_id:text:NO,authorization_digest:text:NO,execution_id:text:NO,provider_use_digest:text:NO,recorded_at_ms:bigint:NO,cleanup_receipt_id:text:YES,cleanup_controller_epoch:bigint:YES,quiescence_digest:text:YES,cleanup_recorded_at_ms:bigint:YES"),
    ("provider_environment_retirements", "operation_id:text:NO,environment_id:text:NO,request_digest:text:NO,request_json:text:NO,expected_controller_epoch:bigint:NO,recorded_at_ms:bigint:NO"),
    ("provider_creation_operations", "attempt_id:text:NO,execution_id:text:NO,instance_id:text:NO,environment_id:text:NO,session_id:text:NO,workspace_id:text:NO,agent_run_id:text:NO,authorization_digest:text:NO,profile_digest:text:NO,provider:text:NO,provider_config_digest:text:NO,request_digest:text:NO,idempotency_key:text:NO,processing_job_id:text:NO,dispatch_state:text:NO,reported_sandbox_id:text:YES,recorded_at_ms:bigint:NO"),
    ("provider_run_cleanup_receipts", "receipt_id:text:NO,agent_run_id:text:NO,environment_id:text:NO,authorization_digest:text:NO,profile_digest:text:NO,provider_config_digest:text:NO,controller_epoch:bigint:NO,settlement_digest:text:NO,operation_count:bigint:NO,recorded_at_ms:bigint:NO"),
    ("resident_capacity_clock", "singleton:boolean:NO,generation:bigint:NO,changed_at_ms:bigint:NO"),
    ("resident_capacity_waiters", "job_id:text:NO,lease_owner:text:NO,observed_generation:bigint:NO"),
    ("environment_creation_attempts", "allocation_execution_id:text:NO,instance_id:text:NO,attempt_id:text:NO,state:text:NO,recorded_at_ms:bigint:NO"),
    ("environment_provision_completions","operation_id:text:NO,environment_id:text:NO,session_id:text:NO,workspace_id:text:NO,host_id:text:NO,instance_id:text:NO,container_id:text:NO,profile_digest:text:NO,controller_epoch:bigint:NO,controller_lease_digest:text:NO,request_digest:text:NO,completion_json:text:NO,recorded_at_ms:bigint:NO"),
    ("environment_maintenance_completions", "operation_id:text:NO,environment_id:text:NO,instance_id:text:NO,container_id:text:NO,profile_digest:text:NO,controller_epoch:bigint:NO,controller_lease_digest:text:NO,request_digest:text:NO,completion_json:text:NO,recorded_at_ms:bigint:NO"),
    ("environment_retirements", "operation_id:text:NO,environment_id:text:NO,instance_id:text:NO,host_id:text:NO,allocation_execution_id:text:NO,container_id:text:NO,request_json:text:NO,state:text:NO,recorded_at_ms:bigint:NO"),
    ("agent_run_cleanup_receipts", "receipt_id:text:NO,agent_run_id:text:NO,environment_id:text:NO,authorization_digest:text:NO,controller_epoch:bigint:NO,allocation_kind:text:NO,instance_id:text:YES,cleanup_complete:boolean:NO,file_protection:text:NO,recorded_at_ms:bigint:NO"),
    ("execution_instances", "execution_id:text:NO,instance_id:text:NO,environment_id:text:NO,session_id:text:NO,agent_run_id:text:NO,host_id:text:NO,authorization_digest:text:NO,binding_digest:text:NO,image_digest:text:NO"),
    (
        "resident_sandboxes",
        "host_id:text:NO,execution_id:text:NO,agent_run_id:text:NO,workspace_id:text:NO,container_id:text:YES,state:text:NO,memory_bytes:bigint:NO,cpu_milli:bigint:NO,pids:bigint:NO,workspace_bytes:bigint:NO,created_at_ms:bigint:NO,updated_at_ms:bigint:NO",
    ),
    (
        "execution_job_tenants",
        "job_id:text:NO,workspace_id:text:NO",
    ),
    (
        "runtime_job_waiters",
        "checkpoint_id:text:NO,tool_call_id:text:NO,source_job_id:text:NO,source_job_kind:text:NO,session_id:text:NO,agent_run_id:text:NO",
    ),
    (
        "schema_migrations",
        "version:bigint:NO,applied_at_ms:bigint:NO",
    ),
    (
        "checkpoints",
        "checkpoint_id:text:NO,kind:text:NO,session_id:text:NO,turn_id:text:NO,status:text:NO,done_reason:text:YES,updated_at_ms:bigint:NO,payload_json:text:NO,wait_handoff_json:text:YES",
    ),
    (
        "session_runtime_snapshots",
        "session_id:text:NO,snapshot_json:text:NO,updated_at_ms:bigint:NO",
    ),
    (
        "runtime_events",
        "event_id:text:NO,session_id:text:NO,task_id:text:YES,event_type:text:NO,at_ms:bigint:NO,visibility:text:NO,payload_json:text:NO",
    ),
    (
        "runtime_jobs",
        "job_id:text:NO,job_kind:text:NO,status:text:NO,run_at_ms:bigint:NO,lease_owner:text:YES,lease_expires_at_ms:bigint:YES,retry_count:bigint:NO,max_retries:bigint:NO,backoff_policy_json:text:NO,idempotency_key:text:NO,session_id:text:YES,branch_id:text:YES,checkpoint_id:text:YES,payload_ref:text:YES,output_refs_json:text:NO,last_error:text:YES,created_at_ms:bigint:NO,updated_at_ms:bigint:NO,heartbeat_at_ms:bigint:YES,lease_reclaim_count:bigint:NO,lease_reclaim_not_before_ms:bigint:YES",
    ),
    (
        "runtime_job_outbox",
        "job_id:text:NO,event_type:text:NO,published_at_ms:bigint:YES,generation:bigint:NO",
    ),
    (
        "runtime_turn_supplement_queues",
        "agent_run_id:text:NO,lifecycle_job_id:text:NO,session_id:text:NO,authorization_digest:text:NO,revision:bigint:NO,next_sequence:bigint:NO,accepting:bigint:NO,entries_json:text:NO,dedupe_json:text:NO,closed_reason:text:YES,updated_at_ms:bigint:NO",
    ),
    (
        "resource_claims",
        "resource_kind:text:NO,resource_key:text:NO,owner:text:NO,owner_kind:text:NO,session_id:text:YES,branch_id:text:YES,expires_at_ms:bigint:NO,metadata_json:text:NO,created_at_ms:bigint:NO,updated_at_ms:bigint:NO",
    ),
    (
        "dead_letters",
        "dead_letter_id:text:NO,original_job_id:text:NO,job_kind:text:NO,status:text:NO,session_id:text:YES,branch_id:text:YES,checkpoint_id:text:YES,payload_ref:text:YES,idempotency_key:text:NO,failure_reason:text:NO,last_error:text:NO,attempts:bigint:NO,first_failed_at_ms:bigint:NO,last_failed_at_ms:bigint:NO,replay_policy_json:text:NO,replayed_job_id:text:YES,dismissed_by:text:YES,dismissed_reason:text:YES,updated_at_ms:bigint:NO",
    ),
    (
        "external_context_objects",
        "object_id:text:NO,schema_version:text:NO,object_kind:text:NO,source_provider_id:text:NO,source_tool_name:text:NO,title:text:NO,content:text:NO,metadata_json:text:NO,updated_at_ms:bigint:NO,inserted_at_ms:bigint:NO",
    ),
    (
        "external_context_links",
        "session_id:text:NO,object_id:text:NO,turn_id:text:NO,tool_call_id:text:NO,source_provider_id:text:NO,source_tool_name:text:NO,linked_at_ms:bigint:NO",
    ),
    (
        "model_observation_contents",
        "session_id:text:NO,content_digest:text:NO,kind:text:NO,content_json:text:NO,content_bytes:bigint:NO,first_seen_at_ms:bigint:NO",
    ),
    (
        "model_observation_manifests",
        "session_id:text:NO,manifest_digest:text:NO,parent_digest:text:YES,manifest_json:text:NO,manifest_bytes:bigint:NO,first_seen_at_ms:bigint:NO",
    ),
    (
        "transcript_projection_heads",
        "session_id:text:NO,projection_version:text:NO,projection_generation:text:NO,source_high_water:bigint:NO,invalidation_reason:text:YES",
    ),
    (
        "transcript_projection_current_generations",
        "session_id:text:NO,projection_version:text:NO,projection_generation:text:NO,source_high_water:bigint:NO",
    ),
    (
        "transcript_block_identities",
        "session_id:text:NO,projection_version:text:NO,projection_generation:text:NO,block_id:text:NO,order_source_sequence:bigint:NO,order_ordinal:bigint:NO",
    ),
    (
        "transcript_block_versions",
        "session_id:text:NO,projection_version:text:NO,projection_generation:text:NO,block_id:text:NO,applied_source_sequence:bigint:NO,block_revision:bigint:NO,block_json:text:NO",
    ),
    (
        "transcript_resume_cursors",
        "session_id:text:NO,projection_version:text:NO,projection_generation:text:NO,stream_id:text:NO,source_high_water:bigint:NO,cursor:text:NO",
    ),
    (
        "transcript_projection_current_recoveries",
        "session_id:text:NO,projection_version:text:NO,projection_generation:text:NO,source_high_water:bigint:NO,checkpoint_json:text:NO,frontier_json:text:NO",
    ),
    (
        "transcript_projection_commits",
        "commit_id:text:NO,session_id:text:NO,projection_version:text:NO,projection_generation:text:NO,expected_source_high_water:bigint:NO,source_high_water:bigint:NO,commit_json:text:NO",
    ),
];

const INDEX_SHAPES: &[(&str, &str)] = &[
    (
        "idx_checkpoints_wait_handoff",
        "((wait_handoff_json::jsonb ->> 'handoffKey'::text)) WHERE (wait_handoff_json IS NOT NULL)",
    ),
    (
        "idx_runtime_job_waiters_owner",
        "(agent_run_id, session_id, checkpoint_id)",
    ),
    (
        "idx_runtime_job_waiters_source",
        "(source_job_id, checkpoint_id, tool_call_id)",
    ),
    (
        "idx_checkpoints_session_updated",
        "(session_id, updated_at_ms DESC, checkpoint_id DESC)",
    ),
    (
        "idx_session_runtime_snapshots_updated",
        "(updated_at_ms DESC, session_id)",
    ),
    (
        "idx_runtime_events_session_at",
        "(session_id, at_ms, event_id)",
    ),
    (
        "idx_runtime_jobs_status_run_at",
        "(status, run_at_ms, job_id)",
    ),
    (
        "idx_runtime_jobs_session_branch_run_at",
        "(session_id, branch_id, run_at_ms, job_id)",
    ),
    (
        "idx_runtime_jobs_lease_expiry",
        "(lease_expires_at_ms, job_id)",
    ),
    (
        "idx_runtime_job_outbox_pending",
        "(published_at_ms, job_id, event_type)",
    ),
    (
        "idx_resource_claims_owner",
        "(owner, resource_kind, updated_at_ms DESC)",
    ),
    (
        "idx_resource_claims_expiry",
        "(expires_at_ms, resource_kind, resource_key)",
    ),
    (
        "idx_dead_letters_status_failed_at",
        "(status, last_failed_at_ms DESC, dead_letter_id DESC)",
    ),
    (
        "idx_dead_letters_session_job_kind",
        "(session_id, job_kind, status, last_failed_at_ms DESC, dead_letter_id DESC)",
    ),
    (
        "idx_external_context_objects_provider_updated",
        "(source_provider_id, source_tool_name, updated_at_ms DESC, object_id)",
    ),
    (
        "idx_external_context_objects_kind_updated",
        "(object_kind, updated_at_ms DESC, object_id)",
    ),
    (
        "idx_external_context_links_session_linked",
        "(session_id, linked_at_ms DESC, object_id)",
    ),
    (
        "idx_external_context_links_object",
        "(object_id, linked_at_ms DESC, session_id)",
    ),
    (
        "idx_transcript_block_identities_page",
        "(session_id, projection_version, projection_generation, order_source_sequence DESC, order_ordinal DESC)",
    ),
    (
        "idx_transcript_block_versions_latest",
        "(session_id, projection_version, projection_generation, block_id, applied_source_sequence DESC)",
    ),
    (
        "idx_transcript_resume_cursors_latest",
        "(session_id, projection_version, projection_generation, stream_id, source_high_water DESC)",
    ),
];

pub(super) fn ensure_schema(client: &mut Client) -> Result<(), String> {
    let exists = client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname = 'runtime')",
            &[],
        )
        .map_err(|error| format!("inspect Postgres runtime schema failed: {error}"))?
        .get::<_, bool>(0);
    if !exists {
        create_schema(client)?;
    } else {
        upgrade_completion_delivery_observations(client)?;
        upgrade_lease_reclaim_budget(client)?;
        upgrade_resident_budget(client)?;
        upgrade_execution_instances(client)?;
        upgrade_retained_accounting(client)?;
        upgrade_cleanup_receipts(client)?;
        upgrade_environment_retirements(client)?;
        upgrade_maintenance_completions(client)?;
        upgrade_environment_owners(client)?;
        upgrade_creation_attempts(client)?;
        upgrade_control_leases(client)?;
        upgrade_capacity_waits(client)?;
        upgrade_provider_operations(client)?;
        upgrade_provider_cleanup(client)?;
        upgrade_retained_provider_uses(client)?;
        upgrade_provider_environment_power(client)?;
        upgrade_retained_provider_history(client)?;
        upgrade_provider_creation_owners(client)?;
        upgrade_provider_maintenance_completions(client)?;
        upgrade_cancelled_automatic_resumes(client)?;
    }
    validate_schema_version(client)?;
    validate_schema(client, CURRENT_OBSERVATION_KINDS)
}

fn upgrade_completion_delivery_observations(client: &mut Client) -> Result<(), String> {
    let versions = schema_versions(client)?;
    if versions != [1, 2, 3, 4] {
        return Ok(());
    }
    let mut tx = client
        .transaction()
        .map_err(|error| format!("begin completion observation upgrade failed: {error}"))?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|error| format!("lock completion observation upgrade failed: {error}"))?;
    // Another pool opener may have completed the same forward migration.
    if schema_versions(&mut tx)? != [1, 2, 3, 4] {
        return Ok(());
    }
    validate_schema_with_reclaim_budget(&mut tx, PREVIOUS_OBSERVATION_KINDS, false)?;
    tx.batch_execute(
        "ALTER TABLE runtime.model_observation_contents DROP CONSTRAINT model_observation_contents_kind_check;
         ALTER TABLE runtime.model_observation_contents ADD CONSTRAINT model_observation_contents_kind_check CHECK(kind IN('system_prompt','message','input_image','tool_catalog','compaction_prompt','input_uptake','required_completion_delivery'));"
    ).map_err(|error| format!("upgrade completion observation constraint failed: {error}"))?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(5,$1)",
        &[&now_ms()?],
    )
    .map_err(|error| format!("record completion observation upgrade failed: {error}"))?;
    validate_schema_with_reclaim_budget(&mut tx, CURRENT_OBSERVATION_KINDS, false)?;
    tx.commit()
        .map_err(|error| format!("commit completion observation upgrade failed: {error}"))
}

fn upgrade_lease_reclaim_budget(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != [1, 2, 3, 4, 5] {
        return Ok(());
    }
    let mut tx = client
        .transaction()
        .map_err(|error| format!("begin lease reclaim schema upgrade failed: {error}"))?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|error| format!("lock lease reclaim schema upgrade failed: {error}"))?;
    if schema_versions(&mut tx)? != [1, 2, 3, 4, 5] {
        return Ok(());
    }
    validate_schema_with_reclaim_budget(&mut tx, CURRENT_OBSERVATION_KINDS, false)?;
    tx.batch_execute(
        "ALTER TABLE runtime.runtime_jobs ADD COLUMN lease_reclaim_count bigint NOT NULL DEFAULT 0 CHECK(lease_reclaim_count>=0);
         ALTER TABLE runtime.runtime_jobs ADD COLUMN lease_reclaim_not_before_ms bigint;"
    ).map_err(|error| format!("add lease reclaim budget columns failed: {error}"))?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(6,$1)",
        &[&now_ms()?],
    )
    .map_err(|error| format!("record lease reclaim schema upgrade failed: {error}"))?;
    validate_schema_with_reclaim_budget(&mut tx, CURRENT_OBSERVATION_KINDS, true)?;
    tx.commit()
        .map_err(|error| format!("commit lease reclaim schema upgrade failed: {error}"))
}

const RESIDENT_DDL: &str = "CREATE TABLE runtime.resident_sandboxes(host_id text NOT NULL,execution_id text NOT NULL,agent_run_id text NOT NULL,workspace_id text NOT NULL,container_id text,state text NOT NULL CHECK(state IN('reserved','resident','releasing')),memory_bytes bigint NOT NULL CHECK(memory_bytes>0),cpu_milli bigint NOT NULL CHECK(cpu_milli>0),pids bigint NOT NULL CHECK(pids>0),workspace_bytes bigint NOT NULL CHECK(workspace_bytes>0),created_at_ms bigint NOT NULL,updated_at_ms bigint NOT NULL,PRIMARY KEY(host_id,execution_id),UNIQUE(host_id,container_id));";

fn upgrade_resident_budget(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != [1, 2, 3, 4, 5, 6] {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|error| error.to_string())?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|error| error.to_string())?;
    if schema_versions(&mut tx)? != [1, 2, 3, 4, 5, 6] {
        return Ok(());
    }
    validate_schema_with_reclaim_budget(&mut tx, CURRENT_OBSERVATION_KINDS, true)?;
    tx.batch_execute(RESIDENT_DDL)
        .map_err(|error| format!("create resident budget failed: {error}"))?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(7,$1)",
        &[&now_ms()?],
    )
    .map_err(|error| error.to_string())?;
    validate_schema_with_resource_budgets(&mut tx, CURRENT_OBSERVATION_KINDS, true, true, false)?;
    tx.commit().map_err(|error| error.to_string())
}

const INSTANCE_DDL: &str = "CREATE TABLE runtime.execution_instances(execution_id text PRIMARY KEY,instance_id text NOT NULL UNIQUE,environment_id text NOT NULL,session_id text NOT NULL,agent_run_id text NOT NULL,host_id text NOT NULL,authorization_digest text NOT NULL,binding_digest text NOT NULL,image_digest text NOT NULL);";

// An anchor records immutable allocation provenance, never liveness. The
// resident row remains the only live resource holding, including uncertainty.
pub(super) const RETAINED_ACCOUNTING_DDL: &str = r#"
ALTER TABLE runtime.execution_instances ADD COLUMN allocation_execution_id text;
UPDATE runtime.execution_instances SET allocation_execution_id=execution_id;
ALTER TABLE runtime.execution_instances ALTER COLUMN allocation_execution_id SET NOT NULL;
ALTER TABLE runtime.execution_instances ADD COLUMN allocation_mode text NOT NULL DEFAULT 'perRun' CHECK(allocation_mode IN('perRun','retained'));
ALTER TABLE runtime.execution_instances ADD COLUMN profile_digest text CHECK((allocation_mode='perRun' AND profile_digest IS NULL) OR (allocation_mode='retained' AND profile_digest IS NOT NULL AND profile_digest ~ '^sha256:[0-9a-f]{64}$'));
ALTER TABLE runtime.execution_instances DROP CONSTRAINT execution_instances_instance_id_key;
ALTER TABLE runtime.execution_instances ADD CONSTRAINT execution_instances_execution_instance_key UNIQUE(execution_id,instance_id);
CREATE UNIQUE INDEX execution_instances_anchor_instance_key ON runtime.execution_instances(instance_id) WHERE allocation_execution_id=execution_id;
ALTER TABLE runtime.execution_instances ADD CONSTRAINT execution_instances_allocation_fkey FOREIGN KEY(allocation_execution_id,instance_id) REFERENCES runtime.execution_instances(execution_id,instance_id);
CREATE FUNCTION runtime.verify_execution_allocation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE anchor runtime.execution_instances;
BEGIN
 IF TG_OP='UPDATE' AND NEW IS DISTINCT FROM OLD THEN RAISE EXCEPTION 'execution allocation facts are immutable'; END IF;
 IF NEW.allocation_execution_id<>NEW.execution_id THEN
  SELECT * INTO anchor FROM runtime.execution_instances WHERE execution_id=NEW.allocation_execution_id FOR KEY SHARE;
  IF NOT FOUND OR anchor.allocation_execution_id<>anchor.execution_id OR anchor.allocation_mode<>'retained'
   OR ROW(NEW.instance_id,NEW.environment_id,NEW.session_id,NEW.host_id,NEW.image_digest,NEW.allocation_mode,NEW.profile_digest)
    IS DISTINCT FROM ROW(anchor.instance_id,anchor.environment_id,anchor.session_id,anchor.host_id,anchor.image_digest,anchor.allocation_mode,anchor.profile_digest)
  THEN RAISE EXCEPTION 'execution allocation anchor mismatch'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER execution_allocation_facts BEFORE INSERT OR UPDATE ON runtime.execution_instances FOR EACH ROW EXECUTE FUNCTION runtime.verify_execution_allocation();
ALTER TABLE runtime.resident_sandboxes ADD COLUMN instance_id text;
ALTER TABLE runtime.resident_sandboxes ADD COLUMN environment_id text;
ALTER TABLE runtime.resident_sandboxes ADD COLUMN allocation_mode text NOT NULL DEFAULT 'unbound' CHECK(allocation_mode IN('unbound','perRun','retained'));
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM runtime.resident_sandboxes s JOIN runtime.execution_instances i ON i.execution_id=s.execution_id WHERE s.host_id<>i.host_id OR s.agent_run_id<>i.agent_run_id)
 THEN RAISE EXCEPTION 'resident allocation provenance mismatch'; END IF;
END $$;
UPDATE runtime.resident_sandboxes s SET instance_id=i.instance_id,environment_id=i.environment_id,allocation_mode=i.allocation_mode FROM runtime.execution_instances i WHERE i.execution_id=s.execution_id;
ALTER TABLE runtime.resident_sandboxes ADD CONSTRAINT resident_sandboxes_allocation_complete CHECK((allocation_mode='unbound' AND instance_id IS NULL AND environment_id IS NULL) OR (allocation_mode<>'unbound' AND instance_id IS NOT NULL AND environment_id IS NOT NULL));
ALTER TABLE runtime.resident_sandboxes ADD CONSTRAINT resident_sandboxes_allocation_fkey FOREIGN KEY(execution_id,instance_id) REFERENCES runtime.execution_instances(execution_id,instance_id);
CREATE UNIQUE INDEX resident_sandboxes_instance_key ON runtime.resident_sandboxes(instance_id) WHERE instance_id IS NOT NULL;
CREATE UNIQUE INDEX resident_sandboxes_environment_key ON runtime.resident_sandboxes(environment_id) WHERE allocation_mode='retained';
CREATE FUNCTION runtime.verify_resident_allocation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE anchor runtime.execution_instances;
BEGIN
 IF NEW.allocation_mode<>'unbound' THEN
  SELECT * INTO anchor FROM runtime.execution_instances WHERE execution_id=NEW.execution_id FOR KEY SHARE;
  IF NOT FOUND OR anchor.allocation_execution_id<>anchor.execution_id
   OR ROW(NEW.instance_id,NEW.environment_id,NEW.host_id,NEW.agent_run_id,NEW.allocation_mode)
    IS DISTINCT FROM ROW(anchor.instance_id,anchor.environment_id,anchor.host_id,anchor.agent_run_id,anchor.allocation_mode)
  THEN RAISE EXCEPTION 'resident allocation anchor mismatch'; END IF;
 END IF;
 IF TG_OP='UPDATE' AND ROW(NEW.host_id,NEW.execution_id,NEW.agent_run_id,NEW.workspace_id,NEW.memory_bytes,NEW.cpu_milli,NEW.pids,NEW.workspace_bytes,NEW.instance_id,NEW.environment_id,NEW.allocation_mode)
  IS DISTINCT FROM ROW(OLD.host_id,OLD.execution_id,OLD.agent_run_id,OLD.workspace_id,OLD.memory_bytes,OLD.cpu_milli,OLD.pids,OLD.workspace_bytes,OLD.instance_id,OLD.environment_id,OLD.allocation_mode)
 THEN RAISE EXCEPTION 'resident allocation declaration is immutable'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER resident_allocation_facts BEFORE INSERT OR UPDATE ON runtime.resident_sandboxes FOR EACH ROW EXECUTE FUNCTION runtime.verify_resident_allocation();
"#;

fn upgrade_retained_accounting(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=8).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=8).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema_with_resource_budgets(&mut tx, CURRENT_OBSERVATION_KINDS, true, true, true)?;
    tx.batch_execute(RETAINED_ACCOUNTING_DDL)
        .map_err(|e| format!("upgrade retained accounting failed: {e:?}"))?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(9,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_environment_retirements(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=10).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=10).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::residency::retirement::DDL)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(11,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_cancelled_automatic_resumes(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=23).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=23).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(&super::provider_operations::retained::power::automatic_resume::ddl())
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(24,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_provider_maintenance_completions(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=22).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=22).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::provider_maintenance_schema::DDL)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(23,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_provider_creation_owners(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=21).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=21).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(&super::provider_operations::origin::schema::ddl())
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(22,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_retained_provider_history(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=20).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=20).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::provider_operations::retained::schema::DDL)
        .map_err(|e| format!("upgrade retained provider history failed: {e}"))?;
    super::provider_operations::retained::schema::validate_backfill(&mut tx)?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(21,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_provider_environment_power(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=19).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=19).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::provider_operations::retained::power::schema::DDL)
        .map_err(|e| format!("upgrade provider environment power failed: {e}"))?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(20,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_retained_provider_uses(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=18).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=18).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::provider_operations::retained::DDL)
        .map_err(|e| format!("upgrade retained provider uses failed: {e}"))?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(19,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_provider_cleanup(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=17).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=17).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::provider_operations::cleanup::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::provider_operations::cleanup::GUARD_DDL)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(18,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_provider_operations(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=16).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=16).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::provider_operations::DDL)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(17,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_capacity_waits(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=15).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=15).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(&super::capacity_wait::ddl())
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(16,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_control_leases(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=14).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=14).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::lifecycle_control::DDL)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(15,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_creation_attempts(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=13).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=13).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::creation_attempts::DDL)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(14,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_environment_owners(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=12).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=12).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::environment_owner_schema::DDL)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(13,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_maintenance_completions(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=11).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=11).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(MAINTENANCE_COMPLETION_DDL)
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(12,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_cleanup_receipts(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != (1..=9).collect::<Vec<_>>() {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    super::resource_commit::require_durable_resource_commit(&mut tx)?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != (1..=9).collect::<Vec<_>>() {
        return Ok(());
    }
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.batch_execute(super::cleanup_receipts::DDL)
        .map_err(|e| format!("upgrade cleanup receipts failed: {e}"))?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(10,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema(&mut tx, CURRENT_OBSERVATION_KINDS)?;
    tx.commit().map_err(|e| e.to_string())
}

fn upgrade_execution_instances(client: &mut Client) -> Result<(), String> {
    if schema_versions(client)? != [1, 2, 3, 4, 5, 6, 7] {
        return Ok(());
    }
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    tx.batch_execute("LOCK TABLE runtime.schema_migrations IN ACCESS EXCLUSIVE MODE")
        .map_err(|e| e.to_string())?;
    if schema_versions(&mut tx)? != [1, 2, 3, 4, 5, 6, 7] {
        return Ok(());
    }
    validate_schema_with_resource_budgets(&mut tx, CURRENT_OBSERVATION_KINDS, true, true, false)?;
    tx.batch_execute(INSTANCE_DDL).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO runtime.schema_migrations(version,applied_at_ms) VALUES(8,$1)",
        &[&now_ms()?],
    )
    .map_err(|e| e.to_string())?;
    validate_schema_with_resource_budgets(&mut tx, CURRENT_OBSERVATION_KINDS, true, true, true)?;
    tx.commit().map_err(|e| e.to_string())
}

fn schema_versions(client: &mut impl GenericClient) -> Result<Vec<i64>, String> {
    client
        .query(
            "SELECT version FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .map(|rows| rows.iter().map(|row| row.get(0)).collect())
        .map_err(|error| format!("query Postgres runtime schema version failed: {error}"))
}

fn validate_schema_version(client: &mut Client) -> Result<(), String> {
    let versions = client
        .query(
            "SELECT version FROM runtime.schema_migrations ORDER BY version",
            &[],
        )
        .map_err(|error| format!("query Postgres runtime schema version failed: {error}"))?
        .into_iter()
        .map(|row| row.get::<_, i64>(0))
        .collect::<Vec<_>>();
    let expected = (1..=STORE_SCHEMA_VERSION).collect::<Vec<_>>();
    if versions != expected {
        return Err(format!(
            "Postgres runtime schema version mismatch: expected {expected:?}, got {versions:?}"
        ));
    }
    Ok(())
}

fn create_schema(client: &mut Client) -> Result<(), String> {
    let mut tx = client
        .transaction()
        .map_err(|error| format!("begin runtime schema creation failed: {error}"))?;
    tx.batch_execute(RUNTIME_DDL)
        .map_err(|error| format!("create Postgres runtime schema failed: {error:?}"))?;
    tx.batch_execute(TRANSCRIPT_DDL)
        .map_err(|error| format!("create Postgres transcript schema failed: {error:?}"))?;
    tx.batch_execute(RESIDENT_DDL)
        .map_err(|error| format!("create Postgres resident budget failed: {error:?}"))?;
    tx.batch_execute(INSTANCE_DDL)
        .map_err(|e| format!("create execution instances failed: {e}"))?;
    tx.batch_execute(RETAINED_ACCOUNTING_DDL)
        .map_err(|e| format!("create retained accounting failed: {e}"))?;
    tx.batch_execute(super::cleanup_receipts::DDL)
        .map_err(|e| format!("create cleanup receipts failed: {e}"))?;
    tx.batch_execute(super::residency::retirement::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(MAINTENANCE_COMPLETION_DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::environment_owner_schema::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::creation_attempts::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::lifecycle_control::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(&super::capacity_wait::ddl())
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::provider_operations::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::provider_operations::cleanup::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::provider_operations::cleanup::GUARD_DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::provider_operations::retained::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::provider_operations::retained::power::schema::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::provider_operations::retained::schema::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(&super::provider_operations::origin::schema::ddl())
        .map_err(|e| e.to_string())?;
    tx.batch_execute(super::provider_maintenance_schema::DDL)
        .map_err(|e| e.to_string())?;
    tx.batch_execute(&super::provider_operations::retained::power::automatic_resume::ddl())
        .map_err(|e| e.to_string())?;
    for version in 1..=STORE_SCHEMA_VERSION {
        tx.execute(
            "INSERT INTO runtime.schema_migrations(version, applied_at_ms) VALUES($1, $2)",
            &[&version, &now_ms()?],
        )
        .map_err(|error| format!("record Postgres runtime schema version failed: {error}"))?;
    }
    tx.commit()
        .map_err(|error| format!("commit runtime schema creation failed: {error}"))
}

fn validate_schema(client: &mut impl GenericClient, kinds: &str) -> Result<(), String> {
    validate_schema_with_resource_budgets(client, kinds, true, true, true)
}

fn validate_schema_with_reclaim_budget(
    client: &mut impl GenericClient,
    kinds: &str,
    reclaim_budget: bool,
) -> Result<(), String> {
    validate_schema_with_resource_budgets(client, kinds, reclaim_budget, false, false)
}

fn validate_schema_with_resource_budgets(
    client: &mut impl GenericClient,
    kinds: &str,
    reclaim_budget: bool,
    resident_budget: bool,
    instance_binding: bool,
) -> Result<(), String> {
    let retained_accounting = instance_binding && schema_versions(client)?.contains(&9);
    let cleanup_receipts = retained_accounting && schema_versions(client)?.contains(&10);
    let retirements = cleanup_receipts && schema_versions(client)?.contains(&11);
    let maintenance_completions = retirements && schema_versions(client)?.contains(&12);
    let typed_owners = maintenance_completions && schema_versions(client)?.contains(&13);
    let creation_attempts = typed_owners && schema_versions(client)?.contains(&14);
    let control_leases = creation_attempts && schema_versions(client)?.contains(&15);
    let capacity_waits = control_leases && schema_versions(client)?.contains(&16);
    let provider_operations = capacity_waits && schema_versions(client)?.contains(&17);
    let provider_cleanup = provider_operations && schema_versions(client)?.contains(&18);
    let retained_provider = provider_cleanup && schema_versions(client)?.contains(&19);
    let provider_power = retained_provider && schema_versions(client)?.contains(&20);
    let provider_history = provider_power && schema_versions(client)?.contains(&21);
    let provider_provision = provider_history && schema_versions(client)?.contains(&22);
    let provider_maintenance = provider_provision && schema_versions(client)?.contains(&23);
    let automatic_resume = provider_maintenance && schema_versions(client)?.contains(&24);
    let tables = object_names(client, "BASE TABLE")?;
    let expected_tables = RUNTIME_TABLES
        .iter()
        .copied()
        .filter(|table| {
            (provider_provision || *table != "provider_environment_provision_completions")
                && (provider_maintenance
                    || *table != "provider_environment_maintenance_completions")
                && (capacity_waits
                    || !matches!(
                        *table,
                        "resident_capacity_clock" | "resident_capacity_waiters"
                    ))
                && (provider_power || *table != "provider_environment_power_operations")
                && (creation_attempts || *table != "environment_creation_attempts")
                && (retained_provider
                    || !matches!(
                        *table,
                        "provider_environment_bindings"
                            | "provider_environment_run_uses"
                            | "provider_environment_retirements"
                    ))
                && (provider_operations || *table != "provider_creation_operations")
                && (provider_cleanup || *table != "provider_run_cleanup_receipts")
                && (resident_budget || *table != "resident_sandboxes")
                && (instance_binding || *table != "execution_instances")
                && (cleanup_receipts || *table != "agent_run_cleanup_receipts")
                && (retirements || *table != "environment_retirements")
                && (maintenance_completions || *table != "environment_maintenance_completions")
                && (typed_owners || *table != "environment_provision_completions")
        })
        .map(str::to_string)
        .collect();
    if tables != expected_tables {
        return Err(format!("Postgres runtime table set mismatch: {tables:?}"));
    }
    for (table, expected) in TABLE_SHAPES {
        if (!capacity_waits
            && matches!(
                *table,
                "resident_capacity_clock" | "resident_capacity_waiters"
            ))
            || (*table == "provider_environment_power_operations" && !provider_power)
            || (*table == "environment_creation_attempts" && !creation_attempts)
            || (!retained_provider
                && matches!(
                    *table,
                    "provider_environment_bindings"
                        | "provider_environment_run_uses"
                        | "provider_environment_retirements"
                ))
            || (*table == "provider_creation_operations" && !provider_operations)
            || (*table == "provider_run_cleanup_receipts" && !provider_cleanup)
            || (*table == "resident_sandboxes" && !resident_budget)
            || (*table == "execution_instances" && !instance_binding)
            || (*table == "agent_run_cleanup_receipts" && !cleanup_receipts)
            || (*table == "environment_retirements" && !retirements)
            || (*table == "environment_maintenance_completions" && !maintenance_completions)
            || (*table == "environment_provision_completions" && !typed_owners)
        {
            continue;
        }
        let expected = if *table == "runtime_jobs" && !reclaim_budget {
            expected
                .strip_suffix(LEASE_RECLAIM_COLUMNS)
                .ok_or("previous runtime job shape missing")?
        } else {
            expected
        };
        let expected = match (*table, retained_accounting) {
            ("execution_instances", true) => format!("{expected},allocation_execution_id:text:NO,allocation_mode:text:NO,profile_digest:text:YES"),
            ("resident_sandboxes", true) => format!("{expected},instance_id:text:YES,environment_id:text:YES,allocation_mode:text:NO"),
            _ => expected.to_string(),
        };
        let expected =
            if typed_owners && matches!(*table, "execution_instances" | "resident_sandboxes") {
                format!(
                    "{},allocation_owner_json:text:NO",
                    expected.replace("agent_run_id:text:NO", "agent_run_id:text:YES")
                )
            } else if typed_owners && *table == "agent_run_cleanup_receipts" {
                format!("{expected},physical_removed:boolean:NO,removed_container_id:text:YES")
            } else {
                expected
            };
        let expected = if control_leases && *table == "runtime_jobs" {
            format!("{expected},lease_purpose:text:NO,lease_control_stage:text:YES")
        } else {
            expected
        };
        let expected = if provider_cleanup && *table == "provider_creation_operations" {
            format!("{expected},verified_sandbox_id:text:YES,verification_digest:text:YES,verified_at_ms:bigint:YES,settlement_id:text:YES,settlement_kind:text:YES,settlement_sandbox_id:text:YES,settlement_controller_epoch:bigint:YES,settled_at_ms:bigint:YES")
        } else {
            expected
        };
        let expected = if provider_history && *table == "provider_environment_run_uses" {
            format!("{expected},creation_attempt_id:text:NO")
        } else {
            expected
        };
        let expected = if provider_provision && *table == "provider_creation_operations" {
            format!(
                "{},creation_owner_json:text:NO,maintenance_operation_id:text:YES",
                expected
                    .replace("agent_run_id:text:NO", "agent_run_id:text:YES")
                    .replace("processing_job_id:text:NO", "processing_job_id:text:YES")
            )
        } else {
            expected
        };
        let expected = if automatic_resume && *table == "provider_environment_power_operations" {
            format!("{expected},automatic_demand_json:text:YES,not_dispatched_at_ms:bigint:YES")
        } else {
            expected
        };
        let actual = client.query(
            "SELECT column_name,data_type,is_nullable FROM information_schema.columns WHERE table_schema='runtime' AND table_name=$1 ORDER BY ordinal_position",
            &[table],
        ).map_err(|error| format!("query Postgres runtime columns failed: {error}"))?.into_iter().map(|row| format!("{}:{}:{}",row.get::<_,String>(0),row.get::<_,String>(1),row.get::<_,String>(2))).collect::<Vec<_>>().join(",");
        if actual != expected {
            return Err(format!(
                "Postgres runtime table definition mismatch: {table}"
            ));
        }
    }
    if provider_maintenance {
        super::provider_maintenance_schema::validate(client)?;
    }
    if capacity_waits {
        super::capacity_wait::validate(client)?;
    }
    if provider_cleanup {
        super::provider_operations::cleanup::validate(client)?;
    }
    if retained_provider {
        super::provider_operations::retained::validate(client)?;
    }
    if provider_power {
        super::provider_operations::retained::power::schema::validate(client)?;
    }
    if provider_operations {
        super::provider_operations::validate(client)?;
    }
    if typed_owners {
        let removal = client.query_opt("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.agent_run_cleanup_receipts'::regclass AND conname='cleanup_physical_removal_facts' AND contype='c' AND convalidated", &[]).map_err(|e|e.to_string())?.map(|row|row.get::<_,String>(0));
        if removal.as_deref()!=Some("CHECK ((((NOT physical_removed) AND (removed_container_id IS NULL)) OR (physical_removed AND (allocation_kind = 'instance'::text) AND (removed_container_id IS NOT NULL) AND (removed_container_id ~ '^[0-9a-f]{64}$'::text))))"){return Err("Postgres cleanup physical removal constraint mismatch".into());}
        let cleanup_trigger=client.query_opt("SELECT tgenabled='O' AND NOT tgisinternal AND tgtype=27 AND tgfoid='runtime.protect_agent_run_cleanup_receipt()'::regprocedure AND tgqual IS NULL AND tgnargs=0 FROM pg_trigger WHERE tgrelid='runtime.agent_run_cleanup_receipts'::regclass AND tgname='agent_run_cleanup_receipt_facts'",&[]).map_err(|e|e.to_string())?;
        if !cleanup_trigger.is_some_and(|row| row.get::<_, bool>(0)) {
            return Err("Postgres cleanup receipt trigger mismatch".into());
        }
        let keys=client.query_one("SELECT count(*),bool_and(contype='p' AND conkey=ARRAY[1]::smallint[] AND convalidated AND NOT condeferrable) FROM pg_constraint WHERE conrelid='runtime.environment_provision_completions'::regclass AND contype<>'n'",&[]).map_err(|e|e.to_string())?;
        if keys.get::<_, i64>(0) != 1 || !keys.get::<_, bool>(1) {
            return Err("Postgres provision completion primary key mismatch".into());
        }
        let trigger=client.query_opt("SELECT tgenabled='O' AND NOT tgisinternal AND tgtype=27 AND tgfoid='runtime.protect_environment_provision_completion()'::regprocedure AND tgqual IS NULL AND tgnargs=0 FROM pg_trigger WHERE tgrelid='runtime.environment_provision_completions'::regclass AND tgname='environment_provision_completion_facts'",&[]).map_err(|e|e.to_string())?;
        if !trigger.is_some_and(|row| row.get::<_, bool>(0)) {
            return Err("Postgres provision completion trigger mismatch".into());
        }
    }
    if instance_binding {
        let keys = client.query_one("SELECT COUNT(*) FILTER (WHERE contype='p' AND conkey=ARRAY[1]::smallint[]),COUNT(*) FILTER (WHERE contype='u' AND conkey=ARRAY[2]::smallint[]),COUNT(*) FILTER (WHERE contype='f') FROM pg_constraint WHERE conrelid='runtime.execution_instances'::regclass", &[]).map_err(|e| e.to_string())?;
        if keys.get::<_, i64>(0) != 1
            || keys.get::<_, i64>(1) != i64::from(!retained_accounting)
            || keys.get::<_, i64>(2) != i64::from(retained_accounting)
        {
            return Err("Postgres instance identity constraints mismatch".into());
        }
    }
    if maintenance_completions {
        let keys = client.query_one("SELECT count(*) FROM pg_constraint WHERE conrelid='runtime.environment_maintenance_completions'::regclass AND contype='p' AND conkey=ARRAY[1]::smallint[] AND convalidated AND NOT condeferrable", &[]).map_err(|e| e.to_string())?.get::<_,i64>(0);
        if keys != 1 {
            return Err("Postgres maintenance completion identity constraints mismatch".into());
        }
        let trigger = client.query_one("SELECT count(*) FROM pg_trigger WHERE tgrelid='runtime.environment_maintenance_completions'::regclass AND tgname='environment_maintenance_completion_facts' AND tgenabled='O' AND NOT tgisinternal AND tgtype=27 AND tgfoid='runtime.protect_environment_maintenance_completion()'::regprocedure AND tgqual IS NULL AND tgnargs=0", &[]).map_err(|e| e.to_string())?.get::<_,i64>(0);
        if trigger != 1 {
            return Err("Postgres immutable maintenance completion trigger missing".into());
        }
    }
    if retirements {
        let keys = client.query_one("SELECT COUNT(*) FILTER (WHERE contype='p' AND conkey=ARRAY[1]::smallint[]),COUNT(*) FILTER (WHERE contype='u' AND conkey=ARRAY[2]::smallint[]),COUNT(*) FILTER (WHERE contype='u' AND conkey=ARRAY[3]::smallint[]),COUNT(*) FILTER (WHERE contype='f' AND conkey=ARRAY[5]::smallint[] AND confrelid='runtime.execution_instances'::regclass AND confkey=ARRAY[1]::smallint[] AND convalidated AND NOT condeferrable AND confupdtype='a' AND confdeltype='a') FROM pg_constraint WHERE conrelid='runtime.environment_retirements'::regclass", &[]).map_err(|e| e.to_string())?;
        if (0..4).any(|i| keys.get::<_, i64>(i) != 1) {
            return Err("Postgres retirement identity constraints mismatch".into());
        }
        let trigger = client.query_one("SELECT count(*) FROM pg_trigger WHERE tgrelid='runtime.environment_retirements'::regclass AND tgname='environment_retirement_facts' AND tgenabled='O' AND NOT tgisinternal AND tgtype=27 AND tgfoid='runtime.protect_environment_retirement()'::regprocedure AND tgqual IS NULL AND tgnargs=0", &[]).map_err(|e| e.to_string())?.get::<_,i64>(0);
        if trigger != 1 {
            return Err("Postgres immutable retirement trigger missing".into());
        }
        let state = client.query_opt("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.environment_retirements'::regclass AND conname='environment_retirements_state_check' AND contype='c' AND convalidated", &[]).map_err(|e| e.to_string())?.map(|r| r.get::<_,String>(0));
        if state.as_deref()
            != Some("CHECK ((state = ANY (ARRAY['pending'::text, 'destroyed'::text])))")
        {
            return Err("Postgres retirement state constraint mismatch".into());
        }
    }
    if cleanup_receipts {
        let keys = client.query_one("SELECT COUNT(*) FILTER (WHERE contype='p' AND conkey=ARRAY[1]::smallint[]),COUNT(*) FILTER (WHERE contype='u' AND conkey=ARRAY[2]::smallint[]) FROM pg_constraint WHERE conrelid='runtime.agent_run_cleanup_receipts'::regclass", &[]).map_err(|e|e.to_string())?;
        if keys.get::<_, i64>(0) != 1 || keys.get::<_, i64>(1) != 1 {
            return Err("Postgres cleanup receipt identity constraints mismatch".into());
        }
        for (name, expected) in [
            ("controller_epoch", "CHECK ((controller_epoch > 0))"),
            (
                "allocation_kind",
                "CHECK ((allocation_kind = ANY (ARRAY['instance'::text, 'neverAllocated'::text])))",
            ),
            ("cleanup_complete", "CHECK (cleanup_complete)"),
            (
                "file_protection",
                "CHECK ((file_protection = 'deferred'::text))",
            ),
        ] {
            let actual = client.query_opt("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.agent_run_cleanup_receipts'::regclass AND conname=$1 AND contype='c' AND convalidated", &[&format!("agent_run_cleanup_receipts_{name}_check")]).map_err(|e|e.to_string())?.map(|row|row.get::<_,String>(0));
            if actual.as_deref() != Some(expected) {
                return Err(format!(
                    "Postgres cleanup receipt {name} constraint mismatch"
                ));
            }
        }
        let identity = client.query_opt("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.agent_run_cleanup_receipts'::regclass AND conname='agent_run_cleanup_receipts_check' AND contype='c' AND convalidated", &[]).map_err(|e|e.to_string())?.map(|row|row.get::<_,String>(0));
        if identity.as_deref() != Some("CHECK ((((allocation_kind = 'instance'::text) AND (instance_id IS NOT NULL) AND (length(instance_id) > 0)) OR ((allocation_kind = 'neverAllocated'::text) AND (instance_id IS NULL))))") {
            return Err("Postgres cleanup receipt allocation constraint mismatch".into());
        }
    }
    if resident_budget {
        let keys = client.query_one("SELECT COUNT(*) FILTER (WHERE contype='p' AND conkey=ARRAY[1,2]::smallint[]),COUNT(*) FILTER (WHERE contype='u' AND conkey=ARRAY[1,5]::smallint[]),COUNT(*) FILTER (WHERE contype='f') FROM pg_constraint WHERE conrelid='runtime.resident_sandboxes'::regclass", &[]).map_err(|error| error.to_string())?;
        if keys.get::<_, i64>(0) != 1
            || keys.get::<_, i64>(1) != 1
            || keys.get::<_, i64>(2) != i64::from(retained_accounting)
        {
            return Err("Postgres resident budget identity constraints mismatch".into());
        }
        for (name, expected) in [
            ("state", "CHECK ((state = ANY (ARRAY['reserved'::text, 'resident'::text, 'releasing'::text])))"),
            ("memory_bytes", "CHECK ((memory_bytes > 0))"),
            ("cpu_milli", "CHECK ((cpu_milli > 0))"),
            ("pids", "CHECK ((pids > 0))"),
            ("workspace_bytes", "CHECK ((workspace_bytes > 0))"),
        ] {
            let constraint = client.query_opt("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.resident_sandboxes'::regclass AND conname=$1 AND contype='c'", &[&format!("resident_sandboxes_{name}_check")]).map_err(|error| error.to_string())?.map(|row| row.get::<_, String>(0));
            if constraint.as_deref() != Some(expected) { return Err(format!("Postgres resident budget {name} constraint mismatch")); }
        }
    }
    if retained_accounting {
        for (table, name) in [
            ("execution_instances", "execution_allocation_facts"),
            ("resident_sandboxes", "resident_allocation_facts"),
        ] {
            let present = client.query_one("SELECT count(*) FROM pg_trigger WHERE tgrelid=('runtime.'||$1)::regclass AND tgname=$2 AND tgenabled='O' AND NOT tgisinternal", &[&table,&name]).map_err(|e|e.to_string())?.get::<_,i64>(0);
            if present != 1 {
                return Err("Postgres immutable allocation trigger missing".into());
            }
        }
        for (name, columns, predicate) in [
            (
                "execution_instances_anchor_instance_key",
                "(instance_id)",
                "(allocation_execution_id = execution_id)",
            ),
            (
                "resident_sandboxes_instance_key",
                "(instance_id)",
                "(instance_id IS NOT NULL)",
            ),
            (
                "resident_sandboxes_environment_key",
                "(environment_id)",
                "(allocation_mode = 'retained'::text)",
            ),
        ] {
            let index = client.query_opt("SELECT pg_get_indexdef(indexrelid),pg_get_expr(indpred,indrelid),indisunique,indisvalid FROM pg_index WHERE indexrelid=to_regclass('runtime.'||$1)", &[&name]).map_err(|e|e.to_string())?.ok_or("Postgres allocation uniqueness index missing")?;
            if !index.get::<_, String>(0).contains(columns)
                || index.get::<_, Option<String>>(1).as_deref() != Some(predicate)
                || !index.get::<_, bool>(2)
                || !index.get::<_, bool>(3)
            {
                return Err("Postgres allocation uniqueness index mismatch".into());
            }
        }
    }
    if reclaim_budget {
        let constraint = client.query_opt(
            "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.runtime_jobs'::regclass AND conname='runtime_jobs_lease_reclaim_count_check' AND contype='c'", &[])
            .map_err(|error| format!("query lease reclaim budget constraint failed: {error}"))?
            .map(|row| row.get::<_, String>(0));
        if constraint.as_deref() != Some("CHECK ((lease_reclaim_count >= 0))") {
            return Err("Postgres lease reclaim budget constraint mismatch".into());
        }
        let default = client.query_one("SELECT column_default FROM information_schema.columns WHERE table_schema='runtime' AND table_name='runtime_jobs' AND column_name='lease_reclaim_count'", &[])
            .map_err(|error| format!("query lease reclaim budget default failed: {error}"))?.get::<_, Option<String>>(0);
        if default.as_deref() != Some("0") {
            return Err("Postgres lease reclaim budget default mismatch".into());
        }
    }
    let observation_kind_check = client.query_opt(
        "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.model_observation_contents'::regclass AND conname='model_observation_contents_kind_check' AND contype='c'",
        &[],
    ).map_err(|error| format!("query model observation kind constraint failed: {error}"))?
        .map(|row| row.get::<_, String>(0));
    if observation_kind_check.as_deref()
        != Some(format!("CHECK ((kind = ANY (ARRAY[{kinds}])))").as_str())
    {
        return Err("Postgres model observation kind constraint mismatch".into());
    }
    let waiter_constraints = client.query_one(
        "SELECT COUNT(*) FILTER (WHERE contype='f' AND confrelid='runtime.checkpoints'::regclass AND confdeltype='c' AND conkey=ARRAY[1]::smallint[] AND confkey=ARRAY[1]::smallint[]), COUNT(*) FILTER (WHERE contype='p' AND conkey=ARRAY[1,2]::smallint[]) FROM pg_constraint WHERE conrelid='runtime.runtime_job_waiters'::regclass",
        &[],
    ).map_err(|error| format!("query waiter index constraints failed: {error}"))?;
    if waiter_constraints.get::<_, i64>(0) != 1 || waiter_constraints.get::<_, i64>(1) != 1 {
        return Err("Postgres waiter index constraint mismatch".to_string());
    }
    let tenant_constraints = client.query_one(
        "SELECT COUNT(*) FILTER (WHERE contype='f' AND confrelid='runtime.runtime_jobs'::regclass AND confdeltype='c' AND conkey=ARRAY[1]::smallint[] AND confkey=ARRAY[1]::smallint[]), COUNT(*) FILTER (WHERE contype='p' AND conkey=ARRAY[1]::smallint[]) FROM pg_constraint WHERE conrelid='runtime.execution_job_tenants'::regclass", &[],
    ).map_err(|error| format!("query execution tenant constraints failed: {error}"))?;
    if tenant_constraints.get::<_, i64>(0) != 1 || tenant_constraints.get::<_, i64>(1) != 1 {
        return Err("Postgres execution tenant constraint mismatch".to_string());
    }
    let indexes = client
        .query(
            "SELECT indexname FROM pg_indexes WHERE schemaname = 'runtime' ORDER BY indexname",
            &[],
        )
        .map_err(|error| format!("query Postgres runtime indexes failed: {error}"))?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .filter(|name| !name.ends_with("_pkey") && !name.ends_with("_key"))
        .collect::<BTreeSet<_>>();
    let expected_indexes = RUNTIME_INDEXES
        .iter()
        .copied()
        .map(str::to_string)
        .collect();
    if indexes != expected_indexes {
        return Err(format!("Postgres runtime index set mismatch: {indexes:?}"));
    }
    for (name, expected_fragment) in INDEX_SHAPES {
        if *name == "idx_checkpoints_wait_handoff" {
            let shape = client.query_one(
                "SELECT indisunique, indnkeyatts, pg_get_expr(indexprs,indrelid), pg_get_expr(indpred,indrelid) FROM pg_index WHERE indexrelid='runtime.idx_checkpoints_wait_handoff'::regclass",
                &[],
            ).map_err(|error| format!("query Postgres wait handoff index failed: {error}"))?;
            if !shape.get::<_, bool>(0)
                || shape.get::<_, i16>(1) != 1
                || shape.get::<_, Option<String>>(2).as_deref()
                    != Some("((wait_handoff_json)::jsonb ->> 'handoffKey'::text)")
                || shape.get::<_, Option<String>>(3).as_deref()
                    != Some("(wait_handoff_json IS NOT NULL)")
            {
                return Err(
                    "Postgres runtime index definition mismatch: idx_checkpoints_wait_handoff"
                        .into(),
                );
            }
            continue;
        }
        let definition = client
            .query_one(
                "SELECT indexdef FROM pg_indexes WHERE schemaname='runtime' AND indexname=$1",
                &[name],
            )
            .map_err(|error| format!("query Postgres runtime index definition failed: {error}"))?
            .get::<_, String>(0);
        if !definition.contains(expected_fragment) {
            return Err(format!(
                "Postgres runtime index definition mismatch: {name}"
            ));
        }
    }
    let functions = client
        .query(
            "SELECT routine_name FROM information_schema.routines WHERE routine_schema='runtime' ORDER BY routine_name",
            &[],
        )
        .map_err(|error| format!("query Postgres runtime functions failed: {error}"))?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    let mut expected_functions = BTreeSet::from(["notify_runtime_job_ready_v1".to_string()]);
    if provider_provision {
        expected_functions.insert("protect_provider_environment_provision_completion".into());
    }
    if provider_maintenance {
        expected_functions.insert("protect_provider_environment_maintenance_completion".into());
    }
    if typed_owners {
        expected_functions.insert("protect_environment_provision_completion".into());
        expected_functions.insert("protect_agent_run_cleanup_receipt".into());
        let body=super::environment_owner_schema::DDL.split_once("CREATE FUNCTION runtime.protect_agent_run_cleanup_receipt() RETURNS trigger LANGUAGE plpgsql AS $$").ok_or("cleanup receipt function missing")?.1.split_once("$$;").ok_or("cleanup receipt body missing")?.0;
        let actual=client.query_one("SELECT prosrc FROM pg_proc WHERE oid='runtime.protect_agent_run_cleanup_receipt()'::regprocedure",&[]).map_err(|e|e.to_string())?.get::<_,String>(0);
        if actual != body {
            return Err("Postgres cleanup receipt function mismatch".into());
        }
        let body=super::environment_owner_schema::DDL.split_once("CREATE FUNCTION runtime.protect_environment_provision_completion() RETURNS trigger LANGUAGE plpgsql AS $$").ok_or("provision completion function missing")?.1.split_once("$$;").ok_or("provision completion body missing")?.0;
        let actual=client.query_one("SELECT prosrc FROM pg_proc WHERE oid='runtime.protect_environment_provision_completion()'::regprocedure",&[]).map_err(|e|e.to_string())?.get::<_,String>(0);
        if actual != body {
            return Err("Postgres provision completion function mismatch".into());
        }
    }
    if maintenance_completions {
        expected_functions.insert("protect_environment_maintenance_completion".into());
        let expected = MAINTENANCE_COMPLETION_DDL.split_once("CREATE FUNCTION runtime.protect_environment_maintenance_completion() RETURNS trigger LANGUAGE plpgsql AS $$").ok_or("maintenance completion function declaration missing")?.1.split_once("$$;").ok_or("maintenance completion function body missing")?.0;
        let actual = client.query_one("SELECT prosrc FROM pg_proc WHERE oid='runtime.protect_environment_maintenance_completion()'::regprocedure", &[]).map_err(|e| e.to_string())?.get::<_,String>(0);
        if actual != expected {
            return Err(
                "Postgres immutable maintenance completion trigger function mismatch".into(),
            );
        }
    }
    if retirements {
        expected_functions.insert("protect_environment_retirement".into());
        let expected = super::residency::retirement::DDL.split_once("CREATE FUNCTION runtime.protect_environment_retirement() RETURNS trigger LANGUAGE plpgsql AS $$").ok_or("retirement function declaration missing")?.1.split_once("$$;").ok_or("retirement function body missing")?.0;
        let actual = client.query_one("SELECT prosrc FROM pg_proc WHERE oid='runtime.protect_environment_retirement()'::regprocedure", &[]).map_err(|e| e.to_string())?.get::<_,String>(0);
        if actual != expected {
            return Err("Postgres immutable retirement function mismatch".into());
        }
    }
    if retained_accounting {
        for name in ["verify_execution_allocation", "verify_resident_allocation"] {
            expected_functions.insert(name.to_string());
            let prefix = format!(
                "CREATE {}FUNCTION runtime.{name}() RETURNS trigger LANGUAGE plpgsql AS $$",
                if typed_owners { "OR REPLACE " } else { "" }
            );
            let ddl = if typed_owners {
                super::environment_owner_schema::DDL
            } else {
                RETAINED_ACCOUNTING_DDL
            };
            let expected_body = ddl
                .split_once(&prefix)
                .ok_or("allocation function declaration missing")?
                .1
                .split_once("$$;")
                .ok_or("allocation function body missing")?
                .0;
            let actual = client
                .query_one(
                    "SELECT prosrc FROM pg_proc WHERE oid=('runtime.'||$1||'()')::regprocedure",
                    &[&name],
                )
                .map_err(|e| e.to_string())?
                .get::<_, String>(0);
            if actual != expected_body {
                return Err("Postgres immutable allocation function mismatch".into());
            }
        }
    }
    if creation_attempts {
        expected_functions.insert("protect_environment_creation_attempt".into());
        let expected=super::creation_attempts::DDL.split_once("CREATE FUNCTION runtime.protect_environment_creation_attempt() RETURNS trigger LANGUAGE plpgsql AS $$").ok_or("creation attempt function missing")?.1.split_once("$$;").ok_or("creation attempt body missing")?.0;
        let actual=client.query_one("SELECT prosrc FROM pg_proc WHERE oid='runtime.protect_environment_creation_attempt()'::regprocedure",&[]).map_err(|e|e.to_string())?.get::<_,String>(0);
        if actual != expected {
            return Err("Postgres creation attempt function mismatch".into());
        }
        let keys=client.query_one("SELECT count(*),bool_and(convalidated AND NOT condeferrable),count(*) FILTER(WHERE contype='p' AND conkey=ARRAY[1]::smallint[]),count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[3]::smallint[]),count(*) FILTER(WHERE contype='f' AND conkey=ARRAY[1,2]::smallint[] AND confrelid='runtime.execution_instances'::regclass AND confkey=ARRAY[1,2]::smallint[]),count(*) FILTER(WHERE contype='c') FROM pg_constraint WHERE conrelid='runtime.environment_creation_attempts'::regclass AND contype<>'n'",&[]).map_err(|e|e.to_string())?;
        if keys.get::<_, i64>(0) != 4
            || !keys.get::<_, bool>(1)
            || (2..=5).any(|n| keys.get::<_, i64>(n) != 1)
        {
            return Err("Postgres creation attempt constraints mismatch".into());
        }
        let trigger=client.query_one("SELECT count(*) FROM pg_trigger WHERE tgrelid='runtime.environment_creation_attempts'::regclass AND tgname='environment_creation_attempt_facts' AND tgenabled='O' AND NOT tgisinternal AND tgtype=31 AND tgfoid='runtime.protect_environment_creation_attempt()'::regprocedure AND tgqual IS NULL AND tgnargs=0",&[]).map_err(|e|e.to_string())?.get::<_,i64>(0);
        if trigger != 1 {
            return Err("Postgres creation attempt trigger mismatch".into());
        }
    }
    if control_leases {
        expected_functions.insert("protect_lifecycle_lease_purpose".into());
        let purpose=client.query_opt("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.runtime_jobs'::regclass AND conname='runtime_jobs_lease_purpose_check' AND contype='c' AND convalidated AND NOT condeferrable",&[]).map_err(|e|e.to_string())?.map(|row|row.get::<_,String>(0));
        if purpose.as_deref()
            != Some("CHECK ((lease_purpose = ANY (ARRAY['execute'::text, 'control'::text])))")
        {
            return Err("Postgres lifecycle lease purpose constraint mismatch".into());
        }
        let stage=client.query_opt("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='runtime.runtime_jobs'::regclass AND conname='runtime_jobs_control_stage_check' AND contype='c' AND convalidated AND NOT condeferrable",&[]).map_err(|e|e.to_string())?.map(|row|row.get::<_,String>(0));
        if stage.as_deref()!=Some("CHECK ((((lease_purpose = 'execute'::text) AND (lease_control_stage IS NULL)) OR ((lease_purpose = 'control'::text) AND (lease_control_stage = ANY (ARRAY['advance'::text, 'cleanup'::text])) AND (lease_control_stage IS NOT NULL))))") {return Err(format!("Postgres lifecycle control stage constraint mismatch: {stage:?}"));}
        let default=client.query_one("SELECT column_default FROM information_schema.columns WHERE table_schema='runtime' AND table_name='runtime_jobs' AND column_name='lease_purpose'",&[]).map_err(|e|e.to_string())?.get::<_,Option<String>>(0);
        if default.as_deref() != Some("'execute'::text") {
            return Err("Postgres lifecycle lease purpose default mismatch".into());
        }
        let expected=super::lifecycle_control::DDL.split_once("CREATE FUNCTION runtime.protect_lifecycle_lease_purpose() RETURNS trigger LANGUAGE plpgsql AS $$").ok_or("lease purpose function missing")?.1.split_once("$$;").ok_or("lease purpose function body missing")?.0;
        let actual=client.query_one("SELECT prosrc FROM pg_proc WHERE oid='runtime.protect_lifecycle_lease_purpose()'::regprocedure",&[]).map_err(|e|e.to_string())?.get::<_,String>(0);
        if actual != expected {
            return Err("Postgres lifecycle lease purpose function mismatch".into());
        }
        let trigger=client.query_one("SELECT count(*) FROM pg_trigger WHERE tgrelid='runtime.runtime_jobs'::regclass AND tgname='lifecycle_lease_purpose_facts' AND tgenabled='O' AND NOT tgisinternal AND tgtype=19 AND tgfoid='runtime.protect_lifecycle_lease_purpose()'::regprocedure AND tgqual IS NULL AND tgnargs=0",&[]).map_err(|e|e.to_string())?.get::<_,i64>(0);
        if trigger != 1 {
            return Err("Postgres lifecycle lease purpose trigger mismatch".into());
        }
    }
    if capacity_waits {
        expected_functions.extend([
            "resident_capacity_release".into(),
            "resident_capacity_job_change".into(),
        ]);
    }
    if provider_operations {
        expected_functions.insert("protect_provider_creation_operation".into());
    }
    if provider_cleanup {
        expected_functions.insert("protect_provider_run_cleanup_receipt".into());
    }
    if retained_provider {
        expected_functions.extend([
            "protect_provider_environment_binding".into(),
            "protect_retained_provider_run_use".into(),
            "protect_provider_environment_retirement".into(),
        ]);
    }
    if provider_power {
        expected_functions.insert("protect_provider_environment_power".into());
    }
    if functions != expected_functions {
        return Err(format!(
            "Postgres runtime function set mismatch: {functions:?}"
        ));
    }
    let triggers = client
        .query(
            "SELECT trigger_name FROM information_schema.triggers WHERE trigger_schema='runtime' ORDER BY trigger_name",
            &[],
        )
        .map_err(|error| format!("query Postgres runtime triggers failed: {error}"))?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    let mut expected_triggers = BTreeSet::from(["runtime_jobs_ready_notify_v1".to_string()]);
    if provider_provision {
        expected_triggers.insert("provider_environment_provision_completion_facts".into());
    }
    if provider_maintenance {
        expected_triggers.insert("provider_environment_maintenance_completion_facts".into());
    }
    if control_leases {
        expected_triggers.insert("lifecycle_lease_purpose_facts".into());
    }
    if typed_owners {
        expected_triggers.insert("environment_provision_completion_facts".into());
        expected_triggers.insert("agent_run_cleanup_receipt_facts".into());
    }
    if maintenance_completions {
        expected_triggers.insert("environment_maintenance_completion_facts".into());
    }
    if retirements {
        expected_triggers.insert("environment_retirement_facts".into());
    }
    if retained_accounting {
        expected_triggers.extend([
            "execution_allocation_facts".into(),
            "resident_allocation_facts".into(),
        ]);
    }
    if creation_attempts {
        expected_triggers.insert("environment_creation_attempt_facts".into());
    }
    if capacity_waits {
        expected_triggers.extend([
            "resident_capacity_release".into(),
            "resident_capacity_job_change".into(),
        ]);
    }
    if provider_operations {
        expected_triggers.insert("provider_creation_operation_facts".into());
    }
    if provider_cleanup {
        expected_triggers.insert("provider_run_cleanup_receipt_facts".into());
    }
    if retained_provider {
        expected_triggers.extend([
            "provider_environment_binding_facts".into(),
            "retained_provider_run_use_facts".into(),
            "provider_environment_retirement_facts".into(),
        ]);
    }
    if provider_power {
        expected_triggers.insert("provider_environment_power_facts".into());
    }
    if triggers != expected_triggers {
        return Err(format!(
            "Postgres runtime trigger set mismatch: {triggers:?}"
        ));
    }
    Ok(())
}

fn object_names(
    client: &mut impl GenericClient,
    table_type: &str,
) -> Result<BTreeSet<String>, String> {
    client
        .query(
            "SELECT table_name FROM information_schema.tables WHERE table_schema = 'runtime' AND table_type = $1 ORDER BY table_name",
            &[&table_type],
        )
        .map_err(|error| format!("query Postgres runtime tables failed: {error}"))
        .map(|rows| rows.into_iter().map(|row| row.get(0)).collect())
}

fn now_ms() -> Result<i64, String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system time before epoch: {error}"))?;
    i64::try_from(duration.as_millis()).map_err(|_| "timestamp overflow".to_string())
}

const RUNTIME_DDL: &str = r#"
CREATE SCHEMA runtime;
CREATE TABLE runtime.schema_migrations(version bigint PRIMARY KEY, applied_at_ms bigint NOT NULL);
CREATE TABLE runtime.checkpoints(checkpoint_id text PRIMARY KEY, kind text NOT NULL CHECK(kind IN('wait','recovery')), session_id text NOT NULL, turn_id text NOT NULL, status text NOT NULL, done_reason text, updated_at_ms bigint NOT NULL, payload_json text NOT NULL, wait_handoff_json text);
CREATE UNIQUE INDEX idx_checkpoints_wait_handoff ON runtime.checkpoints ((wait_handoff_json::jsonb ->> 'handoffKey'::text)) WHERE (wait_handoff_json IS NOT NULL);
CREATE TABLE runtime.runtime_job_waiters(checkpoint_id text NOT NULL REFERENCES runtime.checkpoints(checkpoint_id) ON DELETE CASCADE, tool_call_id text NOT NULL, source_job_id text NOT NULL, source_job_kind text NOT NULL, session_id text NOT NULL, agent_run_id text NOT NULL, PRIMARY KEY(checkpoint_id,tool_call_id));
CREATE INDEX idx_runtime_job_waiters_source ON runtime.runtime_job_waiters(source_job_id,checkpoint_id,tool_call_id);
CREATE INDEX idx_runtime_job_waiters_owner ON runtime.runtime_job_waiters(agent_run_id,session_id,checkpoint_id);
CREATE TABLE runtime.session_runtime_snapshots(session_id text PRIMARY KEY, snapshot_json text NOT NULL, updated_at_ms bigint NOT NULL);
CREATE TABLE runtime.runtime_events(event_id text PRIMARY KEY, session_id text NOT NULL, task_id text, event_type text NOT NULL, at_ms bigint NOT NULL, visibility text NOT NULL, payload_json text NOT NULL);
CREATE TABLE runtime.runtime_jobs(job_id text PRIMARY KEY, job_kind text NOT NULL, status text NOT NULL, run_at_ms bigint NOT NULL, lease_owner text, lease_expires_at_ms bigint, retry_count bigint NOT NULL DEFAULT 0, max_retries bigint NOT NULL DEFAULT 0, backoff_policy_json text NOT NULL, idempotency_key text NOT NULL, session_id text, branch_id text, checkpoint_id text, payload_ref text, output_refs_json text NOT NULL DEFAULT '[]', last_error text, created_at_ms bigint NOT NULL, updated_at_ms bigint NOT NULL, heartbeat_at_ms bigint, lease_reclaim_count bigint NOT NULL DEFAULT 0 CHECK(lease_reclaim_count>=0), lease_reclaim_not_before_ms bigint, UNIQUE(job_kind, idempotency_key));
CREATE TABLE runtime.execution_job_tenants(job_id text PRIMARY KEY REFERENCES runtime.runtime_jobs(job_id) ON DELETE CASCADE,workspace_id text NOT NULL);
CREATE TABLE runtime.runtime_job_outbox(job_id text NOT NULL REFERENCES runtime.runtime_jobs(job_id) ON DELETE CASCADE,event_type text NOT NULL,published_at_ms bigint,generation bigint NOT NULL DEFAULT 0,PRIMARY KEY(job_id,event_type));
CREATE TABLE runtime.runtime_turn_supplement_queues(agent_run_id text PRIMARY KEY,lifecycle_job_id text NOT NULL UNIQUE REFERENCES runtime.runtime_jobs(job_id) ON DELETE CASCADE,session_id text NOT NULL,authorization_digest text NOT NULL,revision bigint NOT NULL,next_sequence bigint NOT NULL,accepting bigint NOT NULL CHECK(accepting IN(0,1)),entries_json text NOT NULL,dedupe_json text NOT NULL,closed_reason text,updated_at_ms bigint NOT NULL);
CREATE TABLE runtime.resource_claims(resource_kind text NOT NULL, resource_key text NOT NULL, owner text NOT NULL, owner_kind text NOT NULL, session_id text, branch_id text, expires_at_ms bigint NOT NULL, metadata_json text NOT NULL, created_at_ms bigint NOT NULL, updated_at_ms bigint NOT NULL, PRIMARY KEY(resource_kind, resource_key));
CREATE TABLE runtime.dead_letters(dead_letter_id text PRIMARY KEY, original_job_id text NOT NULL UNIQUE, job_kind text NOT NULL, status text NOT NULL, session_id text, branch_id text, checkpoint_id text, payload_ref text, idempotency_key text NOT NULL, failure_reason text NOT NULL, last_error text NOT NULL, attempts bigint NOT NULL DEFAULT 0, first_failed_at_ms bigint NOT NULL, last_failed_at_ms bigint NOT NULL, replay_policy_json text NOT NULL, replayed_job_id text, dismissed_by text, dismissed_reason text, updated_at_ms bigint NOT NULL);
CREATE TABLE runtime.external_context_objects(object_id text PRIMARY KEY, schema_version text NOT NULL, object_kind text NOT NULL, source_provider_id text NOT NULL, source_tool_name text NOT NULL, title text NOT NULL, content text NOT NULL, metadata_json text NOT NULL, updated_at_ms bigint NOT NULL, inserted_at_ms bigint NOT NULL);
CREATE TABLE runtime.external_context_links(session_id text NOT NULL, object_id text NOT NULL, turn_id text NOT NULL DEFAULT '', tool_call_id text NOT NULL DEFAULT '', source_provider_id text NOT NULL, source_tool_name text NOT NULL, linked_at_ms bigint NOT NULL, PRIMARY KEY(session_id, object_id, turn_id, tool_call_id));
CREATE TABLE runtime.model_observation_contents(session_id text NOT NULL, content_digest text NOT NULL CHECK(content_digest ~ '^sha256:[0-9a-f]{64}$'), kind text NOT NULL CHECK(kind IN('system_prompt','message','input_image','tool_catalog','compaction_prompt','input_uptake','required_completion_delivery')), content_json text NOT NULL, content_bytes bigint NOT NULL CHECK(content_bytes >= 0), first_seen_at_ms bigint NOT NULL, PRIMARY KEY(session_id, content_digest));
CREATE TABLE runtime.model_observation_manifests(session_id text NOT NULL, manifest_digest text NOT NULL CHECK(manifest_digest ~ '^sha256:[0-9a-f]{64}$'), parent_digest text CHECK(parent_digest ~ '^sha256:[0-9a-f]{64}$'), manifest_json text NOT NULL, manifest_bytes bigint NOT NULL CHECK(manifest_bytes >= 0), first_seen_at_ms bigint NOT NULL, PRIMARY KEY(session_id, manifest_digest));
CREATE FUNCTION runtime.notify_runtime_job_ready_v1() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF OLD.job_kind = 'agent_run.lifecycle' AND OLD.status IN ('leased','running') AND NEW.status NOT IN ('leased','running') THEN
            PERFORM pg_notify('runtime_job_ready_v1', '');
        END IF;
    END IF;
    IF NEW.status = 'queued' AND NEW.job_kind IN ('agent_run.lifecycle','worker.noop') THEN
        IF TG_OP = 'INSERT' THEN
            PERFORM pg_notify('runtime_job_ready_v1', '');
        ELSIF OLD.status IS DISTINCT FROM 'queued' OR OLD.run_at_ms IS DISTINCT FROM NEW.run_at_ms OR OLD.job_kind IS DISTINCT FROM NEW.job_kind THEN
            PERFORM pg_notify('runtime_job_ready_v1', '');
        END IF;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER runtime_jobs_ready_notify_v1 AFTER INSERT OR UPDATE OF status,run_at_ms,job_kind ON runtime.runtime_jobs FOR EACH ROW EXECUTE FUNCTION runtime.notify_runtime_job_ready_v1();
CREATE INDEX idx_checkpoints_session_updated ON runtime.checkpoints(session_id, updated_at_ms DESC, checkpoint_id DESC);
CREATE INDEX idx_session_runtime_snapshots_updated ON runtime.session_runtime_snapshots(updated_at_ms DESC, session_id ASC);
CREATE INDEX idx_runtime_events_session_at ON runtime.runtime_events(session_id, at_ms ASC, event_id ASC);
CREATE INDEX idx_runtime_jobs_status_run_at ON runtime.runtime_jobs(status, run_at_ms ASC, job_id ASC);
CREATE INDEX idx_runtime_jobs_session_branch_run_at ON runtime.runtime_jobs(session_id, branch_id, run_at_ms ASC, job_id ASC);
CREATE INDEX idx_runtime_jobs_lease_expiry ON runtime.runtime_jobs(lease_expires_at_ms ASC, job_id ASC);
CREATE INDEX idx_runtime_job_outbox_pending ON runtime.runtime_job_outbox(published_at_ms ASC,job_id ASC,event_type ASC);
CREATE INDEX idx_resource_claims_owner ON runtime.resource_claims(owner, resource_kind, updated_at_ms DESC);
CREATE INDEX idx_resource_claims_expiry ON runtime.resource_claims(expires_at_ms ASC, resource_kind, resource_key);
CREATE INDEX idx_dead_letters_status_failed_at ON runtime.dead_letters(status, last_failed_at_ms DESC, dead_letter_id DESC);
CREATE INDEX idx_dead_letters_session_job_kind ON runtime.dead_letters(session_id, job_kind, status, last_failed_at_ms DESC, dead_letter_id DESC);
CREATE INDEX idx_external_context_objects_provider_updated ON runtime.external_context_objects(source_provider_id, source_tool_name, updated_at_ms DESC, object_id ASC);
CREATE INDEX idx_external_context_objects_kind_updated ON runtime.external_context_objects(object_kind, updated_at_ms DESC, object_id ASC);
CREATE INDEX idx_external_context_links_session_linked ON runtime.external_context_links(session_id, linked_at_ms DESC, object_id ASC);
CREATE INDEX idx_external_context_links_object ON runtime.external_context_links(object_id, linked_at_ms DESC, session_id ASC);
"#;

const TRANSCRIPT_DDL: &str = r#"
CREATE TABLE runtime.transcript_projection_heads(session_id text NOT NULL,projection_version text NOT NULL,projection_generation text NOT NULL,source_high_water bigint NOT NULL CHECK(source_high_water>=0),invalidation_reason text,PRIMARY KEY(session_id,projection_version,projection_generation));
CREATE TABLE runtime.transcript_projection_current_generations(session_id text NOT NULL,projection_version text NOT NULL,projection_generation text NOT NULL,source_high_water bigint NOT NULL CHECK(source_high_water>=0),PRIMARY KEY(session_id,projection_version),FOREIGN KEY(session_id,projection_version,projection_generation) REFERENCES runtime.transcript_projection_heads(session_id,projection_version,projection_generation) ON DELETE CASCADE);
CREATE TABLE runtime.transcript_block_identities(session_id text NOT NULL,projection_version text NOT NULL,projection_generation text NOT NULL,block_id text NOT NULL,order_source_sequence bigint NOT NULL CHECK(order_source_sequence>0),order_ordinal bigint NOT NULL CHECK(order_ordinal>=0),PRIMARY KEY(session_id,projection_version,projection_generation,block_id),UNIQUE(session_id,projection_version,projection_generation,order_source_sequence,order_ordinal),FOREIGN KEY(session_id,projection_version,projection_generation) REFERENCES runtime.transcript_projection_heads(session_id,projection_version,projection_generation) ON DELETE CASCADE);
CREATE TABLE runtime.transcript_block_versions(session_id text NOT NULL,projection_version text NOT NULL,projection_generation text NOT NULL,block_id text NOT NULL,applied_source_sequence bigint NOT NULL CHECK(applied_source_sequence>0),block_revision bigint NOT NULL CHECK(block_revision>0),block_json text NOT NULL,PRIMARY KEY(session_id,projection_version,projection_generation,block_id,applied_source_sequence),FOREIGN KEY(session_id,projection_version,projection_generation,block_id) REFERENCES runtime.transcript_block_identities(session_id,projection_version,projection_generation,block_id) ON DELETE CASCADE);
CREATE TABLE runtime.transcript_resume_cursors(session_id text NOT NULL,projection_version text NOT NULL,projection_generation text NOT NULL,stream_id text NOT NULL,source_high_water bigint NOT NULL CHECK(source_high_water>0),cursor text NOT NULL,PRIMARY KEY(session_id,projection_version,projection_generation,stream_id,source_high_water),FOREIGN KEY(session_id,projection_version,projection_generation) REFERENCES runtime.transcript_projection_heads(session_id,projection_version,projection_generation) ON DELETE CASCADE);
CREATE TABLE runtime.transcript_projection_current_recoveries(session_id text NOT NULL,projection_version text NOT NULL,projection_generation text NOT NULL,source_high_water bigint NOT NULL CHECK(source_high_water>0),checkpoint_json text NOT NULL,frontier_json text NOT NULL,PRIMARY KEY(session_id,projection_version,projection_generation),FOREIGN KEY(session_id,projection_version,projection_generation) REFERENCES runtime.transcript_projection_heads(session_id,projection_version,projection_generation) ON DELETE CASCADE);
CREATE TABLE runtime.transcript_projection_commits(commit_id text PRIMARY KEY,session_id text NOT NULL,projection_version text NOT NULL,projection_generation text NOT NULL,expected_source_high_water bigint NOT NULL CHECK(expected_source_high_water>=0),source_high_water bigint NOT NULL CHECK(source_high_water>expected_source_high_water),commit_json text NOT NULL,UNIQUE(session_id,projection_version,projection_generation,source_high_water),FOREIGN KEY(session_id,projection_version,projection_generation) REFERENCES runtime.transcript_projection_heads(session_id,projection_version,projection_generation) ON DELETE CASCADE);
CREATE INDEX idx_transcript_block_identities_page ON runtime.transcript_block_identities(session_id,projection_version,projection_generation,order_source_sequence DESC,order_ordinal DESC);
CREATE INDEX idx_transcript_block_versions_latest ON runtime.transcript_block_versions(session_id,projection_version,projection_generation,block_id,applied_source_sequence DESC);
CREATE INDEX idx_transcript_resume_cursors_latest ON runtime.transcript_resume_cursors(session_id,projection_version,projection_generation,stream_id,source_high_water DESC);
"#;

#[cfg(test)]
mod tests {
    use super::{
        INDEX_SHAPES, RUNTIME_DDL, RUNTIME_INDEXES, RUNTIME_TABLES, STORE_SCHEMA_VERSION,
        TRANSCRIPT_DDL,
    };
    use std::collections::BTreeSet;

    #[test]
    fn runtime_index_ddl_and_validation_contracts_match() {
        let declared: Vec<_> = [RUNTIME_DDL, TRANSCRIPT_DDL]
            .into_iter()
            .flat_map(str::lines)
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("CREATE INDEX ")
                    .or_else(|| line.trim().strip_prefix("CREATE UNIQUE INDEX "))
            })
            .map(|line| line.split_once(" ON ").expect("index DDL has table"))
            .collect();
        let names: BTreeSet<_> = declared.iter().map(|(name, _)| *name).collect();
        assert_eq!(names.len(), declared.len(), "duplicate DDL index");
        assert_eq!(names, RUNTIME_INDEXES.iter().copied().collect());
        assert_eq!(names, INDEX_SHAPES.iter().map(|(name, _)| *name).collect());
        assert_eq!(names.len(), RUNTIME_INDEXES.len());
        assert_eq!(names.len(), INDEX_SHAPES.len());
        let normalize = |text: &str| {
            text.replace(" ASC", "")
                .split_whitespace()
                .collect::<String>()
        };
        for (name, definition) in declared {
            let columns =
                definition[definition.find('(').expect("index columns")..].trim_end_matches(';');
            let (_, expected) = INDEX_SHAPES
                .iter()
                .find(|(candidate, _)| *candidate == name)
                .unwrap();
            assert_eq!(
                normalize(columns),
                normalize(expected),
                "index shape drift: {name}"
            );
        }
    }

    #[test]
    fn transcript_read_model_and_wait_handoff_have_explicit_store_schema() {
        assert_eq!(STORE_SCHEMA_VERSION, 24);
        for table in [
            "transcript_projection_heads",
            "transcript_projection_current_generations",
            "transcript_block_identities",
            "transcript_block_versions",
            "transcript_resume_cursors",
            "transcript_projection_current_recoveries",
            "transcript_projection_commits",
        ] {
            assert!(
                RUNTIME_TABLES.contains(&table),
                "missing transcript table {table}"
            );
        }
        for index in [
            "idx_transcript_block_identities_page",
            "idx_transcript_block_versions_latest",
            "idx_transcript_resume_cursors_latest",
        ] {
            assert!(
                RUNTIME_INDEXES.contains(&index),
                "missing transcript index {index}"
            );
        }
    }
}
