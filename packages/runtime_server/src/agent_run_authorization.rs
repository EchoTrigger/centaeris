use std::collections::HashSet;

use centaeris_core::execution::MAX_EXECUTION_INPUT_BYTES;
use centaeris_core::extension::{validate_plugin_activation_snapshot, PluginActivationSnapshotV1};
use centaeris_core::tool::inputs::DeclaredInput;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

pub const AGENT_RUN_AUTHORIZATION_SCHEMA: &str = "workspace.agent_run_authorization.v1";
const AGENT_RUN_AUTHORIZATION_SIGNATURE_DOMAIN: &str = "workspace:agent-run-authorization:v1\0";
const MAX_DECLARED_INPUTS: usize = 64;

#[cfg(test)]
mod parity;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceAgentRunAuthorization {
    pub schema: String,
    pub id: String,
    pub organization_id: String,
    pub workspace_id: String,
    pub user_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub agent_run_id: String,
    pub session_workspace: SessionWorkspace,
    pub model_config_ref: String,
    pub thinking_mode: Option<String>,
    pub artifact_scope_ref: String,
    pub asset_refs: Vec<DeclaredInput>,
    pub message_asset_refs: Vec<String>,
    pub image_capability: String,
    pub image_digest: String,
    pub plugin_activation: PluginActivationSnapshotV1,
    pub resources: SandboxResources,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionWorkspace {
    pub generation: u64,
    pub snapshot_sha256: String,
    pub snapshot_size_bytes: u64,
    pub expanded_size_bytes: u64,
    pub file_count: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxResources {
    pub memory_bytes: u64,
    pub cpu_milli: u32,
    pub pids_limit: u32,
    pub data_tmpfs_bytes: u64,
}

impl WorkspaceAgentRunAuthorization {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != AGENT_RUN_AUTHORIZATION_SCHEMA {
            return Err("workspace_agent_run_authorization_schema_mismatch".to_string());
        }
        for (name, value) in [
            ("id", self.id.as_str()),
            ("organizationId", self.organization_id.as_str()),
            ("workspaceId", self.workspace_id.as_str()),
            ("userId", self.user_id.as_str()),
            ("agentId", self.agent_id.as_str()),
            ("sessionId", self.session_id.as_str()),
            ("agentRunId", self.agent_run_id.as_str()),
            ("modelConfigRef", self.model_config_ref.as_str()),
            ("artifactScopeRef", self.artifact_scope_ref.as_str()),
        ] {
            require_opaque_ref(name, value)?;
        }
        validate_agent_id(self.agent_id.as_str())?;
        if let Some(thinking_mode) = &self.thinking_mode {
            require_non_empty("thinkingMode", thinking_mode)?;
            if thinking_mode.chars().count() > 64 || thinking_mode.chars().any(char::is_control) {
                return Err("thinkingMode is invalid".to_string());
            }
        }
        self.session_workspace.validate()?;
        let mut input_refs = HashSet::new();
        if self.asset_refs.len() > MAX_DECLARED_INPUTS {
            return Err("assetRefs must contain at most 64 direct inputs".to_string());
        }
        let mut total_input_bytes = 0_u64;
        let mut previous_input_ref: Option<&str> = None;
        for input in &self.asset_refs {
            input.validate()?;
            if input.size_bytes > MAX_EXECUTION_INPUT_BYTES {
                return Err("declared input exceeds the direct materialization limit".to_string());
            }
            total_input_bytes = total_input_bytes
                .checked_add(input.size_bytes)
                .ok_or_else(|| "declared input aggregate size overflow".to_string())?;
            if let Some(previous) = previous_input_ref {
                if previous >= input.input_ref.as_str() {
                    return Err("declared inputs must be sorted by unique inputRef".to_string());
                }
            }
            previous_input_ref = Some(input.input_ref.as_str());
            if !input_refs.insert(input.input_ref.as_str()) {
                return Err(format!("duplicate inputRef: {}", input.input_ref));
            }
        }
        let mut previous_message_input_ref: Option<&str> = None;
        for input_ref in &self.message_asset_refs {
            if let Some(previous) = previous_message_input_ref {
                if previous >= input_ref.as_str() {
                    return Err(
                        "messageAssetRefs must be sorted unique authorized inputRefs".to_string(),
                    );
                }
            }
            if !input_refs.contains(input_ref.as_str()) {
                return Err(format!(
                    "messageAssetRefs contains unauthorized inputRef: {input_ref}"
                ));
            }
            previous_message_input_ref = Some(input_ref.as_str());
        }
        if self.image_capability != "workspace_general_v1" {
            return Err("workspace AgentRun imageCapability mismatch".to_string());
        }
        validate_sha256("imageDigest", self.image_digest.as_str())?;
        validate_plugin_activation_snapshot(&self.plugin_activation)?;
        if self.resources.memory_bytes == 0
            || self.resources.cpu_milli == 0
            || self.resources.pids_limit == 0
            || self.resources.data_tmpfs_bytes == 0
        {
            return Err("sandbox resources must be positive".to_string());
        }
        if total_input_bytes > self.resources.data_tmpfs_bytes / 2 {
            return Err("declared inputs must fit within half of dataTmpfsBytes".to_string());
        }
        Ok(())
    }

    pub fn validate_agent_run_binding(&self, agent_run_id: &str) -> Result<(), String> {
        self.validate()?;
        if self.agent_run_id != agent_run_id {
            return Err("workspace AgentRun authorization agentRunId mismatch".to_string());
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        self.digest_validated()
    }

    fn digest_validated(&self) -> Result<String, String> {
        let value = serde_json::to_value(self).map_err(|error| {
            format!("serialize workspace AgentRun authorization failed: {error}")
        })?;
        let canonical = canonical_json(value)?;
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        Ok(format!("sha256:{:x}", hasher.finalize()))
    }

    #[cfg(test)]
    pub fn signature(&self, signing_key: &[u8]) -> Result<String, String> {
        if signing_key.is_empty() {
            return Err("workspace AgentRun authorization signing key is required".to_string());
        }
        let digest = self.digest()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(signing_key)
            .map_err(|_| "invalid workspace AgentRun authorization signing key".to_string())?;
        mac.update(AGENT_RUN_AUTHORIZATION_SIGNATURE_DOMAIN.as_bytes());
        mac.update(digest.as_bytes());
        let bytes = mac.finalize().into_bytes();
        Ok(format!(
            "hmac-sha256:{}",
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ))
    }

    #[cfg(test)]
    pub fn verify_signature(&self, signing_key: &[u8], signature: &str) -> Result<(), String> {
        let bytes = decode_signature(signature)?;
        let digest = self.digest()?;
        Self::verify_digest_signature(signing_key, &bytes, &digest)
    }

    fn verify_digest_signature(
        signing_key: &[u8],
        bytes: &[u8],
        digest: &str,
    ) -> Result<(), String> {
        let mut mac = Hmac::<Sha256>::new_from_slice(signing_key)
            .map_err(|_| "invalid workspace AgentRun authorization signing key".to_string())?;
        mac.update(AGENT_RUN_AUTHORIZATION_SIGNATURE_DOMAIN.as_bytes());
        mac.update(digest.as_bytes());
        mac.verify_slice(bytes)
            .map_err(|_| "workspace AgentRun authorization signature mismatch".to_string())
    }
}

fn decode_signature(signature: &str) -> Result<Vec<u8>, String> {
    let encoded = signature
        .strip_prefix("hmac-sha256:")
        .ok_or_else(|| "workspace AgentRun authorization signature format mismatch".to_string())?;
    if encoded.len() != 64 {
        return Err("workspace AgentRun authorization signature format mismatch".to_string());
    }
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&encoded[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "workspace AgentRun authorization signature format mismatch".to_string())?;
    Ok(bytes)
}

pub(crate) struct ValidatedAuthorization<'a>(&'a WorkspaceAgentRunAuthorization);

impl WorkspaceAgentRunAuthorization {
    pub(crate) fn checked_binding(
        &self,
        agent_run_id: &str,
    ) -> Result<ValidatedAuthorization<'_>, String> {
        self.validate_agent_run_binding(agent_run_id)?;
        Ok(ValidatedAuthorization(self))
    }
}

impl ValidatedAuthorization<'_> {
    pub(crate) fn verify(
        self,
        expected_digest: &str,
        key: &[u8],
        signature: &str,
    ) -> Result<(), String> {
        let digest = self.0.digest_validated()?;
        if digest != expected_digest {
            return Err("workspace AgentRun authorization digest mismatch".into());
        }
        let bytes = decode_signature(signature)?;
        WorkspaceAgentRunAuthorization::verify_digest_signature(key, &bytes, &digest)
    }
}

impl SessionWorkspace {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let is_empty_snapshot = self.snapshot_sha256.is_empty()
            && self.snapshot_size_bytes == 0
            && self.expanded_size_bytes == 0
            && self.file_count == 0;
        if self.generation == 0 && !is_empty_snapshot {
            return Err("sessionWorkspace generation zero requires an empty snapshot".to_string());
        }
        if is_empty_snapshot {
            return Ok(());
        }
        validate_sha256(
            "sessionWorkspace.snapshotSha256",
            self.snapshot_sha256.as_str(),
        )?;
        if self.snapshot_size_bytes == 0 || self.file_count == 0 {
            return Err(
                "sessionWorkspace non-empty snapshot requires size and fileCount".to_string(),
            );
        }
        Ok(())
    }
}

fn require_opaque_ref(name: &str, value: &str) -> Result<(), String> {
    require_non_empty(name, value)?;
    if value.contains('/') || value.contains('\\') {
        return Err(format!("{name} must be an opaque ref, not a path"));
    }
    Ok(())
}

fn validate_agent_id(value: &str) -> Result<(), String> {
    if value.chars().count() > 64 || value.chars().any(char::is_control) {
        return Err("agentId is invalid".to_string());
    }
    Ok(())
}

fn require_non_empty(name: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(format!("{name} is required without outer whitespace"));
    }
    if value.nfc().collect::<String>() != value {
        return Err(format!("{name} must use NFC Unicode normalization"));
    }
    Ok(())
}

fn validate_sha256(name: &str, value: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(format!("{name} must use sha256:<hex>"));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{name} must contain 64 lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

fn canonical_json(value: Value) -> Result<String, String> {
    fn sort_value(value: Value) -> Value {
        match value {
            Value::Array(items) => Value::Array(items.into_iter().map(sort_value).collect()),
            Value::Object(items) => {
                let mut entries = items.into_iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, sort_value(value)))
                        .collect(),
                )
            }
            other => other,
        }
    }

    serde_json::to_string(&sort_value(value))
        .map_err(|error| format!("serialize canonical JSON failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn authorization() -> WorkspaceAgentRunAuthorization {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/agent_run_authorization/v1/valid.json"
        ))
        .expect("shared authorization fixture")
    }

    #[test]
    fn authorization_has_a_stable_signed_digest() {
        let authorization = authorization();
        authorization.validate().expect("authorization");
        let digest = authorization.digest().expect("digest");
        assert_eq!(
            digest,
            "sha256:bd93e8ba466c0d9851e805dd2a8c8a5962b351065c4d982ae961ea0cdb0a6a9f"
        );
        assert_eq!(digest, authorization.digest().expect("repeat digest"));
        let signature = authorization.signature(b"test-key").expect("signature");
        assert_eq!(
            signature,
            "hmac-sha256:86d800a4e2517f1b169894646f9a7bb3297770235a7a3a0cbceffe1897969dd5"
        );
        authorization
            .verify_signature(b"test-key", signature.as_str())
            .expect("verify");
    }

    #[test]
    fn authorization_rejects_oversized_direct_input() {
        let mut authorization = authorization();
        authorization.asset_refs[0].size_bytes = MAX_EXECUTION_INPUT_BYTES + 1;
        assert_eq!(
            authorization.validate().expect_err("oversized input"),
            "declared input exceeds the direct materialization limit"
        );
    }

    #[test]
    fn authorization_reserves_half_of_data_tmpfs_for_generated_files() {
        let mut authorization = authorization();
        authorization.resources.data_tmpfs_bytes = 1;
        assert_eq!(
            authorization.validate().expect_err("input aggregate"),
            "declared inputs must fit within half of dataTmpfsBytes"
        );
    }

    #[test]
    fn authorization_validates_session_workspace_shape() {
        let mut authorization = authorization();
        authorization.session_workspace.snapshot_size_bytes = 0;
        assert_eq!(
            authorization.validate().expect_err("malformed workspace"),
            "sessionWorkspace non-empty snapshot requires size and fileCount"
        );
    }
}
