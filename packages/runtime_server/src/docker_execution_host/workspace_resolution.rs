//! Resolve the signed workspace baseline from durable platform facts.
use std::sync::LazyLock;

use crate::native_run_execution_authority::{with_run_authority, NativeRunExecutionAuthority};

use super::{
    resolved_resolution, SessionWorkspace, SessionWorkspaceApiError, SessionWorkspaceLease,
    SessionWorkspaceResolution, SessionWorkspaceResolveResponse, SESSION_WORKSPACE_RESOLVED_SCHEMA,
};

static AUTHORITY_SQL: LazyLock<String> = LazyLock::new(|| {
    with_run_authority(include_str!(
        "../../../api/app_core/native_session_workspace_resolution.sql"
    ))
});

pub(super) fn resolve(
    authority: Option<&NativeRunExecutionAuthority>,
    lease: &SessionWorkspaceLease,
    frozen: &SessionWorkspace,
) -> Result<Option<SessionWorkspaceResolution>, SessionWorkspaceApiError> {
    let Some(authority) = authority else {
        return Ok(None);
    };
    if !authority.matches_lifecycle_lease(&lease.job_id, &lease.lease_owner) {
        return Err(rejected("session workspace lease binding mismatch"));
    }
    let rows = authority
        .query(AUTHORITY_SQL.as_str(), &[])
        .map_err(SessionWorkspaceApiError::Unavailable)?;
    let row = match rows.as_slice() {
        [row] => row,
        // The existing platform resolver owns classifications outside the
        // affirmative native witness, including revocation and cancellation.
        [] => return Ok(None),
        _ => return Err(rejected("native session workspace resolution is ambiguous")),
    };
    let disposition: &str = row
        .try_get(0)
        .map_err(|_| rejected("native session workspace disposition invalid"))?;
    let current: &str = row
        .try_get(1)
        .map_err(|_| rejected("native session workspace descriptor invalid"))?;
    let signed: &str = row
        .try_get(2)
        .map_err(|_| rejected("native session workspace grant invalid"))?;
    resolve_witness(disposition, current, signed, frozen).map(Some)
}

fn resolve_witness(
    disposition: &str,
    current: &str,
    signed: &str,
    frozen: &SessionWorkspace,
) -> Result<SessionWorkspaceResolution, SessionWorkspaceApiError> {
    let signed: SessionWorkspace = serde_json::from_str(signed)
        .map_err(|_| rejected("native session workspace grant invalid"))?;
    if &signed != frozen {
        return Err(rejected("session workspace signed baseline mismatch"));
    }
    match disposition {
        "baseline_unknown" => return Err(rejected("session_workspace_baseline_unavailable")),
        "baseline_conflict" => return Err(rejected("session_workspace_baseline_conflict")),
        _ => {}
    }
    let session_workspace = serde_json::from_str(current)
        .map_err(|_| rejected("native session workspace descriptor invalid"))?;
    resolved_resolution(
        &SessionWorkspaceResolveResponse {
            schema: SESSION_WORKSPACE_RESOLVED_SCHEMA.into(),
            disposition: disposition.into(),
            session_workspace,
        },
        frozen,
    )
}

fn rejected(reason: &str) -> SessionWorkspaceApiError {
    SessionWorkspaceApiError::Rejected(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_workspace_resolution_checks_signed_baseline_and_same_generation_reply() {
        let empty = r#"{"generation":0,"snapshotSha256":"","snapshotSizeBytes":0,"expandedSizeBytes":0,"fileCount":0}"#;
        let next = r#"{"generation":1,"snapshotSha256":"","snapshotSizeBytes":0,"expandedSizeBytes":0,"fileCount":0}"#;
        let frozen = serde_json::from_str(empty).unwrap();

        assert_eq!(
            resolve_witness("empty", empty, empty, &frozen).unwrap(),
            SessionWorkspaceResolution::Empty,
        );
        assert_eq!(
            resolve_witness("advanced", next, empty, &frozen).unwrap(),
            SessionWorkspaceResolution::Advanced,
        );
        assert!(resolve_witness("empty", next, empty, &frozen).is_err());
        assert!(resolve_witness("empty", empty, next, &frozen).is_err());
        assert!(resolve_witness("advanced", empty, empty, &frozen).is_err());
        assert!(resolve_witness("baseline_unknown", empty, empty, &frozen).is_err());
    }
}
