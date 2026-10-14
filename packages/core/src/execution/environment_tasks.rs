//! Persistent environment task contracts. Hosts translate these contracts.
use serde::{Deserialize, Serialize};

pub const ENVIRONMENT_TASK_SCHEMA: &str = "environment.task.v1";
pub const ENVIRONMENT_TASK_MAX_ACTIVE: usize = 32;
pub const ENVIRONMENT_TASK_MAX_RECORDS: usize = 64;
pub const ENVIRONMENT_TASK_OUTPUT_BYTES: usize = 1024 * 1024;
pub const ENVIRONMENT_TASK_READ_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentTaskEnvelope {
    pub schema: String,
    pub request: EnvironmentTaskRequest,
}
impl EnvironmentTaskEnvelope {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema == ENVIRONMENT_TASK_SCHEMA {
            Ok(())
        } else {
            Err("task_schema_unsupported".into())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentTaskFence {
    pub environment_instance_id: String,
    pub controller_id: String,
    pub controller_epoch: u64,
    pub controller_purpose: EnvironmentTaskControllerPurpose,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentTaskControllerPurpose {
    Execute,
    Cleanup,
}

impl EnvironmentTaskFence {
    pub fn authorize_termination(&self, scope: &EnvironmentTaskScope) -> Result<(), String> {
        if self.controller_purpose == EnvironmentTaskControllerPurpose::Execute
            || matches!(scope, EnvironmentTaskScope::Run { run_id } if run_id == &self.controller_id)
        {
            Ok(())
        } else {
            Err("task_cleanup_scope_denied".into())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum EnvironmentTaskScope {
    Run {
        #[serde(rename = "runId")]
        run_id: String,
    },
    Environment,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentTaskState {
    Accepted,
    Running,
    Exited,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EnvironmentTaskRequest {
    Takeover {
        environment_instance_id: String,
        controller_id: String,
        controller_epoch: u64,
        controller_purpose: EnvironmentTaskControllerPurpose,
    },
    Start {
        fence: EnvironmentTaskFence,
        operation_id: String,
        scope: EnvironmentTaskScope,
        program: String,
        args: Vec<String>,
        cwd: String,
        timeout_ms: u64,
    },
    Query {
        fence: EnvironmentTaskFence,
        task_id: String,
        cursor: u64,
        max_bytes: u32,
    },
    List {
        fence: EnvironmentTaskFence,
    },
    Input {
        fence: EnvironmentTaskFence,
        task_id: String,
        operation_id: String,
        data: Vec<u8>,
        close: bool,
    },
    Terminate {
        fence: EnvironmentTaskFence,
        task_id: String,
        operation_id: String,
    },
    EndRun {
        fence: EnvironmentTaskFence,
        run_id: String,
        operation_id: String,
    },
}

impl EnvironmentTaskRequest {
    /// Controller purpose and Run ownership are host-independent authority rules.
    pub fn validate_authority(&self) -> Result<(), String> {
        match self {
            Self::Start { fence, scope, .. } => {
                if fence.controller_purpose != EnvironmentTaskControllerPurpose::Execute {
                    return Err("task_cleanup_execute_denied".into());
                }
                if matches!(scope, EnvironmentTaskScope::Run { run_id } if run_id != &fence.controller_id)
                {
                    return Err("task_run_scope_denied".into());
                }
            }
            Self::Input { fence, .. }
                if fence.controller_purpose != EnvironmentTaskControllerPurpose::Execute =>
            {
                return Err("task_cleanup_execute_denied".into())
            }
            Self::EndRun { fence, run_id, .. } if run_id != &fence.controller_id => {
                return Err("task_run_scope_denied".into())
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentTaskSnapshot {
    pub task_id: String,
    pub operation_id: String,
    pub scope: EnvironmentTaskScope,
    pub state: EnvironmentTaskState,
    pub exit_code: Option<i32>,
    pub cleanup_complete: bool,
    pub output_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentTaskStream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentTaskOutputChunk {
    pub cursor: u64,
    pub stream: EnvironmentTaskStream,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EnvironmentTaskResult {
    Listed {
        tasks: Vec<EnvironmentTaskSnapshot>,
    },
    Mutated {
        task: EnvironmentTaskSnapshot,
    },
    TakenOver {
        fence: EnvironmentTaskFence,
    },
    Task {
        task: EnvironmentTaskSnapshot,
        output: Vec<EnvironmentTaskOutputChunk>,
        earliest_cursor: u64,
        next_cursor: u64,
        gap: bool,
        has_more: bool,
    },
    RunEnded {
        task_ids: Vec<String>,
        cleanup_complete: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum EnvironmentTaskResponse {
    Success {
        schema: String,
        result: EnvironmentTaskResult,
    },
    Error {
        schema: String,
        error: String,
    },
}

impl EnvironmentTaskResponse {
    pub fn validate(&self) -> Result<(), String> {
        let schema = match self {
            Self::Success { schema, .. } | Self::Error { schema, .. } => schema,
        };
        if schema == ENVIRONMENT_TASK_SCHEMA {
            Ok(())
        } else {
            Err("task_schema_unsupported".into())
        }
    }
    pub fn from_result(result: Result<EnvironmentTaskResult, String>) -> Self {
        match result {
            Ok(result) => Self::Success {
                schema: ENVIRONMENT_TASK_SCHEMA.into(),
                result,
            },
            Err(error) => Self::Error {
                schema: ENVIRONMENT_TASK_SCHEMA.into(),
                error,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unknown_fields_and_legacy_scope_names() {
        assert!(serde_json::from_str::<EnvironmentTaskRequest>(r#"{"operation":"takeover","environmentInstanceId":"i","controllerId":"c","controllerEpoch":1,"extra":true}"#).is_err());
        assert!(
            serde_json::from_str::<EnvironmentTaskScope>(r#"{"kind":"run","run_id":"r"}"#).is_err()
        );
    }
    #[test]
    fn envelope_requires_supported_schema_and_response_has_exclusive_outcome() {
        let json = r#"{"schema":"obsolete","request":{"operation":"takeover","environmentInstanceId":"i","controllerId":"r","controllerEpoch":1,"controllerPurpose":"execute"}}"#;
        assert!(serde_json::from_str::<EnvironmentTaskEnvelope>(json)
            .unwrap()
            .validate()
            .is_err());
        assert!(serde_json::from_str::<EnvironmentTaskEnvelope>(
            r#"{"operation":"list","fence":{}}"#
        )
        .is_err());
        assert!(serde_json::from_str::<EnvironmentTaskResponse>(r#"{"status":"error","schema":"environment.task.v1","error":"denied","result":{"kind":"runEnded","taskIds":[]}}"#).is_err());
    }
    #[test]
    fn cleanup_authority_cannot_execute_or_terminate_environment_tasks() {
        let fence = EnvironmentTaskFence {
            environment_instance_id: "i".into(),
            controller_id: "run".into(),
            controller_epoch: 2,
            controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
        };
        let request = EnvironmentTaskRequest::Start {
            fence: fence.clone(),
            operation_id: "op".into(),
            scope: EnvironmentTaskScope::Environment,
            program: "sh".into(),
            args: vec![],
            cwd: "/mnt/data".into(),
            timeout_ms: 0,
        };
        assert!(request.validate_authority().is_err());
        assert!(EnvironmentTaskRequest::Input {
            fence: fence.clone(),
            task_id: "task".into(),
            operation_id: "op".into(),
            data: vec![],
            close: true
        }
        .validate_authority()
        .is_err());
        assert!(fence
            .authorize_termination(&EnvironmentTaskScope::Environment)
            .is_err());
        assert!(fence
            .authorize_termination(&EnvironmentTaskScope::Run {
                run_id: "other".into()
            })
            .is_err());
        assert!(fence
            .authorize_termination(&EnvironmentTaskScope::Run {
                run_id: "run".into()
            })
            .is_ok());
        assert!(EnvironmentTaskRequest::EndRun {
            fence,
            run_id: "run".into(),
            operation_id: "op".into()
        }
        .validate_authority()
        .is_ok());
    }
}
