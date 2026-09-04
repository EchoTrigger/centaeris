mod bash_deletion;

use std::path::PathBuf;

use crate::tool::canonicalize_tool_name;
use bash_deletion::directly_recursively_deleted_protected_root;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PERMISSION_DECISION_SCHEMA: &str = "permission_decision_v1";
pub const PROTECTED_ROOT_RECURSIVE_DELETE_MESSAGE: &str =
    "Command was not executed: recursive deletion of a protected root is prohibited.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskLevel {
    Safe,
    Restricted,
    HighRisk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDecisionKind {
    Allowed,
    Blocked,
}

impl PermissionDecisionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionNormalizedInput {
    pub tool_name: String,
    pub command_name: Option<String>,
    pub path: Option<String>,
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionDecision {
    pub schema: String,
    pub allowed: bool,
    pub risk_level: RiskLevel,
    pub reason: String,
    pub reason_type: String,
    pub policy_source: String,
    pub normalized_input: PermissionNormalizedInput,
}

impl PermissionDecision {
    pub(crate) fn new(
        allowed: bool,
        risk_level: RiskLevel,
        reason: impl Into<String>,
        reason_type: impl Into<String>,
        policy_source: impl Into<String>,
        normalized_input: PermissionNormalizedInput,
    ) -> Self {
        Self {
            schema: PERMISSION_DECISION_SCHEMA.to_string(),
            allowed,
            risk_level,
            reason: reason.into(),
            reason_type: reason_type.into(),
            policy_source: policy_source.into(),
            normalized_input,
        }
    }

    pub fn decision_kind(&self) -> PermissionDecisionKind {
        if self.allowed {
            PermissionDecisionKind::Allowed
        } else {
            PermissionDecisionKind::Blocked
        }
    }

    pub fn audit_json(&self) -> Value {
        json!({
            "schema": self.schema,
            "reasonType": self.reason_type,
            "policySource": self.policy_source,
            "normalizedInput": self.normalized_input,
            "decision": self.decision_kind().as_str(),
            "allowed": self.allowed,
            "riskLevel": risk_level_to_string(&self.risk_level),
            "reason": self.reason,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPermissionRequest {
    pub tool_name: String,
    pub args_json: Option<String>,
    dynamic_contract: bool,
    bash_cwd: Option<PathBuf>,
}

impl ToolPermissionRequest {
    pub fn new(tool_name: impl Into<String>, args_json: Option<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            args_json,
            dynamic_contract: false,
            bash_cwd: None,
        }
    }

    pub fn with_dynamic_contract(mut self) -> Self {
        self.dynamic_contract = true;
        self
    }

    pub fn with_bash_cwd(mut self, cwd: PathBuf) -> Self {
        self.bash_cwd = Some(cwd);
        self
    }
}

pub fn evaluate_tool_action(request: &ToolPermissionRequest) -> PermissionDecision {
    let canonical = canonicalize_tool_name(request.tool_name.as_str());
    let normalized = canonical.unwrap_or(request.tool_name.as_str()).to_string();
    let normalized_input =
        normalize_permission_input(normalized.as_str(), request.args_json.as_deref());

    if canonical.is_none() && !request.dynamic_contract {
        return PermissionDecision::new(
            false,
            RiskLevel::Restricted,
            format!("unknown tool blocked by runtime contract: {normalized}"),
            "unknown_tool_blocked",
            "core_tool_policy",
            normalized_input,
        );
    }

    if normalized == "bash" {
        if let Some(decision) = evaluate_bash_protected_root_deletion(
            request.args_json.as_deref(),
            request.bash_cwd.as_deref(),
            normalized_input.clone(),
        ) {
            return decision;
        }
    }

    PermissionDecision::new(
        true,
        if normalized == "bash" {
            RiskLevel::HighRisk
        } else {
            RiskLevel::Safe
        },
        "tool allowed by runtime contract",
        if request.dynamic_contract {
            "dynamic_tool_contract_allow"
        } else {
            "static_tool_default_allow"
        },
        "core_tool_policy",
        normalized_input,
    )
}

fn evaluate_bash_protected_root_deletion(
    args_json: Option<&str>,
    execution_cwd: Option<&std::path::Path>,
    mut normalized_input: PermissionNormalizedInput,
) -> Option<PermissionDecision> {
    let args = parse_args_json(args_json)?;
    let command = args.get("command")?.as_str()?;
    let command_cwd = args.get("cwd").and_then(Value::as_str);
    let protected_root = match directly_recursively_deleted_protected_root(
        command,
        command_cwd,
        execution_cwd,
    ) {
        Ok(root) => root?,
        Err(error) => {
            return Some(PermissionDecision::new(
                false,
                RiskLevel::HighRisk,
                format!(
                    "Command was not executed: protected-root deletion safety inspection failed: {error}"
                ),
                "bash_deletion_inspection_failed",
                "core_tool_policy",
                normalized_input,
            ));
        }
    };
    normalized_input.path = Some(protected_root.logical_path().to_string());
    Some(PermissionDecision::new(
        false,
        RiskLevel::HighRisk,
        PROTECTED_ROOT_RECURSIVE_DELETE_MESSAGE,
        "bash_recursive_delete_protected_root",
        "core_tool_policy",
        normalized_input,
    ))
}

fn normalize_permission_input(
    tool_name: &str,
    args_json: Option<&str>,
) -> PermissionNormalizedInput {
    let parsed = parse_args_json(args_json);
    PermissionNormalizedInput {
        tool_name: tool_name.to_string(),
        command_name: parsed
            .as_ref()
            .and_then(|item| item.get("command").and_then(Value::as_str))
            .and_then(first_bash_word),
        path: parsed
            .as_ref()
            .and_then(|item| item.get("path").and_then(Value::as_str))
            .map(|item| item.trim().replace('\\', "/"))
            .filter(|item| !item.is_empty()),
        task_id: parsed
            .as_ref()
            .and_then(|item| item.get("task_id").and_then(Value::as_str))
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string),
    }
}

fn first_bash_word(command: &str) -> Option<String> {
    command
        .split_whitespace()
        .next()
        .map(|item| item.trim_matches(['"', '\'']).to_string())
        .filter(|item| !item.is_empty())
}

fn risk_level_to_string(risk_level: &RiskLevel) -> &'static str {
    match risk_level {
        RiskLevel::Safe => "safe",
        RiskLevel::Restricted => "restricted",
        RiskLevel::HighRisk => "high_risk",
    }
}

fn parse_args_json(args_json: Option<&str>) -> Option<Value> {
    let raw = args_json?.trim();
    if raw.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(raw).ok()
}

#[cfg(test)]
mod tests {
    use super::{
        evaluate_tool_action, PermissionDecisionKind, RiskLevel, ToolPermissionRequest,
        PROTECTED_ROOT_RECURSIVE_DELETE_MESSAGE,
    };

    #[test]
    fn ordinary_tools_are_allowed_and_unknown_tools_are_blocked() {
        assert!(evaluate_tool_action(&ToolPermissionRequest::new("read", None)).allowed);
        assert!(!evaluate_tool_action(&ToolPermissionRequest::new("unknown", None)).allowed);
        assert!(
            evaluate_tool_action(
                &ToolPermissionRequest::new("plugin_tool", None).with_dynamic_contract()
            )
            .allowed
        );
    }

    #[test]
    fn bash_is_allowed_without_an_enforced_sandbox() {
        let decision = evaluate_tool_action(&ToolPermissionRequest::new("bash", None));
        assert_eq!(decision.decision_kind(), PermissionDecisionKind::Allowed);
        assert_eq!(decision.reason_type, "static_tool_default_allow");
        assert_eq!(decision.risk_level, RiskLevel::HighRisk);
    }

    #[test]
    fn direct_recursive_delete_of_protected_root_is_blocked() {
        for (command, protected_path) in [
            ("rm -rf /", "/"),
            ("rm -rf /workspace/*", "$CWD"),
            ("rm --recursive \"$HOME\"", "$HOME"),
            ("rm -rf \"$HOME/.centaeris\"", "$HOME/.centaeris"),
        ] {
            let decision = evaluate_tool_action(
                &ToolPermissionRequest::new(
                    "bash",
                    Some(serde_json::json!({ "command": command }).to_string()),
                )
                .with_bash_cwd(std::path::PathBuf::from("/workspace")),
            );
            assert_eq!(
                decision.decision_kind(),
                PermissionDecisionKind::Blocked,
                "{command}"
            );
            assert_eq!(decision.risk_level, RiskLevel::HighRisk);
            assert_eq!(decision.reason, PROTECTED_ROOT_RECURSIVE_DELETE_MESSAGE);
            assert_eq!(
                decision.normalized_input.path.as_deref(),
                Some(protected_path)
            );
        }
    }

    #[test]
    fn workspace_metadata_remains_a_normal_scoped_target() {
        let decision = evaluate_tool_action(
            &ToolPermissionRequest::new(
                "bash",
                Some(serde_json::json!({ "command": "rm -rf .centaeris" }).to_string()),
            )
            .with_bash_cwd(std::path::PathBuf::from("/workspace")),
        );
        assert!(decision.allowed);
    }
}
