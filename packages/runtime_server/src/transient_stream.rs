use std::time::{Duration, Instant};

use redis::Commands;

const SIGNAL_SCHEMA: &str = "agent_run.transient.signal.v1";
const SIGNAL_FIELD: &str = "signal";
const STREAM_MAX_LENGTH: usize = 256;
const LIVE_FLUSH_INTERVAL: Duration = Duration::from_millis(75);

fn commit_wake_signal(agent_run_id: &str, high_water_sequence: u64) -> serde_json::Value {
    serde_json::json!({
        "schema": SIGNAL_SCHEMA,
        "kind": "commit_wake",
        "agentRunId": agent_run_id,
        "highWaterSequence": high_water_sequence,
    })
}

pub struct TransientAgentRunStream {
    agent_run_id: String,
    signal_stream_key: String,
    connection: redis::Connection,
    stream_ttl_seconds: i64,
    live_ttl_seconds: i64,
    live_revision: u64,
    live_after_sequence: u64,
    live_text: String,
    live_reasoning: Option<serde_json::Value>,
    pending_live: Option<PendingLiveText>,
    last_live_flush: Option<Instant>,
}

#[derive(Debug, Clone)]
struct PendingLiveText {
    turn_id: String,
    message_id: String,
}

pub(crate) fn signal_stream_key(agent_run_id: &str) -> String {
    format!("workspace-agent:agent-run:{{{agent_run_id}}}:signals")
}

pub(crate) fn live_text_key(agent_run_id: &str) -> String {
    format!("workspace-agent:agent-run:{{{agent_run_id}}}:live:text")
}

pub(crate) fn live_meta_key(agent_run_id: &str) -> String {
    format!("workspace-agent:agent-run:{{{agent_run_id}}}:live:meta")
}

#[derive(Debug)]
pub enum LiveTextError {
    Indeterminate(String),
    Fatal(String),
}

impl std::fmt::Display for LiveTextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Indeterminate(message) => {
                write!(formatter, "live text mutation indeterminate: {message}")
            }
            Self::Fatal(message) => write!(formatter, "live text mutation failed: {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTextMeta {
    pub message_id: String,
    pub turn_id: String,
    pub after_sequence: u64,
    pub revision: u64,
}

fn parse_u64(value: Option<&String>, field: &str) -> Result<u64, String> {
    value
        .ok_or_else(|| format!("live meta {field} is missing"))?
        .parse::<u64>()
        .map_err(|_| format!("live meta {field} is invalid"))
}

fn parse_live_meta(
    map: &std::collections::HashMap<String, String>,
) -> Result<Option<LiveTextMeta>, String> {
    if map.is_empty() {
        return Ok(None);
    }
    if map.len() != 4 && !(map.len() == 5 && map.contains_key("reasoning")) {
        return Err("live meta fields mismatch".to_string());
    }
    let message_id = map
        .get("messageId")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "live meta messageId is missing".to_string())?;
    let turn_id = map
        .get("turnId")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "live meta turnId is missing".to_string())?;
    Ok(Some(LiveTextMeta {
        message_id: message_id.clone(),
        turn_id: turn_id.clone(),
        after_sequence: parse_u64(map.get("afterSequence"), "afterSequence")?,
        revision: parse_u64(map.get("revision"), "revision")?,
    }))
}

fn classify_live_error(error: redis::RedisError) -> LiveTextError {
    if matches!(error.kind(), redis::ErrorKind::IoError) {
        LiveTextError::Indeterminate(error.to_string())
    } else {
        LiveTextError::Fatal(error.to_string())
    }
}

// KEYS: [1] live:text [2] live:meta [3] signals
// open:    ARGV = ["open", turnId, messageId, afterSequence, initialText, ttl]
// replace: ARGV = ["replace", fullText, turnId, messageId, expectedRevision, newRevision, signalJson, maxLen, reasoningJson, ttl]
// seal:        ARGV = ["seal", turnId, messageId, expectedRevision, ttl]
// seal_before: ARGV = ["seal_before", turnId, messageId, expectedRevision, committedSequence, ttl]
const LIVE_TEXT_MUTATE_SCRIPT: &str = r#"
local kind = ARGV[1]
local ttl = tonumber(ARGV[#ARGV])
if kind == "open" then
  local existing_after_sequence = tonumber(redis.call("HGET", KEYS[2], "afterSequence"))
  local next_after_sequence = tonumber(ARGV[4])
  if existing_after_sequence and existing_after_sequence >= next_after_sequence then
    return redis.error_reply("live open durable position is not newer")
  end
  redis.call("SET", KEYS[1], ARGV[5])
  redis.call("HDEL", KEYS[2], "reasoning")
  redis.call("HSET", KEYS[2], "turnId", ARGV[2], "messageId", ARGV[3], "afterSequence", ARGV[4], "revision", "0")
  redis.call("EXPIRE", KEYS[1], ttl)
  redis.call("EXPIRE", KEYS[2], ttl)
elseif kind == "replace" then
  if redis.call("HGET", KEYS[2], "messageId") ~= ARGV[4] then
    return redis.error_reply("live meta message identity mismatch")
  end
  if redis.call("HGET", KEYS[2], "turnId") ~= ARGV[3] then
    return redis.error_reply("live meta turn identity mismatch")
  end
  if redis.call("HGET", KEYS[2], "revision") ~= ARGV[5] then
    return redis.error_reply("live revision mismatch")
  end
  redis.call("SET", KEYS[1], ARGV[2])
  redis.call("HSET", KEYS[2], "revision", ARGV[6])
  redis.call("HSET", KEYS[2], "reasoning", ARGV[9])
  local cursor = redis.call("XADD", KEYS[3], "MAXLEN", "~", ARGV[8], "*", "signal", ARGV[7])
  redis.call("EXPIRE", KEYS[1], ttl)
  redis.call("EXPIRE", KEYS[2], ttl)
  redis.call("EXPIRE", KEYS[3], ttl)
  return cursor
elseif kind == "seal" then
  if redis.call("EXISTS", KEYS[2]) == 0 then
    return "ok"
  end
  if redis.call("HGET", KEYS[2], "messageId") ~= ARGV[3] then
    return redis.error_reply("live meta message identity mismatch")
  end
  if redis.call("HGET", KEYS[2], "turnId") ~= ARGV[2] then
    return redis.error_reply("live meta turn identity mismatch")
  end
  if redis.call("HGET", KEYS[2], "revision") ~= ARGV[4] then
    return redis.error_reply("live revision mismatch")
  end
  redis.call("DEL", KEYS[1], KEYS[2])
  redis.call("EXPIRE", KEYS[3], ttl)
  return "ok"
elseif kind == "seal_before" then
  if redis.call("EXISTS", KEYS[2]) == 0 then
    return "missing"
  end
  if redis.call("HGET", KEYS[2], "messageId") ~= ARGV[3] then
    return "retained"
  end
  if redis.call("HGET", KEYS[2], "turnId") ~= ARGV[2] then
    return "retained"
  end
  if redis.call("HGET", KEYS[2], "revision") ~= ARGV[4] then
    return "retained"
  end
  local current_after_sequence = tonumber(redis.call("HGET", KEYS[2], "afterSequence"))
  local committed_sequence = tonumber(ARGV[5])
  if not current_after_sequence or not committed_sequence then
    return redis.error_reply("live supersession sequence is invalid")
  end
  if current_after_sequence >= committed_sequence then
    return "retained"
  end
  redis.call("DEL", KEYS[1], KEYS[2])
  redis.call("EXPIRE", KEYS[3], ttl)
  return "deleted"
else
  return redis.error_reply("unknown live mutation kind: " .. kind)
end
return "ok"
"#;

impl TransientAgentRunStream {
    pub fn connect(
        redis_url: &str,
        agent_run_id: &str,
        ttl_seconds: i64,
        live_ttl_seconds: i64,
    ) -> Result<Self, String> {
        if agent_run_id.trim().is_empty() || ttl_seconds <= 0 || live_ttl_seconds <= 0 {
            return Err("transient stream agentRunId and positive TTLs are required".to_string());
        }
        let client = redis::Client::open(redis_url)
            .map_err(|error| format!("open Redis client failed: {error}"))?;
        let connection = client
            .get_connection()
            .map_err(|error| format!("connect Redis failed: {error}"))?;
        Ok(Self {
            agent_run_id: agent_run_id.to_string(),
            signal_stream_key: signal_stream_key(agent_run_id),
            connection,
            stream_ttl_seconds: ttl_seconds,
            live_ttl_seconds,
            live_revision: 0,
            live_after_sequence: 0,
            live_text: String::new(),
            live_reasoning: None,
            pending_live: None,
            last_live_flush: None,
        })
    }

    pub fn publish_commit_wake(&mut self, high_water_sequence: u64) -> Result<String, String> {
        if high_water_sequence == 0 {
            return Err("commit wake highWaterSequence is invalid".to_string());
        }
        let encoded = serde_json::to_string(&commit_wake_signal(
            self.agent_run_id.as_str(),
            high_water_sequence,
        ))
        .map_err(|error| format!("encode commit wake failed: {error}"))?;
        let cursor = redis::cmd("XADD")
            .arg(self.signal_stream_key.as_str())
            .arg("MAXLEN")
            .arg("~")
            .arg(STREAM_MAX_LENGTH)
            .arg("*")
            .arg(SIGNAL_FIELD)
            .arg(encoded)
            .query::<String>(&mut self.connection)
            .map_err(|error| format!("append Redis commit wake failed: {error}"))?;
        self.connection
            .expire::<_, bool>(self.signal_stream_key.as_str(), self.stream_ttl_seconds)
            .map_err(|error| format!("set Redis signal stream TTL failed: {error}"))?;
        Ok(cursor)
    }

    pub fn live_open(
        &mut self,
        turn_id: &str,
        message_id: &str,
        after_sequence: u64,
        initial_text: &str,
    ) -> Result<(), LiveTextError> {
        if turn_id.trim().is_empty() || message_id.trim().is_empty() {
            return Err(LiveTextError::Fatal(
                "live_open requires non-empty turnId and messageId".to_string(),
            ));
        }
        self.live_flush()?;
        redis::Script::new(LIVE_TEXT_MUTATE_SCRIPT)
            .key(live_text_key(self.agent_run_id.as_str()))
            .key(live_meta_key(self.agent_run_id.as_str()))
            .key(self.signal_stream_key.as_str())
            .arg("open")
            .arg(turn_id)
            .arg(message_id)
            .arg(after_sequence)
            .arg(initial_text)
            .arg(self.live_ttl_seconds)
            .invoke::<String>(&mut self.connection)
            .map(|_| ())
            .map_err(classify_live_error)?;
        self.live_revision = 0;
        self.live_after_sequence = after_sequence;
        self.live_text = initial_text.to_string();
        self.live_reasoning = None;
        self.pending_live = None;
        self.last_live_flush = None;
        Ok(())
    }

    pub fn live_replace(
        &mut self,
        turn_id: &str,
        message_id: &str,
        full_text: &str,
    ) -> Result<(), LiveTextError> {
        if turn_id.trim().is_empty() || message_id.trim().is_empty() {
            return Err(LiveTextError::Fatal(
                "live replacement requires non-empty turnId and messageId".to_string(),
            ));
        }
        self.live_text.clear();
        self.live_text.push_str(full_text);
        self.pending_live = Some(PendingLiveText {
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
        });
        self.flush_live_if_due()
    }

    pub fn live_append_delta(
        &mut self,
        turn_id: &str,
        message_id: &str,
        delta: &str,
    ) -> Result<(), LiveTextError> {
        if turn_id.trim().is_empty() || message_id.trim().is_empty() {
            return Err(LiveTextError::Fatal(
                "live delta requires non-empty turnId and messageId".to_string(),
            ));
        }
        self.live_text.push_str(delta);
        self.pending_live = Some(PendingLiveText {
            turn_id: turn_id.to_string(),
            message_id: message_id.to_string(),
        });
        self.flush_live_if_due()
    }

    fn flush_live_if_due(&mut self) -> Result<(), LiveTextError> {
        if live_flush_is_due(self.last_live_flush, Instant::now()) {
            self.live_flush()?;
        }
        Ok(())
    }

    pub fn live_reasoning(
        &mut self,
        turn_id: &str,
        request_id: &str,
        text: &str,
    ) -> Result<(), LiveTextError> {
        self.live_reasoning = Some(
            serde_json::json!({"blockId":format!("reasoning:{request_id}"),"requestId":request_id,"text":text}),
        );
        self.pending_live = Some(PendingLiveText {
            turn_id: turn_id.to_string(),
            message_id: format!("message:{turn_id}:assistant"),
        });
        self.flush_live_if_due()
    }

    pub fn read_live_reasoning(&mut self) -> Result<Option<serde_json::Value>, String> {
        let text: Option<String> = self
            .connection
            .hget(live_meta_key(&self.agent_run_id), "reasoning")
            .map_err(|error| format!("read live reasoning failed: {error}"))?;
        text.map(|text| {
            serde_json::from_str(&text).map_err(|error| format!("invalid live reasoning: {error}"))
        })
        .transpose()
        .map(Option::flatten)
    }

    pub fn live_flush(&mut self) -> Result<(), LiveTextError> {
        let Some(pending) = self.pending_live.as_ref() else {
            return Ok(());
        };
        let new_revision = self
            .live_revision
            .checked_add(1)
            .ok_or_else(|| LiveTextError::Fatal("live revision exhausted".to_string()))?;
        let encoded = serde_json::to_string(&serde_json::json!({
            "schema": SIGNAL_SCHEMA,
            "kind": "live",
            "agentRunId": self.agent_run_id,
            "afterSequence": self.live_after_sequence,
            "revision": new_revision,
            "turnId": pending.turn_id,
            "messageId": pending.message_id,
            "text": self.live_text,
            "reasoning": self.live_reasoning,
        }))
        .map_err(|error| {
            LiveTextError::Fatal(format!("encode live stream event failed: {error}"))
        })?;
        let result = redis::Script::new(LIVE_TEXT_MUTATE_SCRIPT)
            .key(live_text_key(self.agent_run_id.as_str()))
            .key(live_meta_key(self.agent_run_id.as_str()))
            .key(self.signal_stream_key.as_str())
            .arg("replace")
            .arg(self.live_text.as_str())
            .arg(pending.turn_id.as_str())
            .arg(pending.message_id.as_str())
            .arg(self.live_revision)
            .arg(new_revision)
            .arg(encoded)
            .arg(STREAM_MAX_LENGTH)
            .arg(
                serde_json::to_string(&self.live_reasoning)
                    .map_err(|error| LiveTextError::Fatal(error.to_string()))?,
            )
            .arg(self.live_ttl_seconds)
            .invoke::<String>(&mut self.connection);
        if let Err(error) = result {
            self.pending_live = None;
            return Err(classify_live_error(error));
        }
        self.live_revision = new_revision;
        self.pending_live = None;
        self.last_live_flush = Some(Instant::now());
        Ok(())
    }

    pub fn live_seal_before_sequence(
        &mut self,
        turn_id: &str,
        message_id: &str,
        committed_sequence: u64,
    ) -> Result<bool, LiveTextError> {
        if turn_id.trim().is_empty() || message_id.trim().is_empty() || committed_sequence == 0 {
            return Err(LiveTextError::Fatal(
                "live_seal_before_sequence requires non-empty identities and a committed sequence"
                    .to_string(),
            ));
        }
        self.live_flush()?;
        let result = redis::Script::new(LIVE_TEXT_MUTATE_SCRIPT)
            .key(live_text_key(self.agent_run_id.as_str()))
            .key(live_meta_key(self.agent_run_id.as_str()))
            .key(self.signal_stream_key.as_str())
            .arg("seal_before")
            .arg(turn_id)
            .arg(message_id)
            .arg(self.live_revision)
            .arg(committed_sequence)
            .arg(self.live_ttl_seconds)
            .invoke::<String>(&mut self.connection)
            .map_err(classify_live_error)?;
        match result.as_str() {
            "deleted" => {
                self.live_text.clear();
                self.pending_live = None;
                self.last_live_flush = None;
                Ok(true)
            }
            "missing" | "retained" => Ok(false),
            other => Err(LiveTextError::Fatal(format!(
                "live supersession returned an unknown outcome: {other}"
            ))),
        }
    }

    pub fn settle_live_meta(&mut self, meta: &LiveTextMeta) -> Result<(), LiveTextError> {
        self.live_seal_at_revision(
            meta.turn_id.as_str(),
            meta.message_id.as_str(),
            meta.revision,
        )
    }

    fn live_seal_at_revision(
        &mut self,
        turn_id: &str,
        message_id: &str,
        revision: u64,
    ) -> Result<(), LiveTextError> {
        redis::Script::new(LIVE_TEXT_MUTATE_SCRIPT)
            .key(live_text_key(self.agent_run_id.as_str()))
            .key(live_meta_key(self.agent_run_id.as_str()))
            .key(self.signal_stream_key.as_str())
            .arg("seal")
            .arg(turn_id)
            .arg(message_id)
            .arg(revision)
            .arg(self.live_ttl_seconds)
            .invoke::<String>(&mut self.connection)
            .map(|_| ())
            .map_err(classify_live_error)
    }

    pub fn read_live_text(&mut self) -> Result<Option<String>, String> {
        redis::cmd("GET")
            .arg(live_text_key(self.agent_run_id.as_str()))
            .query::<Option<String>>(&mut self.connection)
            .map_err(|error| format!("read live text failed: {error}"))
    }

    pub fn read_live_meta(&mut self) -> Result<Option<LiveTextMeta>, String> {
        redis::cmd("HGETALL")
            .arg(live_meta_key(self.agent_run_id.as_str()))
            .query::<std::collections::HashMap<String, String>>(&mut self.connection)
            .map_err(|error| format!("read live meta failed: {error}"))
            .and_then(|map| parse_live_meta(&map))
    }
}

fn live_flush_is_due(last_flush: Option<Instant>, now: Instant) -> bool {
    last_flush
        .map(|flushed_at| now.duration_since(flushed_at) >= LIVE_FLUSH_INTERVAL)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires isolated TEST_REASONING_REDIS_URL Docker service"]
    fn redis_reasoning_snapshot_survives_reconnect_and_seals_with_answer() {
        let url = std::env::var("TEST_REASONING_REDIS_URL").expect("isolated test Redis URL");
        let run = format!("reasoning-test-{}", std::process::id());
        let mut stream = TransientAgentRunStream::connect(&url, &run, 60, 60).unwrap();
        stream
            .live_open("turn-1", "message:turn-1:assistant", 1, "")
            .unwrap();
        stream
            .live_reasoning("turn-1", "request-1", "first")
            .unwrap();
        stream
            .live_reasoning("turn-1", "request-1", "first second")
            .unwrap();
        stream
            .live_append_delta("turn-1", "message:turn-1:assistant", "answer")
            .unwrap();
        stream.live_flush().unwrap();
        let mut restored = TransientAgentRunStream::connect(&url, &run, 60, 60).unwrap();
        let thinking = restored.read_live_reasoning().unwrap().unwrap();
        assert_eq!(thinking["text"], "first second");
        let text: String = restored.connection.get(live_text_key(&run)).unwrap();
        assert_eq!(text, "answer");
        let records: redis::streams::StreamRangeReply = restored
            .connection
            .xrevrange_count(signal_stream_key(&run), "+", "-", 1)
            .unwrap();
        let signal: String = records.ids[0].get("signal").unwrap();
        let signal: serde_json::Value = serde_json::from_str(&signal).unwrap();
        assert_eq!(signal["reasoning"], thinking);
        assert_eq!(signal["text"], text);
        assert!(stream
            .live_seal_before_sequence("turn-1", "message:turn-1:assistant", 2)
            .unwrap());
        assert!(restored.read_live_reasoning().unwrap().is_none());
        let _: usize = restored.connection.del(signal_stream_key(&run)).unwrap();
    }
    use std::collections::HashMap;

    fn live_meta_is_superseded(
        meta: &LiveTextMeta,
        turn_id: &str,
        message_id: &str,
        expected_revision: u64,
        committed_sequence: u64,
    ) -> bool {
        meta.turn_id == turn_id
            && meta.message_id == message_id
            && meta.revision == expected_revision
            && meta.after_sequence < committed_sequence
    }

    #[test]
    fn redis_keys_share_the_agent_run_hash_tag() {
        assert_eq!(
            signal_stream_key("agent_run_1"),
            "workspace-agent:agent-run:{agent_run_1}:signals"
        );
        assert_eq!(
            live_text_key("agent_run_1"),
            "workspace-agent:agent-run:{agent_run_1}:live:text"
        );
        assert_eq!(
            live_meta_key("agent_run_1"),
            "workspace-agent:agent-run:{agent_run_1}:live:meta"
        );
    }

    #[test]
    fn live_meta_is_exact_and_ordered_after_a_committed_sequence() {
        let mut map = HashMap::from([
            ("messageId".to_string(), "message:1".to_string()),
            ("turnId".to_string(), "turn:1".to_string()),
            ("afterSequence".to_string(), "42".to_string()),
            ("revision".to_string(), "3".to_string()),
        ]);
        let meta = parse_live_meta(&map).expect("valid").expect("present");
        assert_eq!(meta.after_sequence, 42);
        assert_eq!(meta.revision, 3);
        map.insert("banana".to_string(), "true".to_string());
        assert!(parse_live_meta(&map).is_err());
    }

    #[test]
    fn live_open_is_fenced_by_the_durable_session_position() {
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains(
            "existing_after_sequence and existing_after_sequence >= next_after_sequence"
        ));
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains("live open durable position is not newer"));
    }

    #[test]
    fn live_script_only_buffers_full_replacements() {
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains("XADD"));
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains("SET"));
        assert!(!LIVE_TEXT_MUTATE_SCRIPT.contains("APPEND"));
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains("DEL"));
        assert_eq!(SIGNAL_FIELD, "signal");
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains("\"signal\""));
        assert!(!LIVE_TEXT_MUTATE_SCRIPT.contains("\"event\""));
        assert_eq!(
            LIVE_TEXT_MUTATE_SCRIPT
                .matches("live revision mismatch")
                .count(),
            2
        );
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains("ARGV[8]"));
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains("redis.call(\"SET\", KEYS[1], ARGV[5])"));
        assert_eq!(STREAM_MAX_LENGTH, 256);
    }

    #[test]
    fn live_supersession_requires_identity_revision_and_an_older_anchor() {
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains("kind == \"seal_before\""));
        assert!(LIVE_TEXT_MUTATE_SCRIPT.contains("current_after_sequence >= committed_sequence"));
        assert!(LIVE_TEXT_MUTATE_SCRIPT
            .contains("redis.call(\"HGET\", KEYS[2], \"messageId\") ~= ARGV[3]"));
        assert!(LIVE_TEXT_MUTATE_SCRIPT
            .contains("redis.call(\"HGET\", KEYS[2], \"turnId\") ~= ARGV[2]"));
        assert!(LIVE_TEXT_MUTATE_SCRIPT
            .contains("redis.call(\"HGET\", KEYS[2], \"revision\") ~= ARGV[4]"));
        assert_eq!(
            LIVE_TEXT_MUTATE_SCRIPT
                .matches("return \"retained\"")
                .count(),
            4
        );

        let old = LiveTextMeta {
            turn_id: "turn_1".to_string(),
            message_id: "message:turn_1:assistant".to_string(),
            after_sequence: 41,
            revision: 3,
        };
        assert!(live_meta_is_superseded(
            &old,
            "turn_1",
            "message:turn_1:assistant",
            3,
            42
        ));

        let at_barrier = LiveTextMeta {
            after_sequence: 42,
            ..old.clone()
        };
        assert!(!live_meta_is_superseded(
            &at_barrier,
            "turn_1",
            "message:turn_1:assistant",
            3,
            42
        ));
        assert!(!live_meta_is_superseded(
            &old,
            "turn_2",
            "message:turn_1:assistant",
            3,
            42
        ));
        assert!(!live_meta_is_superseded(
            &old,
            "turn_1",
            "message:turn_2:assistant",
            3,
            42
        ));
        assert!(!live_meta_is_superseded(
            &old,
            "turn_1",
            "message:turn_1:assistant",
            4,
            42
        ));
    }

    #[test]
    fn live_flush_is_immediate_then_coalesced_for_75ms() {
        let flushed_at = Instant::now();
        assert!(live_flush_is_due(None, flushed_at));
        assert!(!live_flush_is_due(
            Some(flushed_at),
            flushed_at + Duration::from_millis(74)
        ));
        assert!(live_flush_is_due(
            Some(flushed_at),
            flushed_at + Duration::from_millis(75)
        ));
    }

    #[test]
    fn commit_wake_is_an_exact_payload_free_hint() {
        let signal = commit_wake_signal("run_1", 42);

        assert_eq!(
            signal,
            serde_json::json!({
                "schema": "agent_run.transient.signal.v1",
                "kind": "commit_wake",
                "agentRunId": "run_1",
                "highWaterSequence": 42,
            })
        );
    }
}
