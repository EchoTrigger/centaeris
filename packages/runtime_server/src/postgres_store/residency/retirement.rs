//! Independently authorized maintenance of one recorded physical incarnation.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RetirementRequest {
    pub schema: String,
    pub operation_id: String,
    pub environment_id: String,
    pub instance_id: String,
    pub host_id: String,
    pub expected_controller_epoch: i64,
    pub acknowledge_tmpfs_loss: bool,
}

impl RetirementRequest {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let identity = |id: &str| {
            !id.is_empty()
                && id.len() <= 160
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
        };
        if self.schema != "runtime.environment.retire.v1"
            || !identity(&self.operation_id)
            || !identity(&self.environment_id)
            || !identity(&self.instance_id)
            || !identity(&self.host_id)
            || self.expected_controller_epoch < 0
            || !self.acknowledge_tmpfs_loss
        {
            return Err("environment_retirement_request_invalid".into());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetirementResult {
    pub schema: &'static str,
    pub operation_id: String,
    pub environment_id: String,
    pub instance_id: String,
    pub status: &'static str,
}

#[derive(Debug)]
pub(crate) enum RetirementError {
    /// No retirement was accepted: the locked Environment epoch had changed.
    ControllerEpochConflict,
    Rejected(String),
}

impl From<String> for RetirementError {
    fn from(error: String) -> Self {
        Self::Rejected(error)
    }
}

impl From<&str> for RetirementError {
    fn from(error: &str) -> Self {
        Self::Rejected(error.into())
    }
}

impl std::fmt::Display for RetirementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ControllerEpochConflict => f.write_str("environment_retirement_epoch_conflict"),
            Self::Rejected(error) => f.write_str(error),
        }
    }
}

impl From<RetirementError> for String {
    fn from(error: RetirementError) -> Self {
        error.to_string()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RetirementTarget {
    pub allocation_execution_id: String,
    pub allocation_agent_run_id: String,
    pub environment_id: String,
    pub instance_id: String,
    pub container_id: String,
    pub image_digest: String,
    pub binding_digest: String,
}

impl PostgresRuntimeStore {
    /// Caller authenticates independent maintenance authority. The durable barrier
    /// survives lost Docker acknowledgements and controller/Runtime restarts.
    pub(crate) fn retire_retained_instance(
        &self,
        request: &RetirementRequest,
        retire: impl FnOnce(&RetirementTarget) -> Result<bool, String> + Send,
    ) -> Result<RetirementResult, RetirementError> {
        request.validate()?;
        let body = serde_json::to_string(request).map_err(|e| e.to_string())?;
        self.with_execution_control_client_error(|client| {
            let lock = lock_host(client)?;
            let mut tx = lock.0.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            // Match controller authority's lock order at the Environment boundary.
            // All accepted task RPCs pin this row through physical dispatch.
            let env = tx.query_opt("SELECT controller_epoch FROM public.app_core_sessionenvironment WHERE id=$1 FOR UPDATE", &[&request.environment_id]).map_err(|e| e.to_string())?.ok_or("retirement environment missing")?;
            let previous = tx.query_opt("SELECT request_json,state,allocation_execution_id,container_id FROM runtime.environment_retirements WHERE operation_id=$1 OR environment_id=$2 OR instance_id=$3 FOR UPDATE", &[&request.operation_id,&request.environment_id,&request.instance_id]).map_err(|e| e.to_string())?;
            let (execution, container) = if let Some(row) = previous {
                if row.get::<_,String>(0) != body { return Err("retirement operation conflict".into()); }
                if row.get::<_,String>(1) == "destroyed" {
                    tx.commit().map_err(|e| e.to_string())?;
                    return Ok(retirement_result(request, "destroyed"));
                }
                (row.get::<_,String>(2), row.get::<_,String>(3))
            } else {
                if env.get::<_,i64>(0) != request.expected_controller_epoch { return Err(RetirementError::ControllerEpochConflict); }
                let row = tx.query_opt("SELECT s.execution_id,s.container_id FROM runtime.resident_sandboxes s JOIN runtime.execution_instances i ON i.execution_id=s.execution_id AND i.instance_id=s.instance_id WHERE s.environment_id=$1 AND s.instance_id=$2 AND s.host_id=$3 AND s.allocation_mode='retained' AND i.allocation_mode='retained' FOR UPDATE OF s", &[&request.environment_id,&request.instance_id,&request.host_id]).map_err(|e| e.to_string())?.ok_or("retirement allocation missing or mismatched")?;
                let execution: String = row.get(0);
                let container = row.get::<_,Option<String>>(1).ok_or("retirement physical identity unknown")?;
                tx.execute("INSERT INTO runtime.environment_retirements(operation_id,environment_id,instance_id,host_id,allocation_execution_id,container_id,request_json,state,recorded_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,'pending',(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint)", &[&request.operation_id,&request.environment_id,&request.instance_id,&request.host_id,&execution,&container,&body]).map_err(|e| e.to_string())?;
                (execution, container)
            };
            let row = tx.query_one("SELECT agent_run_id,image_digest,binding_digest FROM runtime.execution_instances WHERE execution_id=$1 AND allocation_execution_id=execution_id AND environment_id=$2 AND instance_id=$3 AND host_id=$4 AND allocation_mode='retained'", &[&execution,&request.environment_id,&request.instance_id,&request.host_id]).map_err(|e| e.to_string())?;
            let target = RetirementTarget { allocation_execution_id: execution.clone(), allocation_agent_run_id: row.get(0), environment_id: request.environment_id.clone(), instance_id: request.instance_id.clone(), container_id: container, image_digest: row.get(1), binding_digest: row.get(2) };
            tx.execute("UPDATE runtime.resident_sandboxes SET state='releasing' WHERE execution_id=$1 AND host_id=$2 AND instance_id=$3", &[&execution,&request.host_id,&request.instance_id]).map_err(|e| e.to_string())?;
            // No physical mutation before this synchronous resource commit.
            tx.commit().map_err(|e| e.to_string())?;
            // Unknown physical outcomes retain both the barrier and the debit.
            if retire(&target) != Ok(true) { return Ok(retirement_result(request, "pending")); }
            let mut tx = lock.0.transaction().map_err(|e| e.to_string())?;
            super::super::resource_commit::require_durable_resource_commit(&mut tx)?;
            tx.execute("DELETE FROM runtime.resident_sandboxes WHERE execution_id=$1 AND host_id=$2 AND instance_id=$3 AND state='releasing'", &[&execution,&request.host_id,&request.instance_id]).map_err(|e| e.to_string())?;
            tx.execute("UPDATE runtime.environment_retirements SET state='destroyed' WHERE operation_id=$1 AND state='pending'", &[&request.operation_id]).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            Ok(retirement_result(request, "destroyed"))
        })
    }
}

fn retirement_result(request: &RetirementRequest, status: &'static str) -> RetirementResult {
    RetirementResult {
        schema: "runtime.environment.retire.result.v1",
        operation_id: request.operation_id.clone(),
        environment_id: request.environment_id.clone(),
        instance_id: request.instance_id.clone(),
        status,
    }
}

pub(crate) const DDL: &str = r#"
CREATE TABLE runtime.environment_retirements(operation_id text PRIMARY KEY,environment_id text NOT NULL UNIQUE,instance_id text NOT NULL UNIQUE,host_id text NOT NULL,allocation_execution_id text NOT NULL REFERENCES runtime.execution_instances(execution_id),container_id text NOT NULL,request_json text NOT NULL,state text NOT NULL CHECK(state IN('pending','destroyed')),recorded_at_ms bigint NOT NULL);
CREATE FUNCTION runtime.protect_environment_retirement() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
IF TG_OP='DELETE' THEN RAISE EXCEPTION 'environment retirement history is immutable'; END IF;
IF (to_jsonb(NEW)-'state') IS DISTINCT FROM (to_jsonb(OLD)-'state') OR (OLD.state='destroyed' AND NEW.state<>'destroyed') THEN RAISE EXCEPTION 'environment retirement identity is immutable'; END IF;
RETURN NEW; END $$;
CREATE TRIGGER environment_retirement_facts BEFORE UPDATE OR DELETE ON runtime.environment_retirements FOR EACH ROW EXECUTE FUNCTION runtime.protect_environment_retirement();
"#;
