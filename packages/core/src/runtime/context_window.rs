use serde_json::{json, Value};

use crate::runtime::keys::metadata as runtime_metadata_keys;
use crate::session::state::{
    ChatMessage, MessageRole, ModelMessageSemanticsV1, SessionStateSnapshot,
};

const MATERIALIZATION_SCHEMA: &str = "context_window_materialization_v1";
const STRATEGY: &str = "active_compaction_projection";
const CONTEXT_COMPACTION_KIND: &str = "context_compaction";
const PROMPT_COMPACTION_USER_REPLAY_KIND: &str = "prompt_compaction_user_replay";
pub(crate) const LIFECYCLE_HOOK_CONTEXT_META_KEY: &str = "lifecycle_hook_context_v1";
const FIRST_KEPT_MESSAGE_ID: &str = "first_kept_message_id";
const COMPACTION_ID: &str = "compaction_id";

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaterializationDiagnostic {
    reason: &'static str,
    message_id: String,
    message_index: usize,
}

#[derive(Debug, Clone)]
pub struct ContextWindowMaterialization {
    pub messages: Vec<ChatMessage>,
    diagnostics: Vec<MaterializationDiagnostic>,
    source_message_count: usize,
}

pub fn refresh_session_context_window(session: &mut SessionStateSnapshot) {
    let materialized = materialize_context_window(session.messages.as_slice());
    let metadata_json = materialized.metadata_json();
    session.context_window = materialized.messages;
    session.metadata.insert(
        runtime_metadata_keys::CONTEXT_WINDOW_MATERIALIZATION.to_string(),
        metadata_json,
    );
}

pub fn materialize_context_window(messages: &[ChatMessage]) -> ContextWindowMaterialization {
    let (active_messages, diagnostics) = active_projection_messages(messages);
    ContextWindowMaterialization {
        messages: active_messages,
        diagnostics,
        source_message_count: messages.len(),
    }
}

fn active_projection_messages(
    messages: &[ChatMessage],
) -> (Vec<ChatMessage>, Vec<MaterializationDiagnostic>) {
    let Some((summary_index, summary)) = messages
        .iter()
        .enumerate()
        .rfind(|(_, message)| is_context_compaction_summary(message))
    else {
        return (messages.to_vec(), Vec::new());
    };
    let mut diagnostics = Vec::new();
    let suffix_start = match summary.metadata.get(FIRST_KEPT_MESSAGE_ID) {
        Some(first_kept_message_id) => match messages
            .iter()
            .position(|message| message.message_id == *first_kept_message_id)
        {
            Some(index) => index,
            None => {
                diagnostics.push(MaterializationDiagnostic {
                    reason: "context_compaction_first_kept_message_missing",
                    message_id: summary.message_id.clone(),
                    message_index: summary_index,
                });
                summary_index.saturating_add(1)
            }
        },
        None => summary_index.saturating_add(1),
    };
    let compaction_id = summary.metadata.get(COMPACTION_ID).map(String::as_str);
    let mut active_messages = vec![summary.clone()];
    active_messages.extend(
        messages
            .iter()
            .filter(|message| {
                is_prompt_compaction_user_replay(message)
                    && message.metadata.get(COMPACTION_ID).map(String::as_str) == compaction_id
            })
            .cloned(),
    );
    active_messages.extend(
        messages[suffix_start..]
            .iter()
            .filter(|message| {
                !is_context_compaction_summary(message)
                    && !is_prompt_compaction_user_replay(message)
            })
            .cloned(),
    );
    (active_messages, diagnostics)
}

fn is_prompt_compaction_user_replay(message: &ChatMessage) -> bool {
    message.role == MessageRole::User
        && message.metadata.get("kind").map(String::as_str)
            == Some(PROMPT_COMPACTION_USER_REPLAY_KIND)
}

fn is_context_compaction_summary(message: &ChatMessage) -> bool {
    message.role == MessageRole::System
        && message.metadata.get("kind").map(String::as_str) == Some(CONTEXT_COMPACTION_KIND)
}

pub(crate) fn is_lifecycle_hook_context(message: &ChatMessage) -> bool {
    message.role == MessageRole::System
        && message
            .metadata
            .get(LIFECYCLE_HOOK_CONTEXT_META_KEY)
            .map(String::as_str)
            == Some("true")
}

pub(crate) fn is_reliable_tool_chain_user_anchor(message: &ChatMessage) -> bool {
    message.role == MessageRole::User
        && message.metadata.get("kind").map(String::as_str)
            != Some(PROMPT_COMPACTION_USER_REPLAY_KIND)
        && matches!(
            message
                .metadata
                .get(crate::runtime::keys::metadata::MESSAGE_SEMANTIC_KIND)
                .map(String::as_str),
            Some(
                super::MESSAGE_SEMANTIC_USER_REQUEST
                    | super::MESSAGE_SEMANTIC_TURN_SUPPLEMENT
                    | super::MESSAGE_SEMANTIC_ANSWER_NOW
            )
        )
}

pub fn validate_model_context_window(
    messages: &[ChatMessage],
    model_semantics: &std::collections::BTreeMap<String, ModelMessageSemanticsV1>,
) -> Result<(), String> {
    let mut pending_tool_call_ids = Vec::<String>::new();
    for message in messages {
        if !pending_tool_call_ids.is_empty() && message.role != MessageRole::Tool {
            return Err(format!(
                "context_window_materialization_invalid_tool_pairing: assistant tool call {} must be followed by tool result before message {}",
                pending_tool_call_ids[0], message.message_id
            ));
        }

        match message.role {
            MessageRole::Assistant => {
                pending_tool_call_ids.extend(assistant_tool_call_ids(message, model_semantics)?);
            }
            MessageRole::Tool => {
                let tool_call_id = tool_result_call_id(message, model_semantics)?;
                let expected = pending_tool_call_ids.first().ok_or_else(|| {
                    format!(
                        "context_window_materialization_invalid_tool_pairing: tool message {} has no preceding assistant tool call",
                        message.message_id
                    )
                })?;
                if expected != &tool_call_id {
                    return Err(format!(
                        "context_window_materialization_invalid_tool_pairing: tool message {} has tool_call_id={} but expected {}",
                        message.message_id, tool_call_id, expected
                    ));
                }
                pending_tool_call_ids.remove(0);
            }
            MessageRole::System | MessageRole::User => {}
        }
    }

    if let Some(tool_call_id) = pending_tool_call_ids.first() {
        return Err(format!(
            "context_window_materialization_invalid_tool_pairing: assistant tool call {tool_call_id} has no following tool result"
        ));
    }

    Ok(())
}

pub fn validate_context_window_materialization(
    session: &SessionStateSnapshot,
) -> Result<(), String> {
    let Some(raw) = session
        .metadata
        .get(runtime_metadata_keys::CONTEXT_WINDOW_MATERIALIZATION)
    else {
        return Ok(());
    };
    let value = serde_json::from_str::<Value>(raw)
        .map_err(|err| format!("parse context window materialization metadata failed: {err}"))?;
    let diagnostics = value
        .get("diagnostics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for diagnostic in diagnostics {
        let reason = diagnostic
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if is_critical_compaction_materialization_diagnostic(reason) {
            let message_id = diagnostic
                .get("messageId")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Err(format!(
                "context_window_materialization_invalid_compaction_lifecycle: reason={reason} messageId={message_id}"
            ));
        }
    }
    Ok(())
}

fn is_critical_compaction_materialization_diagnostic(reason: &str) -> bool {
    reason == "context_compaction_first_kept_message_missing"
}

pub fn trailing_unpaired_model_tool_call_ids(
    messages: &[ChatMessage],
    model_semantics: &std::collections::BTreeMap<String, ModelMessageSemanticsV1>,
) -> Result<Vec<String>, String> {
    let mut pending_tool_call_ids = Vec::<String>::new();
    for message in messages {
        if !pending_tool_call_ids.is_empty() && message.role != MessageRole::Tool {
            return Err(format!(
                "context_window_materialization_invalid_tool_pairing: assistant tool call {} must be followed by tool result before message {}",
                pending_tool_call_ids[0], message.message_id
            ));
        }

        match message.role {
            MessageRole::Assistant => {
                pending_tool_call_ids.extend(assistant_tool_call_ids(message, model_semantics)?);
            }
            MessageRole::Tool => {
                let tool_call_id = tool_result_call_id(message, model_semantics)?;
                let expected = pending_tool_call_ids.first().ok_or_else(|| {
                    format!(
                        "context_window_materialization_invalid_tool_pairing: tool message {} has no preceding assistant tool call",
                        message.message_id
                    )
                })?;
                if expected != &tool_call_id {
                    return Err(format!(
                        "context_window_materialization_invalid_tool_pairing: tool message {} has tool_call_id={} but expected {}",
                        message.message_id, tool_call_id, expected
                    ));
                }
                pending_tool_call_ids.remove(0);
            }
            MessageRole::System | MessageRole::User => {}
        }
    }
    Ok(pending_tool_call_ids)
}

fn assistant_tool_call_ids(
    message: &ChatMessage,
    model_semantics: &std::collections::BTreeMap<String, ModelMessageSemanticsV1>,
) -> Result<Vec<String>, String> {
    match model_semantics.get(message.message_id.as_str()) {
        Some(ModelMessageSemanticsV1::Plain) => Ok(Vec::new()),
        Some(ModelMessageSemanticsV1::Assistant { tool_calls, .. }) => Ok(tool_calls
            .iter()
            .map(|call| call.id.clone())
            .collect()),
        Some(_) => Err(format!(
            "context_window_materialization_invalid_tool_pairing: assistant message {} has incompatible typed semantics",
            message.message_id
        )),
        None => Err(format!(
            "context_window_materialization_invalid_tool_pairing: assistant message {} has no typed semantics",
            message.message_id
        )),
    }
}

fn tool_result_call_id(
    message: &ChatMessage,
    model_semantics: &std::collections::BTreeMap<String, ModelMessageSemanticsV1>,
) -> Result<String, String> {
    match model_semantics.get(message.message_id.as_str()) {
        Some(ModelMessageSemanticsV1::ToolResult { tool_call_id, .. })
            if !tool_call_id.trim().is_empty() => Ok(tool_call_id.clone()),
        Some(_) => Err(format!(
            "context_window_materialization_invalid_tool_pairing: tool message {} has incompatible typed semantics",
            message.message_id
        )),
        None => Err(format!(
            "context_window_materialization_invalid_tool_pairing: tool message {} has no typed semantics",
            message.message_id
        )),
    }
}

impl ContextWindowMaterialization {
    fn metadata_json(&self) -> String {
        json!({
            "schema": MATERIALIZATION_SCHEMA,
            "strategy": STRATEGY,
            "sourceMessageCount": self.source_message_count,
            "contextMessageCount": self.messages.len(),
            "diagnostics": self
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    json!({
                        "reason": diagnostic.reason,
                        "messageId": diagnostic.message_id,
                        "messageIndex": diagnostic.message_index,
                    })
                })
                .collect::<Vec<_>>(),
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::contracts::JsonMap;
    use crate::session::state::{
        ChatMessage, MessageRole, ModelMessageSemanticsV1, ModelToolCallStateV1,
        SessionStateSnapshot,
    };

    use super::{
        materialize_context_window, refresh_session_context_window,
        validate_context_window_materialization, validate_model_context_window,
    };

    fn message(id: &str, role: MessageRole, content: &str, metadata: JsonMap) -> ChatMessage {
        ChatMessage {
            message_id: id.to_string(),
            role,
            content: content.to_string(),
            created_at_ms: 0,
            metadata,
        }
    }

    fn assistant_tool_call(id: &str, tool_call_ids: &[&str]) -> ChatMessage {
        let mut metadata = JsonMap::new();
        metadata.insert(
            "test_model_semantics_json".to_string(),
            serde_json::to_string(&ModelMessageSemanticsV1::Assistant {
                reasoning_content: None,
                tool_calls: tool_call_ids
                    .iter()
                    .map(|tool_call_id| ModelToolCallStateV1 {
                        id: (*tool_call_id).to_string(),
                        name: "bash".to_string(),
                        args_json: "{}".to_string(),
                    })
                    .collect::<Vec<_>>(),
            })
            .expect("serialize assistant semantics"),
        );
        message(id, MessageRole::Assistant, "", metadata)
    }

    fn tool_result(id: &str, tool_call_id: &str) -> ChatMessage {
        let mut metadata = JsonMap::new();
        metadata.insert(
            "test_model_semantics_json".to_string(),
            serde_json::to_string(&ModelMessageSemanticsV1::ToolResult {
                tool_call_id: tool_call_id.to_string(),
                tool_name: "bash".to_string(),
                status: "ok".to_string(),
                result_state: "success_with_output".to_string(),
                error_kind: None,
                object_refs: vec![],
                transition_reason: None,
            })
            .expect("serialize tool result semantics"),
        );
        message(id, MessageRole::Tool, "tool output", metadata)
    }

    fn model_semantics(
        messages: &[ChatMessage],
    ) -> std::collections::BTreeMap<String, ModelMessageSemanticsV1> {
        messages
            .iter()
            .map(|message| {
                let semantics = message
                    .metadata
                    .get("test_model_semantics_json")
                    .map(|raw| serde_json::from_str(raw).expect("test semantics"))
                    .unwrap_or(ModelMessageSemanticsV1::Plain);
                (message.message_id.clone(), semantics)
            })
            .collect()
    }

    #[test]
    fn materializer_retains_all_messages_and_complete_tool_groups() {
        let mut messages = vec![
            message("user-1", MessageRole::User, "one", JsonMap::new()),
            assistant_tool_call("assistant-call-1", &["call-1"]),
            tool_result("tool-1", "call-1"),
        ];
        for index in 0..19 {
            messages.push(message(
                format!("user-tail-{index}").as_str(),
                MessageRole::User,
                "tail",
                JsonMap::new(),
            ));
        }

        let semantics = model_semantics(messages.as_slice());
        let materialized = materialize_context_window(messages.as_slice());

        assert_eq!(materialized.messages.len(), messages.len());
        assert_eq!(materialized.messages[0].message_id, "user-1");
        assert_eq!(materialized.messages[1].message_id, "assistant-call-1");
        assert_eq!(materialized.messages[2].message_id, "tool-1");
        validate_model_context_window(materialized.messages.as_slice(), &semantics)
            .expect("materialized context must preserve OpenAI tool pairing");
    }

    #[test]
    fn materializer_keeps_parallel_tool_results_with_their_assistant_call() {
        let messages = vec![
            message("old", MessageRole::User, "old", JsonMap::new()),
            assistant_tool_call("assistant-call-1", &["call-1", "call-2"]),
            tool_result("tool-1", "call-1"),
            tool_result("tool-2", "call-2"),
            message("tail", MessageRole::User, "tail", JsonMap::new()),
        ];

        let semantics = model_semantics(messages.as_slice());
        let materialized = materialize_context_window(messages.as_slice());

        assert_eq!(
            materialized
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["old", "assistant-call-1", "tool-1", "tool-2", "tail"]
        );
        validate_model_context_window(materialized.messages.as_slice(), &semantics)
            .expect("parallel tool group must stay complete");
    }

    #[test]
    fn validator_rejects_orphan_tool_result() {
        let messages = vec![tool_result("tool-1", "call-1")];

        let error = validate_model_context_window(messages.as_slice(), &model_semantics(&messages))
            .expect_err("orphan tool result must fail loudly");

        assert!(error.contains("has no preceding assistant tool call"));
    }

    #[test]
    fn validator_rejects_mismatched_tool_result() {
        let messages = vec![
            assistant_tool_call("assistant-call-1", &["call-1"]),
            tool_result("tool-2", "call-2"),
        ];

        let error = validate_model_context_window(messages.as_slice(), &model_semantics(&messages))
            .expect_err("mismatched tool result must fail loudly");

        assert!(error.contains("but expected call-1"));
    }

    #[test]
    fn materializer_is_deterministic_for_same_session_messages() {
        let messages = vec![
            message("user-1", MessageRole::User, "one", JsonMap::new()),
            assistant_tool_call("assistant-call-1", &["call-1"]),
            tool_result("tool-1", "call-1"),
            message("user-2", MessageRole::User, "two", JsonMap::new()),
        ];

        let left = materialize_context_window(messages.as_slice());
        let right = materialize_context_window(messages.as_slice());

        assert_eq!(
            left.messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            right
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn compaction_summary_replaces_prefix_at_first_kept_boundary() {
        let mut summary_metadata = JsonMap::new();
        summary_metadata.insert("kind".to_string(), "context_compaction".to_string());
        summary_metadata.insert("compaction_id".to_string(), "compact-1".to_string());
        summary_metadata.insert("first_kept_message_id".to_string(), "tail".to_string());
        let messages = vec![
            message(
                "old",
                MessageRole::User,
                "old audit message",
                JsonMap::new(),
            ),
            message("tail", MessageRole::User, "tail remains", JsonMap::new()),
            message(
                "summary",
                MessageRole::System,
                "# Summary\n\nExact markdown.",
                summary_metadata,
            ),
        ];

        let materialized = materialize_context_window(messages.as_slice());

        assert_eq!(
            materialized
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["summary", "tail"]
        );
        assert_eq!(
            materialized.messages[0].content,
            "# Summary\n\nExact markdown."
        );
        assert_eq!(messages[0].content, "old audit message");
    }

    #[test]
    fn missing_first_kept_boundary_fails_materialization_validation() {
        let mut summary_metadata = JsonMap::new();
        summary_metadata.insert("kind".to_string(), "context_compaction".to_string());
        summary_metadata.insert("compaction_id".to_string(), "compact-1".to_string());
        summary_metadata.insert("first_kept_message_id".to_string(), "banana".to_string());
        let messages = vec![
            message("old", MessageRole::User, "old", JsonMap::new()),
            message(
                "summary",
                MessageRole::System,
                "# Summary",
                summary_metadata,
            ),
        ];
        let mut session = SessionStateSnapshot::new("chat-missing-boundary".to_string(), 0);
        session.messages = messages;
        session.model_semantics = model_semantics(session.messages.as_slice());

        refresh_session_context_window(&mut session);

        assert!(validate_context_window_materialization(&session)
            .expect_err("missing compaction boundary must fail")
            .contains("context_compaction_first_kept_message_missing"));
    }

    #[test]
    fn query_loop_compaction_summary_and_replay_prefix_stay_byte_stable_until_next_compaction() {
        let mut summary_metadata = JsonMap::new();
        summary_metadata.insert("kind".to_string(), "context_compaction".to_string());
        summary_metadata.insert("compaction_id".to_string(), "compact-1".to_string());
        summary_metadata.insert("first_kept_message_id".to_string(), "tail-0".to_string());
        let mut replay_metadata = JsonMap::new();
        replay_metadata.insert(
            "kind".to_string(),
            "prompt_compaction_user_replay".to_string(),
        );
        replay_metadata.insert("compaction_id".to_string(), "compact-1".to_string());
        let mut messages = vec![
            message("old", MessageRole::User, "old", JsonMap::new()),
            message("tail-0", MessageRole::User, "tail 0", JsonMap::new()),
            message("tail-1", MessageRole::Assistant, "tail 1", JsonMap::new()),
            message("tail-2", MessageRole::User, "tail 2", JsonMap::new()),
            message(
                "summary",
                MessageRole::System,
                "# Summary\n\nExact bytes.",
                summary_metadata,
            ),
            message(
                "replay",
                MessageRole::User,
                "exact replay bytes",
                replay_metadata,
            ),
        ];
        let before = materialize_context_window(messages.as_slice());
        let stable_prefix_before =
            serde_json::to_vec(&before.messages[..2]).expect("serialize stable compaction prefix");

        messages.push(message(
            "tail-3",
            MessageRole::Assistant,
            "tail 3",
            JsonMap::new(),
        ));
        let after = materialize_context_window(messages.as_slice());
        let stable_prefix_after =
            serde_json::to_vec(&after.messages[..2]).expect("serialize stable compaction prefix");

        assert_eq!(stable_prefix_before, stable_prefix_after);
        assert_eq!(
            after
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["summary", "replay", "tail-0", "tail-1", "tail-2", "tail-3"]
        );
    }

    #[test]
    fn query_loop_unbounded_projection_preserves_early_tool_groups_and_rejects_malformed_input() {
        let mut messages = vec![
            message("old", MessageRole::User, "old", JsonMap::new()),
            assistant_tool_call("assistant-call", &["call-1", "call-2"]),
            tool_result("tool-1", "call-1"),
            tool_result("tool-2", "call-2"),
        ];
        for index in 0..4_095 {
            messages.push(message(
                format!("tail-{index}").as_str(),
                MessageRole::User,
                "tail",
                JsonMap::new(),
            ));
        }
        let semantics = model_semantics(messages.as_slice());
        let materialized = materialize_context_window(messages.as_slice());

        assert_eq!(materialized.messages.len(), messages.len());
        assert_eq!(materialized.messages.len(), 4_099);
        assert_eq!(materialized.messages[0].message_id, "old");
        assert_eq!(materialized.messages[1].message_id, "assistant-call");
        assert_eq!(materialized.messages[2].message_id, "tool-1");
        assert_eq!(materialized.messages[3].message_id, "tool-2");
        validate_model_context_window(materialized.messages.as_slice(), &semantics)
            .expect("all parallel tool groups must remain intact");

        let malformed = vec![tool_result("orphan-tool", "orphan-call")];
        let malformed_semantics = model_semantics(malformed.as_slice());
        let malformed = materialize_context_window(malformed.as_slice());
        assert!(
            validate_model_context_window(malformed.messages.as_slice(), &malformed_semantics)
                .expect_err("orphan tool result must fail loudly without message-count pruning")
                .contains("has no preceding assistant tool call")
        );
    }

    #[test]
    fn query_loop_reliable_tool_chain_anchor_semantics_are_exact() {
        for (semantic_kind, expected) in [
            (super::super::MESSAGE_SEMANTIC_USER_REQUEST, true),
            (super::super::MESSAGE_SEMANTIC_TURN_SUPPLEMENT, true),
            (super::super::MESSAGE_SEMANTIC_ANSWER_NOW, true),
            (super::super::MESSAGE_SEMANTIC_TOOL_CONTINUATION, false),
            ("", false),
        ] {
            let mut metadata = JsonMap::new();
            metadata.insert(
                crate::runtime::keys::metadata::MESSAGE_SEMANTIC_KIND.to_string(),
                semantic_kind.to_string(),
            );
            let candidate = message("anchor", MessageRole::User, "anchor", metadata);
            assert_eq!(
                super::is_reliable_tool_chain_user_anchor(&candidate),
                expected,
                "semanticKind={semantic_kind}"
            );
        }

        let mut replay_metadata = JsonMap::new();
        replay_metadata.insert(
            crate::runtime::keys::metadata::MESSAGE_SEMANTIC_KIND.to_string(),
            super::super::MESSAGE_SEMANTIC_USER_REQUEST.to_string(),
        );
        replay_metadata.insert(
            "kind".to_string(),
            "prompt_compaction_user_replay".to_string(),
        );
        let replay = message("replay", MessageRole::User, "replay", replay_metadata);
        assert!(!super::is_reliable_tool_chain_user_anchor(&replay));
    }
}
