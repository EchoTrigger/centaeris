//! Persistent environment task contracts. Hosts translate these contracts.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ENVIRONMENT_TASK_SCHEMA: &str = "environment.task.v3";
pub const ENVIRONMENT_TASK_MAX_ACTIVE: usize = 32;
pub const ENVIRONMENT_TASK_MAX_RECORDS: usize = 64;
pub const ENVIRONMENT_TASK_OUTPUT_BYTES: usize = 1024 * 1024;
pub const ENVIRONMENT_TASK_READ_BYTES: usize = 64 * 1024;

/// Identity of one original stdin mutation, including its EOF flag.
pub fn environment_task_input_digest(data: &[u8], close: bool) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(data, close)).map_err(|_| "task_encoding_failed")?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

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
    pub controller_owner: EnvironmentTaskOwner,
    pub controller_epoch: u64,
    pub controller_purpose: EnvironmentTaskControllerPurpose,
}

/// Authority identifies a real Run or a separately authorized maintenance operation.
/// Hosts verify this identity against their authoritative controller record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EnvironmentTaskOwner {
    AgentRun { run_id: String },
    MaintenanceOperation { operation_id: String },
}

impl EnvironmentTaskOwner {
    pub fn id(&self) -> &str {
        match self {
            Self::AgentRun { run_id } => run_id,
            Self::MaintenanceOperation { operation_id } => operation_id,
        }
    }

    fn validate(&self) -> Result<(), String> {
        let id = self.id();
        if id.is_empty() || id.len() > 160 || id.chars().any(char::is_control) {
            return Err("task_invalid_controller_owner".into());
        }
        Ok(())
    }

    fn owns_scope(&self, scope: &EnvironmentTaskScope) -> bool {
        match (self, scope) {
            (Self::AgentRun { run_id: owner }, EnvironmentTaskScope::Run { run_id }) => {
                owner == run_id
            }
            (
                Self::MaintenanceOperation {
                    operation_id: owner,
                },
                EnvironmentTaskScope::Operation { operation_id },
            ) => owner == operation_id,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentTaskControllerPurpose {
    Execute,
    Cleanup,
}

impl EnvironmentTaskFence {
    pub fn validate(&self) -> Result<(), String> {
        self.controller_owner.validate()?;
        if self.environment_instance_id.is_empty()
            || self.environment_instance_id.len() > 128
            || self.environment_instance_id.chars().any(char::is_control)
            || self.controller_epoch == 0
        {
            return Err("task_invalid_fence".into());
        }
        Ok(())
    }

    /// Resolve the stable task address across renewals of the same typed owner.
    /// This derives an address only; a query must still check the current fence
    /// and the returned original operation identity.
    pub fn task_id(&self, operation_id: &str) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        self.validate_operation_id(operation_id)?;
        let (_, opaque) = operation_id
            .split_once(':')
            .ok_or("task_invalid_operation_id")?;
        let encoded = serde_json::to_vec(&(
            "centaeris:environment-task-identity:v3",
            &self.environment_instance_id,
            &self.controller_owner,
            opaque,
        ))
        .map_err(|_| "task_encoding_failed")?;
        Ok(format!("task-{:x}", Sha256::digest(encoded)))
    }

    /// Create once from the original owner fence. Forwarders must preserve this identity.
    pub fn operation_id(&self, opaque: &str) -> String {
        format!("{}:{opaque}", self.controller_epoch)
    }

    pub fn validate_operation_id(&self, id: &str) -> Result<(), String> {
        self.validate()?;
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
    /// A same-owner controller renewal can observe an earlier operation receipt.
    /// This is only a comparison of observations; request validation still rejects
    /// expired epochs before any mutation is accepted. Callers must also compare
    /// the owner-scoped task address and immutable task scope.
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

    /// Compare an observed Start receipt without reinterpreting its owner or scope.
    /// This observation check grants no execution or mutation authority.
    pub fn matches_start_receipt(
        &self,
        task: &EnvironmentTaskSnapshot,
        operation_id: &str,
        scope: &EnvironmentTaskScope,
    ) -> bool {
        self.task_id(operation_id)
            .is_ok_and(|id| id == task.task_id)
            && self.matches_original_operation_id(&task.operation_id, operation_id)
            && task.scope == *scope
            && (*scope == EnvironmentTaskScope::Environment
                || self.controller_owner.owns_scope(scope))
    }

    pub fn authorize_start(&self, scope: &EnvironmentTaskScope) -> Result<(), String> {
        self.authorize_execution()?;
        self.authorize_execution_scope(scope)
    }

    /// Input requests carry no scope. A host must check the loaded task's scope
    /// with this method before writing bytes or closing its input.
    pub fn authorize_input(&self, scope: &EnvironmentTaskScope) -> Result<(), String> {
        self.authorize_execution()?;
        self.authorize_execution_scope(scope)
    }

    fn authorize_execution(&self) -> Result<(), String> {
        self.validate()?;
        if self.controller_purpose != EnvironmentTaskControllerPurpose::Execute {
            return Err("task_cleanup_execute_denied".into());
        }
        Ok(())
    }

    fn authorize_execution_scope(&self, scope: &EnvironmentTaskScope) -> Result<(), String> {
        if *scope == EnvironmentTaskScope::Environment || self.controller_owner.owns_scope(scope) {
            return Ok(());
        }
        Err("task_owner_scope_denied".into())
    }

    pub fn authorize_termination(&self, scope: &EnvironmentTaskScope) -> Result<(), String> {
        self.validate()?;
        if self.controller_purpose == EnvironmentTaskControllerPurpose::Execute {
            return self.authorize_execution_scope(scope);
        }
        if self.controller_owner.owns_scope(scope) {
            return Ok(());
        }
        Err("task_cleanup_scope_denied".into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum EnvironmentTaskScope {
    Run {
        #[serde(rename = "runId")]
        run_id: String,
    },
    Operation {
        #[serde(rename = "operationId")]
        operation_id: String,
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
        controller_owner: EnvironmentTaskOwner,
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
    QueryInputReceipt {
        fence: EnvironmentTaskFence,
        task_id: String,
        operation_id: String,
        request_digest: String,
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
    EndOperation {
        fence: EnvironmentTaskFence,
        owner_operation_id: String,
        operation_id: String,
    },
}

impl EnvironmentTaskRequest {
    /// Owner kind, controller purpose and task scope are host-independent authority rules.
    pub fn validate_authority(&self) -> Result<(), String> {
        match self {
            Self::Takeover {
                environment_instance_id,
                controller_owner,
                controller_epoch,
                controller_purpose,
            } => EnvironmentTaskFence {
                environment_instance_id: environment_instance_id.clone(),
                controller_owner: controller_owner.clone(),
                controller_epoch: *controller_epoch,
                controller_purpose: controller_purpose.clone(),
            }
            .validate()?,
            Self::Start { fence, .. }
            | Self::Query { fence, .. }
            | Self::List { fence }
            | Self::Input { fence, .. }
            | Self::QueryInputReceipt { fence, .. }
            | Self::Terminate { fence, .. }
            | Self::EndRun { fence, .. }
            | Self::EndOperation { fence, .. } => fence.validate()?,
        }
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
            | Self::QueryInputReceipt {
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
            }
            | Self::EndOperation {
                fence,
                operation_id,
                ..
            } => fence.validate_operation_id(operation_id)?,
            _ => {}
        }
        match self {
            Self::Start { fence, scope, .. } => {
                fence.authorize_start(scope)?;
            }
            Self::Input { fence, .. } | Self::QueryInputReceipt { fence, .. } => {
                fence.authorize_execution()?;
            }
            Self::EndRun { fence, run_id, .. } => {
                if !fence
                    .controller_owner
                    .owns_scope(&EnvironmentTaskScope::Run {
                        run_id: run_id.clone(),
                    })
                {
                    return Err("task_run_scope_denied".into());
                }
            }
            Self::EndOperation {
                fence,
                owner_operation_id,
                ..
            } => {
                if !fence
                    .controller_owner
                    .owns_scope(&EnvironmentTaskScope::Operation {
                        operation_id: owner_operation_id.clone(),
                    })
                {
                    return Err("task_operation_scope_denied".into());
                }
            }
            _ => {}
        }
        if let Self::QueryInputReceipt { request_digest, .. } = self {
            if request_digest.len() != 64
                || !request_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err("task_invalid_input_digest".into());
            }
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
    OperationEnded {
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

    fn owner_fence(kind: &str, id: &str, epoch: u64, purpose: &str) -> serde_json::Value {
        let owner = match kind {
            "agentRun" => serde_json::json!({"kind": kind, "runId": id}),
            "maintenanceOperation" => serde_json::json!({"kind": kind, "operationId": id}),
            _ => panic!("invalid fixture owner"),
        };
        serde_json::json!({
            "environmentInstanceId": "instance",
            "controllerOwner": owner,
            "controllerEpoch": epoch,
            "controllerPurpose": purpose
        })
    }

    fn owner_start(fence: serde_json::Value, scope: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "operation": "start", "fence": fence, "operationId": "2:start",
            "scope": scope, "program": "/app/tool", "args": [], "cwd": "/app",
            "timeoutMs": 30_000
        })
    }

    #[test]
    fn maintenance_owner_starts_an_independent_operation_without_an_agent_run() {
        let value = owner_start(
            owner_fence("maintenanceOperation", "prepare", 2, "execute"),
            serde_json::json!({"kind": "operation", "operationId": "prepare"}),
        );
        let request: EnvironmentTaskRequest = serde_json::from_value(value.clone())
            .expect("maintenance execution must have its own typed owner");
        request.validate_authority().unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
    }

    #[test]
    fn maintenance_owner_cannot_start_or_close_a_run_with_the_same_textual_id() {
        for value in [
            owner_start(
                owner_fence("maintenanceOperation", "same", 2, "execute"),
                serde_json::json!({"kind": "run", "runId": "same"}),
            ),
            serde_json::json!({
                "operation": "endRun",
                "fence": owner_fence("maintenanceOperation", "same", 2, "cleanup"),
                "runId": "same", "operationId": "2:close"
            }),
        ] {
            let request: EnvironmentTaskRequest = serde_json::from_value(value)
                .expect("a typed maintenance owner must be understood before authorization");
            assert!(request.validate_authority().is_err());
        }
    }

    #[test]
    fn operation_scope_and_close_require_the_exact_maintenance_owner() {
        for (kind, id, allowed) in [
            ("maintenanceOperation", "prepare", true),
            ("maintenanceOperation", "other", false),
            ("agentRun", "prepare", false),
        ] {
            for value in [
                owner_start(
                    owner_fence(kind, id, 2, "execute"),
                    serde_json::json!({"kind": "operation", "operationId": "prepare"}),
                ),
                serde_json::json!({
                    "operation": "endOperation", "fence": owner_fence(kind, id, 2, "cleanup"),
                    "ownerOperationId": "prepare", "operationId": "2:close"
                }),
            ] {
                let request: EnvironmentTaskRequest = serde_json::from_value(value.clone())
                    .expect("operation lifetime must not be represented as an AgentRun");
                assert_eq!(request.validate_authority().is_ok(), allowed);
                assert_eq!(serde_json::to_value(request).unwrap(), value);
            }
        }
    }

    #[test]
    fn cleanup_owners_cannot_execute_or_write_input_for_either_owner_kind() {
        for kind in ["agentRun", "maintenanceOperation"] {
            let scope = if kind == "agentRun" {
                serde_json::json!({"kind": "run", "runId": "owner"})
            } else {
                serde_json::json!({"kind": "operation", "operationId": "owner"})
            };
            for value in [
                owner_start(owner_fence(kind, "owner", 2, "cleanup"), scope),
                serde_json::json!({
                    "operation": "input", "fence": owner_fence(kind, "owner", 2, "cleanup"),
                    "taskId": "task", "operationId": "2:input", "data": [1], "close": false
                }),
            ] {
                let request: EnvironmentTaskRequest = serde_json::from_value(value).unwrap();
                assert_eq!(
                    request.validate_authority().unwrap_err(),
                    "task_cleanup_execute_denied"
                );
            }
        }
    }

    #[test]
    fn typed_owner_separates_task_addresses_and_preserves_same_owner_renewal() {
        let run: EnvironmentTaskFence =
            serde_json::from_value(owner_fence("agentRun", "same", 2, "execute")).unwrap();
        let maintenance: EnvironmentTaskFence =
            serde_json::from_value(owner_fence("maintenanceOperation", "same", 2, "execute"))
                .unwrap();
        assert_ne!(
            run.task_id("2:start").unwrap(),
            maintenance.task_id("2:start").unwrap()
        );
        for original in [run, maintenance] {
            let renewed = EnvironmentTaskFence {
                controller_epoch: 3,
                ..original.clone()
            };
            assert_eq!(
                original.task_id("2:start").unwrap(),
                renewed.task_id("3:start").unwrap()
            );
            assert!(renewed.matches_original_operation_id("2:start", "3:start"));
            assert_eq!(
                renewed.validate_operation_id("2:start").unwrap_err(),
                "task_operation_replay_expired"
            );
        }
    }

    #[test]
    fn task_mutations_require_the_stored_scope_and_exact_typed_owner() {
        for kind in ["agentRun", "maintenanceOperation"] {
            for purpose in ["execute", "cleanup"] {
                let fence: EnvironmentTaskFence =
                    serde_json::from_value(owner_fence(kind, "same", 2, purpose)).unwrap();
                for (scope, owned) in [
                    (
                        EnvironmentTaskScope::Run {
                            run_id: "same".into(),
                        },
                        kind == "agentRun",
                    ),
                    (
                        EnvironmentTaskScope::Operation {
                            operation_id: "same".into(),
                        },
                        kind == "maintenanceOperation",
                    ),
                    (
                        EnvironmentTaskScope::Run {
                            run_id: "other".into(),
                        },
                        false,
                    ),
                    (
                        EnvironmentTaskScope::Operation {
                            operation_id: "other".into(),
                        },
                        false,
                    ),
                    (EnvironmentTaskScope::Environment, false),
                ] {
                    let execute_scope = purpose == "execute"
                        && (owned || scope == EnvironmentTaskScope::Environment);
                    assert_eq!(fence.authorize_start(&scope).is_ok(), execute_scope);
                    assert_eq!(fence.authorize_input(&scope).is_ok(), execute_scope);
                    assert_eq!(
                        fence.authorize_termination(&scope).is_ok(),
                        owned || execute_scope
                    );
                }
            }
        }
    }

    #[test]
    fn start_receipt_preserves_owner_instance_operation_and_immutable_scope() {
        let original: EnvironmentTaskFence =
            serde_json::from_value(owner_fence("agentRun", "same", 2, "execute")).unwrap();
        let scope = EnvironmentTaskScope::Run {
            run_id: "same".into(),
        };
        let task = EnvironmentTaskSnapshot {
            task_id: original.task_id("2:start").unwrap(),
            operation_id: "2:start".into(),
            scope: scope.clone(),
            state: EnvironmentTaskState::Running,
            exit_code: None,
            cleanup_complete: false,
            output_complete: false,
        };
        let renewed = EnvironmentTaskFence {
            controller_epoch: 3,
            ..original.clone()
        };
        assert!(renewed.matches_start_receipt(&task, "3:start", &scope));
        assert!(!renewed.matches_start_receipt(&task, "3:other", &scope));
        assert!(!renewed.matches_start_receipt(
            &task,
            "3:start",
            &EnvironmentTaskScope::Environment
        ));
        let changed_scope = EnvironmentTaskSnapshot {
            scope: EnvironmentTaskScope::Environment,
            ..task.clone()
        };
        assert!(!renewed.matches_start_receipt(&changed_scope, "3:start", &scope));
        let maintenance: EnvironmentTaskFence =
            serde_json::from_value(owner_fence("maintenanceOperation", "same", 3, "execute"))
                .unwrap();
        assert!(!maintenance.matches_start_receipt(&task, "3:start", &scope));
        let other_instance = EnvironmentTaskFence {
            environment_instance_id: "other".into(),
            ..renewed.clone()
        };
        assert!(!other_instance.matches_start_receipt(&task, "3:start", &scope));
        let future_operation = EnvironmentTaskSnapshot {
            operation_id: "4:start".into(),
            ..task
        };
        assert!(!renewed.matches_start_receipt(&future_operation, "3:start", &scope));
    }

    #[test]
    fn owner_and_epoch_are_validated_for_read_only_requests_and_takeover() {
        for (kind, id, epoch) in [
            ("agentRun", "", 2),
            ("maintenanceOperation", "bad\nowner", 2),
            ("maintenanceOperation", "prepare", 0),
        ] {
            let fence = owner_fence(kind, id, epoch, "execute");
            for value in [
                serde_json::json!({"operation": "list", "fence": fence}),
                serde_json::json!({
                    "operation": "takeover", "environmentInstanceId": "instance",
                    "controllerOwner": fence["controllerOwner"], "controllerEpoch": epoch,
                    "controllerPurpose": "execute"
                }),
            ] {
                let request: EnvironmentTaskRequest = serde_json::from_value(value).unwrap();
                assert!(request.validate_authority().is_err());
            }
        }
    }

    #[test]
    fn owner_contract_requires_v3_and_rejects_ambiguous_owner_or_scope_fields() {
        assert_eq!(ENVIRONMENT_TASK_SCHEMA, "environment.task.v3");
        let mut fence = owner_fence("maintenanceOperation", "prepare", 2, "execute");
        fence["controllerId"] = serde_json::json!("prepare");
        assert!(serde_json::from_value::<EnvironmentTaskFence>(fence).is_err());
        let mut fence = owner_fence("maintenanceOperation", "prepare", 2, "execute");
        fence["controllerOwner"]["runId"] = serde_json::json!("prepare");
        assert!(serde_json::from_value::<EnvironmentTaskFence>(fence).is_err());
        assert!(
            serde_json::from_value::<EnvironmentTaskScope>(serde_json::json!({
                "kind": "operation", "operationId": "prepare", "runId": "prepare"
            }))
            .is_err()
        );
        let legacy = serde_json::json!({
            "schema": "environment.task.v2",
            "request": {"operation": "takeover", "environmentInstanceId": "instance",
                "controllerId": "run", "controllerEpoch": 2, "controllerPurpose": "execute"}
        });
        assert!(
            serde_json::from_value::<EnvironmentTaskEnvelope>(legacy)
                .map_or(true, |envelope| envelope.validate().is_err())
        );
    }

    #[test]
    fn input_receipt_query_has_exact_read_only_identity() {
        let value = serde_json::json!({
            "operation":"queryInputReceipt",
            "fence":{"environmentInstanceId":"i","controllerOwner":{"kind":"agentRun","runId":"run"},"controllerEpoch":2,"controllerPurpose":"execute"},
            "taskId":"task-original","operationId":"2:input-original",
            "requestDigest":"a".repeat(64)
        });
        let request: EnvironmentTaskRequest = serde_json::from_value(value.clone())
            .expect("the original input receipt must be queryable without sending its bytes again");
        request.validate_authority().unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
        let mut invalid = value.clone();
        invalid["data"] = serde_json::json!([1, 2, 3]);
        assert!(serde_json::from_value::<EnvironmentTaskRequest>(invalid).is_err());
        let mut cleanup = value;
        cleanup["fence"]["controllerPurpose"] = serde_json::json!("cleanup");
        assert_eq!(
            serde_json::from_value::<EnvironmentTaskRequest>(cleanup)
                .unwrap()
                .validate_authority()
                .unwrap_err(),
            "task_cleanup_execute_denied"
        );
    }
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
    fn older_task_service_cannot_accept_current_owner_protocol() {
        let old = EnvironmentTaskEnvelope {
            schema: "environment.task.v1".into(),
            request: EnvironmentTaskRequest::Takeover {
                environment_instance_id: "instance".into(),
                controller_owner: EnvironmentTaskOwner::AgentRun {
                    run_id: "run".into(),
                },
                controller_epoch: 1,
                controller_purpose: EnvironmentTaskControllerPurpose::Execute,
            },
        };
        for schema in ["environment.task.v1", "environment.task.v2"] {
            let old = EnvironmentTaskEnvelope {
                schema: schema.into(),
                ..old.clone()
            };
            assert_eq!(old.validate().unwrap_err(), "task_schema_unsupported");
            assert_eq!(
                EnvironmentTaskResponse::Error {
                    schema: schema.into(),
                    error: "denied".into()
                }
                .validate()
                .unwrap_err(),
                "task_schema_unsupported"
            );
        }
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
            controller_owner: EnvironmentTaskOwner::AgentRun { run_id: "r".into() },
            controller_epoch: u64::MAX,
            controller_purpose: EnvironmentTaskControllerPurpose::Execute,
        };
        let identity = fence.operation_id(&"x".repeat(128));
        fence.validate_operation_id(&identity).unwrap();
        assert!(identity.len() <= 160);
        for invalid in ["legacy", "01:start", "+1:start", "0:start", "1:", "1:bad\n"] {
            assert!(fence.validate_operation_id(invalid).is_err());
        }
        assert!(
            fence
                .validate_operation_id(&fence.operation_id(&"x".repeat(129)))
                .is_err()
        );
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
                controller_owner: EnvironmentTaskOwner::AgentRun { run_id: "r".into() },
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
        let json = r#"{"schema":"obsolete","request":{"operation":"takeover","environmentInstanceId":"i","controllerOwner":{"kind":"agentRun","runId":"r"},"controllerEpoch":1,"controllerPurpose":"execute"}}"#;
        assert!(
            serde_json::from_str::<EnvironmentTaskEnvelope>(json)
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(
            serde_json::from_str::<EnvironmentTaskEnvelope>(r#"{"operation":"list","fence":{}}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<EnvironmentTaskResponse>(r#"{"status":"error","schema":"environment.task.v1","error":"denied","result":{"kind":"runEnded","taskIds":[]}}"#).is_err());
    }
    #[test]
    fn cleanup_authority_cannot_execute_or_terminate_environment_tasks() {
        let fence = EnvironmentTaskFence {
            environment_instance_id: "i".into(),
            controller_owner: EnvironmentTaskOwner::AgentRun {
                run_id: "run".into(),
            },
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
        assert!(
            EnvironmentTaskRequest::Input {
                fence: fence.clone(),
                task_id: "task".into(),
                operation_id: "2:op".into(),
                data: vec![],
                close: true
            }
            .validate_authority()
            .is_err()
        );
        assert!(
            fence
                .authorize_termination(&EnvironmentTaskScope::Environment)
                .is_err()
        );
        assert!(
            fence
                .authorize_termination(&EnvironmentTaskScope::Run {
                    run_id: "other".into()
                })
                .is_err()
        );
        assert!(
            fence
                .authorize_termination(&EnvironmentTaskScope::Run {
                    run_id: "run".into()
                })
                .is_ok()
        );
        assert!(
            EnvironmentTaskRequest::EndRun {
                fence,
                run_id: "run".into(),
                operation_id: "2:op".into()
            }
            .validate_authority()
            .is_ok()
        );
    }
}
