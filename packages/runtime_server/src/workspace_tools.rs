use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use centaeris_core::tool::layer::{
    DynamicToolProvider, DynamicToolProviderRequest, DynamicToolProviderResponse, ToolExecutionFact,
};
use centaeris_core::tool::{DynamicToolContract, ToolTurnBehavior};
use serde::Deserialize;
use serde_json::json;
#[cfg(test)]
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::artifact_publication::{
    ArtifactPublicationReceipt, ArtifactPublicationRequest, WorkspaceArtifactPublicationPort,
};
pub(crate) const ARTIFACT_PROVIDER_ID: &str = "centaeris.artifact";
const PUBLISH_ARTIFACT_TOOL_NAME: &str = "publish_artifact";

pub(crate) fn workspace_tool_contracts() -> Vec<DynamicToolContract> {
    vec![publish_artifact_contract()]
}

fn publish_artifact_contract() -> DynamicToolContract {
    DynamicToolContract {
        name: PUBLISH_ARTIFACT_TOOL_NAME.to_string(),
        category: "runtime.output".to_string(),
        summary: "Publish one completed delivery file from /mnt/data. Call once per file only after writing is complete. Runtime derives the filename, type, size, hash and attachment identity, then appends every successful publication after the final Markdown; do not write attachment links yourself.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "pattern": "^/mnt/data/.+", "description": "Canonical absolute path to one completed regular file inside /mnt/data." }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        provider_id: ARTIFACT_PROVIDER_ID.to_string(),
        scopes: vec![],
        concurrency_safe: false,
        turn_behavior: ToolTurnBehavior::ContinueTurn,
    }
}

pub(crate) struct WorkspaceArtifactToolProvider {
    port: Arc<WorkspaceArtifactPublicationPort>,
}

impl WorkspaceArtifactToolProvider {
    pub(crate) fn new(port: Arc<WorkspaceArtifactPublicationPort>) -> Self {
        Self { port }
    }
}

impl DynamicToolProvider for WorkspaceArtifactToolProvider {
    fn provider_id(&self) -> &str {
        ARTIFACT_PROVIDER_ID
    }

    fn execute<'a>(
        &'a self,
        request: DynamicToolProviderRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DynamicToolProviderResponse, String>> + Send + 'a>>
    {
        let port = self.port.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || execute_publish_artifact(request, port.as_ref()))
                .await
                .map_err(|error| format!("workspace artifact provider task failed: {error}"))?
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishArtifactArgs {
    path: String,
}

fn execute_publish_artifact(
    request: DynamicToolProviderRequest,
    port: &WorkspaceArtifactPublicationPort,
) -> Result<DynamicToolProviderResponse, String> {
    execute_publish_artifact_with(request, |request| port.publish_artifact(request))
}

fn execute_publish_artifact_with(
    request: DynamicToolProviderRequest,
    publish: impl FnOnce(ArtifactPublicationRequest) -> Result<ArtifactPublicationReceipt, String>,
) -> Result<DynamicToolProviderResponse, String> {
    validate_provider_request(&request, PUBLISH_ARTIFACT_TOOL_NAME, ARTIFACT_PROVIDER_ID)?;
    let args = serde_json::from_str::<PublishArtifactArgs>(request.args_json.as_str())
        .map_err(|error| format!("publish_artifact input is invalid: {error}"))?;
    validate_artifact_path(args.path.as_str())?;
    let tool_call_id = request.tool_call_id.clone();
    let receipt = publish(ArtifactPublicationRequest {
        tool_call_id: request.tool_call_id,
        path: args.path,
    })?;
    validate_artifact_receipt(&receipt)?;
    Ok(DynamicToolProviderResponse {
        content: format!("Published {}.", receipt.filename),
        details: json!({
            "schema": "artifact.publication.result.v1",
            "publicationId": receipt.publication_id,
            "artifactRef": receipt.artifact_ref,
            "filename": receipt.filename,
            "sizeBytes": receipt.size_bytes,
            "sha256": receipt.sha256,
            "outputRef": receipt.artifact_ref,
        }),
        is_error: false,
        facts: vec![ToolExecutionFact::ArtifactPublished(json!({
            "publicationId": receipt.publication_id,
            "artifactRef": receipt.artifact_ref,
            "toolCallId": tool_call_id,
            "filename": receipt.filename,
            "sizeBytes": receipt.size_bytes,
            "sha256": receipt.sha256,
        }))],
        transition_reason: Some("workspace_artifact_publication".to_string()),
    })
}

fn validate_artifact_path(path: &str) -> Result<(), String> {
    if !path.starts_with("/mnt/data/")
        || path.ends_with('/')
        || path.trim() != path
        || path.contains('\\')
        || path.nfc().collect::<String>() != path
        || path.chars().any(char::is_control)
        || path
            .trim_start_matches("/mnt/data/")
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(
            "publish_artifact.path must be one canonical absolute file path inside /mnt/data"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_artifact_receipt(receipt: &ArtifactPublicationReceipt) -> Result<(), String> {
    let publication_hash = receipt
        .publication_id
        .strip_prefix("pub_")
        .unwrap_or_default();
    let artifact_id = receipt
        .artifact_ref
        .strip_prefix("artifact:")
        .unwrap_or_default();
    let sha256 = receipt.sha256.strip_prefix("sha256:").unwrap_or_default();
    if publication_hash.len() != 64
        || artifact_id.is_empty()
        || artifact_id.len() > 160
        || sha256.len() != 64
        || publication_hash
            .bytes()
            .chain(sha256.bytes())
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
        || receipt.filename.is_empty()
        || receipt.filename.len() > 255
        || receipt.filename.contains('/')
        || receipt.filename.contains('\\')
    {
        return Err("artifact publication receipt is invalid".to_string());
    }
    Ok(())
}

fn validate_provider_request(
    request: &DynamicToolProviderRequest,
    tool_name: &str,
    provider_id: &str,
) -> Result<(), String> {
    if request.tool_name != tool_name
        || request.contract.name != tool_name
        || request.contract.provider_id.as_deref() != Some(provider_id)
        || request
            .contract
            .schema_hash
            .as_deref()
            .is_none_or(str::is_empty)
        || !request.contract.dynamic
    {
        return Err("workspace dynamic tool identity mismatch".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::tool::DynamicToolRegistry;

    fn assert_snake_case_properties(value: &Value) {
        match value {
            Value::Object(object) => {
                if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                    for key in properties.keys() {
                        assert!(
                            key.chars().all(|character| {
                                character.is_ascii_lowercase()
                                    || character.is_ascii_digit()
                                    || character == '_'
                            }),
                            "model tool parameter is not lower_snake_case: {key}"
                        );
                    }
                }
                for nested in object.values() {
                    assert_snake_case_properties(nested);
                }
            }
            Value::Array(items) => {
                for nested in items {
                    assert_snake_case_properties(nested);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn workspace_contracts_are_exact_and_exclude_retired_knowledge_search() {
        let contracts = workspace_tool_contracts();
        assert_eq!(contracts.len(), 1);
        assert_eq!(contracts[0].name, PUBLISH_ARTIFACT_TOOL_NAME);
        assert_eq!(contracts[0].provider_id, ARTIFACT_PROVIDER_ID);
        assert!(contracts
            .iter()
            .all(|contract| contract.input_schema["additionalProperties"] == json!(false)));
        for contract in &contracts {
            assert_snake_case_properties(&contract.input_schema);
        }
        assert!(!contracts
            .iter()
            .any(|contract| contract.name == "search_knowledge"));
    }

    #[test]
    fn artifact_path_rejects_non_canonical_workspace_paths() {
        assert!(validate_artifact_path("/mnt/data/report.docx").is_ok());
        for path in [
            "report.docx",
            "/mnt/data/../report.docx",
            "/mnt/data/report\\copy.docx",
            "/mnt/data/report.docx/",
        ] {
            assert!(validate_artifact_path(path).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn artifact_contract_produces_a_typed_publication_fact() {
        let registry = DynamicToolRegistry::from_contracts(vec![publish_artifact_contract()])
            .expect("artifact registry");
        let contract = registry
            .find_contract(PUBLISH_ARTIFACT_TOOL_NAME)
            .expect("artifact contract");
        let response = execute_publish_artifact_with(
            DynamicToolProviderRequest {
                tool_call_id: "call_publish".to_string(),
                tool_name: PUBLISH_ARTIFACT_TOOL_NAME.to_string(),
                args_json: r#"{"path":"/mnt/data/report.docx"}"#.to_string(),
                contract,
                cancellation_probe: None,
            },
            |request| {
                Ok(ArtifactPublicationReceipt {
                    publication_id: format!("pub_{}", "a".repeat(64)),
                    artifact_ref: "artifact:report".to_string(),
                    filename: request
                        .path
                        .rsplit('/')
                        .next()
                        .expect("validated path")
                        .to_string(),
                    size_bytes: 42,
                    sha256: format!("sha256:{}", "b".repeat(64)),
                })
            },
        )
        .expect("artifact response");
        assert!(matches!(
            response.facts.as_slice(),
            [ToolExecutionFact::ArtifactPublished(_)]
        ));
    }
}
