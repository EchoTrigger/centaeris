use crate::{commands::RuntimeHostCommand as C, errors::RuntimeHostError as E};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SshTool {
    pub destination: String,
    pub command: String,
    #[serde(default)]
    pub timeout_ms: u64,
    pub connect_timeout_seconds: Option<u32>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SshRequest {
    session_id: String,
    service_instance_id: String,
    operation_id: String,
    destination: String,
    command: String,
    #[serde(default)]
    timeout_ms: u64,
    connect_timeout_seconds: Option<u32>,
}
pub(crate) fn handle(command: C, payload: Value) -> Result<Value, E> {
    let result = match command {
        C::ScheduleManage => {
            crate::schedules::manage(crate::runtime_bridge::deserialize_request(payload)?)
        }
        C::SshStart => {
            let request: SshRequest = crate::runtime_bridge::deserialize_request(payload)?;
            let args = centaeris_runtime::openssh::command_args_with_timeout(
                &request.destination,
                &request.command,
                request.connect_timeout_seconds,
            )
            .map_err(E::invalid_request)?;
            crate::process_sessions::manager()
                .start(crate::process_sessions::StartRequest {
                    session_id: request.session_id,
                    service_instance_id: request.service_instance_id,
                    operation_id: request.operation_id,
                    program: "ssh".into(),
                    args,
                    timeout_ms: request.timeout_ms,
                })
                .and_then(|s| serde_json::to_value(s).map_err(|e| e.to_string()))
        }
        _ => return Err(E::invalid_request("not an automation command")),
    };
    result.map_err(|e| E::new("host_automation_failed", e))
}
// Convert only the explicitly defined tool field spellings. Unknown keys stay
// unknown and are rejected by the strict Host deserializer.
pub(crate) fn tool_request(value: Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| {
                    let key = match key.as_str() {
                        "operation_id" => "operationId",
                        "schedule_id" => "scheduleId",
                        "expected_revision" => "expectedRevision",
                        "provider_id" => "providerId",
                        "thinking_mode" => "thinkingMode",
                        "expires_at" => "expiresAt",
                        _ => &key,
                    }
                    .to_string();
                    (key, tool_request(value))
                })
                .collect(),
        ),
        other => other,
    }
}
pub(crate) fn schedule_tool(value: Value, session_id: &str) -> Result<Value, String> {
    if value.get("action").and_then(Value::as_str) == Some("context") {
        if value.as_object().is_none_or(|object| object.len() != 1) {
            return Err("schedule context accepts only action".into());
        }
        let binding = crate::sessions::runtime_binding_for_session_id(session_id)?;
        let mut config =
            crate::runtime_config::get(crate::runtime_config::AgentRuntimeConfigGetRequest {})?;
        let model = crate::schedules::ModelSelection {
            provider_id: config
                .model_provider_id
                .clone()
                .ok_or("current model provider is not configured")?,
            model: config
                .model
                .clone()
                .ok_or("current model is not configured")?,
            thinking_mode: config.model_thinking_mode.clone(),
        };
        model.apply(&mut config)?;
        let status = crate::schedules::manage(crate::schedules::Request::List {
            limit: None,
            cursor: None,
        })?;
        return Ok(json!({
            "cwd": binding.cwd,
            "model": model,
            "now": chrono::Utc::now().to_rfc3339(),
            "localTime": chrono::Local::now().to_rfc3339(),
            "serviceEnabled": status["serviceEnabled"],
            "scheduleCount": status["total"],
            "machineAwakeRequired": true
        }));
    }
    crate::schedules::manage(
        serde_json::from_value(tool_request(value)).map_err(|e| e.to_string())?,
    )
}
pub(crate) fn tool_schema() -> Value {
    json!({"type":"object","properties":{
        "action":{"type":"string","enum":["context","list","create","update","pause","resume","delete","history","run","service"]},
        "limit":{"type":"integer","minimum":1,"maximum":100},"cursor":{"type":"string"},
        "operation_id":{"type":"string"},"schedule_id":{"type":"string"},"expected_revision":{"type":"integer","minimum":1},"enabled":{"type":"boolean"},
        "spec":{"type":"object","properties":{
            "name":{"type":"string"},"cwd":{"type":"string"},"prompt":{"type":"string"},
            "cron":{"type":["string","null"]},"at":{"type":["integer","null"],"description":"Unix timestamp in milliseconds for a one-shot; exactly one of at/cron must be non-null"},
            "expires_at":{"type":["integer","null"],"description":"Optional absolute expiry in milliseconds for a one-shot; omitted means catch up once whenever the service resumes"},
            "timezone":{"type":"string","description":"IANA time zone, e.g. Asia/Taipei or UTC"},
            "model":{"type":"object","properties":{"provider_id":{"type":"string"},"model":{"type":"string"},"thinking_mode":{"type":["string","null"]}},"required":["provider_id","model","thinking_mode"],"additionalProperties":false}
        },"required":["name","cwd","prompt","cron","at","timezone","model"],"additionalProperties":false}
    },"required":["action"],"additionalProperties":false})
}
