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

fn live_wake_signal(
    agent_run_id: &str,
    after_sequence: u64,
    revision: u64,
    pending: &PendingLiveText,
) -> serde_json::Value {
    serde_json::json!({
        "schema": SIGNAL_SCHEMA,
        "kind": "live",
        "agentRunId": agent_run_id,
        "afterSequence": after_sequence,
        "revision": revision,
        "turnId": pending.turn_id,
        "messageId": pending.message_id,
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
    live_reasoning: Option<LiveReasoning>,
    flushed_text_bytes: usize,
    flushed_reasoning_bytes: usize,
    replace_text: bool,
    replace_reasoning: bool,
    pending_live: Option<PendingLiveText>,
    last_live_flush: Option<Instant>,
}

#[derive(Debug, Clone)]
struct PendingLiveText {
    turn_id: String,
    message_id: String,
}

struct LiveReasoning {
    request_id: String,
    text: String,
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

pub(crate) fn live_reasoning_key(agent_run_id: &str) -> String {
    format!("workspace-agent:agent-run:{{{agent_run_id}}}:live:reasoning")
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
    if map.len() != 7
        || !["textResetRevision", "reasoningResetRevision", "reasoning"]
            .iter()
            .all(|field| map.contains_key(*field))
    {
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

// KEYS: [1] live:text [2] live:meta [3] signals [4] live:reasoning
// open:    ARGV = ["open", turnId, messageId, afterSequence, initialText, ttl]
// update: ARGV = ["update", textMode, text, textOffset, turnId, messageId,
// expectedRevision, newRevision, signalJson, maxLen, reasoningMode, reasoning,
// reasoningOffset, reasoningIdentityJson, ttl]
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
  redis.call("SET", KEYS[4], "")
  redis.call("HSET", KEYS[2], "turnId", ARGV[2], "messageId", ARGV[3], "afterSequence", ARGV[4], "revision", "0", "textResetRevision", "0", "reasoningResetRevision", "0", "reasoning", "null")
  redis.call("EXPIRE", KEYS[1], ttl)
  redis.call("EXPIRE", KEYS[2], ttl)
  redis.call("EXPIRE", KEYS[4], ttl)
elseif kind == "update" then
  if redis.call("HGET", KEYS[2], "messageId") ~= ARGV[6] then
    return redis.error_reply("live meta message identity mismatch")
  end
  if redis.call("HGET", KEYS[2], "turnId") ~= ARGV[5] then
    return redis.error_reply("live meta turn identity mismatch")
  end
  if redis.call("HGET", KEYS[2], "revision") ~= ARGV[7] then
    return redis.error_reply("live revision mismatch")
  end
  if ARGV[2] ~= "append" and ARGV[2] ~= "replace" then
    return redis.error_reply("unknown live text update mode")
  end
  if ARGV[11] ~= "append" and ARGV[11] ~= "replace" then
    return redis.error_reply("unknown live reasoning update mode")
  end
  if ARGV[2] == "append" and redis.call("STRLEN", KEYS[1]) ~= tonumber(ARGV[4]) then
    return redis.error_reply("live text offset mismatch")
  end
  if ARGV[11] == "append" and (redis.call("STRLEN", KEYS[4]) ~= tonumber(ARGV[13]) or redis.call("HGET", KEYS[2], "reasoning") ~= ARGV[14]) then
    return redis.error_reply("live reasoning offset or identity mismatch")
  end
  if ARGV[2] == "replace" then
    redis.call("SET", KEYS[1], ARGV[3])
    redis.call("HSET", KEYS[2], "textResetRevision", ARGV[8])
  elseif string.len(ARGV[3]) > 0 then
    redis.call("APPEND", KEYS[1], ARGV[3])
  end
  if ARGV[11] == "replace" then
    redis.call("SET", KEYS[4], ARGV[12])
    redis.call("HSET", KEYS[2], "reasoningResetRevision", ARGV[8])
  elseif string.len(ARGV[12]) > 0 then
    redis.call("APPEND", KEYS[4], ARGV[12])
  end
  redis.call("HSET", KEYS[2], "revision", ARGV[8], "reasoning", ARGV[14])
  local cursor = redis.call("XADD", KEYS[3], "MAXLEN", "=", ARGV[10], "*", "signal", ARGV[9])
  redis.call("EXPIRE", KEYS[1], ttl)
  redis.call("EXPIRE", KEYS[2], ttl)
  redis.call("EXPIRE", KEYS[3], ttl)
  redis.call("EXPIRE", KEYS[4], ttl)
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
  redis.call("DEL", KEYS[1], KEYS[2], KEYS[4])
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
  redis.call("DEL", KEYS[1], KEYS[2], KEYS[4])
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
            flushed_text_bytes: 0,
            flushed_reasoning_bytes: 0,
            replace_text: false,
            replace_reasoning: false,
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
            .arg("=")
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
            .key(live_reasoning_key(self.agent_run_id.as_str()))
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
        self.flushed_text_bytes = initial_text.len();
        self.flushed_reasoning_bytes = 0;
        self.replace_text = false;
        self.replace_reasoning = false;
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
        self.replace_text |= !full_text.starts_with(&self.live_text);
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
        self.replace_reasoning |= self.live_reasoning.as_ref().is_none_or(|previous| {
            previous.request_id != request_id || !text.starts_with(&previous.text)
        });
        self.live_reasoning = Some(LiveReasoning {
            request_id: request_id.to_string(),
            text: text.to_string(),
        });
        self.pending_live = Some(PendingLiveText {
            turn_id: turn_id.to_string(),
            message_id: format!("message:{turn_id}:assistant"),
        });
        self.flush_live_if_due()
    }

    pub fn read_live_reasoning(&mut self) -> Result<Option<serde_json::Value>, String> {
        let (identity, text): (Option<String>, Option<String>) = redis::pipe()
            .atomic()
            .hget(live_meta_key(&self.agent_run_id), "reasoning")
            .get(live_reasoning_key(&self.agent_run_id))
            .query(&mut self.connection)
            .map_err(|error| format!("read live reasoning failed: {error}"))?;
        let Some(identity) = identity else {
            return Ok(None);
        };
        let mut identity: serde_json::Value = serde_json::from_str(&identity)
            .map_err(|error| format!("invalid live reasoning: {error}"))?;
        if identity.is_null() {
            return Ok(None);
        }
        let object = identity
            .as_object_mut()
            .ok_or("invalid live reasoning identity")?;
        if object.len() != 2 || !object.contains_key("blockId") || !object.contains_key("requestId")
        {
            return Err("invalid live reasoning identity".to_string());
        }
        object.insert(
            "text".to_string(),
            serde_json::Value::String(text.ok_or("live reasoning text is missing")?),
        );
        Ok(Some(identity))
    }

    pub fn live_flush(&mut self) -> Result<(), LiveTextError> {
        let Some(pending) = self.pending_live.as_ref() else {
            return Ok(());
        };
        let new_revision = self
            .live_revision
            .checked_add(1)
            .ok_or_else(|| LiveTextError::Fatal("live revision exhausted".to_string()))?;
        // Wakes retain only positions. Redis holds one complete snapshot, while
        // ordinary publication transmits only the newly appended UTF-8 bytes.
        let encoded = serde_json::to_string(&live_wake_signal(
            &self.agent_run_id,
            self.live_after_sequence,
            new_revision,
            pending,
        ))
        .map_err(|error| {
            LiveTextError::Fatal(format!("encode live stream event failed: {error}"))
        })?;
        let reasoning = self.live_reasoning.as_ref().map(|value| {
            (serde_json::json!({"blockId": format!("reasoning:{}", value.request_id), "requestId": value.request_id}),
             value.text.as_str())
        });
        let reasoning_text = reasoning.as_ref().map_or("", |(_, text)| *text);
        let reasoning_identity =
            serde_json::to_string(&reasoning.as_ref().map(|(identity, _)| identity))
                .map_err(|error| LiveTextError::Fatal(error.to_string()))?;
        let text_value = if self.replace_text {
            self.live_text.as_str()
        } else {
            &self.live_text[self.flushed_text_bytes..]
        };
        let reasoning_value = if self.replace_reasoning {
            reasoning_text
        } else {
            &reasoning_text[self.flushed_reasoning_bytes..]
        };
        let result = redis::Script::new(LIVE_TEXT_MUTATE_SCRIPT)
            .key(live_text_key(self.agent_run_id.as_str()))
            .key(live_meta_key(self.agent_run_id.as_str()))
            .key(self.signal_stream_key.as_str())
            .key(live_reasoning_key(self.agent_run_id.as_str()))
            .arg("update")
            .arg(if self.replace_text {
                "replace"
            } else {
                "append"
            })
            .arg(text_value)
            .arg(self.flushed_text_bytes)
            .arg(pending.turn_id.as_str())
            .arg(pending.message_id.as_str())
            .arg(self.live_revision)
            .arg(new_revision)
            .arg(encoded)
            .arg(STREAM_MAX_LENGTH)
            .arg(if self.replace_reasoning {
                "replace"
            } else {
                "append"
            })
            .arg(reasoning_value)
            .arg(self.flushed_reasoning_bytes)
            .arg(reasoning_identity)
            .arg(self.live_ttl_seconds)
            .invoke::<String>(&mut self.connection);
        if let Err(error) = result {
            self.pending_live = None;
            return Err(classify_live_error(error));
        }
        self.live_revision = new_revision;
        self.flushed_text_bytes = self.live_text.len();
        self.flushed_reasoning_bytes = reasoning_text.len();
        self.replace_text = false;
        self.replace_reasoning = false;
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
            .key(live_reasoning_key(self.agent_run_id.as_str()))
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
                self.live_reasoning = None;
                self.flushed_text_bytes = 0;
                self.flushed_reasoning_bytes = 0;
                self.replace_text = false;
                self.replace_reasoning = false;
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
            .key(live_reasoning_key(self.agent_run_id.as_str()))
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
    #[ignore = "requires isolated TEST_REASONING_REDIS_URL service"]
    fn redis_signal_history_has_an_exact_cap_for_live_and_commit_wakes() {
        let url = std::env::var("TEST_REASONING_REDIS_URL").expect("isolated test Redis URL");
        let run = format!("wake-cap-{}", std::process::id());
        let mut stream = TransientAgentRunStream::connect(&url, &run, 60, 60).unwrap();
        stream
            .live_open("turn-1", "message:turn-1:assistant", 4, "")
            .unwrap();
        for index in 0..300 {
            stream
                .live_replace("turn-1", "message:turn-1:assistant", "answer")
                .unwrap();
            stream.live_flush().unwrap();
            let count: usize = stream.connection.xlen(signal_stream_key(&run)).unwrap();
            assert!(
                count <= STREAM_MAX_LENGTH,
                "live wake count {count} exceeds exact cap"
            );
            stream.publish_commit_wake(index + 5).unwrap();
            let count: usize = stream.connection.xlen(signal_stream_key(&run)).unwrap();
            assert!(
                count <= STREAM_MAX_LENGTH,
                "commit wake count {count} exceeds exact cap"
            );
        }
        let _: usize = stream
            .connection
            .del((
                signal_stream_key(&run),
                live_text_key(&run),
                live_meta_key(&run),
                live_reasoning_key(&run),
            ))
            .unwrap();
    }

    #[test]
    #[ignore = "requires isolated TEST_REASONING_REDIS_URL service with MEMORY USAGE"]
    fn redis_live_history_retains_one_complete_snapshot_with_bounded_wakes() {
        let url = std::env::var("TEST_REASONING_REDIS_URL").expect("isolated test Redis URL");
        let mut streams: Vec<_> = (0..2)
            .map(|index| {
                let run = format!("bounded-live-{}-{index}", std::process::id());
                let mut stream = TransientAgentRunStream::connect(&url, &run, 60, 60).unwrap();
                stream
                    .live_open("turn-1", "message:turn-1:assistant", 4, "")
                    .unwrap();
                stream
            })
            .collect();
        let text = "完整 answer ".repeat(512);
        let mut reasoning = String::new();
        for revision in 1..=320 {
            reasoning.push_str(&"thinking 完整 ".repeat(32));
            for stream in &mut streams {
                stream
                    .live_replace("turn-1", "message:turn-1:assistant", &text)
                    .unwrap();
                stream
                    .live_reasoning("turn-1", "request-1", &reasoning)
                    .unwrap();
                stream.live_flush().unwrap();
                stream.publish_commit_wake(4 + revision).unwrap();
            }
        }
        for stream in &mut streams {
            let run = stream.agent_run_id.clone();
            let mut restored = TransientAgentRunStream::connect(&url, &run, 60, 60).unwrap();
            assert_eq!(
                restored.read_live_text().unwrap().as_deref(),
                Some(text.as_str())
            );
            assert_eq!(
                restored.read_live_reasoning().unwrap().unwrap()["text"],
                reasoning
            );
            let meta = restored.read_live_meta().unwrap().unwrap();
            assert_eq!(meta.after_sequence, 4);
            assert_eq!(meta.turn_id, "turn-1");
            assert_eq!(meta.message_id, "message:turn-1:assistant");
            assert_eq!(meta.revision, stream.live_revision);
            let count: usize = restored.connection.xlen(signal_stream_key(&run)).unwrap();
            assert!(
                count <= STREAM_MAX_LENGTH,
                "wake count {count} exceeds exact cap"
            );
            let mut total_bytes = 0usize;
            for key in [
                signal_stream_key(&run),
                live_text_key(&run),
                live_meta_key(&run),
                live_reasoning_key(&run),
            ] {
                let bytes: usize = redis::cmd("MEMORY")
                    .arg("USAGE")
                    .arg(&key)
                    .query(&mut restored.connection)
                    .unwrap();
                total_bytes += bytes;
                let ttl: i64 = restored.connection.ttl(&key).unwrap();
                assert!((1..=60).contains(&ttl), "transient key must expire");
            }
            let latest_bytes = text.len() + reasoning.len();
            println!(
                "{run}: retainedBytes={total_bytes}, latestSnapshotBytes={latest_bytes}, wakeCount={count}"
            );
            assert!(
                total_bytes <= latest_bytes * 3 + 256 * 1024,
                "retained {total_bytes} bytes for latest snapshot of {latest_bytes} bytes"
            );
            assert!(stream
                .live_seal_before_sequence("turn-1", "message:turn-1:assistant", 5)
                .unwrap());
            assert!(restored.read_live_text().unwrap().is_none());
            assert!(restored.read_live_reasoning().unwrap().is_none());
            let _: usize = restored.connection.del(signal_stream_key(&run)).unwrap();
        }
    }

    #[test]
    #[ignore = "requires isolated TEST_REASONING_REDIS_URL service"]
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
        assert_eq!(
            signal,
            serde_json::json!({
                "schema": SIGNAL_SCHEMA,
                "kind": "live",
                "agentRunId": run,
                "afterSequence": 1,
                "revision": restored.read_live_meta().unwrap().unwrap().revision,
                "turnId": "turn-1",
                "messageId": "message:turn-1:assistant",
            })
        );
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
            ("textResetRevision".to_string(), "0".to_string()),
            ("reasoningResetRevision".to_string(), "0".to_string()),
            ("reasoning".to_string(), "null".to_string()),
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
    #[ignore = "requires isolated TEST_REASONING_REDIS_URL service"]
    fn redis_live_append_replace_and_reasoning_keep_exact_utf8_and_reset_boundaries() {
        let url = std::env::var("TEST_REASONING_REDIS_URL").expect("isolated test Redis URL");
        let run = format!("live-delta-{}", std::process::id());
        let mut stream = TransientAgentRunStream::connect(&url, &run, 60, 60).unwrap();
        stream.live_open("turn", "message", 4, "").unwrap();
        stream
            .live_append_delta("turn", "message", "原内容")
            .unwrap();
        stream.live_flush().unwrap();
        stream.live_replace("turn", "message", "新文本").unwrap();
        stream.live_flush().unwrap();
        let replacement = stream.live_revision;
        stream.live_append_delta("turn", "message", "🚀").unwrap();
        stream.live_reasoning("turn", "request-1", "考虑").unwrap();
        stream.live_flush().unwrap();
        stream
            .live_reasoning("turn", "request-1", "考虑更多")
            .unwrap();
        stream.live_flush().unwrap();
        assert_eq!(
            stream.read_live_text().unwrap().as_deref(),
            Some("新文本🚀")
        );
        assert_eq!(
            stream.read_live_reasoning().unwrap().unwrap()["text"],
            "考虑更多"
        );
        let meta: HashMap<String, String> = stream.connection.hgetall(live_meta_key(&run)).unwrap();
        assert_eq!(meta["textResetRevision"], replacement.to_string());
        assert_eq!(
            meta["reasoning"],
            r#"{"blockId":"reasoning:request-1","requestId":"request-1"}"#
        );
        let before = stream.read_live_meta().unwrap().unwrap();
        let rejected = redis::Script::new(LIVE_TEXT_MUTATE_SCRIPT)
            .key(live_text_key(&run))
            .key(live_meta_key(&run))
            .key(signal_stream_key(&run))
            .key(live_reasoning_key(&run))
            .arg("update")
            .arg("append")
            .arg("bad")
            .arg(999)
            .arg("turn")
            .arg("message")
            .arg(before.revision)
            .arg(before.revision + 1)
            .arg("{}")
            .arg(STREAM_MAX_LENGTH)
            .arg("append")
            .arg("")
            .arg("考虑更多".len())
            .arg(&meta["reasoning"])
            .arg(60)
            .invoke::<String>(&mut stream.connection);
        assert!(rejected
            .unwrap_err()
            .to_string()
            .contains("offset mismatch"));
        assert_eq!(stream.read_live_meta().unwrap().unwrap(), before);
        assert_eq!(
            stream.read_live_text().unwrap().as_deref(),
            Some("新文本🚀")
        );
        stream
            .live_reasoning("turn", "request-2", "另一段")
            .unwrap();
        stream.live_flush().unwrap();
        assert_eq!(
            stream.read_live_reasoning().unwrap().unwrap()["requestId"],
            "request-2"
        );
        stream
            .live_open("next-turn", "next-message", 5, "")
            .unwrap();
        assert!(stream.read_live_reasoning().unwrap().is_none());
        let _: usize = stream
            .connection
            .del((
                signal_stream_key(&run),
                live_text_key(&run),
                live_meta_key(&run),
                live_reasoning_key(&run),
            ))
            .unwrap();
    }

    #[test]
    #[ignore = "requires isolated TEST_REASONING_REDIS_URL service with exclusive traffic"]
    fn redis_live_append_cost_grows_with_new_bytes() {
        fn received(connection: &mut redis::Connection) -> u64 {
            let info: String = redis::cmd("INFO").arg("stats").query(connection).unwrap();
            info.lines()
                .find_map(|line| line.strip_prefix("total_net_input_bytes:"))
                .unwrap()
                .parse()
                .unwrap()
        }
        let url = std::env::var("TEST_REASONING_REDIS_URL").expect("isolated test Redis URL");
        let run = format!("live-cost-{}", std::process::id());
        let mut stream = TransientAgentRunStream::connect(&url, &run, 60, 60).unwrap();
        stream.live_open("turn", "message", 4, "").unwrap();
        let chunk = "流式A\n".repeat(512);
        let before = received(&mut stream.connection);
        for _ in 0..256 {
            stream.live_append_delta("turn", "message", &chunk).unwrap();
            stream.live_flush().unwrap();
        }
        let bytes = received(&mut stream.connection) - before;
        let expected = chunk.repeat(256);
        assert_eq!(
            stream.read_live_text().unwrap().as_deref(),
            Some(expected.as_str())
        );
        assert!(
            bytes < (expected.len() * 2) as u64,
            "{bytes} Redis input bytes for {} generated bytes",
            expected.len()
        );
        println!("generatedBytes={}, redisInputBytes={bytes}", expected.len());
        let _: usize = stream
            .connection
            .del((
                signal_stream_key(&run),
                live_text_key(&run),
                live_meta_key(&run),
                live_reasoning_key(&run),
            ))
            .unwrap();
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
    fn transient_wakes_match_the_shared_api_contract() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/agent-run-transient-signals.json"
        ))
        .unwrap();
        assert_eq!(commit_wake_signal("run-1", 42), fixture["commitWake"]);
        assert_eq!(
            live_wake_signal(
                "run-1",
                4,
                2,
                &PendingLiveText {
                    turn_id: "turn-1".to_string(),
                    message_id: "message:turn-1:assistant".to_string(),
                }
            ),
            fixture["live"]
        );
    }
}
