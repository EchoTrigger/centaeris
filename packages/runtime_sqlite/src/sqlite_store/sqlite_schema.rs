use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::STORE_SCHEMA_VERSION;

const EXTERNAL_CONTEXT_OBJECTS_SQL: &str = "
CREATE TABLE external_context_objects (
    object_id TEXT PRIMARY KEY,
    schema_version TEXT NOT NULL,
    object_kind TEXT NOT NULL,
    source_provider_id TEXT NOT NULL,
    source_tool_name TEXT NOT NULL,
    title TEXT NOT NULL,
    content BLOB NOT NULL,
    content_codec TEXT NOT NULL CHECK (content_codec IN ('identity_v1', 'zstd_v1')),
    content_uncompressed_bytes INTEGER NOT NULL,
    content_sha256 TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    inserted_at_ms INTEGER NOT NULL
)";
const EXTERNAL_CONTEXT_PROVIDER_INDEX_SQL: &str = "CREATE INDEX idx_external_context_objects_provider_updated ON external_context_objects(source_provider_id, source_tool_name, updated_at_ms DESC, object_id ASC)";
const EXTERNAL_CONTEXT_KIND_INDEX_SQL: &str = "CREATE INDEX idx_external_context_objects_kind_updated ON external_context_objects(object_kind, updated_at_ms DESC, object_id ASC)";

pub(super) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    let empty_db = runtime_schema_user_table_count(conn)? == 0;
    if empty_db {
        create_schema(conn)?;
        validate_schema_shape(conn)?;
        return Ok(());
    }

    let current_version = validate_schema_history(conn)?;
    apply_forward_migrations(conn, current_version)?;
    validate_schema_history(conn)?;
    validate_schema_shape(conn)
}

fn validate_schema_history(conn: &Connection) -> Result<i64, String> {
    if object_sql(conn, "table", "schema_migrations")?.is_none() {
        return Err("runtime sqlite schema_migrations table is missing".to_string());
    }
    let versions = schema_versions(conn)?;
    let current_version = versions
        .last()
        .copied()
        .ok_or_else(|| "runtime sqlite schema_migrations history must not be empty".to_string())?;
    if current_version > STORE_SCHEMA_VERSION {
        return Err(format!(
            "runtime sqlite refuses schema downgrade: store version {current_version}, runtime version {STORE_SCHEMA_VERSION}"
        ));
    }
    let expected = (1..=current_version).collect::<Vec<_>>();
    if versions != expected {
        return Err(format!(
            "runtime sqlite schema migration history is not contiguous: expected {expected:?}, got {versions:?}"
        ));
    }
    Ok(current_version)
}

struct ForwardMigration {
    from_version: i64,
    to_version: i64,
    apply: fn(&Connection) -> Result<(), String>,
}

// v1 is the initial schema. Add an exact n -> n+1 entry only when that
// forward migration exists; missing paths fail without touching the store.
const FORWARD_MIGRATIONS: &[ForwardMigration] = &[
    ForwardMigration {
        from_version: 1,
        to_version: 2,
        apply: migrate_v1_to_v2,
    },
    ForwardMigration {
        from_version: 2,
        to_version: 3,
        apply: migrate_v2_to_v3,
    },
];

fn migrate_v1_to_v2(conn: &Connection) -> Result<(), String> {
    let mut ddl = String::new();
    for table in TRANSCRIPT_TABLES.iter().filter(|table| {
        !matches!(
            table.name,
            "transcript_projection_current_generations"
                | "transcript_projection_current_recoveries"
        )
    }) {
        ddl.push_str(table.sql);
        ddl.push_str(";\n");
    }
    for index in TRANSCRIPT_INDEXES {
        ddl.push_str(index.sql);
        ddl.push_str(";\n");
    }
    conn.execute_batch(ddl.as_str())
        .map_err(|error| format!("create transcript read model schema failed: {error}"))
}

fn migrate_v2_to_v3(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(TRANSCRIPT_CURRENT_GENERATIONS_SQL)
        .map_err(|error| format!("create transcript generation owner schema failed: {error}"))?;
    conn.execute_batch(TRANSCRIPT_CURRENT_RECOVERIES_SQL)
        .map_err(|error| format!("create transcript current recovery schema failed: {error}"))
}

fn apply_forward_migrations(conn: &Connection, current_version: i64) -> Result<(), String> {
    apply_forward_migrations_to(
        conn,
        current_version,
        STORE_SCHEMA_VERSION,
        FORWARD_MIGRATIONS,
    )
}

fn apply_forward_migrations_to(
    conn: &Connection,
    current_version: i64,
    target_version: i64,
    available_migrations: &[ForwardMigration],
) -> Result<(), String> {
    if current_version == target_version {
        return Ok(());
    }
    let migrations = (current_version..target_version)
        .map(|from_version| {
            available_migrations
                .iter()
                .find(|migration| {
                    migration.from_version == from_version
                        && migration.to_version == from_version + 1
                })
                .ok_or_else(|| {
                    format!(
                        "runtime sqlite has no forward migration from version {from_version} to {}",
                        from_version + 1
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    create_migration_backup(conn, current_version, target_version)?;
    let transaction = conn
        .unchecked_transaction()
        .map_err(|error| format!("begin runtime sqlite migration transaction failed: {error}"))?;
    for migration in migrations {
        (migration.apply)(&transaction)?;
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at_ms) VALUES(?1, CAST(strftime('%s','now') AS INTEGER) * 1000)",
            params![migration.to_version],
        )
        .map_err(|error| {
            format!(
                "record runtime sqlite schema migration {} failed: {error}",
                migration.to_version
            )
        })?;
    }
    transaction
        .commit()
        .map_err(|error| format!("commit runtime sqlite migrations failed: {error}"))
}

fn create_migration_backup(
    conn: &Connection,
    from_version: i64,
    to_version: i64,
) -> Result<Option<PathBuf>, String> {
    let database_path = conn
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name = 'main'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| format!("resolve runtime sqlite path before migration failed: {error}"))?;
    if database_path.is_empty() {
        return Ok(None);
    }
    let database_path = PathBuf::from(database_path);
    let file_name = database_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "runtime sqlite path has no valid file name".to_string())?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("resolve migration backup timestamp failed: {error}"))?
        .as_nanos();
    let backup_path = database_path.with_file_name(format!(
        "{file_name}.pre-v{from_version}-to-v{to_version}-{timestamp}.backup"
    ));
    conn.execute(
        "VACUUM INTO ?1",
        params![backup_path.to_string_lossy().to_string()],
    )
    .map_err(|error| {
        format!(
            "create runtime sqlite migration backup {} failed: {error}",
            backup_path.display()
        )
    })?;
    Ok(Some(backup_path))
}

fn create_schema(conn: &Connection) -> Result<(), String> {
    let mut ddl = String::new();
    for table in REQUIRED_TABLES.iter().chain(TRANSCRIPT_TABLES) {
        ddl.push_str(table.sql);
        ddl.push_str(";\n");
    }
    for index in REQUIRED_INDEXES.iter().chain(TRANSCRIPT_INDEXES) {
        ddl.push_str(index.sql);
        ddl.push_str(";\n");
    }
    let transaction = conn
        .unchecked_transaction()
        .map_err(|error| format!("begin runtime sqlite schema transaction failed: {error}"))?;
    transaction
        .execute_batch(ddl.as_str())
        .map_err(|err| format!("create runtime sqlite schema failed: {err}"))?;
    for version in 1..=STORE_SCHEMA_VERSION {
        transaction
            .execute(
                "
            INSERT INTO schema_migrations(version, applied_at_ms)
            VALUES(?1, CAST(strftime('%s','now') AS INTEGER) * 1000)
            ",
                params![version],
            )
            .map_err(|err| format!("record schema version failed: {err}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("commit runtime sqlite schema failed: {error}"))
}

struct RequiredObject {
    name: &'static str,
    sql: &'static str,
}

const REQUIRED_TABLES: &[RequiredObject] = &[
    RequiredObject {
        name: "schema_migrations",
        sql: "
        CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at_ms INTEGER NOT NULL
        )
        ",
    },
    RequiredObject {
        name: "checkpoints",
        sql: "
        CREATE TABLE checkpoints (
            checkpoint_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL CHECK (kind IN ('wait', 'recovery')),
            session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            status TEXT NOT NULL,
            done_reason TEXT,
            updated_at_ms INTEGER NOT NULL,
            payload_json TEXT NOT NULL
        )
        ",
    },
    RequiredObject {
        name: "session_runtime_snapshots",
        sql: "
        CREATE TABLE session_runtime_snapshots (
            session_id TEXT PRIMARY KEY,
            snapshot_json TEXT NOT NULL,
            updated_at_ms INTEGER NOT NULL
        )
        ",
    },
    RequiredObject {
        name: "runtime_job_waiters",
        sql: "CREATE TABLE runtime_job_waiters (
            checkpoint_id TEXT NOT NULL REFERENCES checkpoints(checkpoint_id) ON DELETE CASCADE,
            tool_call_id TEXT NOT NULL,
            source_job_id TEXT NOT NULL,
            source_job_kind TEXT NOT NULL,
            session_id TEXT NOT NULL,
            agent_run_id TEXT NOT NULL,
            PRIMARY KEY(checkpoint_id,tool_call_id)
        )",
    },
    RequiredObject {
        name: "runtime_events",
        sql: "
        CREATE TABLE runtime_events (
            event_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            task_id TEXT,
            event_type TEXT NOT NULL,
            at_ms INTEGER NOT NULL,
            visibility TEXT NOT NULL,
            payload_json TEXT NOT NULL
        )
        ",
    },
    RequiredObject {
        name: "runtime_jobs",
        sql: "
        CREATE TABLE runtime_jobs (
            job_id TEXT PRIMARY KEY,
            job_kind TEXT NOT NULL,
            status TEXT NOT NULL,
            run_at_ms INTEGER NOT NULL,
            lease_owner TEXT,
            lease_expires_at_ms INTEGER,
            retry_count INTEGER NOT NULL DEFAULT 0,
            max_retries INTEGER NOT NULL DEFAULT 0,
            backoff_policy_json TEXT NOT NULL,
            idempotency_key TEXT NOT NULL,
            session_id TEXT,
            branch_id TEXT,
            checkpoint_id TEXT,
            payload_ref TEXT,
            output_refs_json TEXT NOT NULL DEFAULT '[]',
            last_error TEXT,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            heartbeat_at_ms INTEGER,
            UNIQUE(job_kind, idempotency_key)
        )
        ",
    },
    RequiredObject {
        name: "runtime_job_outbox",
        sql: "
        CREATE TABLE runtime_job_outbox (
            job_id TEXT NOT NULL REFERENCES runtime_jobs(job_id) ON DELETE CASCADE,
            event_type TEXT NOT NULL,
            published_at_ms INTEGER,
            generation INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (job_id, event_type)
        )
        ",
    },
    RequiredObject {
        name: "runtime_turn_supplement_queues",
        sql: "
        CREATE TABLE runtime_turn_supplement_queues (
            agent_run_id TEXT PRIMARY KEY,
            lifecycle_job_id TEXT NOT NULL UNIQUE REFERENCES runtime_jobs(job_id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            authorization_digest TEXT NOT NULL,
            revision INTEGER NOT NULL,
            next_sequence INTEGER NOT NULL,
            accepting INTEGER NOT NULL CHECK (accepting IN (0, 1)),
            entries_json TEXT NOT NULL,
            dedupe_json TEXT NOT NULL,
            closed_reason TEXT,
            updated_at_ms INTEGER NOT NULL
        )
        ",
    },
    RequiredObject {
        name: "resource_claims",
        sql: "
        CREATE TABLE resource_claims (
            resource_kind TEXT NOT NULL,
            resource_key TEXT NOT NULL,
            owner TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            session_id TEXT,
            branch_id TEXT,
            expires_at_ms INTEGER NOT NULL,
            metadata_json TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY (resource_kind, resource_key)
        )
        ",
    },
    RequiredObject {
        name: "dead_letters",
        sql: "
        CREATE TABLE dead_letters (
            dead_letter_id TEXT PRIMARY KEY,
            original_job_id TEXT NOT NULL,
            job_kind TEXT NOT NULL,
            status TEXT NOT NULL,
            session_id TEXT,
            branch_id TEXT,
            checkpoint_id TEXT,
            payload_ref TEXT,
            idempotency_key TEXT NOT NULL,
            failure_reason TEXT NOT NULL,
            last_error TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            first_failed_at_ms INTEGER NOT NULL,
            last_failed_at_ms INTEGER NOT NULL,
            replay_policy_json TEXT NOT NULL,
            replayed_job_id TEXT,
            dismissed_by TEXT,
            dismissed_reason TEXT,
            updated_at_ms INTEGER NOT NULL,
            UNIQUE(original_job_id)
        )
        ",
    },
    RequiredObject {
        name: "external_context_objects",
        sql: EXTERNAL_CONTEXT_OBJECTS_SQL,
    },
    RequiredObject {
        name: "external_context_links",
        sql: "
        CREATE TABLE external_context_links (
            session_id TEXT NOT NULL,
            object_id TEXT NOT NULL,
            turn_id TEXT NOT NULL DEFAULT '',
            tool_call_id TEXT NOT NULL DEFAULT '',
            source_provider_id TEXT NOT NULL,
            source_tool_name TEXT NOT NULL,
            linked_at_ms INTEGER NOT NULL,
            PRIMARY KEY (session_id, object_id, turn_id, tool_call_id)
        )
        ",
    },
];

const TRANSCRIPT_CURRENT_GENERATIONS_SQL: &str = "
CREATE TABLE transcript_projection_current_generations (
    session_id TEXT NOT NULL,
    projection_version TEXT NOT NULL,
    projection_generation TEXT NOT NULL,
    source_high_water INTEGER NOT NULL,
    PRIMARY KEY (session_id, projection_version),
    FOREIGN KEY (session_id, projection_version, projection_generation)
        REFERENCES transcript_projection_heads(session_id, projection_version, projection_generation)
        ON DELETE CASCADE
)";

const TRANSCRIPT_CURRENT_RECOVERIES_SQL: &str = "
CREATE TABLE transcript_projection_current_recoveries (
    session_id TEXT NOT NULL,
    projection_version TEXT NOT NULL,
    projection_generation TEXT NOT NULL,
    source_high_water INTEGER NOT NULL,
    checkpoint_json TEXT NOT NULL,
    frontier_json TEXT NOT NULL,
    PRIMARY KEY (session_id, projection_version, projection_generation),
    FOREIGN KEY (session_id, projection_version, projection_generation)
        REFERENCES transcript_projection_heads(session_id, projection_version, projection_generation)
        ON DELETE CASCADE
)";

const TRANSCRIPT_TABLES: &[RequiredObject] = &[
    RequiredObject {
        name: "transcript_projection_heads",
        sql: "
        CREATE TABLE transcript_projection_heads (
            session_id TEXT NOT NULL,
            projection_version TEXT NOT NULL,
            projection_generation TEXT NOT NULL,
            source_high_water INTEGER NOT NULL,
            invalidation_reason TEXT,
            PRIMARY KEY (session_id, projection_version, projection_generation)
        )
        ",
    },
    RequiredObject {
        name: "transcript_projection_current_generations",
        sql: TRANSCRIPT_CURRENT_GENERATIONS_SQL,
    },
    RequiredObject {
        name: "transcript_projection_current_recoveries",
        sql: TRANSCRIPT_CURRENT_RECOVERIES_SQL,
    },
    RequiredObject {
        name: "transcript_block_identities",
        sql: "
        CREATE TABLE transcript_block_identities (
            session_id TEXT NOT NULL,
            projection_version TEXT NOT NULL,
            projection_generation TEXT NOT NULL,
            block_id TEXT NOT NULL,
            order_source_sequence INTEGER NOT NULL,
            order_ordinal INTEGER NOT NULL,
            PRIMARY KEY (session_id, projection_version, projection_generation, block_id),
            UNIQUE (session_id, projection_version, projection_generation, order_source_sequence, order_ordinal),
            FOREIGN KEY (session_id, projection_version, projection_generation)
                REFERENCES transcript_projection_heads(session_id, projection_version, projection_generation)
                ON DELETE CASCADE
        )
        ",
    },
    RequiredObject {
        name: "transcript_block_versions",
        sql: "
        CREATE TABLE transcript_block_versions (
            session_id TEXT NOT NULL,
            projection_version TEXT NOT NULL,
            projection_generation TEXT NOT NULL,
            block_id TEXT NOT NULL,
            applied_source_sequence INTEGER NOT NULL,
            block_revision INTEGER NOT NULL,
            block_json TEXT NOT NULL,
            PRIMARY KEY (session_id, projection_version, projection_generation, block_id, applied_source_sequence),
            UNIQUE (session_id, projection_version, projection_generation, block_id, block_revision),
            FOREIGN KEY (session_id, projection_version, projection_generation, block_id)
                REFERENCES transcript_block_identities(session_id, projection_version, projection_generation, block_id)
                ON DELETE CASCADE
        )
        ",
    },
    RequiredObject {
        name: "transcript_resume_cursors",
        sql: "
        CREATE TABLE transcript_resume_cursors (
            session_id TEXT NOT NULL,
            projection_version TEXT NOT NULL,
            projection_generation TEXT NOT NULL,
            stream_id TEXT NOT NULL,
            source_high_water INTEGER NOT NULL,
            cursor TEXT NOT NULL,
            PRIMARY KEY (session_id, projection_version, projection_generation, stream_id, source_high_water),
            FOREIGN KEY (session_id, projection_version, projection_generation)
                REFERENCES transcript_projection_heads(session_id, projection_version, projection_generation)
                ON DELETE CASCADE
        )
        ",
    },
    RequiredObject {
        name: "transcript_projection_frontiers",
        sql: "
        CREATE TABLE transcript_projection_frontiers (
            frontier_ref TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            projection_version TEXT NOT NULL,
            projection_generation TEXT NOT NULL,
            source_high_water INTEGER NOT NULL,
            frontier_json TEXT NOT NULL,
            UNIQUE (session_id, projection_version, projection_generation, source_high_water),
            FOREIGN KEY (session_id, projection_version, projection_generation)
                REFERENCES transcript_projection_heads(session_id, projection_version, projection_generation)
                ON DELETE CASCADE
        )
        ",
    },
    RequiredObject {
        name: "transcript_projection_checkpoints",
        sql: "
        CREATE TABLE transcript_projection_checkpoints (
            session_id TEXT NOT NULL,
            projection_version TEXT NOT NULL,
            projection_generation TEXT NOT NULL,
            source_high_water INTEGER NOT NULL,
            frontier_ref TEXT NOT NULL REFERENCES transcript_projection_frontiers(frontier_ref),
            checkpoint_json TEXT NOT NULL,
            PRIMARY KEY (session_id, projection_version, projection_generation, source_high_water),
            FOREIGN KEY (session_id, projection_version, projection_generation)
                REFERENCES transcript_projection_heads(session_id, projection_version, projection_generation)
                ON DELETE CASCADE
        )
        ",
    },
    RequiredObject {
        name: "transcript_projection_commits",
        sql: "
        CREATE TABLE transcript_projection_commits (
            commit_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            projection_version TEXT NOT NULL,
            projection_generation TEXT NOT NULL,
            expected_source_high_water INTEGER NOT NULL,
            source_high_water INTEGER NOT NULL,
            commit_json TEXT NOT NULL,
            FOREIGN KEY (session_id, projection_version, projection_generation)
                REFERENCES transcript_projection_heads(session_id, projection_version, projection_generation)
                ON DELETE CASCADE
        )
        ",
    },
];

const REQUIRED_INDEXES: &[RequiredObject] = &[
    RequiredObject {
        name: "idx_runtime_job_waiters_source",
        sql: "CREATE INDEX idx_runtime_job_waiters_source ON runtime_job_waiters(source_job_id,checkpoint_id,tool_call_id)",
    },
    RequiredObject {
        name: "idx_checkpoints_session_updated",
        sql: "CREATE INDEX idx_checkpoints_session_updated ON checkpoints(session_id, updated_at_ms DESC, checkpoint_id DESC)",
    },
    RequiredObject {
        name: "idx_session_runtime_snapshots_updated",
        sql: "CREATE INDEX idx_session_runtime_snapshots_updated ON session_runtime_snapshots(updated_at_ms DESC, session_id ASC)",
    },
    RequiredObject {
        name: "idx_runtime_events_session_at",
        sql: "CREATE INDEX idx_runtime_events_session_at ON runtime_events(session_id, at_ms ASC, event_id ASC)",
    },
    RequiredObject {
        name: "idx_runtime_jobs_status_run_at",
        sql: "CREATE INDEX idx_runtime_jobs_status_run_at ON runtime_jobs(status, run_at_ms ASC, job_id ASC)",
    },
    RequiredObject {
        name: "idx_runtime_jobs_session_branch_run_at",
        sql: "CREATE INDEX idx_runtime_jobs_session_branch_run_at ON runtime_jobs(session_id, branch_id, run_at_ms ASC, job_id ASC)",
    },
    RequiredObject {
        name: "idx_runtime_jobs_lease_expiry",
        sql: "CREATE INDEX idx_runtime_jobs_lease_expiry ON runtime_jobs(lease_expires_at_ms ASC, job_id ASC)",
    },
    RequiredObject {
        name: "idx_runtime_job_outbox_pending",
        sql: "CREATE INDEX idx_runtime_job_outbox_pending ON runtime_job_outbox(published_at_ms ASC, job_id ASC, event_type ASC)",
    },
    RequiredObject {
        name: "idx_resource_claims_owner",
        sql: "CREATE INDEX idx_resource_claims_owner ON resource_claims(owner, resource_kind, updated_at_ms DESC)",
    },
    RequiredObject {
        name: "idx_resource_claims_expiry",
        sql: "CREATE INDEX idx_resource_claims_expiry ON resource_claims(expires_at_ms ASC, resource_kind, resource_key)",
    },
    RequiredObject {
        name: "idx_dead_letters_status_failed_at",
        sql: "CREATE INDEX idx_dead_letters_status_failed_at ON dead_letters(status, last_failed_at_ms DESC, dead_letter_id DESC)",
    },
    RequiredObject {
        name: "idx_dead_letters_session_job_kind",
        sql: "CREATE INDEX idx_dead_letters_session_job_kind ON dead_letters(session_id, job_kind, status, last_failed_at_ms DESC, dead_letter_id DESC)",
    },
    RequiredObject {
        name: "idx_external_context_objects_provider_updated",
        sql: EXTERNAL_CONTEXT_PROVIDER_INDEX_SQL,
    },
    RequiredObject {
        name: "idx_external_context_objects_kind_updated",
        sql: EXTERNAL_CONTEXT_KIND_INDEX_SQL,
    },
    RequiredObject {
        name: "idx_external_context_links_session_linked",
        sql: "CREATE INDEX idx_external_context_links_session_linked ON external_context_links(session_id, linked_at_ms DESC, object_id ASC)",
    },
    RequiredObject {
        name: "idx_external_context_links_object",
        sql: "CREATE INDEX idx_external_context_links_object ON external_context_links(object_id, linked_at_ms DESC, session_id ASC)",
    },
];

const TRANSCRIPT_INDEXES: &[RequiredObject] = &[
    RequiredObject {
        name: "idx_transcript_block_identities_page",
        sql: "CREATE INDEX idx_transcript_block_identities_page ON transcript_block_identities(session_id, projection_version, projection_generation, order_source_sequence DESC, order_ordinal DESC, block_id ASC)",
    },
    RequiredObject {
        name: "idx_transcript_block_versions_waterline",
        sql: "CREATE INDEX idx_transcript_block_versions_waterline ON transcript_block_versions(session_id, projection_version, projection_generation, block_id, applied_source_sequence DESC)",
    },
    RequiredObject {
        name: "idx_transcript_resume_cursors_waterline",
        sql: "CREATE INDEX idx_transcript_resume_cursors_waterline ON transcript_resume_cursors(session_id, projection_version, projection_generation, stream_id, source_high_water DESC)",
    },
    RequiredObject {
        name: "idx_transcript_projection_checkpoints_waterline",
        sql: "CREATE INDEX idx_transcript_projection_checkpoints_waterline ON transcript_projection_checkpoints(session_id, projection_version, projection_generation, source_high_water DESC)",
    },
    RequiredObject {
        name: "idx_transcript_projection_commits_session",
        sql: "CREATE INDEX idx_transcript_projection_commits_session ON transcript_projection_commits(session_id, projection_version, projection_generation, source_high_water ASC, commit_id ASC)",
    },
];

pub(super) fn runtime_schema_user_table_count(conn: &Connection) -> Result<i64, String> {
    conn.query_row(
        "
        SELECT COUNT(*)
        FROM sqlite_master
        WHERE type = 'table'
          AND name NOT LIKE 'sqlite_%'
        ",
        [],
        |row| row.get(0),
    )
    .map_err(|err| format!("count runtime schema tables failed: {err}"))
}

pub(super) fn validate_schema_shape(conn: &Connection) -> Result<(), String> {
    for table_name in user_tables(conn)? {
        if REQUIRED_TABLES
            .iter()
            .chain(TRANSCRIPT_TABLES)
            .all(|required| required.name != table_name)
        {
            return Err(format!("runtime sqlite unknown table: {table_name}"));
        }
    }
    for index_name in user_indexes(conn)? {
        if REQUIRED_INDEXES
            .iter()
            .chain(TRANSCRIPT_INDEXES)
            .all(|required| required.name != index_name)
        {
            return Err(format!("runtime sqlite unknown index: {index_name}"));
        }
    }
    for table in REQUIRED_TABLES.iter().chain(TRANSCRIPT_TABLES) {
        validate_object_sql(conn, "table", table.name, table.sql)?;
    }
    for index in REQUIRED_INDEXES.iter().chain(TRANSCRIPT_INDEXES) {
        validate_object_sql(conn, "index", index.name, index.sql)?;
    }
    Ok(())
}

fn validate_object_sql(
    conn: &Connection,
    object_type: &str,
    name: &str,
    expected_sql: &str,
) -> Result<(), String> {
    let actual_sql = object_sql(conn, object_type, name)?
        .ok_or_else(|| format!("runtime sqlite required {object_type} missing: {name}"))?;
    let actual = normalize_sql(actual_sql.as_str());
    let expected = normalize_sql(expected_sql);
    if actual != expected {
        return Err(format!(
            "runtime sqlite {object_type} definition mismatch: {name}"
        ));
    }
    Ok(())
}

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn object_sql(conn: &Connection, object_type: &str, name: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "
        SELECT sql
        FROM sqlite_master
        WHERE type = ?1 AND name = ?2
        ",
        params![object_type, name],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(|value| value.flatten())
    .map_err(|err| format!("load sqlite object sql failed: type={object_type} name={name}: {err}"))
}

fn schema_versions(conn: &Connection) -> Result<Vec<i64>, String> {
    let mut stmt = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version ASC")
        .map_err(|err| format!("prepare schema version query failed: {err}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(|err| format!("query schema versions failed: {err}"))?;
    let mut versions = Vec::new();
    for row in rows {
        versions.push(row.map_err(|err| format!("decode schema version failed: {err}"))?);
    }
    Ok(versions)
}

fn user_tables(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "
            SELECT name
            FROM sqlite_master
            WHERE type = 'table'
              AND name NOT LIKE 'sqlite_%'
            ORDER BY name ASC
            ",
        )
        .map_err(|err| format!("prepare user table query failed: {err}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| format!("query user tables failed: {err}"))?;
    let mut tables = Vec::new();
    for row in rows {
        tables.push(row.map_err(|err| format!("decode user table failed: {err}"))?);
    }
    Ok(tables)
}

fn user_indexes(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(
            "
            SELECT name
            FROM sqlite_master
            WHERE type = 'index'
              AND sql IS NOT NULL
              AND name NOT LIKE 'sqlite_%'
            ORDER BY name ASC
            ",
        )
        .map_err(|err| format!("prepare user index query failed: {err}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|err| format!("query user indexes failed: {err}"))?;
    let mut indexes = Vec::new();
    for row in rows {
        indexes.push(row.map_err(|err| format!("decode user index failed: {err}"))?);
    }
    Ok(indexes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteRuntimeStore;
    use std::fs;

    fn failing_migration(conn: &Connection) -> Result<(), String> {
        conn.execute_batch("CREATE TABLE partial_migration(value INTEGER NOT NULL);")
            .map_err(|error| error.to_string())?;
        Err("injected migration failure".to_string())
    }

    #[test]
    fn forward_migration_backs_up_and_rolls_back_as_one_transaction() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-sqlite-migration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("root");
        let database_path = root.join("runtime.db");
        let conn = Connection::open(&database_path).expect("database");
        conn.execute_batch(
            "CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at_ms INTEGER NOT NULL);
             INSERT INTO schema_migrations(version, applied_at_ms) VALUES(1, 1);
             CREATE TABLE stable(value TEXT NOT NULL);
             INSERT INTO stable(value) VALUES('preserved');",
        )
        .expect("v1 fixture");

        let backup = create_migration_backup(&conn, 1, 2)
            .expect("backup")
            .expect("file database backup");
        let backup_conn = Connection::open(&backup).expect("open backup");
        assert_eq!(
            backup_conn
                .query_row("SELECT value FROM stable", [], |row| row
                    .get::<_, String>(0))
                .expect("backup value"),
            "preserved"
        );
        drop(backup_conn);

        let error = apply_forward_migrations_to(
            &conn,
            1,
            2,
            &[ForwardMigration {
                from_version: 1,
                to_version: 2,
                apply: failing_migration,
            }],
        )
        .expect_err("migration must fail");
        assert!(error.contains("injected migration failure"));
        assert!(object_sql(&conn, "table", "partial_migration")
            .expect("partial table query")
            .is_none());
        assert_eq!(schema_versions(&conn).expect("versions"), vec![1]);

        drop(conn);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn v1_store_migrates_to_the_persistent_transcript_read_model() {
        let root = std::env::temp_dir().join(format!(
            "centaeris-sqlite-transcript-migration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("root");
        let database_path = root.join("runtime.db");
        let conn = Connection::open(&database_path).expect("database");
        let mut ddl = String::new();
        for table in REQUIRED_TABLES {
            ddl.push_str(table.sql);
            ddl.push_str(";\n");
        }
        for index in REQUIRED_INDEXES {
            ddl.push_str(index.sql);
            ddl.push_str(";\n");
        }
        conn.execute_batch(ddl.as_str()).expect("v1 schema");
        conn.execute(
            "INSERT INTO schema_migrations(version, applied_at_ms) VALUES(1, 1)",
            [],
        )
        .expect("v1 history");
        drop(conn);

        drop(SqliteRuntimeStore::new(&database_path).expect("migrate v1 store"));
        let migrated = Connection::open(&database_path).expect("migrated database");
        assert!(
            object_sql(&migrated, "table", "transcript_projection_heads")
                .expect("transcript table lookup")
                .is_some()
        );
        assert!(
            object_sql(&migrated, "table", "transcript_projection_frontiers")
                .expect("transcript frontier table lookup")
                .is_some()
        );
        assert!(object_sql(
            &migrated,
            "table",
            "transcript_projection_current_generations"
        )
        .expect("transcript generation owner lookup")
        .is_some());
        assert_eq!(schema_versions(&migrated).expect("versions"), vec![1, 2, 3]);
        drop(migrated);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
