use std::sync::LazyLock;

use crate::native_run_execution_authority::{with_run_authority, NativeRunExecutionAuthority};

use super::{ProjectedInputState, ProjectedInputValidation};

static AUTHORITY_SQL: LazyLock<String> = LazyLock::new(|| {
    with_run_authority(include_str!(
        "../../../api/app_core/native_projected_input_authority.sql"
    ))
});

pub(super) fn validate(
    authority: &NativeRunExecutionAuthority,
    inputs: &[ProjectedInputValidation<'_>],
) -> Result<Option<Vec<ProjectedInputState>>, String> {
    if inputs
        .iter()
        .any(|input| !input.input_ref.starts_with("library:"))
    {
        return Ok(None);
    }
    let candidates_json =
        serde_json::to_string(inputs).map_err(|_| "projected_input_authority_request_invalid")?;
    let rows = authority
        .query(AUTHORITY_SQL.as_str(), &[&candidates_json])
        .map_err(|_| "projected_input_authority_unavailable")?;
    if rows.len() != inputs.len() {
        return Err("projected_input_authority_denied".into());
    }
    let mut states = Vec::with_capacity(inputs.len());
    for (input, row) in inputs.iter().zip(rows) {
        let input_ref: &str = row
            .try_get(0)
            .map_err(|_| "projected_input_authority_result_invalid")?;
        let state: &str = row
            .try_get(1)
            .map_err(|_| "projected_input_authority_result_invalid")?;
        if input_ref != input.input_ref {
            return Err("projected_input_authority_result_invalid".into());
        }
        states.push(match state {
            "legacy" => None,
            "active" => Some(ProjectedInputState::Active),
            "asset_removed" => Some(ProjectedInputState::AssetRemoved),
            "access_revoked" => Some(ProjectedInputState::AccessRevoked),
            "stale_generation" => Some(ProjectedInputState::StaleGeneration),
            _ => return Err("projected_input_authority_result_invalid".into()),
        });
    }
    // A mixed batch is revalidated in full by Django. Never carry a
    // native allow across the potentially slow platform request.
    Ok(states.into_iter().collect())
}
