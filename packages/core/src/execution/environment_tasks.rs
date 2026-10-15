//! Persistent environment task contracts. Hosts translate these contracts.
use serde::{Deserialize, Serialize};

pub const ENVIRONMENT_TASK_SCHEMA: &str = "environment.task.v2";
pub const ENVIRONMENT_TASK_MAX_ACTIVE: usize = 32;
pub const ENVIRONMENT_TASK_MAX_RECORDS: usize = 64;
pub const ENVIRONMENT_TASK_OUTPUT_BYTES: usize = 1024 * 1024;
pub const ENVIRONMENT_TASK_READ_BYTES: usize = 64 * 1024;

/// Exact service rejections that prove this individual RPC was not accepted.
/// The service must resolve a prior operation identity before these checks.
pub fn environment_task_capacity_rejection(reason: &str) -> bool {
    matches!(
        reason,
        "task_history_capacity"
            | "task_capacity"
            | "task_thread_capacity"
            | "task_output_capacity"
            | "task_journal_capacity"
            | "task_input_receipt_capacity"
    )
}

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
    /// Create once from the original Run fence. Forwarders must preserve this identity.
    pub fn operation_id(&self, opaque: &str) -> String {
        format!("{}:{opaque}", self.controller_epoch)
    }

    pub fn validate_operation_id(&self, id: &str) -> Result<(), String> {
        let (epoch, opaque) = id.split_once(':').ok_or("task_invalid_operation_id")?;
        let epoch: u64 = epoch.parse().map_err(|_| "task_invalid_operation_id")?;
        if id.len() > 160
            || opaque.is_empty()
            || opaque.len() > 128
            || id.chars().any(char::is_control)
            || epoch == 0
            || !id.starts_with(&format!("{epoch}:"))
        {
            return Err("task_invalid_operation_id".into());
        }
        if epoch < self.controller_epoch {
            return Err("task_operation_replay_expired".into());
        }
        if epoch != self.controller_epoch {
            return Err("task_invalid_operation_id".into());
        }
        Ok(())
    }
    /// A same-Run controller renewal can observe an earlier operation receipt.
    /// This is only a comparison of observations; request validation still rejects
    /// expired epochs before any mutation is accepted.
    pub fn matches_original_operation_id(&self, original: &str, requested: &str) -> bool {
        if self.validate_operation_id(requested).is_err() {
            return false;
        }
        let Some((epoch, opaque)) = original.split_once(':') else {
            return false;
        };
        let Ok(epoch) = epoch.parse::<u64>() else {
            return false;
        };
        if epoch == 0 || epoch > self.controller_epoch {
            return false;
        }
        let original_fence = Self {
            controller_epoch: epoch,
            ..self.clone()
        };
        original_fence.validate_operation_id(original).is_ok()
            && requested
                .split_once(':')
                .is_some_and(|(_, current)| current == opaque)
    }
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
            Self::Start {
                fence,
                operation_id,
                ..
            }
            | Self::Input {
                fence,
                operation_id,
                ..
            }
            | Self::Terminate {
                fence,
                operation_id,
                ..
            }
            | Self::EndRun {
                fence,
                operation_id,
                ..
            } => fence.validate_operation_id(operation_id)?,
            _ => {}
        }
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
    fn capacity_rejection_requires_an_exact_pre_acceptance_service_reason() {
        for reason in [
            "task_capacity",
            "task_history_capacity",
            "task_thread_capacity",
            "task_output_capacity",
            "task_journal_capacity",
            "task_input_receipt_capacity",
        ] {
            assert!(environment_task_capacity_rejection(reason));
            assert!(!environment_task_capacity_rejection(&format!(
                "{reason}: lost acknowledgement"
            )));
            assert!(!environment_task_capacity_rejection(&format!(" {reason}")));
        }
        assert!(!environment_task_capacity_rejection(
            "task_journal_write_failed"
        ));
        assert!(!environment_task_capacity_rejection(
            "task_journal_commit_failed"
        ));
    }

    #[test]
    fn older_task_service_cannot_accept_retirement_protocol_operations() {
        let old = EnvironmentTaskEnvelope {
            schema: "environment.task.v1".into(),
            request: EnvironmentTaskRequest::Takeover {
                environment_instance_id: "instance".into(),
                controller_id: "run".into(),
                controller_epoch: 1,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            },
        };
        assert_eq!(old.validate().unwrap_err(), "task_schema_unsupported");
        assert_eq!(
            EnvironmentTaskResponse::Error {
                schema: "environment.task.v1".into(),
                error: "denied".into()
            }
            .validate()
            .unwrap_err(),
            "task_schema_unsupported"
        );
        let current = EnvironmentTaskEnvelope {
            schema: ENVIRONMENT_TASK_SCHEMA.into(),
            ..old
        };
        current.validate().unwrap();
    }
    #[test]
    fn operation_identity_is_canonical_bounded_and_keeps_original_epoch() {
        let fence = EnvironmentTaskFence {
            environment_instance_id: "i".into(),
            controller_id: "r".into(),
            controller_epoch: u64::MAX,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        };
        let identity = fence.operation_id(&"x".repeat(128));
        fence.validate_operation_id(&identity).unwrap();
        assert!(identity.len() <= 160);
        for invalid in ["legacy", "01:start", "+1:start", "0:start", "1:", "1:bad\n"] {
            assert!(fence.validate_operation_id(invalid).is_err());
        }
        assert!(fence
            .validate_operation_id(&fence.operation_id(&"x".repeat(129)))
            .is_err());
        let future = EnvironmentTaskFence {
            controller_epoch: 2,
            ..fence
        };
        assert_eq!(
            future.validate_operation_id("1:original").unwrap_err(),
            "task_operation_replay_expired"
        );
        assert_eq!(
            future.validate_operation_id("3:future").unwrap_err(),
            "task_invalid_operation_id"
        );
        assert!(future.matches_original_operation_id("1:original", "2:original"));
        assert!(!future.matches_original_operation_id("1:original", "1:original"));
        assert!(!future.matches_original_operation_id("3:original", "2:original"));
        assert!(!future.matches_original_operation_id("1:original", "2:different"));
        assert!(!future.matches_original_operation_id("original", "2:original"));
    }
    #[test]
    fn mutation_replay_from_retired_epoch_is_rejected_with_current_fence() {
        let request = EnvironmentTaskRequest::Start {
            fence: EnvironmentTaskFence {
                environment_instance_id: "i".into(),
                controller_id: "r".into(),
                controller_epoch: 2,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            },
            operation_id: "1:old-start".into(),
            scope: EnvironmentTaskScope::Environment,
            program: "sh".into(),
            args: vec![],
            cwd: "/mnt/data".into(),
            timeout_ms: 0,
        };
        assert_eq!(
            request.validate_authority().unwrap_err(),
            "task_operation_replay_expired"
        );
    }
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
            operation_id: "2:op".into(),
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
            operation_id: "2:op".into(),
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
            operation_id: "2:op".into()
        }
        .validate_authority()
        .is_ok());
    }
}
