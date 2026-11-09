use crate::agent_run_authorization::AGENT_RUN_AUTHORIZATION_SCHEMA;
use crate::contract::AgentRunStart;
use crate::postgres_store::RequestDeadline;
use std::time::Duration;

use super::{DockerExecutionHostRunner, ProjectedInputState, ProjectedInputValidation};

const AUTHORITY_SQL: &str =
    include_str!("../../../api/app_core/native_projected_input_authority.sql");

pub(crate) struct ProjectedInputAuthority {
    authorization_signature: String,
    authorization_payload: String,
    environment_id: String,
    controller_epoch: i64,
}

impl ProjectedInputAuthority {
    pub(crate) fn from_verified_start(start: &AgentRunStart) -> Result<Option<Self>, String> {
        if start.authorization.schema != AGENT_RUN_AUTHORIZATION_SCHEMA {
            return Ok(None);
        }
        let environment = &start.environment_authorization.payload;
        let controller = environment
            .controller
            .as_ref()
            .ok_or("projected_input_authority_unavailable")?;
        Ok(Some(Self {
            environment_id: environment.environment_id.clone(),
            controller_epoch: controller.controller_epoch,
            authorization_signature: start.authorization_signature.clone(),
            authorization_payload: serde_json::to_string(&start.authorization)
                .map_err(|_| "projected_input_authorization_encoding_failed")?,
        }))
    }

    pub(super) fn validate(
        &self,
        host: &DockerExecutionHostRunner,
        inputs: &[ProjectedInputValidation<'_>],
    ) -> Result<Option<Vec<ProjectedInputState>>, String> {
        if inputs
            .iter()
            .any(|input| !input.input_ref.starts_with("library:"))
        {
            return Ok(None);
        }
        let candidates_json = serde_json::to_string(inputs)
            .map_err(|_| "projected_input_authority_request_invalid")?;
        let store = host
            .resident_store
            .as_ref()
            .ok_or("projected_input_authority_unavailable")?;
        let lease = host
            .resident_lease
            .as_ref()
            .ok_or("projected_input_authority_unavailable")?;
        let _deadline = RequestDeadline::limit(Duration::from_secs(10));
        let rows = store.with_client(|client| {
            client
                .query(
                    AUTHORITY_SQL,
                    &[
                        &host.agent_run_id,
                        &host.authorization_digest,
                        &self.authorization_signature,
                        &self.authorization_payload,
                        &lease.lease_owner,
                        &candidates_json,
                        &self.environment_id,
                        &self.controller_epoch,
                    ],
                )
                .map_err(|_| "projected_input_authority_unavailable".to_string())
        })?;
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
}
