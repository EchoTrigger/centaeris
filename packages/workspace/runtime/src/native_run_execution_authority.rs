use std::sync::Arc;
use std::time::Duration;

use postgres::types::ToSql;
use postgres::Row;

use crate::agent_run_authorization::AGENT_RUN_AUTHORIZATION_SCHEMA;
use crate::contract::AgentRunStart;
use crate::postgres_store::{PostgresRuntimeStore, RequestDeadline};

const RUN_AUTHORITY: &str = include_str!("../../api/app_core/native_run_execution_authority.sql");

pub(crate) fn with_run_authority(sql: &str) -> String {
    sql.replace("/* native_run_execution_authority */", RUN_AUTHORITY)
}

pub(crate) struct NativeRunExecutionAuthority {
    store: Arc<PostgresRuntimeStore>,
    run_id: String,
    authorization_digest: String,
    authorization_signature: String,
    authorization_payload: String,
    lease_owner: String,
    environment_id: String,
    workspace_id: String,
    controller_epoch: i64,
}

impl NativeRunExecutionAuthority {
    pub(crate) fn from_verified_start(
        start: &AgentRunStart,
        store: Arc<PostgresRuntimeStore>,
        lease_owner: String,
    ) -> Result<Option<Arc<Self>>, String> {
        if start.authorization.schema != AGENT_RUN_AUTHORIZATION_SCHEMA {
            return Ok(None);
        }
        let environment = &start.environment_authorization.payload;
        let controller = environment
            .controller
            .as_ref()
            .ok_or("native_run_controller_missing")?;
        Ok(Some(Arc::new(Self {
            store,
            run_id: start.agent_run_id.clone(),
            authorization_digest: start.authorization_digest.clone(),
            authorization_signature: start.authorization_signature.clone(),
            authorization_payload: serde_json::to_string(&start.authorization)
                .map_err(|_| "native_run_authorization_encoding_failed")?,
            lease_owner,
            environment_id: environment.environment_id.clone(),
            workspace_id: start.authorization.workspace_id.clone(),
            controller_epoch: controller.controller_epoch,
        })))
    }

    pub(crate) fn query(
        &self,
        sql: &str,
        resource_params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, String> {
        let mut params: Vec<&(dyn ToSql + Sync)> = vec![
            &self.run_id,
            &self.authorization_digest,
            &self.authorization_signature,
            &self.authorization_payload,
            &self.lease_owner,
            &self.environment_id,
            &self.controller_epoch,
        ];
        params.extend_from_slice(resource_params);
        let _deadline = RequestDeadline::limit(Duration::from_secs(10));
        self.store.with_client(|client| {
            client
                .query(sql, params.as_slice())
                .map_err(|_| "native_run_authority_unavailable".into())
        })
    }

    pub(crate) fn matches_lifecycle_lease(&self, job_id: &str, lease_owner: &str) -> bool {
        job_id.split_once(':')
            == Some((
                centaeris_core::session::reliability::AGENT_RUN_LIFECYCLE_JOB_KIND,
                self.run_id.as_str(),
            ))
            && lease_owner == self.lease_owner
    }

    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}
