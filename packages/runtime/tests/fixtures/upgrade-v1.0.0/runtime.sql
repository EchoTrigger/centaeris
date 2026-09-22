BEGIN TRANSACTION;
CREATE TABLE checkpoints (
            checkpoint_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL CHECK (kind IN ('wait', 'recovery')),
            session_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            status TEXT NOT NULL,
            done_reason TEXT,
            updated_at_ms INTEGER NOT NULL,
            payload_json TEXT NOT NULL
        );
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
        );
CREATE TABLE external_context_links (
            session_id TEXT NOT NULL,
            object_id TEXT NOT NULL,
            turn_id TEXT NOT NULL DEFAULT '',
            tool_call_id TEXT NOT NULL DEFAULT '',
            source_provider_id TEXT NOT NULL,
            source_tool_name TEXT NOT NULL,
            linked_at_ms INTEGER NOT NULL,
            PRIMARY KEY (session_id, object_id, turn_id, tool_call_id)
        );
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
);
INSERT INTO "external_context_objects" VALUES('source-object-1','external_context.v1','text','fixture','read','notice.md',X'6E6F7469636520636F6E74656E7473','identity_v1',15,'sha256:df190da3df2cfa48cccd18888b6f8b902dda7d97c27f504fdefa731252f82878','{"fixture":"released-v1"}',1,1);
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
        );
CREATE TABLE runtime_events (
            event_id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            task_id TEXT,
            event_type TEXT NOT NULL,
            at_ms INTEGER NOT NULL,
            visibility TEXT NOT NULL,
            payload_json TEXT NOT NULL
        );
CREATE TABLE runtime_job_outbox (
            job_id TEXT NOT NULL REFERENCES runtime_jobs(job_id) ON DELETE CASCADE,
            event_type TEXT NOT NULL,
            published_at_ms INTEGER,
            generation INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (job_id, event_type)
        );
CREATE TABLE runtime_job_waiters (
            checkpoint_id TEXT NOT NULL REFERENCES checkpoints(checkpoint_id) ON DELETE CASCADE,
            tool_call_id TEXT NOT NULL,
            source_job_id TEXT NOT NULL,
            source_job_kind TEXT NOT NULL,
            session_id TEXT NOT NULL,
            agent_run_id TEXT NOT NULL,
            PRIMARY KEY(checkpoint_id,tool_call_id)
        );
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
        );
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
        );
CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at_ms INTEGER NOT NULL
        );
INSERT INTO "schema_migrations" VALUES(1,1789972872000);
CREATE TABLE session_runtime_snapshots (
            session_id TEXT PRIMARY KEY,
            snapshot_json TEXT NOT NULL,
            updated_at_ms INTEGER NOT NULL
        );
INSERT INTO "session_runtime_snapshots" VALUES('session-1','{"fixture":"released-v1"}',1);
CREATE INDEX idx_runtime_job_waiters_source ON runtime_job_waiters(source_job_id,checkpoint_id,tool_call_id);
CREATE INDEX idx_checkpoints_session_updated ON checkpoints(session_id, updated_at_ms DESC, checkpoint_id DESC);
CREATE INDEX idx_session_runtime_snapshots_updated ON session_runtime_snapshots(updated_at_ms DESC, session_id ASC);
CREATE INDEX idx_runtime_events_session_at ON runtime_events(session_id, at_ms ASC, event_id ASC);
CREATE INDEX idx_runtime_jobs_status_run_at ON runtime_jobs(status, run_at_ms ASC, job_id ASC);
CREATE INDEX idx_runtime_jobs_session_branch_run_at ON runtime_jobs(session_id, branch_id, run_at_ms ASC, job_id ASC);
CREATE INDEX idx_runtime_jobs_lease_expiry ON runtime_jobs(lease_expires_at_ms ASC, job_id ASC);
CREATE INDEX idx_runtime_job_outbox_pending ON runtime_job_outbox(published_at_ms ASC, job_id ASC, event_type ASC);
CREATE INDEX idx_resource_claims_owner ON resource_claims(owner, resource_kind, updated_at_ms DESC);
CREATE INDEX idx_resource_claims_expiry ON resource_claims(expires_at_ms ASC, resource_kind, resource_key);
CREATE INDEX idx_dead_letters_status_failed_at ON dead_letters(status, last_failed_at_ms DESC, dead_letter_id DESC);
CREATE INDEX idx_dead_letters_session_job_kind ON dead_letters(session_id, job_kind, status, last_failed_at_ms DESC, dead_letter_id DESC);
CREATE INDEX idx_external_context_objects_provider_updated ON external_context_objects(source_provider_id, source_tool_name, updated_at_ms DESC, object_id ASC);
CREATE INDEX idx_external_context_objects_kind_updated ON external_context_objects(object_kind, updated_at_ms DESC, object_id ASC);
CREATE INDEX idx_external_context_links_session_linked ON external_context_links(session_id, linked_at_ms DESC, object_id ASC);
CREATE INDEX idx_external_context_links_object ON external_context_links(object_id, linked_at_ms DESC, session_id ASC);
COMMIT;
