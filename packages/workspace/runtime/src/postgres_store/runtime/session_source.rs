//! A bounded, verified runtime view bound to an unchanged canonical source.

use super::SessionSourceObservations;
use centaeris_core::session::state::SessionStateSnapshot;
use postgres::GenericClient;

pub(in crate::postgres_store) const DDL: &str = r#"
CREATE TABLE runtime.session_source_mutations(session_id text PRIMARY KEY,revision bigint NOT NULL CHECK(revision>0));
CREATE TABLE runtime.session_source_checkpoints(session_id text PRIMARY KEY,source_sequence bigint NOT NULL CHECK(source_sequence>0),mutation_revision bigint NOT NULL CHECK(mutation_revision>=0),snapshot_json text NOT NULL,observations_json text NOT NULL);
CREATE FUNCTION runtime.record_session_source_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='UPDATE' AND OLD IS NOT DISTINCT FROM NEW THEN RETURN NULL; END IF;
 IF TG_OP='UPDATE' AND TG_TABLE_NAME='app_core_sessionevent'
  AND (to_jsonb(OLD)-'projects_to_agent_run_stream'-'insertedAt') IS NOT DISTINCT FROM (to_jsonb(NEW)-'projects_to_agent_run_stream'-'insertedAt') THEN RETURN NULL; END IF;
 INSERT INTO runtime.session_source_mutations(session_id,revision) VALUES(OLD.session_id,1)
 ON CONFLICT(session_id) DO UPDATE SET revision=session_source_mutations.revision+1;
 IF TG_OP='UPDATE' AND NEW.session_id IS DISTINCT FROM OLD.session_id THEN
  INSERT INTO runtime.session_source_mutations(session_id,revision) VALUES(NEW.session_id,1)
  ON CONFLICT(session_id) DO UPDATE SET revision=session_source_mutations.revision+1;
 END IF;
 RETURN NULL;
END;
$$;
CREATE TRIGGER model_observation_content_source_mutation AFTER UPDATE OR DELETE ON runtime.model_observation_contents FOR EACH ROW EXECUTE FUNCTION runtime.record_session_source_mutation();
CREATE TRIGGER model_observation_manifest_source_mutation AFTER UPDATE OR DELETE ON runtime.model_observation_manifests FOR EACH ROW EXECUTE FUNCTION runtime.record_session_source_mutation();
CREATE TRIGGER model_input_image_source_mutation AFTER UPDATE OR DELETE ON runtime.session_model_input_images FOR EACH ROW EXECUTE FUNCTION runtime.record_session_source_mutation();
"#;

pub(in crate::postgres_store) const CANONICAL_TRIGGER_DDL: &str = r#"
CREATE TRIGGER session_event_source_mutation AFTER UPDATE OR DELETE ON public.app_core_sessionevent FOR EACH ROW EXECUTE FUNCTION runtime.record_session_source_mutation();
CREATE INDEX session_tool_call_origins ON public.app_core_sessionevent(session_id,((payload->'payload')->>'callId')) WHERE payload->>'type'='tool_call';
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionSourceWitness {
    pub(crate) sequence: u64,
    pub(crate) mutation_revision: i64,
}

pub(in crate::postgres_store) fn revision(
    client: &mut impl GenericClient,
    session_id: &str,
) -> Result<i64, String> {
    client.query_one(
        "SELECT COALESCE((SELECT revision FROM runtime.session_source_mutations WHERE session_id=$1),0)",
        &[&session_id],
    ).map(|row| row.get(0)).map_err(|error| error.to_string())
}

/// The source table belongs to the platform and may be absent in store-only
/// databases. Any operation that reuses source verification requires its guard.
pub(in crate::postgres_store) fn require_source_guard(
    client: &mut impl GenericClient,
) -> Result<(), String> {
    let guarded: bool = client.query_one(
        "SELECT EXISTS(SELECT 1 FROM pg_trigger WHERE tgrelid='public.app_core_sessionevent'::regclass AND tgname='session_event_source_mutation' AND tgenabled='O' AND NOT tgisinternal AND tgtype=25 AND tgfoid='runtime.record_session_source_mutation()'::regprocedure)",
        &[],
    ).map_err(|error| error.to_string())?.get(0);
    if !guarded {
        return Err("canonical Session source mutation guard missing".into());
    }
    Ok(())
}

pub(in crate::postgres_store) fn load_checkpoint(
    client: &mut impl GenericClient,
    session_id: &str,
    sequence: u64,
) -> Result<
    Option<(
        SessionStateSnapshot,
        SessionSourceWitness,
        SessionSourceObservations,
    )>,
    String,
> {
    require_source_guard(client)?;
    let sequence = i64::try_from(sequence).map_err(|_| "Session source sequence overflow")?;
    let row = client.query_opt(
        "SELECT c.snapshot_json,c.mutation_revision,c.observations_json FROM runtime.session_source_checkpoints c WHERE c.session_id=$1 AND c.source_sequence=$2 AND c.mutation_revision=COALESCE((SELECT revision FROM runtime.session_source_mutations WHERE session_id=$1),0)",
        &[&session_id, &sequence],
    ).map_err(|error| error.to_string())?;
    row.map(|row| {
        let snapshot: SessionStateSnapshot =
            serde_json::from_str(&row.get::<_, String>(0)).map_err(|error| error.to_string())?;
        centaeris_core::session::history::validate_session_snapshot_history(&snapshot)?;
        if snapshot.session_id != session_id {
            return Err("source checkpoint Session mismatch".into());
        }
        let observations: SessionSourceObservations =
            serde_json::from_str(&row.get::<_, String>(2)).map_err(|error| error.to_string())?;
        if let Some(root) = &observations.root_digest {
            super::validate_model_observation_digest(root)?;
        } else if !observations.references.is_empty() {
            return Err("source observation refs have no root".into());
        }
        for reference in &observations.references {
            super::validate_model_observation_digest(&reference.content_digest)?;
            if !matches!(reference.kind.as_str(),"system_prompt"|"message"|"input_image"|"tool_catalog"|"compaction_prompt"|"input_uptake"|"required_completion_delivery") {
                return Err("unsupported source observation reference kind".into());
            }
        }
        let canonical_sequence = i32::try_from(sequence).map_err(|_| "canonical source sequence overflow")?;
        let source = client.query_opt(
            "SELECT payload::text FROM public.app_core_sessionevent WHERE session_id=$1 AND sequence<=$2 AND payload->>'type'='model_request_started' ORDER BY sequence DESC LIMIT 1",
            &[&session_id,&canonical_sequence],
        ).map_err(|error| error.to_string())?;
        let source_root = source.map(|row| {
            let mut wire = serde_json::from_str(&row.get::<_,String>(0)).map_err(|error| error.to_string())?;
            super::super::decode_session_payload(&mut wire)?;
            super::model_request_manifest_key(&wire)?.ok_or_else(|| "checkpoint source model manifest missing".to_string())
        }).transpose()?;
        if source_root.as_ref().map(|(owner,_)|owner.as_str()).is_some_and(|owner|owner!=session_id)
            || source_root.as_ref().map(|(_,root)|root) != observations.root_digest.as_ref()
        { return Err("source checkpoint observation root binding mismatch".into()); }
        if let Some(root) = &observations.root_digest {
            let row = client.query_opt(
                "SELECT parent_digest,manifest_json,manifest_bytes,first_seen_at_ms FROM runtime.model_observation_manifests WHERE session_id=$1 AND manifest_digest=$2",
                &[&session_id,&root],
            ).map_err(|error| error.to_string())?.ok_or("checkpoint source manifest missing")?;
            let manifest = super::decode_model_observation_manifest(root.clone(),row.get(0),row.get(1),row.get(2),row.get(3))?;
            if manifest.observation_count != observations.references.len()
                || manifest.changes.iter().any(|change|observations.references.get(change.index)!=Some(&change.reference))
            { return Err("source checkpoint observation refs binding mismatch".into()); }
        }
        Ok((
            snapshot,
            SessionSourceWitness {
                sequence: sequence as u64,
                mutation_revision: row.get(1),
            },
            observations,
        ))
    })
    .transpose()
}

pub(in crate::postgres_store) fn save_checkpoint(
    client: &mut impl GenericClient,
    snapshot: &centaeris_core::session::history::SessionSnapshotWriteV1,
    witness: SessionSourceWitness,
    observations: &SessionSourceObservations,
) -> Result<(), String> {
    let sequence =
        i64::try_from(witness.sequence).map_err(|_| "Session source sequence overflow")?;
    let observations_json =
        serde_json::to_string(observations).map_err(|error| error.to_string())?;
    client.execute(
        "INSERT INTO runtime.session_source_checkpoints(session_id,source_sequence,mutation_revision,snapshot_json,observations_json) VALUES($1,$2,$3,$4,$5) ON CONFLICT(session_id) DO UPDATE SET source_sequence=excluded.source_sequence,mutation_revision=excluded.mutation_revision,snapshot_json=excluded.snapshot_json,observations_json=excluded.observations_json",
        &[&snapshot.session_id(),&sequence,&witness.mutation_revision,&snapshot.snapshot_json(),&observations_json],
    ).map_err(|error| error.to_string())?;
    Ok(())
}
