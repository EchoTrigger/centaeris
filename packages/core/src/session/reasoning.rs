use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ReasoningLedger {
    requests: HashMap<String, (String, String, String)>,
    blocks: HashMap<String, (String, String, Value)>,
    sealed_runs: HashSet<String>,
}

impl ReasoningLedger {
    pub(super) fn apply(&mut self, event: &SessionLogRecord) -> Result<(), String> {
        match event.event_type {
            SessionRecordType::ModelRequestStarted => {
                let request_id =
                    required_payload_string(payload_object(event)?, "requestId", event)?;
                let identity = (
                    required_event_agent_run_id(event)?,
                    required_event_turn_id(event)?,
                    required_payload_string(payload_object(event)?, "purpose", event)?,
                );
                if let Some(existing) = self.requests.get(&request_id) {
                    if existing != &identity {
                        return Err("model request identity conflict".to_string());
                    }
                } else {
                    self.requests.insert(request_id, identity);
                }
            }
            SessionRecordType::ReasoningBlock => {
                let run = required_event_agent_run_id(event)?;
                let turn = required_event_turn_id(event)?;
                let payload = payload_object(event)?;
                let request = required_payload_string(payload, "requestId", event)?;
                if self.requests.get(&request)
                    != Some(&(run.clone(), turn.clone(), "main".to_string()))
                {
                    return Err("reasoning block must reference a prior main request in the same turn and AgentRun".to_string());
                }
                if self.sealed_runs.contains(&run) {
                    return Err("reasoning block follows AgentRun terminal".to_string());
                }
                let block = required_payload_string(payload, "blockId", event)?;
                if self.blocks.contains_key(&block) {
                    return Err("reasoning block is already sealed".to_string());
                }
                self.blocks
                    .insert(block, (run, turn, event.payload.clone()));
            }
            SessionRecordType::AgentRunCompleted
            | SessionRecordType::AgentRunFailed
            | SessionRecordType::AgentRunInterrupted => {
                self.sealed_runs.insert(required_event_agent_run_id(event)?);
            }
            _ => {}
        }
        Ok(())
    }
}

pub(super) fn validate_reasoning_block(event: &SessionLogRecord) -> Result<(), String> {
    required_event_turn_id(event)?;
    required_event_agent_run_id(event)?;
    let payload = payload_object(event)?;
    require_exact_payload_fields(payload, &["blockId", "requestId", "text", "status"], event)?;
    let request = required_payload_string(payload, "requestId", event)?;
    let block = required_payload_string(payload, "blockId", event)?;
    if block != format!("reasoning:{request}") {
        return Err("reasoning blockId does not match requestId".to_string());
    }
    required_payload_string(payload, "text", event)?;
    if !matches!(
        required_payload_string(payload, "status", event)?.as_str(),
        "done" | "interrupted"
    ) {
        return Err("reasoning block must be sealed".to_string());
    }
    Ok(())
}

impl AgentRunSessionState {
    pub fn recover_reasoning_snapshot(
        &mut self,
        turn_id: &str,
        value: &Value,
        at_ms: i64,
    ) -> Result<Option<SequencedSessionRecord>, String> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Snapshot {
            block_id: String,
            request_id: String,
            text: String,
        }
        let snapshot: Snapshot = serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid reasoning snapshot: {error}"))?;
        if snapshot.request_id.trim().is_empty()
            || snapshot.block_id != format!("reasoning:{}", snapshot.request_id)
        {
            return Err("reasoning snapshot identity mismatch".to_string());
        }
        if self.reasoning.requests.get(&snapshot.request_id)
            != Some(&(
                self.agent_run_id.clone(),
                turn_id.to_string(),
                "main".to_string(),
            ))
        {
            return Err("reasoning snapshot request binding mismatch".to_string());
        }
        if self.has_reasoning_block(&snapshot.request_id) {
            return Ok(None);
        }
        if snapshot.text.trim().is_empty() {
            return Ok(None);
        }
        self.record_reasoning_block(
            turn_id,
            &snapshot.request_id,
            &snapshot.text,
            "interrupted",
            at_ms,
        )
    }
    pub fn has_reasoning_block(&self, request_id: &str) -> bool {
        self.reasoning
            .blocks
            .contains_key(&format!("reasoning:{request_id}"))
    }
    pub fn record_reasoning_block(
        &mut self,
        turn_id: &str,
        request_id: &str,
        text: &str,
        status: &str,
        at_ms: i64,
    ) -> Result<Option<SequencedSessionRecord>, String> {
        let block_id = format!("reasoning:{request_id}");
        let payload = serde_json::json!({"blockId":block_id,"requestId":request_id,"text":text,"status":status});
        if let Some((run, turn, existing)) = self.reasoning.blocks.get(&block_id) {
            return if run == &self.agent_run_id && turn == turn_id && existing == &payload {
                Ok(None)
            } else {
                Err("reasoning block terminal conflict".to_string())
            };
        }
        let record = canonical_session_record(
            stable_session_event_id("reasoning_block", &[&block_id]),
            SessionRecordType::ReasoningBlock,
            self.session_id(),
            Some(turn_id.to_string()),
            Some(self.agent_run_id.clone()),
            at_ms,
            payload,
        )?;
        self.record(record).map(Some)
    }
}
