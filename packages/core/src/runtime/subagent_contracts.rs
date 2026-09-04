use crate::runtime::contracts::{JsonMap, TaskStatus, TimestampMs};
use crate::tool::ToolContract;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const AGENT_RUN_CONTEXT_SCHEMA: &str = "agent_run_context_v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentRef {
    pub agent_id: String,
    pub agent_run_id: String,
}

impl AgentRef {
    pub fn new(agent_id: impl Into<String>, agent_run_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            agent_run_id: agent_run_id.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunContext {
    pub schema: String,
    pub session_id: String,
    pub branch_id: String,
    pub turn_id: String,
    pub agent_run_id: String,
    pub agent_ref: AgentRef,
    pub parent_agent_ref: Option<AgentRef>,
    pub parent_turn_id: Option<String>,
    pub depth: u8,
    pub cwd: String,
    pub cancellation_scope_id: String,
    pub parent_cancellation_scope_id: Option<String>,
    pub cancel_on_parent_cancel: bool,
    pub created_at_ms: TimestampMs,
}

impl AgentRunContext {
    pub fn root(
        session_id: impl Into<String>,
        branch_id: impl Into<String>,
        turn_id: impl Into<String>,
        agent_run_id: impl Into<String>,
        agent_id: impl Into<String>,
        cwd: impl Into<String>,
        created_at_ms: TimestampMs,
    ) -> Self {
        let agent_run_id = agent_run_id.into();
        Self {
            schema: AGENT_RUN_CONTEXT_SCHEMA.to_string(),
            session_id: session_id.into(),
            branch_id: branch_id.into(),
            turn_id: turn_id.into(),
            agent_ref: AgentRef::new(agent_id, agent_run_id.clone()),
            agent_run_id: agent_run_id.clone(),
            parent_agent_ref: None,
            parent_turn_id: None,
            depth: 0,
            cwd: cwd.into(),
            cancellation_scope_id: format!("cancel:{agent_run_id}"),
            parent_cancellation_scope_id: None,
            cancel_on_parent_cancel: false,
            created_at_ms,
        }
    }

    pub fn child(
        parent: &AgentRunContext,
        branch_id: impl Into<String>,
        turn_id: impl Into<String>,
        agent_run_id: impl Into<String>,
        agent_id: impl Into<String>,
        created_at_ms: TimestampMs,
    ) -> Self {
        let agent_run_id = agent_run_id.into();
        Self {
            schema: AGENT_RUN_CONTEXT_SCHEMA.to_string(),
            session_id: parent.session_id.clone(),
            branch_id: branch_id.into(),
            turn_id: turn_id.into(),
            agent_ref: AgentRef::new(agent_id, agent_run_id.clone()),
            agent_run_id: agent_run_id.clone(),
            parent_agent_ref: Some(parent.agent_ref.clone()),
            parent_turn_id: Some(parent.turn_id.clone()),
            depth: parent.depth.saturating_add(1),
            cwd: parent.cwd.clone(),
            cancellation_scope_id: format!("cancel:{agent_run_id}"),
            parent_cancellation_scope_id: Some(parent.cancellation_scope_id.clone()),
            cancel_on_parent_cancel: true,
            created_at_ms,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != AGENT_RUN_CONTEXT_SCHEMA {
            return Err(format!("agent_run_context_invalid_schema: {}", self.schema));
        }
        validate_required_id("sessionId", self.session_id.as_str())?;
        validate_required_id("branchId", self.branch_id.as_str())?;
        validate_required_id("turnId", self.turn_id.as_str())?;
        validate_required_id("agentRunId", self.agent_run_id.as_str())?;
        validate_required_id("agentId", self.agent_ref.agent_id.as_str())?;
        validate_required_id("agentRunId", self.agent_ref.agent_run_id.as_str())?;
        if self.agent_ref.agent_run_id != self.agent_run_id {
            return Err("agent_run_context_ref_agent_run_id_mismatch".to_string());
        }
        validate_cwd(self.cwd.as_str())?;
        validate_required_id("cancellationScopeId", self.cancellation_scope_id.as_str())?;
        if self.depth > 0 {
            if self.parent_agent_ref.is_none() {
                return Err("agent_run_context_missing_parent_lineage".to_string());
            }
            let parent_turn_id = self.parent_turn_id.as_deref().unwrap_or_default();
            validate_required_id("parentTurnId", parent_turn_id)?;
            if self.turn_id == parent_turn_id {
                return Err("agent_run_context_child_turn_matches_parent".to_string());
            }
            if self
                .parent_cancellation_scope_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
            {
                return Err("agent_run_context_missing_parent_cancellation_lineage".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContextTransferMode {
    Borrow,
    Snapshot,
    Move,
}

impl ContextTransferMode {
    pub fn requires_owned_snapshot(&self) -> bool {
        matches!(self, Self::Snapshot | Self::Move)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContextRefKind {
    Checkpoint,
    MemorySummary,
    FileBundle,
    Task,
    Artifact,
    HotView,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextRef {
    pub ref_id: String,
    pub kind: ContextRefKind,
    pub object_key: String,
    pub summary: Option<String>,
    pub checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskBrief {
    pub task_id: Option<String>,
    pub objective: String,
    pub success_criteria: Vec<String>,
    pub constraints: Vec<String>,
    pub output_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct HotView {
    pub summary: String,
    pub recent_message_ids: Vec<String>,
    pub state_kv: JsonMap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OutputContract {
    pub response_mode: String,
    pub expected_sections: Vec<String>,
    pub require_artifact_refs: bool,
    pub max_summary_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegatedToolContractV1 {
    pub name: String,
    pub provider_id: String,
    pub contract_digest: String,
    pub concurrency_safe: bool,
}

impl DelegatedToolContractV1 {
    pub fn from_tool_contract(contract: &ToolContract) -> Result<Self, String> {
        Ok(Self {
            name: contract.name.clone(),
            provider_id: contract.provider_id.clone().ok_or_else(|| {
                format!("delegated tool providerId is required: {}", contract.name)
            })?,
            contract_digest: contract.contract_digest()?,
            concurrency_safe: contract.concurrency_safe,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubAgentWorkPacket {
    pub run_context: AgentRunContext,
    pub task_brief: TaskBrief,
    pub hot_view: HotView,
    pub object_refs: Vec<ContextRef>,
    #[serde(default, rename = "allowedTools")]
    pub allowed_tools: Vec<String>,
    #[serde(default, rename = "delegatedToolContracts")]
    pub delegated_tool_contracts: Vec<DelegatedToolContractV1>,
    #[serde(default, rename = "writablePathPrefixes")]
    pub writable_path_prefixes: Vec<String>,
    pub output_contract: OutputContract,
    pub parent_checkpoint_id: Option<String>,
    pub context_mode: ContextTransferMode,
}

impl SubAgentWorkPacket {
    pub fn new(
        run_context: AgentRunContext,
        task_brief: TaskBrief,
        hot_view: HotView,
        output_contract: OutputContract,
        context_mode: ContextTransferMode,
    ) -> Self {
        Self {
            run_context,
            task_brief,
            hot_view,
            object_refs: vec![],
            allowed_tools: vec![],
            delegated_tool_contracts: vec![],
            writable_path_prefixes: vec![],
            output_contract,
            parent_checkpoint_id: None,
            context_mode,
        }
    }

    pub fn validate_for_agent_runtime(&self) -> Result<(), String> {
        self.run_context.validate()?;
        validate_required_id("taskObjective", self.task_brief.objective.as_str())?;
        if self.allowed_tools.is_empty() {
            return Err("subagent_work_packet_missing_allowed_tools".to_string());
        }
        if self.allowed_tools.len() > 62 {
            return Err("subagent_work_packet_too_many_allowed_tools".to_string());
        }
        let mut allowed = HashSet::with_capacity(self.allowed_tools.len());
        for tool in &self.allowed_tools {
            if !is_canonical_tool_name(tool) {
                return Err("subagent_work_packet_invalid_allowed_tool".to_string());
            }
            if matches!(tool.as_str(), "agent" | "task_output") {
                return Err("subagent_work_packet_non_delegatable_tool".to_string());
            }
            if !allowed.insert(tool.as_str()) {
                return Err("subagent_work_packet_duplicate_allowed_tool".to_string());
            }
        }
        if self.delegated_tool_contracts.len() != self.allowed_tools.len() {
            return Err("subagent_work_packet_delegated_tool_contract_mismatch".to_string());
        }
        let mut bound = HashSet::with_capacity(self.delegated_tool_contracts.len());
        for contract in &self.delegated_tool_contracts {
            if !allowed.contains(contract.name.as_str()) || !bound.insert(contract.name.as_str()) {
                return Err("subagent_work_packet_delegated_tool_contract_mismatch".to_string());
            }
            validate_required_id("delegatedTool.providerId", contract.provider_id.as_str())?;
            if !is_sha256_digest(contract.contract_digest.as_str()) {
                return Err("subagent_work_packet_invalid_tool_contract_digest".to_string());
            }
        }
        if self
            .writable_path_prefixes
            .iter()
            .any(|path| path.trim().is_empty())
        {
            return Err("subagent_work_packet_blank_writable_path_prefix".to_string());
        }
        if self.output_contract.response_mode.trim().is_empty() {
            return Err("subagent_work_packet_missing_output_contract_mode".to_string());
        }
        if self.context_mode.requires_owned_snapshot() && self.parent_checkpoint_id.is_none() {
            return Err("subagent_work_packet_missing_parent_checkpoint".to_string());
        }
        Ok(())
    }
}

fn is_canonical_tool_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() <= 128
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.last() != Some(&b'_')
        && !bytes.windows(2).any(|pair| pair == b"__")
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    pub finding_id: String,
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRef {
    pub artifact_id: String,
    pub artifact_type: String,
    pub object_key: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NextAction {
    pub action_type: String,
    pub label: String,
    pub args_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResultEnvelope {
    pub status: TaskStatus,
    pub summary: String,
    pub findings: Vec<Finding>,
    pub produced_refs: Vec<ContextRef>,
    pub artifacts: Vec<ArtifactRef>,
    pub suggested_next_actions: Vec<NextAction>,
}

fn validate_required_id(field_name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!(
            "agent_runtime_sdk_missing_required_field: {field_name}"
        ));
    }
    Ok(())
}

fn validate_cwd(cwd: &str) -> Result<(), String> {
    let trimmed = cwd.trim();
    validate_required_id("cwd", trimmed)?;
    if !std::path::Path::new(trimmed).is_absolute() {
        return Err(format!("agent_runtime_sdk_cwd_must_be_absolute: {trimmed}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AgentRunContext, ContextRef, ContextRefKind, ContextTransferMode, HotView, OutputContract,
        ResultEnvelope, SubAgentWorkPacket, TaskBrief,
    };
    use crate::runtime::contracts::{JsonMap, TaskStatus};

    fn test_parent_run_context() -> AgentRunContext {
        AgentRunContext::root(
            "chat-agent-sdk",
            "turn-parent",
            "turn-parent",
            "agent-run-parent",
            "main-agent",
            std::env::temp_dir().to_string_lossy(),
            100,
        )
    }

    fn test_child_run_context(parent: &AgentRunContext) -> AgentRunContext {
        AgentRunContext::child(
            parent,
            "turn-parent",
            "turn-child",
            "agent-run-child",
            "subagent-child",
            110,
        )
    }

    #[test]
    fn context_transfer_mode_helpers_match_expected_semantics() {
        assert!(ContextTransferMode::Snapshot.requires_owned_snapshot());
        assert!(ContextTransferMode::Move.requires_owned_snapshot());
        assert!(!ContextTransferMode::Borrow.requires_owned_snapshot());
    }

    #[test]
    fn sub_agent_work_packet_captures_minimal_dispatch_contract() {
        let mut hot_view = HotView {
            summary: "current branch is stabilizing query loop".to_string(),
            recent_message_ids: vec!["m1".to_string(), "m2".to_string()],
            state_kv: JsonMap::new(),
        };
        hot_view
            .state_kv
            .insert("activeTask".to_string(), "task-7".to_string());

        let parent_context = test_parent_run_context();
        let child_context = test_child_run_context(&parent_context);
        let mut packet = SubAgentWorkPacket::new(
            child_context,
            TaskBrief {
                task_id: Some("task-7".to_string()),
                objective: "inspect query loop retry path".to_string(),
                success_criteria: vec!["identify blocking branch".to_string()],
                constraints: vec!["do not edit files".to_string()],
                output_hint: Some("return concise findings".to_string()),
            },
            hot_view,
            OutputContract {
                response_mode: "summary".to_string(),
                expected_sections: vec!["findings".to_string()],
                require_artifact_refs: false,
                max_summary_chars: Some(480),
            },
            ContextTransferMode::Borrow,
        );
        packet.parent_checkpoint_id = Some("cp-42".to_string());
        packet.allowed_tools = vec!["read".to_string(), "bash".to_string()];
        packet.delegated_tool_contracts = crate::tool::list_tool_contracts()
            .iter()
            .filter(|contract| packet.allowed_tools.contains(&contract.name))
            .map(|contract| {
                super::DelegatedToolContractV1::from_tool_contract(contract)
                    .expect("delegated contract")
            })
            .collect();
        packet
            .validate_for_agent_runtime()
            .expect("valid work packet");

        packet.object_refs.push(ContextRef {
            ref_id: "ctx-1".to_string(),
            kind: ContextRefKind::Checkpoint,
            object_key: "checkpoint:cp-42".to_string(),
            summary: None,
            checkpoint_id: None,
        });

        assert_eq!(packet.parent_checkpoint_id.as_deref(), Some("cp-42"));
        assert_eq!(packet.object_refs.len(), 1);
        assert_eq!(packet.context_mode, ContextTransferMode::Borrow);
        assert_eq!(packet.allowed_tools, vec!["read", "bash"]);
        assert_eq!(packet.run_context.depth, 1);
        assert_eq!(
            packet.run_context.parent_agent_ref.as_ref(),
            Some(&parent_context.agent_ref)
        );
        assert_ne!(
            packet.run_context.turn_id,
            packet
                .run_context
                .parent_turn_id
                .as_deref()
                .unwrap_or_default()
        );
    }

    #[test]
    fn child_context_keeps_its_own_turn_identity() {
        let parent = test_parent_run_context();
        let mut child = test_child_run_context(&parent);
        child.turn_id = parent.turn_id;

        assert_eq!(
            child.validate().unwrap_err(),
            "agent_run_context_child_turn_matches_parent"
        );
    }

    #[test]
    fn result_envelope_defaults_to_done_and_serializable_shape() {
        let envelope = ResultEnvelope {
            status: TaskStatus::Done,
            summary: "query retry path inspected".to_string(),
            findings: vec![],
            produced_refs: vec![ContextRef {
                ref_id: "ctx-2".to_string(),
                kind: ContextRefKind::Artifact,
                object_key: "artifact:report-1".to_string(),
                summary: None,
                checkpoint_id: None,
            }],
            artifacts: vec![],
            suggested_next_actions: vec![],
        };

        let serialized =
            serde_json::to_string(&envelope).expect("serialize result envelope to json");
        let decoded: ResultEnvelope =
            serde_json::from_str(&serialized).expect("deserialize result envelope from json");

        assert_eq!(decoded.status, TaskStatus::Done);
        assert_eq!(decoded.summary, "query retry path inspected");
        assert_eq!(decoded.produced_refs.len(), 1);
    }
}
