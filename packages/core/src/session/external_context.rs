use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const EXTERNAL_CONTEXT_SCHEMA_VERSION: &str = "external_context.v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalToolResult {
    pub provider_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub payload: Value,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_timestamp_ms: Option<i64>,
    #[serde(default = "default_external_context_schema_version")]
    pub schema_version: String,
}

impl ExternalToolResult {
    pub fn for_http(
        provider_id: &str,
        tool_name: &str,
        tool_call_id: &str,
        payload: Value,
        updated_at_ms: i64,
    ) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            tool_name: tool_name.to_string(),
            tool_call_id: tool_call_id.to_string(),
            payload,
            source: Some("http.tool".to_string()),
            source_timestamp_ms: Some(updated_at_ms),
            schema_version: EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalContextObject {
    #[serde(default = "default_external_context_schema_version")]
    pub schema_version: String,
    pub object_id: String,
    pub object_kind: String,
    pub source_provider_id: String,
    pub source_tool_name: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub metadata: Value,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalContextPointer {
    pub object_id: String,
    pub object_kind: String,
    pub source: String,
    pub recency: String,
    pub trust: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    pub reason: String,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalContextObjectPayload {
    pub mode: String,
    pub pointer: ExternalContextPointer,
    pub object: ExternalContextObject,
}

impl ExternalContextObjectPayload {
    pub fn to_json_value(&self) -> Result<Value, String> {
        serde_json::to_value(self).map_err(|err| err.to_string())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalContextNormalizationConfig {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub title_path: Option<String>,
    #[serde(default)]
    pub content_path: Option<String>,
    #[serde(default)]
    pub metadata_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalContextObjectLink {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub object_id: String,
    pub source_provider_id: String,
    pub source_tool_name: String,
    pub linked_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListExternalContextObjectsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalContextObjectIndexEntry {
    pub object_id: String,
    pub object_kind: String,
    pub source_provider_id: String,
    pub source_tool_name: String,
    pub title: String,
    pub updated_at_ms: i64,
    pub link_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_linked_at_ms: Option<i64>,
}

pub trait ExternalContextStorePort {
    fn upsert_external_context_object(&self, object: ExternalContextObject) -> Result<(), String>;
    fn load_external_context_object(
        &self,
        object_id: &str,
    ) -> Result<Option<ExternalContextObject>, String>;
    fn link_external_context_object(&self, link: ExternalContextObjectLink) -> Result<(), String>;
    fn load_external_context_object_link(
        &self,
        session_id: &str,
        object_id: &str,
        turn_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<ExternalContextObjectLink>, String>;
    fn list_external_context_objects(
        &self,
        req: ListExternalContextObjectsRequest,
    ) -> Result<Vec<ExternalContextObjectIndexEntry>, String>;
}

fn normalize_external_result(
    result: &ExternalToolResult,
    config: &ExternalContextNormalizationConfig,
) -> Result<ExternalContextObject, String> {
    let kind = config
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("externalKnowledge")
        .to_string();
    let title = normalize_external_text(
        match normalize_config_path(config.title_path.as_deref()) {
            Some(path) => extract_first_json_path_text(&result.payload, path)
                .ok_or_else(|| format!("external context title path produced no value: {path}"))?,
            None => format!("{} result", result.tool_name),
        }
        .as_str(),
    );
    let content = normalize_external_text(
        match normalize_config_path(config.content_path.as_deref()) {
            Some(path) => {
                let extracted = extract_json_path_texts(&result.payload, path).join("\n\n");
                if extracted.trim().is_empty() {
                    return Err(format!(
                        "external context content path produced no value: {path}"
                    ));
                }
                extracted
            }
            None => canonical_json_string(&result.payload),
        }
        .as_str(),
    );
    let metadata = canonicalize_json_value(
        match normalize_config_path(config.metadata_path.as_deref()) {
            Some(path) => {
                extract_first_json_path_value(&result.payload, path).ok_or_else(|| {
                    format!("external context metadata path produced no value: {path}")
                })?
            }
            None => json!({}),
        },
    );
    let object_id = build_external_context_object_id(
        result.provider_id.as_str(),
        result.tool_name.as_str(),
        kind.as_str(),
        title.as_str(),
        content.as_str(),
        &metadata,
    );
    Ok(ExternalContextObject {
        schema_version: normalize_schema_version(result.schema_version.as_str()),
        object_id,
        object_kind: kind,
        source_provider_id: result.provider_id.clone(),
        source_tool_name: result.tool_name.clone(),
        title,
        content,
        metadata,
        updated_at_ms: result.source_timestamp_ms.unwrap_or_default(),
    })
}

fn project_context_pointer(
    object: &ExternalContextObject,
    source: &str,
    reason: &str,
) -> ExternalContextPointer {
    ExternalContextPointer {
        object_id: object.object_id.clone(),
        object_kind: object.object_kind.clone(),
        source: source.trim().to_string(),
        recency: "warm".to_string(),
        trust: "raw".to_string(),
        score: Some(0.72),
        reason: reason.to_string(),
        updated_at_ms: object.updated_at_ms,
    }
}

pub fn build_external_context_object_id(
    provider_id: &str,
    tool_name: &str,
    object_kind: &str,
    title: &str,
    content: &str,
    metadata: &Value,
) -> String {
    let preimage = json!({
        "schemaVersion": EXTERNAL_CONTEXT_SCHEMA_VERSION,
        "providerId": provider_id.trim(),
        "toolName": tool_name.trim(),
        "objectKind": object_kind.trim(),
        "title": normalize_external_text(title),
        "content": normalize_external_text(content),
        "metadata": canonicalize_json_value(metadata.clone()),
    });
    format!(
        "external_context:{:016x}",
        stable_fnv1a64(canonical_json_string(&preimage).as_str())
    )
}

pub fn canonical_json_string(value: &Value) -> String {
    canonicalize_json_value(value.clone()).to_string()
}

pub fn canonicalize_json_value(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(canonicalize_json_value)
                .collect::<Vec<_>>(),
        ),
        Value::Object(object) => {
            let mut keys = object.keys().cloned().collect::<Vec<_>>();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                if let Some(value) = object.get(key.as_str()) {
                    sorted.insert(key, canonicalize_json_value(value.clone()));
                }
            }
            Value::Object(sorted)
        }
        Value::String(text) => Value::String(normalize_external_text(text.as_str())),
        other => other,
    }
}

fn stable_fnv1a64(input: &str) -> u64 {
    let mut hash = 1469598103934665603u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

fn normalize_external_text(raw: &str) -> String {
    raw.trim().replace("\r\n", "\n")
}

pub fn build_http_external_object_output(
    provider_id: &str,
    tool_name: &str,
    tool_call_id: &str,
    config: &ExternalContextNormalizationConfig,
    body: &Value,
    updated_at_ms: i64,
) -> Result<ExternalContextObjectPayload, String> {
    let result = ExternalToolResult::for_http(
        provider_id,
        tool_name,
        tool_call_id,
        body.clone(),
        updated_at_ms,
    );
    let object = normalize_external_result(&result, config)?;
    let pointer = project_context_pointer(
        &object,
        "http.tool",
        format!("external HTTP provider result from {}", tool_name).as_str(),
    );
    Ok(ExternalContextObjectPayload {
        mode: "externalObject".to_string(),
        pointer,
        object,
    })
}

pub fn build_http_external_object_json(
    provider_id: &str,
    tool_name: &str,
    tool_call_id: &str,
    config: Option<&ExternalContextNormalizationConfig>,
    body: &Value,
    updated_at_ms: i64,
) -> Result<Value, String> {
    let default_config = ExternalContextNormalizationConfig::default();
    build_http_external_object_output(
        provider_id,
        tool_name,
        tool_call_id,
        config.unwrap_or(&default_config),
        body,
        updated_at_ms,
    )?
    .to_json_value()
}

pub fn extract_first_json_path_text(value: &Value, path: &str) -> Option<String> {
    extract_json_path_texts(value, path).into_iter().next()
}

pub fn extract_json_path_texts(value: &Value, path: &str) -> Vec<String> {
    extract_json_path_values(value, path)
        .into_iter()
        .map(|value| match value {
            Value::String(text) => text,
            other => other.to_string(),
        })
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>()
}

pub fn extract_first_json_path_value(value: &Value, path: &str) -> Option<Value> {
    extract_json_path_values(value, path).into_iter().next()
}

pub fn extract_json_path_values(value: &Value, path: &str) -> Vec<Value> {
    let normalized = path.trim().strip_prefix("$.").unwrap_or(path.trim());
    if normalized.is_empty() || normalized == "$" {
        return vec![value.clone()];
    }
    let mut current = vec![value.clone()];
    for segment in normalized.split('.') {
        let is_array_wildcard = segment.ends_with("[*]");
        let key = if is_array_wildcard {
            segment.trim_end_matches("[*]")
        } else {
            segment
        };
        let mut next = vec![];
        for item in current {
            let Some(child) = item.get(key) else {
                continue;
            };
            if is_array_wildcard {
                if let Some(items) = child.as_array() {
                    next.extend(items.iter().cloned());
                }
            } else {
                next.push(child.clone());
            }
        }
        current = next;
        if current.is_empty() {
            break;
        }
    }
    current
}

fn default_external_context_schema_version() -> String {
    EXTERNAL_CONTEXT_SCHEMA_VERSION.to_string()
}

fn normalize_schema_version(schema_version: &str) -> String {
    let trimmed = schema_version.trim();
    if trimmed.is_empty() {
        return default_external_context_schema_version();
    }
    trimmed.to_string()
}

fn normalize_config_path(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        build_http_external_object_json, build_http_external_object_output,
        extract_json_path_values, normalize_external_result, project_context_pointer,
        ExternalContextNormalizationConfig, ExternalContextObject,
    };
    use serde_json::{json, to_value, Value};

    #[test]
    fn json_path_extracts_array_values() {
        let payload = json!({
            "chunks": [
                { "content": "first" },
                { "content": "second" }
            ]
        });

        let extracted = extract_json_path_values(&payload, "$.chunks[*].content");

        assert_eq!(
            extracted,
            vec![
                Value::String("first".to_string()),
                Value::String("second".to_string())
            ]
        );
    }

    #[test]
    fn http_normalizer_builds_external_context_object() {
        let result = super::ExternalToolResult {
            provider_id: "http.knowledge".to_string(),
            tool_name: "external_lookup".to_string(),
            tool_call_id: "call-1".to_string(),
            payload: json!({
                "title": "Clinic guideline",
                "chunks": [{ "content": "first chunk" }, { "content": "second chunk" }],
                "metadata": { "source": "ragflow" }
            }),
            source: Some("http.tool".to_string()),
            source_timestamp_ms: Some(42),
            schema_version: "external_context.v1".to_string(),
        };
        let config = ExternalContextNormalizationConfig {
            kind: Some("externalKnowledge".to_string()),
            title_path: Some("$.title".to_string()),
            content_path: Some("$.chunks[*].content".to_string()),
            metadata_path: Some("$.metadata".to_string()),
        };

        let object = normalize_external_result(&result, &config).expect("normalize");

        assert_eq!(object.object_kind, "externalKnowledge");
        assert_eq!(object.title, "Clinic guideline");
        assert!(object.content.contains("second chunk"));
        assert_eq!(object.updated_at_ms, 42);
    }

    #[test]
    fn projector_can_build_pointer_from_object() {
        let object = ExternalContextObject {
            schema_version: "external_context.v1".to_string(),
            object_id: "obj-1".to_string(),
            object_kind: "externalKnowledge".to_string(),
            source_provider_id: "http.knowledge".to_string(),
            source_tool_name: "external_lookup".to_string(),
            title: "Clinic guideline".to_string(),
            content: "text".to_string(),
            metadata: json!({}),
            updated_at_ms: 99,
        };

        let pointer = project_context_pointer(
            &object,
            "http.tool",
            "external HTTP provider result from external_lookup",
        );

        assert_eq!(pointer.object_id, "obj-1");
        assert_eq!(pointer.source, "http.tool");
        assert_eq!(pointer.updated_at_ms, 99);
    }

    #[test]
    fn external_object_output_contains_pointer_and_object() {
        let config = ExternalContextNormalizationConfig {
            kind: Some("externalKnowledge".to_string()),
            title_path: Some("$.title".to_string()),
            content_path: Some("$.chunks[*].content".to_string()),
            metadata_path: Some("$.metadata".to_string()),
        };
        let output = build_http_external_object_output(
            "http.knowledge",
            "external_lookup",
            "call-2",
            &config,
            &json!({
                "title": "Clinic guideline",
                "chunks": [{ "content": "first chunk" }, { "content": "second chunk" }],
                "metadata": { "source": "ragflow" }
            }),
            123,
        )
        .expect("build external object output");
        let output = to_value(output).expect("serialize external object payload");

        assert_eq!(
            output.get("mode").and_then(Value::as_str),
            Some("externalObject")
        );
        assert_eq!(
            output
                .get("object")
                .and_then(|item| item.get("title"))
                .and_then(Value::as_str),
            Some("Clinic guideline")
        );
        assert_eq!(
            output
                .get("pointer")
                .and_then(|item| item.get("source"))
                .and_then(Value::as_str),
            Some("http.tool")
        );
    }

    #[test]
    fn external_object_json_uses_default_config_when_missing() {
        let output = build_http_external_object_json(
            "http.knowledge",
            "external_lookup",
            "call-3",
            None,
            &json!({
                "answer": "external hit"
            }),
            456,
        )
        .expect("build external object json");

        assert_eq!(
            output
                .get("object")
                .and_then(|item| item.get("objectKind"))
                .and_then(Value::as_str),
            Some("externalKnowledge")
        );
        assert_eq!(
            output
                .get("pointer")
                .and_then(|item| item.get("reason"))
                .and_then(Value::as_str),
            Some("external HTTP provider result from external_lookup")
        );
    }

    #[test]
    fn explicit_external_object_path_miss_fails_loudly() {
        let config = ExternalContextNormalizationConfig {
            kind: Some("externalKnowledge".to_string()),
            title_path: Some("$.missing_title".to_string()),
            content_path: Some("$.answer".to_string()),
            metadata_path: None,
        };

        let err = build_http_external_object_json(
            "http.knowledge",
            "external_lookup",
            "call-missing-path",
            Some(&config),
            &json!({
                "answer": "external hit"
            }),
            456,
        )
        .expect_err("explicit path miss should fail");

        assert!(err.contains("title path produced no value"));
    }

    #[test]
    fn same_http_input_normalizes_to_same_object() {
        let config = ExternalContextNormalizationConfig {
            kind: Some("externalKnowledge".to_string()),
            title_path: Some("$.title".to_string()),
            content_path: Some("$.chunks[*].content".to_string()),
            metadata_path: Some("$.metadata".to_string()),
        };
        let first = super::ExternalToolResult::for_http(
            "http.knowledge",
            "external_lookup",
            "call-stable",
            json!({
                "title": "Clinic guideline",
                "chunks": [{ "content": "first chunk" }, { "content": "second chunk" }],
                "metadata": { "source": "ragflow" }
            }),
            777,
        );
        let second = super::ExternalToolResult::for_http(
            "http.knowledge",
            "external_lookup",
            "call-different",
            json!({
                "title": "Clinic guideline",
                "chunks": [{ "content": "first chunk" }, { "content": "second chunk" }],
                "metadata": { "source": "ragflow" }
            }),
            999,
        );

        let first_object = normalize_external_result(&first, &config).expect("normalize first");
        let second_object = normalize_external_result(&second, &config).expect("normalize second");

        assert_eq!(first_object.object_id, second_object.object_id);
        assert_eq!(first_object.title, second_object.title);
        assert_eq!(first_object.content, second_object.content);
        assert_eq!(first_object.metadata, second_object.metadata);
        assert_eq!(first_object.updated_at_ms, 777);
        assert_eq!(second_object.updated_at_ms, 999);
    }

    #[test]
    fn provider_field_order_change_keeps_same_object_id() {
        let config = ExternalContextNormalizationConfig::default();
        let first = super::ExternalToolResult::for_http(
            "http.knowledge",
            "external_lookup",
            "call-a",
            json!({
                "title": "Clinic guideline",
                "metadata": { "b": 2, "a": 1 },
                "chunks": [{ "content": "first chunk" }, { "content": "second chunk" }]
            }),
            1,
        );
        let second = super::ExternalToolResult::for_http(
            "http.knowledge",
            "external_lookup",
            "call-b",
            json!({
                "chunks": [{ "content": "first chunk" }, { "content": "second chunk" }],
                "metadata": { "a": 1, "b": 2 },
                "title": "Clinic guideline"
            }),
            2,
        );

        let first_object = normalize_external_result(&first, &config).expect("normalize first");
        let second_object = normalize_external_result(&second, &config).expect("normalize second");

        assert_eq!(first_object.object_id, second_object.object_id);
        assert_eq!(
            super::canonical_json_string(&first.payload),
            super::canonical_json_string(&second.payload)
        );
    }

    #[test]
    fn extracted_metadata_is_canonicalized_before_object_id() {
        let config = ExternalContextNormalizationConfig {
            kind: Some("externalKnowledge".to_string()),
            title_path: Some("$.title".to_string()),
            content_path: Some("$.content".to_string()),
            metadata_path: Some("$.metadata".to_string()),
        };
        let first = super::ExternalToolResult::for_http(
            "http.knowledge",
            "external_lookup",
            "call-a",
            json!({
                "title": "Clinic guideline",
                "content": "same content",
                "metadata": { "z": true, "a": { "right": 2, "left": 1 } }
            }),
            1,
        );
        let second = super::ExternalToolResult::for_http(
            "http.knowledge",
            "external_lookup",
            "call-b",
            json!({
                "title": "Clinic guideline",
                "content": "same content",
                "metadata": { "a": { "left": 1, "right": 2 }, "z": true }
            }),
            2,
        );

        let first_object = normalize_external_result(&first, &config).expect("normalize first");
        let second_object = normalize_external_result(&second, &config).expect("normalize second");

        assert_eq!(first_object.object_id, second_object.object_id);
        assert_eq!(first_object.metadata, second_object.metadata);
    }

    #[test]
    fn same_object_projects_to_same_pointer() {
        let object = ExternalContextObject {
            schema_version: "external_context.v1".to_string(),
            object_id: "obj-stable".to_string(),
            object_kind: "externalKnowledge".to_string(),
            source_provider_id: "http.knowledge".to_string(),
            source_tool_name: "external_lookup".to_string(),
            title: "Clinic guideline".to_string(),
            content: "text".to_string(),
            metadata: json!({ "source": "ragflow" }),
            updated_at_ms: 888,
        };

        let first = project_context_pointer(
            &object,
            "http.tool",
            "external HTTP provider result from external_lookup",
        );
        let second = project_context_pointer(
            &object,
            "http.tool",
            "external HTTP provider result from external_lookup",
        );

        assert_eq!(first.object_id, second.object_id);
        assert_eq!(first.object_kind, second.object_kind);
        assert_eq!(first.source, second.source);
        assert_eq!(first.reason, second.reason);
        assert_eq!(first.updated_at_ms, second.updated_at_ms);
    }
}
