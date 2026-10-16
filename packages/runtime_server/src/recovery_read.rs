//! Bounded PostgreSQL transport for authoritative recovery records.
use postgres::GenericClient;
use serde_json::Value;

const HEADER_PAGE_RECORDS: i64 = 64;
const PAGE_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(crate) struct ReadStats {
    pub(crate) pages: usize,
    pub(crate) records: usize,
    pub(crate) max_encoded_page_bytes: usize,
}

#[derive(Clone, Copy)]
pub(crate) enum Scope<'a> {
    Session(&'a str),
    Run(&'a str),
    BoundRun {
        run: &'a str,
        session: &'a str,
        workspace: &'a str,
    },
}

impl<'a> Scope<'a> {
    fn key(self) -> &'a str {
        match self {
            Self::Session(key) | Self::Run(key) => key,
            Self::BoundRun { run, .. } => run,
        }
    }
}

fn validate_run_envelope(
    wire: &Value,
    scope: Scope<'_>,
    id: &str,
    session: &str,
    global_sequence: i32,
    run: &str,
    workspace: &str,
) -> Result<(), String> {
    if wire["eventId"].as_str() != Some(id) || wire["sessionId"].as_str() != Some(session) {
        return Err("recovery stored envelope identity mismatch".into());
    }
    if let Scope::BoundRun {
        run: expected_run,
        session: expected_session,
        workspace: expected_workspace,
    } = scope
    {
        if run != expected_run
            || session != expected_session
            || workspace != expected_workspace
            || wire["agentRunId"].as_str() != Some(expected_run)
            || wire["sequence"].as_i64() != Some(i64::from(global_sequence))
        {
            return Err("recovery stored Run binding mismatch".into());
        }
    }
    Ok(())
}

fn page_end(after: i32, headers: &[(i32, i32)], budget: usize) -> Result<i32, String> {
    let mut end = after;
    let mut bytes = 0usize;
    for &(sequence, length) in headers {
        if sequence != end.checked_add(1).ok_or("recovery sequence overflow")? || length < 0 {
            return Err("recovery page sequence or length is invalid".into());
        }
        let length = length as usize;
        // One atomic record may exceed the page budget. Its existing record
        // contract remains unchanged; no second record shares that page.
        if end > after && bytes.saturating_add(length) > budget {
            break;
        }
        bytes = bytes
            .checked_add(length)
            .ok_or("recovery page byte overflow")?;
        end = sequence;
        if bytes >= budget {
            break;
        }
    }
    Ok(end)
}

pub(crate) fn visit<C: GenericClient>(
    client: &mut C,
    scope: Scope<'_>,
    upper: i32,
    mut consume: impl FnMut(&mut C, i32, Value) -> Result<(), String>,
) -> Result<ReadStats, String> {
    if upper < 0 {
        return Err("recovery upper bound is invalid".into());
    }
    let header_sql = match scope {
        Scope::Session(_) => "SELECT sequence,octet_length(payload::text) FROM app_core_sessionevent WHERE session_id=$1 AND sequence>$2 AND sequence<=$3 ORDER BY sequence LIMIT $4",
        Scope::Run(_) | Scope::BoundRun { .. } => "SELECT agent_run_sequence,octet_length(payload::text) FROM app_core_sessionevent WHERE agent_run_id=$1 AND session_level=false AND agent_run_sequence>$2 AND agent_run_sequence<=$3 ORDER BY agent_run_sequence LIMIT $4",
    };
    let body_sql = match scope {
        Scope::Session(_) => "SELECT sequence,payload::text FROM app_core_sessionevent WHERE session_id=$1 AND sequence>$2 AND sequence<=$3 ORDER BY sequence",
        Scope::Run(_) | Scope::BoundRun { .. } => r#"SELECT agent_run_sequence,payload::text,"eventId",session_id,sequence,agent_run_id,workspace_id FROM app_core_sessionevent WHERE agent_run_id=$1 AND session_level=false AND agent_run_sequence>$2 AND agent_run_sequence<=$3 ORDER BY agent_run_sequence"#,
    };
    let mut after = 0;
    let mut stats = ReadStats::default();
    while after < upper {
        let headers = client
            .query(
                header_sql,
                &[&scope.key(), &after, &upper, &HEADER_PAGE_RECORDS],
            )
            .map_err(|error| format!("read recovery page headers failed: {error}"))?;
        let headers = headers
            .iter()
            .map(|row| (row.get::<_, i32>(0), row.get::<_, i32>(1)))
            .collect::<Vec<_>>();
        let end = page_end(after, &headers, PAGE_BYTES)?;
        if end == after {
            return Err("recovery high-water mark is missing".into());
        }
        let rows = client
            .query(body_sql, &[&scope.key(), &after, &end])
            .map_err(|error| format!("read recovery page bodies failed: {error}"))?;
        if rows.len() != usize::try_from(end - after).map_err(|_| "recovery page range invalid")? {
            return Err("recovery page changed while reading".into());
        }
        stats.pages += 1;
        stats.records += rows.len();
        stats.max_encoded_page_bytes = stats.max_encoded_page_bytes.max(
            headers
                .iter()
                .filter(|(sequence, _)| *sequence <= end)
                .map(|(_, bytes)| *bytes as usize)
                .sum(),
        );
        for row in rows {
            let sequence = row.get::<_, i32>(0);
            if sequence != after + 1 {
                return Err("recovery page sequence changed".into());
            }
            let raw = row.get::<_, String>(1);
            let expected_length = headers
                .iter()
                .find(|(position, _)| *position == sequence)
                .ok_or("recovery page header missing")?
                .1;
            if raw.len() != expected_length as usize {
                return Err("recovery page body changed while reading".into());
            }
            let mut wire: Value = serde_json::from_str(&raw)
                .map_err(|error| format!("decode recovery page wire failed: {error}"))?;
            crate::postgres_store::decode_session_payload(&mut wire)?;
            if matches!(scope, Scope::Run(_) | Scope::BoundRun { .. }) {
                let id = row.get::<_, String>(2);
                let session = row.get::<_, String>(3);
                validate_run_envelope(
                    &wire,
                    scope,
                    &id,
                    &session,
                    row.get(4),
                    row.get::<_, String>(5).as_str(),
                    row.get::<_, String>(6).as_str(),
                )?;
            }
            consume(client, sequence, wire)?;
            after = sequence;
        }
    }
    Ok(stats)
}

pub(crate) fn validate_for_replay(
    mut wire: Value,
    hydrate: impl FnOnce(&mut Value) -> Result<(), String>,
) -> Result<centaeris_core::session::SessionLogRecord, String> {
    hydrate(&mut wire)?;
    centaeris_core::session::parse_wire_record(&wire)
        .map(|record| record.event)
        .map_err(|error| format!("decode recovery Session record failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bound_run_rejects_foreign_ownership_and_global_sequence() {
        let scope = Scope::BoundRun {
            run: "run",
            session: "session",
            workspace: "workspace",
        };
        let wire = serde_json::json!({"eventId":"event","sessionId":"session","agentRunId":"run","sequence":17});
        assert!(
            validate_run_envelope(&wire, scope, "event", "session", 17, "run", "workspace").is_ok()
        );
        for (session, sequence, run, workspace) in [
            ("foreign", 17, "run", "workspace"),
            ("session", 18, "run", "workspace"),
            ("session", 17, "foreign", "workspace"),
            ("session", 17, "run", "foreign"),
        ] {
            assert!(validate_run_envelope(
                &wire, scope, "event", session, sequence, run, workspace
            )
            .is_err());
        }
        let mut foreign_wire = wire;
        foreign_wire["agentRunId"] = serde_json::json!("foreign");
        assert!(validate_run_envelope(
            &foreign_wire,
            scope,
            "event",
            "session",
            17,
            "run",
            "workspace"
        )
        .is_err());
    }
    #[test]
    fn inactive_history_still_requires_its_content_and_full_record_validation() {
        let unavailable = validate_for_replay(serde_json::json!({}), |_| {
            Err("required historical content missing".into())
        });
        assert!(
            unavailable.is_err(),
            "a tombstone must not bypass missing stored content"
        );
        let invalid =
            validate_for_replay(serde_json::json!({"type":"model_request_started"}), |_| {
                Ok(())
            });
        assert!(
            invalid.is_err(),
            "an inactive malformed historical request must still reject"
        );
    }

    #[test]
    fn byte_budget_preserves_order_and_resumes_without_loss() {
        assert_eq!(page_end(0, &[(1, 4), (2, 5), (3, 6)], 10).unwrap(), 2);
        assert_eq!(page_end(2, &[(3, 6)], 10).unwrap(), 3);
        assert_eq!(page_end(0, &[(1, 10), (2, 1)], 10).unwrap(), 1);
    }
    #[test]
    fn oversized_atomic_record_is_preserved_on_its_own_page() {
        assert_eq!(page_end(0, &[(1, 11), (2, 1)], 10).unwrap(), 1);
        assert_eq!(page_end(1, &[(2, 1)], 10).unwrap(), 2);
    }
    #[test]
    fn missing_positions_and_invalid_lengths_reject() {
        assert!(page_end(0, &[(2, 1)], 10).is_err());
        assert!(page_end(0, &[(1, -1)], 10).is_err());
        assert!(page_end(i32::MAX, &[(0, 1)], 10).is_err());
    }
}
