//! Immutable message revisions behind the bounded runtime working state.

use postgres::GenericClient;

use centaeris_core::session::history::{
    SessionHistoryEntryV1, SessionHistoryRecordV1, SessionHistorySelectionV1,
    SessionSnapshotWriteV1,
};
use centaeris_core::session::state::SessionStateSnapshot;

/// The snapshot write has already locked the Session row. Keep its head changes
/// in that transaction, so positions and body revisions become visible together.
pub(in crate::postgres_store) fn append_history_write(
    client: &mut impl GenericClient,
    session_id: &str,
    write: &SessionSnapshotWriteV1,
) -> Result<(), String> {
    if !write.entries().is_empty() {
        let mut record_ids = Vec::with_capacity(write.entries().len());
        let mut message_ids = Vec::with_capacity(write.entries().len());
        let mut bodies = Vec::with_capacity(write.entries().len());
        let mut original_users = Vec::with_capacity(write.entries().len());
        for entry in write.entries() {
            record_ids.push(entry.record_id.clone());
            message_ids.push(entry.message.message_id.clone());
            bodies.push(entry.canonical_json()?);
            original_users.push(entry.is_original_user_request());
        }
        client.execute(
            "INSERT INTO runtime.session_message_history_entries(session_id,record_id,message_id,entry_json) SELECT $1,v.record_id,v.message_id,v.entry_json FROM unnest($2::text[],$3::text[],$4::text[]) AS v(record_id,message_id,entry_json) ON CONFLICT(session_id,record_id) DO NOTHING",
            &[&session_id, &record_ids, &message_ids, &bodies],
        ).map_err(|e| format!("append immutable Session history failed: {e}"))?;
        let mismatches: i64 = client.query_one(
            "SELECT count(*) FROM unnest($2::text[],$3::text[],$4::text[]) AS v(record_id,message_id,entry_json) JOIN runtime.session_message_history_entries e ON e.session_id=$1 AND e.record_id=v.record_id WHERE e.message_id<>v.message_id OR e.entry_json<>v.entry_json",
            &[&session_id, &record_ids, &message_ids, &bodies],
        ).map_err(|e| e.to_string())?.get(0);
        if mismatches != 0 {
            return Err("Session history immutable body changed".into());
        }
        let last_position: i64 = client.query_one(
            "SELECT COALESCE(MAX(position),0) FROM runtime.session_message_history_heads WHERE session_id=$1",
            &[&session_id],
        ).map_err(|e| e.to_string())?.get(0);
        last_position
            .checked_add(
                i64::try_from(record_ids.len()).map_err(|_| "Session history batch too large")?,
            )
            .ok_or("Session history position overflow")?;
        client.execute(
            "INSERT INTO runtime.session_message_history_heads(session_id,message_id,position,current_record_id,is_original_user_request,retired) SELECT $1,v.message_id,$5+v.ordinal,v.record_id,v.original_user,FALSE FROM unnest($2::text[],$3::text[],$4::boolean[]) WITH ORDINALITY AS v(message_id,record_id,original_user,ordinal) ON CONFLICT(session_id,message_id) DO UPDATE SET current_record_id=EXCLUDED.current_record_id,is_original_user_request=EXCLUDED.is_original_user_request,retired=FALSE",
            &[&session_id, &message_ids, &record_ids, &original_users, &last_position],
        ).map_err(|e| format!("advance Session history heads failed: {e}"))?;
    }
    if !write.retired_message_ids().is_empty() {
        client.execute(
            "UPDATE runtime.session_message_history_heads SET retired=TRUE WHERE session_id=$1 AND message_id=ANY($2::text[])",
            &[&session_id, &write.retired_message_ids()],
        ).map_err(|e| format!("retire Session history heads failed: {e}"))?;
    }
    if !write.recovery_retired_message_ids().is_empty() {
        client.execute(
            "UPDATE runtime.session_message_history_heads SET retired=TRUE WHERE session_id=$1 AND message_id=ANY($2::text[])",
            &[&session_id,&write.recovery_retired_message_ids()],
        ).map_err(|error| format!("retire canonical Session history failed: {error}"))?;
    }
    if !write.retained_entries().is_empty() {
        let messages = write
            .retained_entries()
            .iter()
            .map(|entry| entry.message_id.clone())
            .collect::<Vec<_>>();
        let records = write
            .retained_entries()
            .iter()
            .map(|entry| entry.record_id.clone())
            .collect::<Vec<_>>();
        let matched: i64 = client.query_one(
            "SELECT count(*) FROM unnest($2::text[],$3::text[]) AS v(message_id,record_id) JOIN runtime.session_message_history_heads h ON h.session_id=$1 AND h.message_id=v.message_id AND h.current_record_id=v.record_id AND NOT h.retired",
            &[&session_id, &messages, &records],
        ).map_err(|e| e.to_string())?.get(0);
        if usize::try_from(matched).ok() != Some(write.retained_entries().len()) {
            return Err("Session working-state history binding mismatch".into());
        }
    }
    Ok(())
}

pub(in crate::postgres_store) fn load_session_history_page(
    client: &mut impl GenericClient,
    session_id: &str,
    before_position: Option<u64>,
    limit: usize,
    selection: SessionHistorySelectionV1,
) -> Result<Vec<SessionHistoryRecordV1>, String> {
    let before = before_position
        .map(i64::try_from)
        .transpose()
        .map_err(|_| "Session history position exceeds PostgreSQL range")?;
    let limit =
        i64::try_from(limit).map_err(|_| "Session history page exceeds PostgreSQL range")?;
    let original_users = selection == SessionHistorySelectionV1::OriginalUserRequests;
    client.query(
        "SELECT h.position,h.message_id,h.current_record_id,e.entry_json FROM runtime.session_message_history_heads h JOIN runtime.session_message_history_entries e ON e.session_id=h.session_id AND e.record_id=h.current_record_id WHERE h.session_id=$1 AND NOT h.retired AND ($2::bigint IS NULL OR h.position<$2) AND (NOT $3::boolean OR h.is_original_user_request) ORDER BY h.position DESC LIMIT $4",
        &[&session_id, &before, &original_users, &limit],
    ).map_err(|e| format!("read Session history page failed: {e}"))?
        .into_iter().map(|row| {
            let position = u64::try_from(row.get::<_, i64>(0))
                .map_err(|_| "Session history position is negative")?;
            let entry: SessionHistoryEntryV1 = serde_json::from_str(&row.get::<_, String>(3))
                .map_err(|e| e.to_string())?;
            entry.validate()?;
            if entry.message.message_id != row.get::<_, String>(1)
                || entry.record_id != row.get::<_, String>(2)
            {
                return Err("Session history head body binding mismatch".into());
            }
            Ok(SessionHistoryRecordV1 { position, entry })
        }).collect()
}

/// Explicit history consumers may hydrate the transcript; execution uses only
/// the bounded snapshot and never calls this function.
pub(in crate::postgres_store) fn hydrate_session_history(
    client: &mut impl GenericClient,
    session_id: &str,
    mut snapshot: SessionStateSnapshot,
) -> Result<SessionStateSnapshot, String> {
    let mut before = None;
    let mut entries = Vec::new();
    loop {
        let page = load_session_history_page(
            client,
            session_id,
            before,
            256,
            SessionHistorySelectionV1::All,
        )?;
        if page.is_empty() {
            break;
        }
        before = page.last().map(|record| record.position);
        entries.extend(page.into_iter().map(|record| record.entry));
    }
    entries.reverse();
    snapshot.messages = entries.iter().map(|entry| entry.message.clone()).collect();
    snapshot.model_semantics = entries
        .into_iter()
        .map(|entry| (entry.message.message_id, entry.model_semantics))
        .collect();
    Ok(snapshot)
}
