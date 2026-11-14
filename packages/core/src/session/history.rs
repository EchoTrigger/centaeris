//! Immutable message revisions and bounded Session working-state persistence.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::state::{
    ChatMessage, CompletedTurnProjectionV1, ModelMessageSemanticsV1, SessionStateSnapshot,
};
use crate::runtime::contracts::{JsonMap, TimestampMs};

/// The current persisted history-reference contract.
pub const SESSION_HISTORY_SCHEMA_V1: &str = "runtime.session_history.v1";
const ENTRY_DOMAIN: &str = "centaeris.session_history_entry.v1";
const CONTEXT_DOMAIN: &str = "centaeris.session_context.v1";

/// One immutable body revision retained by a bounded working state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionHistoryRefV1 {
    pub message_id: String,
    pub record_id: String,
}

/// A context identity and the immutable revisions currently held in memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionHistoryStateV1 {
    pub schema: String,
    pub context_revision: String,
    pub retained_entries: Vec<SessionHistoryRefV1>,
    #[serde(deserialize_with = "required_prompt_usage_anchor")]
    pub prompt_usage_anchor: Option<crate::runtime::prompt_usage::PromptUsageAnchorV1>,
}

impl Default for SessionHistoryStateV1 {
    fn default() -> Self {
        let mut digest = Sha256::new();
        digest.update(CONTEXT_DOMAIN.as_bytes());
        digest.update([0]);
        digest.update(b"[[],[]]");
        Self {
            schema: SESSION_HISTORY_SCHEMA_V1.into(),
            context_revision: format!("sha256:{:x}", digest.finalize()),
            retained_entries: Vec::new(),
            prompt_usage_anchor: None,
        }
    }
}

/// An immutable message body and its model-facing meaning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionHistoryEntryV1 {
    pub record_id: String,
    #[serde(with = "message_wire")]
    pub message: ChatMessage,
    pub model_semantics: ModelMessageSemanticsV1,
}

impl SessionHistoryEntryV1 {
    /// Encodes stable stored bytes independently of map insertion order.
    pub fn canonical_json(&self) -> Result<String, String> {
        String::from_utf8(crate::runtime::canonical_json::to_vec(self)?)
            .map_err(|error| error.to_string())
    }

    /// Creates an identity bound to both original message bytes and semantics.
    pub fn new(
        message: ChatMessage,
        model_semantics: ModelMessageSemanticsV1,
    ) -> Result<Self, String> {
        let record_id = entry_record_id(&message, &model_semantics)?;
        Ok(Self {
            record_id,
            message,
            model_semantics,
        })
    }

    /// Checks the stored immutable body's identity.
    pub fn validate(&self) -> Result<(), String> {
        if self.message.message_id.trim().is_empty() {
            return Err("session_history_message_id_required".into());
        }
        if self.record_id != entry_record_id(&self.message, &self.model_semantics)? {
            return Err("session_history_entry_identity_mismatch".into());
        }
        Ok(())
    }

    /// Whether this body is a canonical original user request, rather than replay text.
    pub fn is_original_user_request(&self) -> bool {
        self.message.role == super::state::MessageRole::User
            && self
                .message
                .metadata
                .get("message_semantic_kind")
                .map(String::as_str)
                == Some("user_request")
            && self.message.metadata.get("kind").map(String::as_str)
                != Some("prompt_compaction_user_replay")
    }
}

/// A current message revision at its stable, first-seen Session position.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionHistoryRecordV1 {
    pub position: u64,
    pub entry: SessionHistoryEntryV1,
}

/// The historical bodies requested by a paged read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionHistorySelectionV1 {
    All,
    OriginalUserRequests,
}

/// An atomic working-state replacement and its new immutable facts.
#[derive(Debug)]
pub struct SessionSnapshotWriteV1 {
    snapshot: SessionStateSnapshot,
    snapshot_json: String,
    entries: Vec<SessionHistoryEntryV1>,
    retired_message_ids: Vec<String>,
    recovery_retired_message_ids: Vec<String>,
}

impl SessionSnapshotWriteV1 {
    /// The Session that owns this atomic write.
    pub fn session_id(&self) -> &str {
        &self.snapshot.session_id
    }

    /// The exact canonical bytes to commit alongside the archived facts.
    pub fn snapshot_json(&self) -> &str {
        &self.snapshot_json
    }

    /// The normalized working state represented by the committed bytes.
    pub fn snapshot(&self) -> &SessionStateSnapshot {
        &self.snapshot
    }

    /// Newly created or revised immutable source bodies.
    pub fn entries(&self) -> &[SessionHistoryEntryV1] {
        &self.entries
    }

    /// Verified source revisions retained by the normalized working state.
    pub fn retained_entries(&self) -> &[SessionHistoryRefV1] {
        &self.snapshot.history.retained_entries
    }

    /// Explicit caller removals, excluding cold working-set eviction.
    pub fn retired_message_ids(&self) -> &[String] {
        &self.retired_message_ids
    }

    /// Inactive canonical sources retired by authoritative cold reconstruction.
    /// Their heads may be absent when no earlier snapshot archived those sources.
    pub fn recovery_retired_message_ids(&self) -> &[String] {
        &self.recovery_retired_message_ids
    }

    /// Returns the exact normalized working state committed by a typed store.
    pub fn into_snapshot(self) -> SessionStateSnapshot {
        self.snapshot
    }
}

fn entry_record_id(
    message: &ChatMessage,
    semantics: &ModelMessageSemanticsV1,
) -> Result<String, String> {
    crate::runtime::canonical_json::sha256(ENTRY_DOMAIN, &(message, semantics))
}

fn context_revision(
    refs: &[SessionHistoryRefV1],
    context: &[ChatMessage],
) -> Result<String, String> {
    crate::runtime::canonical_json::sha256(
        CONTEXT_DOMAIN,
        &(
            refs,
            context
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
        ),
    )
}

/// Derives the current model-context identity without copying the Session.
pub fn session_context_revision(snapshot: &SessionStateSnapshot) -> Result<String, String> {
    if snapshot.history.schema != SESSION_HISTORY_SCHEMA_V1 {
        return Err("session_history_schema_unsupported".into());
    }
    let materialized =
        crate::runtime::context_window::materialize_context_window(&snapshot.messages);
    materialized.ensure_valid()?;
    let active = materialized
        .messages
        .iter()
        .map(|message| message.message_id.as_str())
        .collect::<BTreeSet<_>>();
    let refs = snapshot
        .messages
        .iter()
        .filter(|message| active.contains(message.message_id.as_str()))
        .map(|message| {
            Ok(SessionHistoryRefV1 {
                message_id: message.message_id.clone(),
                record_id: entry_record_id(
                    message,
                    snapshot.model_semantics_for(&message.message_id)?,
                )?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    context_revision(&refs, &materialized.messages)
}

/// Checks a stored working state against the immutable revisions it names.
pub fn validate_session_snapshot_history(snapshot: &SessionStateSnapshot) -> Result<(), String> {
    if snapshot.history.schema != SESSION_HISTORY_SCHEMA_V1 {
        return Err("session_history_schema_unsupported".into());
    }
    if let Some(anchor) = &snapshot.history.prompt_usage_anchor {
        anchor.validate()?;
    }
    if snapshot.history.retained_entries.len() != snapshot.messages.len() {
        return Err("session_history_working_membership_mismatch".into());
    }
    for (message, reference) in snapshot
        .messages
        .iter()
        .zip(&snapshot.history.retained_entries)
    {
        if message.message_id != reference.message_id
            || entry_record_id(message, snapshot.model_semantics_for(&message.message_id)?)?
                != reference.record_id
        {
            return Err("session_history_working_revision_mismatch".into());
        }
    }
    let materialized =
        crate::runtime::context_window::materialize_context_window(&snapshot.messages);
    materialized.ensure_valid()?;
    if context_revision(&snapshot.history.retained_entries, &materialized.messages)?
        != snapshot.history.context_revision
    {
        return Err("session_history_context_revision_mismatch".into());
    }
    Ok(())
}

/// Validates and bounds a current snapshot while preserving new source bodies.
///
/// Callers commit the returned entries, head changes and snapshot atomically.
/// Cold eviction changes the working set, not the historical visibility of a message.
pub fn prepare_session_snapshot_write(raw: &str) -> Result<SessionSnapshotWriteV1, String> {
    let snapshot = serde_json::from_str::<SessionStateSnapshot>(raw)
        .map_err(|error| format!("decode current Session snapshot failed: {error}"))?;
    prepare_session_snapshot_state_write(snapshot)
}

/// Prepares an owned working state without a serialization and parsing round trip.
pub fn prepare_session_snapshot_state_write(
    mut snapshot: SessionStateSnapshot,
) -> Result<SessionSnapshotWriteV1, String> {
    if snapshot.history.schema != SESSION_HISTORY_SCHEMA_V1 {
        return Err("session_history_schema_unsupported".into());
    }
    if let Some(anchor) = &snapshot.history.prompt_usage_anchor {
        anchor.validate()?;
    }
    let previous = snapshot
        .history
        .retained_entries
        .iter()
        .map(|entry| (entry.message_id.as_str(), entry.record_id.as_str()))
        .collect::<BTreeMap<_, _>>();
    if previous.len() != snapshot.history.retained_entries.len() {
        return Err("session_history_duplicate_retained_identity".into());
    }
    let source_ids = snapshot
        .messages
        .iter()
        .map(|message| message.message_id.as_str())
        .collect::<BTreeSet<_>>();
    if source_ids.len() != snapshot.messages.len() {
        return Err("session_history_duplicate_message_identity".into());
    }
    let retired_message_ids = previous
        .keys()
        .filter(|id| !source_ids.contains(**id))
        .map(|id| (*id).to_string())
        .collect();
    let materialized =
        crate::runtime::context_window::materialize_context_window(&snapshot.messages);
    materialized.ensure_valid()?;
    let context = materialized.messages;
    let active = context
        .iter()
        .map(|message| message.message_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut entries = Vec::new();
    let mut retained_entries = Vec::new();
    for message in &snapshot.messages {
        let semantics = snapshot.model_semantics_for(&message.message_id)?;
        let record_id = entry_record_id(message, semantics)?;
        if previous.get(message.message_id.as_str()).copied() != Some(record_id.as_str()) {
            entries.push(SessionHistoryEntryV1 {
                record_id: record_id.clone(),
                message: message.clone(),
                model_semantics: semantics.clone(),
            });
        }
        if active.contains(message.message_id.as_str()) {
            retained_entries.push(SessionHistoryRefV1 {
                message_id: message.message_id.clone(),
                record_id,
            });
        }
    }
    snapshot
        .messages
        .retain(|message| active.contains(message.message_id.as_str()));
    snapshot
        .model_semantics
        .retain(|id, _| active.contains(id.as_str()));
    snapshot.history = SessionHistoryStateV1 {
        schema: SESSION_HISTORY_SCHEMA_V1.into(),
        context_revision: context_revision(&retained_entries, &context)?,
        retained_entries,
        prompt_usage_anchor: snapshot.history.prompt_usage_anchor,
    };
    snapshot.context_window = context;
    let snapshot_json = String::from_utf8(crate::runtime::canonical_json::to_vec(&snapshot)?)
        .map_err(|error| format!("encode bounded Session snapshot failed: {error}"))?;
    Ok(SessionSnapshotWriteV1 {
        snapshot,
        snapshot_json,
        entries,
        retired_message_ids,
        recovery_retired_message_ids: Vec::new(),
    })
}

/// Prepares authoritative reconstruction with explicit inactive-source retirements.
pub fn prepare_recovered_session_snapshot_write(
    snapshot: SessionStateSnapshot,
    retired_message_ids: Vec<String>,
) -> Result<SessionSnapshotWriteV1, String> {
    let mut prepared = prepare_session_snapshot_state_write(snapshot)?;
    let active = prepared
        .retained_entries()
        .iter()
        .map(|entry| entry.message_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut retired = BTreeSet::new();
    for id in retired_message_ids {
        if id.trim().is_empty() || active.contains(id.as_str()) {
            return Err("session_history_recovery_retirement_invalid".into());
        }
        retired.insert(id);
    }
    prepared.recovery_retired_message_ids = retired.into_iter().collect();
    Ok(prepared)
}

fn required_prompt_usage_anchor<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<crate::runtime::prompt_usage::PromptUsageAnchorV1>, D::Error> {
    Option::deserialize(deserializer)
}

/// Converts the previous exact snapshot contract during an explicit store upgrade.
pub fn migrate_legacy_session_snapshot(raw: &str) -> Result<SessionSnapshotWriteV1, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LegacySnapshot {
        session_id: String,
        messages: Vec<ChatMessage>,
        context_window: Vec<ChatMessage>,
        model_semantics: BTreeMap<String, ModelMessageSemanticsV1>,
        #[serde(rename = "completedTurn", deserialize_with = "required_completed_turn")]
        completed_turn: Option<CompletedTurnProjectionV1>,
        updated_at_ms: TimestampMs,
        metadata: JsonMap,
    }
    let legacy: LegacySnapshot = serde_json::from_str(raw)
        .map_err(|error| format!("decode legacy Session snapshot failed: {error}"))?;
    prepare_session_snapshot_state_write(SessionStateSnapshot {
        session_id: legacy.session_id,
        messages: legacy.messages,
        context_window: legacy.context_window,
        model_semantics: legacy.model_semantics,
        completed_turn: legacy.completed_turn,
        updated_at_ms: legacy.updated_at_ms,
        metadata: legacy.metadata,
        history: SessionHistoryStateV1::default(),
    })
}

fn required_completed_turn<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<CompletedTurnProjectionV1>, D::Error> {
    Option::deserialize(deserializer)
}

mod message_wire {
    use super::*;

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Message {
        message_id: String,
        role: super::super::state::MessageRole,
        content: String,
        created_at_ms: TimestampMs,
        metadata: JsonMap,
    }

    pub fn serialize<S: serde::Serializer>(
        value: &ChatMessage,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SessionHistoryMessage", 5)?;
        state.serialize_field("messageId", &value.message_id)?;
        state.serialize_field("role", &value.role)?;
        state.serialize_field("content", &value.content)?;
        state.serialize_field("createdAtMs", &value.created_at_ms)?;
        state.serialize_field("metadata", &value.metadata)?;
        state.end()
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ChatMessage, D::Error> {
        let value = Message::deserialize(deserializer)?;
        Ok(ChatMessage {
            message_id: value.message_id,
            role: value.role,
            content: value.content,
            created_at_ms: value.created_at_ms,
            metadata: value.metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::state::MessageRole;

    fn message(id: &str, role: MessageRole, text: &str) -> ChatMessage {
        let mut metadata = JsonMap::new();
        if role == MessageRole::User {
            metadata.insert("message_semantic_kind".into(), "user_request".into());
        }
        ChatMessage {
            message_id: id.into(),
            role,
            content: text.into(),
            created_at_ms: 1,
            metadata,
        }
    }

    fn compacted() -> SessionStateSnapshot {
        let mut state = SessionStateSnapshot::new("history".into(), 1);
        state.messages = vec![
            message(
                "older",
                MessageRole::User,
                "Keep this earlier request in history",
            ),
            message("anchor", MessageRole::User, "Current exact task\n"),
            message("cold", MessageRole::Assistant, "Old result"),
            message("tail", MessageRole::Assistant, "Current result"),
        ];
        let mut summary = message("summary", MessageRole::System, "Summary");
        summary
            .metadata
            .insert("kind".into(), "context_compaction".into());
        summary
            .metadata
            .insert("first_kept_message_id".into(), "tail".into());
        summary
            .metadata
            .insert("compaction_id".into(), "compaction".into());
        summary.metadata.insert(
            crate::runtime::context_window::CAUSAL_ANCHOR_REFERENCE.into(),
            crate::runtime::context_window::compaction_causal_anchor_reference(&state.messages[1])
                .unwrap(),
        );
        state.messages.push(summary);
        state.model_semantics = state
            .messages
            .iter()
            .map(|message| (message.message_id.clone(), ModelMessageSemanticsV1::Plain))
            .collect();
        state
    }

    #[test]
    fn working_state_archives_all_sources_and_retains_exact_anchor_without_cold_history() {
        let state = compacted();
        let write =
            prepare_session_snapshot_write(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(write.entries.len(), 5);
        let bounded: SessionStateSnapshot = serde_json::from_str(&write.snapshot_json).unwrap();
        assert_eq!(
            bounded
                .messages
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            ["anchor", "tail", "summary"]
        );
        assert_eq!(bounded.messages[0].content, "Current exact task\n");
        assert_eq!(
            bounded
                .context_window
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            ["summary", "anchor", "tail"]
        );
        assert_eq!(bounded.model_semantics.len(), 3);
        validate_session_snapshot_history(&bounded).unwrap();
        let repeated = prepare_session_snapshot_write(&write.snapshot_json).unwrap();
        assert!(repeated.entries.is_empty());
        assert!(repeated.retired_message_ids.is_empty());
        assert_eq!(repeated.snapshot_json, write.snapshot_json);
    }

    #[test]
    fn history_revisions_track_body_changes_and_explicit_removal_without_rewriting_old_facts() {
        let mut state = SessionStateSnapshot::new("history".into(), 1);
        state.messages = vec![
            message("a", MessageRole::User, "first"),
            message("b", MessageRole::User, "second"),
        ];
        state.model_semantics = state
            .messages
            .iter()
            .map(|m| (m.message_id.clone(), ModelMessageSemanticsV1::Plain))
            .collect();
        let original =
            prepare_session_snapshot_write(&serde_json::to_string(&state).unwrap()).unwrap();
        let mut current: SessionStateSnapshot =
            serde_json::from_str(&original.snapshot_json).unwrap();
        current.messages.remove(0);
        current.messages[0].content = "changed".into();
        let updated =
            prepare_session_snapshot_write(&serde_json::to_string(&current).unwrap()).unwrap();
        assert_eq!(updated.retired_message_ids, ["a"]);
        assert_eq!(updated.entries.len(), 1);
        assert_ne!(original.entries[1].record_id, updated.entries[0].record_id);
        assert_eq!(original.entries[1].message.content, "second");
        let mut tampered = updated.entries[0].clone();
        tampered.message.content = "tampered".into();
        assert!(tampered.validate().is_err());
    }

    #[test]
    fn context_identity_ignores_diagnostics_and_requires_explicit_legacy_conversion() {
        let initial =
            prepare_session_snapshot_write(&serde_json::to_string(&compacted()).unwrap()).unwrap();
        let mut state: SessionStateSnapshot = serde_json::from_str(&initial.snapshot_json).unwrap();
        let identity = state.context_revision().unwrap();
        state.metadata.insert(
            "prompt_compaction_stats_json".into(),
            "diagnostic change".into(),
        );
        state.updated_at_ms = 99;
        assert_eq!(identity, state.context_revision().unwrap());
        let mut legacy = serde_json::to_value(compacted()).unwrap();
        legacy.as_object_mut().unwrap().remove("history");
        let raw = serde_json::to_string(&legacy).unwrap();
        assert!(prepare_session_snapshot_write(&raw).is_err());
        assert_eq!(
            migrate_legacy_session_snapshot(&raw).unwrap().entries.len(),
            5
        );
        legacy.as_object_mut().unwrap().remove("completedTurn");
        assert!(migrate_legacy_session_snapshot(&serde_json::to_string(&legacy).unwrap()).is_err());
    }
}
