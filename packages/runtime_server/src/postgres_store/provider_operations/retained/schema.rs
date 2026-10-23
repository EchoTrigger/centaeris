//! Forward-only physical-history schema; the legacy DDL remains authoritative
//! for validating and testing released versions before the history anchor.

use postgres::GenericClient;

/// The caller owns the migration transaction, version and complete validation.
pub(crate) const DDL: &str = r#"
LOCK TABLE runtime.provider_environment_run_uses IN ACCESS EXCLUSIVE MODE;
DROP TRIGGER retained_provider_run_use_facts ON runtime.provider_environment_run_uses;
ALTER TABLE runtime.provider_environment_run_uses ADD COLUMN creation_attempt_id text;
UPDATE runtime.provider_environment_run_uses u SET creation_attempt_id=b.creation_attempt_id
 FROM runtime.provider_environment_bindings b JOIN runtime.provider_creation_operations p
 ON p.attempt_id=b.creation_attempt_id AND p.environment_id=b.environment_id
 WHERE u.environment_id=b.environment_id AND p.verified_sandbox_id IS NOT NULL AND p.verification_digest IS NOT NULL;
ALTER TABLE runtime.provider_environment_run_uses ALTER COLUMN creation_attempt_id SET NOT NULL;
ALTER TABLE runtime.provider_environment_run_uses ADD CONSTRAINT retained_provider_run_use_creation_attempt_fkey
 FOREIGN KEY(creation_attempt_id) REFERENCES runtime.provider_creation_operations(attempt_id);
DROP FUNCTION runtime.protect_retained_provider_run_use();
CREATE FUNCTION runtime.protect_retained_provider_run_use() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'provider consumer responsibility is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.cleanup_receipt_id IS NOT NULL OR NEW.cleanup_controller_epoch IS NOT NULL OR NEW.quiescence_digest IS NOT NULL OR NEW.cleanup_recorded_at_ms IS NOT NULL
  THEN RAISE EXCEPTION 'provider consumer starts without cleanup evidence'; END IF;
 ELSE
  IF ROW(NEW.provider_use_id,NEW.agent_run_id,NEW.environment_id,NEW.authorization_digest,NEW.execution_id,NEW.provider_use_digest,NEW.recorded_at_ms,NEW.creation_attempt_id)
   IS DISTINCT FROM ROW(OLD.provider_use_id,OLD.agent_run_id,OLD.environment_id,OLD.authorization_digest,OLD.execution_id,OLD.provider_use_digest,OLD.recorded_at_ms,OLD.creation_attempt_id)
   OR OLD.cleanup_receipt_id IS NOT NULL OR NEW.cleanup_receipt_id IS NULL
  THEN RAISE EXCEPTION 'provider consumer identity or evidence transition rejected'; END IF;
 END IF;
 IF NOT EXISTS(SELECT 1 FROM runtime.provider_creation_operations p
  JOIN public.app_core_agentrun r ON r.id=NEW.agent_run_id AND r.environment_id=NEW.environment_id
  JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=r.id
  WHERE p.attempt_id=NEW.creation_attempt_id AND p.environment_id=NEW.environment_id
   AND p.verified_sandbox_id IS NOT NULL AND p.verification_digest IS NOT NULL
   AND g.digest=NEW.authorization_digest AND g.payload->>'agentRunId'=NEW.agent_run_id
   AND p.session_id=g.payload->>'sessionId' AND p.workspace_id=g.payload->>'workspaceId'
   AND p.profile_digest=g.payload->>'environmentProfileDigest'
   AND p.provider_config_digest=g.payload->'environmentProfile'->>'providerConfigDigest'
   AND g.payload->'environmentProfile'->>'schema'='runtime.e2b_environment_profile.v1'
   AND g.payload->'environmentProfile'->>'lifetime'='retained')
 THEN RAISE EXCEPTION 'provider consumer creation scope rejected'; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER retained_provider_run_use_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_environment_run_uses FOR EACH ROW EXECUTE FUNCTION runtime.protect_retained_provider_run_use();
"#;

pub(crate) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let valid: bool = db.query_one("SELECT count(*)=1 FROM pg_constraint WHERE conrelid='runtime.provider_environment_run_uses'::regclass AND conname='retained_provider_run_use_creation_attempt_fkey' AND contype='f' AND conkey=ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid='runtime.provider_environment_run_uses'::regclass AND attname='creation_attempt_id' AND NOT attisdropped)]::smallint[] AND confrelid='runtime.provider_creation_operations'::regclass AND confkey=ARRAY[(SELECT attnum FROM pg_attribute WHERE attrelid='runtime.provider_creation_operations'::regclass AND attname='attempt_id' AND NOT attisdropped)]::smallint[] AND confdeltype='a' AND confupdtype='a' AND confmatchtype='s' AND convalidated AND NOT condeferrable AND NOT condeferred", &[]).map_err(|e|e.to_string())?.get(0);
    if !valid {
        return Err("retained_provider_history_schema_key_rejected".into());
    }
    Ok(())
}

/// Keyset pages bound memory while the migration transaction pins this table.
/// No provider call, receipt rewrite or settlement repair is implied.
pub(crate) fn validate_backfill(db: &mut impl GenericClient) -> Result<(), String> {
    let mut after: Option<String> = None;
    loop {
        let rows = db.query("SELECT provider_use_id,agent_run_id FROM runtime.provider_environment_run_uses WHERE ($1::text IS NULL OR provider_use_id>$1) ORDER BY provider_use_id LIMIT 256", &[&after]).map_err(|e|e.to_string())?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in rows {
            after = Some(row.get(0));
            let run: String = row.get(1);
            super::load_use(db, &run)?.ok_or("retained_provider_history_use_missing")?;
            let _receipt = super::receipts::load_receipt(db, &run)?;
        }
    }
}

#[cfg(test)]
pub(in crate::postgres_store) fn restore_previous_fixture(db: &mut postgres::Client) {
    let exists: bool = db.query_one("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema='runtime' AND table_name='provider_environment_run_uses' AND column_name='creation_attempt_id')", &[]).unwrap().get(0);
    if !exists {
        return;
    }
    let mut tx = db.transaction().unwrap();
    tx.batch_execute("LOCK TABLE runtime.schema_migrations, runtime.provider_environment_run_uses IN ACCESS EXCLUSIVE MODE; DROP TRIGGER retained_provider_run_use_facts ON runtime.provider_environment_run_uses; ALTER TABLE runtime.provider_environment_run_uses DROP COLUMN creation_attempt_id; DROP FUNCTION runtime.protect_retained_provider_run_use();").unwrap();
    let start = "CREATE FUNCTION runtime.protect_retained_provider_run_use()";
    let guard = super::DDL
        .split_once(start)
        .unwrap()
        .1
        .split_once("$$;")
        .unwrap()
        .0;
    tx.batch_execute(&format!("{start}{guard}$$; CREATE TRIGGER retained_provider_run_use_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_environment_run_uses FOR EACH ROW EXECUTE FUNCTION runtime.protect_retained_provider_run_use(); DELETE FROM runtime.schema_migrations WHERE version=21;")).unwrap();
    tx.commit().unwrap();
}
