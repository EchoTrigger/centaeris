use std::collections::BTreeSet;

use postgres::{Client, GenericClient};

const STORE_SCHEMA_VERSION: i64 = 6;
const LEASE_RECLAIM_COLUMNS: &str =
    ",lease_reclaim_count:bigint:NO,lease_reclaim_not_before_ms:bigint:YES";
const PREVIOUS_OBSERVATION_KINDS: &str = "'system_prompt'::text, 'message'::text, 'input_image'::text, 'tool_catalog'::text, 'compaction_prompt'::text, 'input_uptake'::text";
const CURRENT_OBSERVATION_KINDS: &str = "'system_prompt'::text, 'message'::text, 'input_image'::text, 'tool_catalog'::text, 'compaction_prompt'::text, 'input_uptake'::text, 'required_completion_delivery'::text";

pub(super) const RUNTIME_TABLES: &[&str] = &[
    "execution_job_tenants",
    "checkpoints",
    "dead_letters",
    "external_context_links",
    "external_context_objects",
    "model_observation_contents",
    "model_observation_manifests",
    "resource_claims",
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
    validate_schema_with_reclaim_budget(client, kinds, true)
}

fn validate_schema_with_reclaim_budget(
    client: &mut impl GenericClient,
    kinds: &str,
    reclaim_budget: bool,
) -> Result<(), String> {
    let tables = object_names(client, "BASE TABLE")?;
    let expected_tables = RUNTIME_TABLES.iter().copied().map(str::to_string).collect();
    if tables != expected_tables {
        return Err(format!("Postgres runtime table set mismatch: {tables:?}"));
    }
    for (table, expected) in TABLE_SHAPES {
        let expected = if *table == "runtime_jobs" && !reclaim_budget {
            expected
                .strip_suffix(LEASE_RECLAIM_COLUMNS)
                .ok_or("previous runtime job shape missing")?
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
    if functions != BTreeSet::from(["notify_runtime_job_ready_v1".to_string()]) {
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
    if triggers != BTreeSet::from(["runtime_jobs_ready_notify_v1".to_string()]) {
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
        assert_eq!(STORE_SCHEMA_VERSION, 6);
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
