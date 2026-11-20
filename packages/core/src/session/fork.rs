use super::{
    active_session_records, host_event_input::HostEventInput, payload_object,
    recovery::retired_plaintext_source_message_ids, reduce_events, required_payload_string,
    required_payload_string_allow_empty, stable_session_event_id, MessageAttachmentRef,
    ReducedAgentRunState, SessionLogRecord, SessionRecordType,
};
use crate::runtime::ModelObservationV1;
use serde_json::{to_value, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

/// A Session prefix ending before an independently admitted user turn.
/// Admission provenance comes from the host's durable input records.
pub struct SessionFork {
    prefix: Vec<SessionLogRecord>,
    openings: HashSet<(String, String)>,
    retired_messages: HashSet<String>,
    pub draft: String,
    pub attachments: Vec<ForkDraftAttachment>,
}

pub struct ForkDraftAttachment {
    pub input_ref: String,
    pub content_type: String,
    pub placeholder: String,
}

pub struct ForkHistory {
    pub records: Vec<SessionLogRecord>,
    pub openings: HashSet<(String, String)>,
}

impl SessionFork {
    pub fn before_latest_user_turn(
        session_id: &str,
        records: &[SessionLogRecord],
        verified_openings: &HashSet<(String, String)>,
    ) -> Result<Self, String> {
        let mut prefix = active_session_records(session_id, records)?;
        let cutoff = prefix
            .iter()
            .rposition(|record| {
                record.event_type == SessionRecordType::AgentRunStarted
                    && opening_identity(record).is_some_and(|id| verified_openings.contains(&id))
            })
            .ok_or("No verified user turn is available to fork")?;
        let opening = &prefix[cutoff];
        let prompt = prefix[cutoff + 1..]
            .iter()
            .find(|record| {
                record.event_type == SessionRecordType::UserMessage
                    && record.agent_run_id == opening.agent_run_id
                    && record.turn_id == opening.turn_id
            })
            .ok_or("The verified user turn has no committed opening message")?;
        let payload = payload_object(prompt)?;
        let draft = required_payload_string_allow_empty(payload, "text", prompt)?;
        let attachments =
            serde_json::from_value::<Vec<MessageAttachmentRef>>(payload["attachments"].clone())
                .map_err(|error| format!("decode opening attachments failed: {error}"))?
                .into_iter()
                .map(|reference| {
                    Ok(ForkDraftAttachment {
                        input_ref: reference.input_ref,
                        content_type: reference.content_type,
                        placeholder: reference.placeholder.ok_or(
                            "The opening message contains an attachment that cannot be edited",
                        )?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
        prefix.truncate(cutoff);
        let openings = prefix
            .iter()
            .filter(|record| record.event_type == SessionRecordType::AgentRunStarted)
            .filter_map(opening_identity)
            .filter(|id| verified_openings.contains(id))
            .collect();
        Ok(Self {
            prefix,
            openings,
            retired_messages: retired_plaintext_source_message_ids(session_id, records)?
                .into_iter()
                .collect(),
            draft,
            attachments,
        })
    }

    /// Rebind committed history to a fresh Session without carrying over the
    /// source's live execution, checkpoints, or input and delivery controls.
    pub fn rebase(&self, session_id: &str) -> Result<ForkHistory, String> {
        let identities = ForkIdentity::new(session_id, &self.prefix, &self.retired_messages)?;
        let records = self
            .prefix
            .iter()
            .filter(|record| {
                !matches!(
                    record.event_type,
                    SessionRecordType::SessionMeta
                        | SessionRecordType::AgentRunExecutionStarted
                        | SessionRecordType::AgentRunExecutionEnded
                        | SessionRecordType::AgentRunRecoveryAttempted
                        | SessionRecordType::CheckpointRef
                )
            })
            .map(|record| identities.record(record))
            .collect::<Result<Vec<_>, _>>()?;
        let projection = reduce_events(session_id, records.iter())?;
        if projection
            .agent_runs
            .values()
            .any(|run| run.state == ReducedAgentRunState::Running)
        {
            return Err("The Session prefix includes an unfinished earlier run".to_string());
        }
        Ok(ForkHistory {
            records,
            openings: self
                .openings
                .iter()
                .map(|(run, turn)| {
                    (
                        identities.path_id("agent-run", run),
                        identities.path_id("turn", turn),
                    )
                })
                .collect(),
        })
    }
}

fn opening_identity(record: &SessionLogRecord) -> Option<(String, String)> {
    Some((record.agent_run_id.clone()?, record.turn_id.clone()?))
}

struct ForkIdentity<'a> {
    session_id: &'a str,
    bound_messages: HashMap<String, String>,
    retired_messages: &'a HashSet<String>,
}

impl<'a> ForkIdentity<'a> {
    fn new(
        session_id: &'a str,
        records: &[SessionLogRecord],
        retired_messages: &'a HashSet<String>,
    ) -> Result<Self, String> {
        let mut identities = Self {
            session_id,
            bound_messages: HashMap::new(),
            retired_messages,
        };
        for record in records {
            let replacement = match record.event_type {
                SessionRecordType::HostEventInput => identities.host_input(record)?.message_id,
                SessionRecordType::UserMessage | SessionRecordType::AssistantMessage => {
                    let turn = super::required_event_turn_id(record)?;
                    let role = if record.event_type == SessionRecordType::UserMessage {
                        "user"
                    } else {
                        "assistant"
                    };
                    format!("message:{}:{role}", identities.path_id("turn", &turn))
                }
                SessionRecordType::TurnSupplement => {
                    let payload = payload_object(record)?;
                    let supplement = required_payload_string(payload, "supplementId", record)?;
                    let turn = super::required_event_turn_id(record)?;
                    format!(
                        "message:{}:supplement:{supplement}",
                        identities.path_id("turn", &turn)
                    )
                }
                _ => continue,
            };
            let source = required_payload_string(payload_object(record)?, "messageId", record)?;
            identities.bound_messages.insert(source, replacement);
        }
        Ok(identities)
    }

    fn id(&self, kind: &str, original: &str) -> String {
        stable_session_event_id(kind, &[self.session_id, original])
    }

    fn path_id(&self, kind: &str, original: &str) -> String {
        let preimage = serde_json::to_vec(&(kind, self.session_id, original))
            .expect("fork identity contains only strings");
        format!("{kind}-{:x}", Sha256::digest(preimage))
    }

    fn message_id(&self, original: &str) -> String {
        self.bound_messages
            .get(original)
            .cloned()
            .unwrap_or_else(|| self.id("message", original))
    }

    fn host_input(&self, record: &SessionLogRecord) -> Result<HostEventInput, String> {
        let old: HostEventInput = serde_json::from_value(record.payload.clone())
            .map_err(|error| format!("decode historical host input failed: {error}"))?;
        HostEventInput::new(self.session_id, old.input_id, old.source, old.content)
    }

    fn observations(&self, value: &Value) -> Result<Value, String> {
        let mut observations: Vec<ModelObservationV1> = serde_json::from_value(value.clone())
            .map_err(|error| format!("decode historical model observations failed: {error}"))?;
        observations.retain_mut(|observation| match observation {
            ModelObservationV1::InputUptake { .. }
            | ModelObservationV1::RequiredCompletionDelivery { .. } => false,
            ModelObservationV1::ContextMessage { message }
            | ModelObservationV1::CompactionPrompt { message } => {
                if self.retired_messages.contains(&message.message_id) {
                    return false;
                }
                message.message_id = self.message_id(&message.message_id);
                true
            }
            ModelObservationV1::InputImage { image } => {
                if self.retired_messages.contains(&image.message_id) {
                    return false;
                }
                image.message_id = self.message_id(&image.message_id);
                true
            }
            _ => true,
        });
        to_value(observations).map_err(|error| format!("encode fork observations failed: {error}"))
    }

    fn record(&self, source: &SessionLogRecord) -> Result<SessionLogRecord, String> {
        let mut record = source.clone();
        record.session_id = self.session_id.to_owned();
        record.event_id = self.id("fork_event", &source.event_id);
        record.agent_run_id = source
            .agent_run_id
            .as_deref()
            .map(|id| self.path_id("agent-run", id));
        record.turn_id = source.turn_id.as_deref().map(|id| self.path_id("turn", id));
        let payload = &mut record.payload;
        match source.event_type {
            SessionRecordType::UserMessage
            | SessionRecordType::TurnSupplement
            | SessionRecordType::AssistantMessage => {
                let id = required_payload_string(payload_object(source)?, "messageId", source)?;
                payload["messageId"] = self.message_id(&id).into();
            }
            SessionRecordType::HostEventInput => {
                *payload = to_value(self.host_input(source)?)
                    .map_err(|error| format!("encode fork host input failed: {error}"))?;
            }
            SessionRecordType::ModelRequestStarted | SessionRecordType::ReasoningBlock => {
                let id = required_payload_string(payload_object(source)?, "requestId", source)?;
                let request_id = self.id("request", &id);
                if source.event_type == SessionRecordType::ModelRequestStarted {
                    payload["observations"] = self.observations(&payload["observations"])?;
                } else {
                    payload["blockId"] = format!("reasoning:{request_id}").into();
                }
                payload["requestId"] = request_id.into();
            }
            SessionRecordType::Compaction => {
                let id = required_payload_string(payload_object(source)?, "compactionId", source)?;
                payload["compactionId"] = self.id("compaction", &id).into();
                for field in ["summaryMessageId", "firstKeptMessageId"] {
                    if let Some(id) = payload[field].as_str() {
                        payload[field] = self.message_id(id).into();
                    }
                }
                if let Some(observations) = payload.get("contextObservations") {
                    payload["contextObservations"] = self.observations(observations)?;
                }
            }
            _ => {}
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{
        completed_agent_run_record, sealed_assistant_message_record, started_agent_run_records,
        started_agent_run_records_with_attachments, turn_supplement_record,
    };
    use serde_json::json;

    #[test]
    fn fork_uses_external_opening_and_rebases_earlier_closed_history() {
        let mut source = started_agent_run_records("source", "first", "run-1", "earlier", 1)
            .unwrap()
            .to_vec();
        source.push(
            turn_supplement_record("source", "first", "run-1", "steer-1", "earlier steer", 2)
                .unwrap(),
        );
        source.push(
            sealed_assistant_message_record("source", "first", "run-1", "reply", "done", 3)
                .unwrap(),
        );
        source.push(
            completed_agent_run_record("source", "first", "run-1", "final_response", 4).unwrap(),
        );
        source.extend(
            started_agent_run_records_with_attachments(
                "source",
                "next",
                "run-2",
                "  latest [Image #1]\n",
                vec![json!({
                    "inputRef": "local-image:sample", "contentType": "image/png",
                    "displayName": "Image 1", "placeholder": "[Image #1]",
                })],
                5,
            )
            .unwrap(),
        );
        source.push(
            turn_supplement_record("source", "next", "run-2", "steer-2", "later steer", 6).unwrap(),
        );
        source.extend(
            started_agent_run_records("source", "internal", "run-3", "internal input", 7).unwrap(),
        );
        let openings = HashSet::from([
            ("run-1".into(), "first".into()),
            ("run-2".into(), "next".into()),
        ]);

        let fork = SessionFork::before_latest_user_turn("source", &source, &openings).unwrap();
        assert_eq!(fork.draft, "  latest [Image #1]\n");
        assert_eq!(fork.attachments[0].placeholder, "[Image #1]");
        assert_eq!(fork.attachments[0].input_ref, "local-image:sample");
        let history = fork.rebase("new-session").unwrap();
        assert!(history.records.iter().all(|record| {
            record.session_id == "new-session"
                && source
                    .iter()
                    .all(|original| original.event_id != record.event_id)
        }));
        let restored =
            super::super::recovery::recover_session_records("new-session", &history.records)
                .unwrap();
        let messages: Vec<_> = restored
            .snapshot
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect();
        assert_eq!(messages, ["earlier", "earlier steer", "reply"]);
        let inherited = SessionFork::before_latest_user_turn(
            "new-session",
            &history.records,
            &history.openings,
        )
        .unwrap();
        assert_eq!(inherited.draft, "earlier");
        assert!(inherited.rebase("second-fork").unwrap().records.is_empty());
    }

    #[test]
    fn a_user_shaped_record_does_not_supply_unverified_origin() {
        let source =
            started_agent_run_records("source", "turn", "run", "internal response", 1).unwrap();
        let result = SessionFork::before_latest_user_turn("source", &source, &HashSet::new());
        assert!(result.is_err());
    }
}
