//! Core-selected workspace roots, witnessed against concurrent durable changes.
use super::{PostgresRuntimeStore, SessionWireHydrator};
use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;
use centaeris_core::runtime::contracts::{
    CheckpointKindV1, CheckpointRecord, RuntimeRecoveryCheckpointV1,
};
use centaeris_core::session::{
    AgentRunSessionState, SequencedSessionRecord, SessionLogRecord, SessionRecordType,
};
use postgres::GenericClient;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const WITNESS_SQL: &str = include_str!("../../../api/app_core/workspace_snapshot_gc_witness.sql");
const MAX_ROOTS: usize = 256;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    schema: String,
    session_id: String,
}

/// Return bounded workspace roots with a witness the collector must recheck.
pub(crate) fn resolve(
    body: &[u8],
    store: &PostgresRuntimeStore,
) -> crate::job_protocol::ProtocolResult {
    let request: Request =
        serde_json::from_slice(body).map_err(|_| (400, "workspace_snapshot_roots_invalid"))?;
    if request.schema != "runtime.workspace_snapshot.roots.get.v1"
        || request.session_id.is_empty()
        || request.session_id.len() > 128
        || request.session_id.chars().any(char::is_control)
    {
        return Err((400, "workspace_snapshot_roots_invalid"));
    }
    store
        .workspace_snapshot_roots(&request.session_id)
        .map_err(|error| {
            if error == "workspace_snapshot_roots_session_missing" {
                (404, "workspace_snapshot_roots_session_missing")
            } else {
                (503, "workspace_snapshot_roots_unavailable")
            }
        })
}

fn witness(db: &mut impl GenericClient, session: &str) -> Result<(i64, String), String> {
    let row = db
        .query_opt(WITNESS_SQL, &[&session])
        .map_err(|error| format!("snapshot root witness unavailable: {error}"))?
        .ok_or("workspace_snapshot_roots_session_missing")?;
    Ok((row.get(0), row.get(1)))
}

impl PostgresRuntimeStore {
    /// Resolve Core checkpoint roots while pinning the Session against appends.
    pub(crate) fn workspace_snapshot_roots(&self, session: &str) -> Result<Value, String> {
        self.with_execution_control_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            let row = tx
                .query_opt(
                    "SELECT workspace_id FROM public.app_core_session WHERE id=$1 FOR SHARE NOWAIT",
                    &[&session],
                )
                .map_err(|error| error.to_string())?
                .ok_or("workspace_snapshot_roots_session_missing")?;
            let workspace: String = row.get(0);
            let before = witness(&mut tx, session)?;
            let facts: Value =
                serde_json::from_str(&before.1).map_err(|error| error.to_string())?;
            let ids = facts["protectedAgentRunIds"]
                .as_array()
                .ok_or("snapshot root witness identities missing")?;
            if facts["runsOverflow"] != false || ids.len() > MAX_ROOTS {
                return Err("snapshot root responsibility bound exceeded".into());
            }
            let mut roots = Vec::new();
            for id in ids {
                let run = id.as_str().ok_or("snapshot root Run identity invalid")?;
                if let Some(root) = run_root(&mut tx, session, &workspace, run, before.0)? {
                    roots.push(root);
                }
            }
            if roots.len() > MAX_ROOTS || witness(&mut tx, session)? != before {
                return Err("snapshot root witness changed".into());
            }
            let fingerprint = format!("sha256:{:x}", Sha256::digest(before.1.as_bytes()));
            let result = json!({"schema":"runtime.workspace_snapshot.roots.v1","sessionId":session,
                "sessionSequence":before.0,"authorityFingerprint":fingerprint,
                "protectedAgentRunIds":ids,"roots":roots});
            tx.commit().map_err(|error| error.to_string())?;
            Ok(result)
        })
    }
}

fn run_root(
    db: &mut impl GenericClient,
    session: &str,
    workspace: &str,
    run: &str,
    high_water: i64,
) -> Result<Option<Value>, String> {
    let row=db.query_opt("SELECT a.digest,a.payload::text,j.job_kind,j.session_id,j.payload_ref,j.idempotency_key FROM public.app_core_agentrun r JOIN public.app_core_agentrunauthorization a ON a.agent_run_id=r.id LEFT JOIN runtime.runtime_jobs j ON j.job_id='agent_run.lifecycle:'||r.id WHERE r.id=$1 AND r.session_id=$2 AND r.workspace_id=$3",&[&run,&session,&workspace]).map_err(|error| error.to_string())?.ok_or("snapshot root original authority missing")?;
    let digest: String = row.get(0);
    let authorization: WorkspaceAgentRunAuthorization =
        serde_json::from_str(&row.get::<_, String>(1)).map_err(|error| error.to_string())?;
    authorization
        .checked_binding(run)?
        .verify_digest_binding(&digest)?;
    if authorization.session_id != session || authorization.workspace_id != workspace {
        return Err("snapshot root authority owner mismatch".into());
    }
    if let Some(kind) = row.get::<_, Option<String>>(2) {
        if kind != "agent_run.lifecycle"
            || row.get::<_, Option<String>>(3).as_deref() != Some(session)
            || row.get::<_, Option<String>>(4).as_deref()
                != Some(format!("record:agent_run:{run}").as_str())
            || row.get::<_, Option<String>>(5).as_deref()
                != Some(format!("agent_run.lifecycle:{run}:{digest}").as_str())
        {
            return Err("snapshot root canonical Job binding mismatch".into());
        }
    }
    let positions=db.query_one("SELECT COALESCE(MAX(agent_run_sequence),0),COUNT(*) FILTER(WHERE agent_run_sequence IS NULL OR agent_run_sequence<=0) FROM public.app_core_sessionevent WHERE agent_run_id=$1 AND NOT session_level",&[&run]).map_err(|error| error.to_string())?;
    let upper: i32 = positions.get(0);
    if positions.get::<_, i64>(1) != 0 {
        return Err("snapshot root Run sequence invalid".into());
    }
    let mut state = AgentRunSessionState::new(session, run)?;
    let mut hydrator = SessionWireHydrator::default();
    let mut latest_reference: Option<SessionLogRecord> = None;
    crate::recovery_read::visit(
        db,
        crate::recovery_read::Scope::BoundRun {
            run,
            session,
            workspace,
        },
        upper,
        |client, position, mut wire| {
            hydrator.hydrate_record(client, &mut wire)?;
            let event = crate::parse_wire_record(&wire)
                .map_err(|error| error.to_string())?
                .event;
            if event.event_type == SessionRecordType::CheckpointRef
                && event.payload["kind"] == "recovery"
            {
                latest_reference = Some(event.clone());
            }
            state.restore(SequencedSessionRecord {
                sequence: position as u64,
                event,
            })
        },
    )?;
    let Some(id) = state.latest_recovery_checkpoint_id() else {
        return Ok(None);
    };
    let row=db.query_opt("SELECT checkpoint_id,kind,session_id,turn_id,status,done_reason,updated_at_ms,payload_json,wait_handoff_json FROM runtime.checkpoints WHERE checkpoint_id=$1",&[&id]).map_err(|error| error.to_string())?.ok_or("snapshot root checkpoint missing")?;
    let record = CheckpointRecord {
        checkpoint_id: row.get(0),
        kind: CheckpointKindV1::parse(&row.get::<_, String>(1))?,
        session_id: row.get(2),
        turn_id: row.get(3),
        status: row.get(4),
        done_reason: row.get(5),
        updated_at_ms: row.get(6),
        payload_json: row.get(7),
    };
    let payload: RuntimeRecoveryCheckpointV1 =
        serde_json::from_str(&record.payload_json).map_err(|error| error.to_string())?;
    payload.validate()?;
    if record.kind != CheckpointKindV1::Recovery
        || record.status != "committed"
        || record.done_reason.is_some()
        || record.checkpoint_id != payload.checkpoint_id
        || record.session_id != session
        || payload.session_id != session
        || payload.agent_run_id != run
        || payload.authorization_digest != digest
        || payload.session_sequence
            > u64::try_from(high_water).map_err(|_| "snapshot root high-water invalid")?
        || !state.has_checkpoint(&payload.checkpoint_id)
    {
        return Err("snapshot root checkpoint binding mismatch".into());
    }
    crate::wait_recovery::WaitHandoff::validate_reference(
        &record,
        &latest_reference.ok_or("snapshot root checkpoint reference missing")?,
    )?;
    if crate::wait_recovery::WaitHandoff::is_wait_checkpoint(id) {
        let attached: crate::wait_recovery::WaitHandoff = serde_json::from_str(
            &row.get::<_, Option<String>>(8)
                .ok_or("snapshot root wait handoff missing")?,
        )
        .map_err(|error| error.to_string())?;
        attached.validate(&payload)?;
    }
    // A used latest checkpoint remains the active Execution's workspace safety
    // baseline. Core's pending recovery and wait references share this frontier.
    let snapshot = &payload.workspace_snapshot;
    let Some(object) = snapshot.object_ref.as_deref() else {
        return Ok(None);
    };
    let digest_hex = snapshot
        .snapshot_sha256
        .strip_prefix("sha256:")
        .ok_or("snapshot root digest invalid")?;
    let key = if crate::recovery_uses_session_workspace(snapshot, &authorization.session_workspace)?
    {
        format!(
            "workspaces/{workspace}/sessions/{session}/snapshots/{}/{digest_hex}.snapshot",
            authorization.session_workspace.generation
        )
    } else {
        let prefix = format!(
            "workspaces/{workspace}/sessions/{session}/agent-runs/{run}/execution-checkpoints/"
        );
        let (directory, file) = object
            .strip_prefix(&prefix)
            .and_then(|suffix| suffix.split_once('/'))
            .ok_or("snapshot root object owner mismatch")?;
        if directory.len() != 64
            || !directory
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || file != format!("{digest_hex}.snapshot")
        {
            return Err("snapshot root object owner mismatch".into());
        }
        object.to_owned()
    };
    Ok(Some(
        json!({"agentRunId":run,"authorizationDigest":digest,"checkpointId":id,
        "objectRef":key,"snapshotSha256":snapshot.snapshot_sha256,
        "snapshotSizeBytes":snapshot.snapshot_size_bytes,"expandedSizeBytes":snapshot.expanded_size_bytes,
        "fileCount":snapshot.file_count}),
    ))
}
