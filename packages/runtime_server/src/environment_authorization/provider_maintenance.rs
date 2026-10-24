//! Provider task authority is distinct from native container authority.
use super::*;
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskFence, EnvironmentTaskOwner,
};

pub(crate) const SCHEMA: &str = "workspace.provider_environment_maintenance_authorization.v1";
const DOMAIN: &[u8] = b"workspace:provider-environment-maintenance-authorization:v1\0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderMaintenanceTarget {
    pub provider: String,
    pub creation_attempt_id: String,
    pub instance_id: String,
    pub sandbox_id: String,
    pub verification_digest: String,
    pub provider_config_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderMaintenanceBinding {
    pub schema: String,
    pub operation_kind: String,
    pub workspace_id: String,
    pub session_id: String,
    pub environment_id: String,
    pub provider_target: ProviderMaintenanceTarget,
    pub profile_digest: String,
    pub controller_owner: EnvironmentTaskOwner,
    pub controller_epoch: i64,
    pub controller_lease_digest: String,
    pub controller_purpose: EnvironmentTaskControllerPurpose,
    pub request_digest: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderMaintenanceAuthorization {
    pub payload: ProviderMaintenanceBinding,
    pub signature: String,
}

fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identity(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
impl ProviderMaintenanceTarget {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.provider != "e2b"
            || !identity(&self.creation_attempt_id, 256)
            || !identity(&self.sandbox_id, 256)
            || !self.instance_id.strip_prefix("instance_").is_some_and(hex)
            || !self
                .verification_digest
                .strip_prefix("sha256:")
                .is_some_and(hex)
            || !self
                .provider_config_digest
                .strip_prefix("sha256:")
                .is_some_and(hex)
        {
            return Err("provider_maintenance_target_invalid".into());
        }
        Ok(())
    }
}
impl ProviderMaintenanceBinding {
    pub(crate) fn digest(&self) -> Result<String, String> {
        let EnvironmentTaskOwner::MaintenanceOperation { operation_id } = &self.controller_owner
        else {
            return Err("provider_maintenance_authorization_owner_invalid".into());
        };
        if self.schema != SCHEMA
            || self.operation_kind != "task"
            || self.controller_epoch <= 0
            || self.expires_at_ms <= 0
        {
            return Err("provider_maintenance_authorization_fence_invalid".into());
        }
        for value in [
            &self.workspace_id,
            &self.session_id,
            &self.environment_id,
            operation_id,
        ] {
            if !identity(value, 160) {
                return Err("provider_maintenance_authorization_identity_invalid".into());
            }
        }
        self.provider_target.validate()?;
        for value in [
            &self.profile_digest,
            &self.controller_lease_digest,
            &self.request_digest,
        ] {
            if !value.strip_prefix("sha256:").is_some_and(hex) {
                return Err("provider_maintenance_authorization_digest_invalid".into());
            }
        }
        let canonical = crate::agent_run_authorization::canonical_json(
            serde_json::to_value(self)
                .map_err(|_| "provider_maintenance_authorization_encoding_invalid")?,
        )?;
        Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
    }
}

impl ProviderMaintenanceAuthorization {
    /// `current` is reconstructed from locked durable operation, grant and
    /// provider records. Never pass the incoming payload as its own authority.
    pub(crate) fn verify_current(
        &self,
        current: &ProviderMaintenanceBinding,
        now_ms: i64,
        key: &[u8],
    ) -> Result<EnvironmentTaskFence, String> {
        if self.payload != *current
            || key.is_empty()
            || now_ms < 0
            || now_ms >= current.expires_at_ms
        {
            return Err("provider_maintenance_authorization_binding_rejected".into());
        }
        let digest = self.payload.digest()?;
        let encoded = self
            .signature
            .strip_prefix("hmac-sha256:")
            .filter(|value| hex(value))
            .ok_or("provider_maintenance_authorization_signature_invalid")?;
        let bytes: Vec<_> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16).expect("validated hex"))
            .collect();
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| "provider_maintenance_authorization_key_invalid")?;
        mac.update(DOMAIN);
        mac.update(digest.as_bytes());
        mac.verify_slice(&bytes)
            .map_err(|_| "provider_maintenance_authorization_signature_invalid")?;
        let fence = EnvironmentTaskFence {
            environment_instance_id: current.provider_target.instance_id.clone(),
            controller_owner: current.controller_owner.clone(),
            controller_epoch: current.controller_epoch as u64,
            controller_purpose: current.controller_purpose.clone(),
        };
        fence.validate()?;
        Ok(fence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector() -> ProviderMaintenanceAuthorization {
        serde_json::from_str(include_str!("../../../../tests/workspace/fixtures/provider_environment_maintenance_authorization/v1/valid.json")).unwrap()
    }

    #[test]
    fn python_provider_signature_requires_the_exact_current_target_and_fence() {
        let auth = vector();
        let fence = auth.verify_current(&auth.payload, 1, b"test-key").unwrap();
        assert_eq!(
            fence.environment_instance_id,
            auth.payload.provider_target.instance_id
        );
        assert_eq!(fence.controller_owner, auth.payload.controller_owner);
        assert_eq!(fence.controller_epoch, 2);
        for field in [
            "workspaceId",
            "sessionId",
            "environmentId",
            "profileDigest",
            "controllerLeaseDigest",
            "requestDigest",
        ] {
            let mut value = serde_json::to_value(&auth.payload).unwrap();
            value[field] = serde_json::json!(format!("{}x", value[field].as_str().unwrap()));
            let current = serde_json::from_value(value).unwrap();
            assert!(
                auth.verify_current(&current, 1, b"test-key").is_err(),
                "{field}"
            );
        }
        for field in [
            "provider",
            "creationAttemptId",
            "instanceId",
            "sandboxId",
            "verificationDigest",
            "providerConfigDigest",
        ] {
            let mut value = serde_json::to_value(&auth.payload).unwrap();
            value["providerTarget"][field] = serde_json::json!(format!(
                "{}x",
                value["providerTarget"][field].as_str().unwrap()
            ));
            let current = serde_json::from_value(value).unwrap();
            assert!(
                auth.verify_current(&current, 1, b"test-key").is_err(),
                "{field}"
            );
        }
        let mut current = auth.payload.clone();
        current.controller_epoch += 1;
        assert!(auth.verify_current(&current, 1, b"test-key").is_err());
        current = auth.payload.clone();
        current.controller_purpose = EnvironmentTaskControllerPurpose::Cleanup;
        assert!(auth.verify_current(&current, 1, b"test-key").is_err());
        current = auth.payload.clone();
        current.controller_owner = EnvironmentTaskOwner::MaintenanceOperation {
            operation_id: "other".into(),
        };
        assert!(auth.verify_current(&current, 1, b"test-key").is_err());
        for now in [
            -1,
            auth.payload.expires_at_ms,
            auth.payload.expires_at_ms + 1,
        ] {
            assert!(auth
                .verify_current(&auth.payload, now, b"test-key")
                .is_err());
        }
        for key in [b"wrong-key".as_slice(), b"".as_slice()] {
            assert!(auth.verify_current(&auth.payload, 1, key).is_err());
        }
    }

    #[test]
    fn provider_signature_cannot_hide_target_or_authority_tampering() {
        let auth = vector();
        for (field, value) in [
            ("controllerEpoch", serde_json::json!(3)),
            ("expiresAtMs", serde_json::json!(1800000000001_i64)),
            ("controllerPurpose", serde_json::json!("cleanup")),
            (
                "requestDigest",
                serde_json::json!(format!("sha256:{}", "0".repeat(64))),
            ),
        ] {
            let mut value_auth = serde_json::to_value(&auth).unwrap();
            value_auth["payload"][field] = value;
            let altered: ProviderMaintenanceAuthorization =
                serde_json::from_value(value_auth).unwrap();
            assert!(
                altered
                    .verify_current(&altered.payload, 1, b"test-key")
                    .is_err(),
                "{field}"
            );
        }
        let mut changed = auth.clone();
        changed.payload.provider_target.sandbox_id = "another_sandbox".into();
        assert!(changed
            .verify_current(&changed.payload, 1, b"test-key")
            .is_err());
        let mut domain = Hmac::<Sha256>::new_from_slice(b"test-key").unwrap();
        domain.update(b"workspace:environment-maintenance-authorization:v1\0");
        domain.update(auth.payload.digest().unwrap().as_bytes());
        changed = auth.clone();
        changed.signature = format!("hmac-sha256:{:x}", domain.finalize().into_bytes());
        assert!(changed
            .verify_current(&changed.payload, 1, b"test-key")
            .is_err());
    }

    #[test]
    fn provider_authority_rejects_native_aliases_unknown_and_duplicate_fields() {
        let source = include_str!("../../../../tests/workspace/fixtures/provider_environment_maintenance_authorization/v1/valid.json");
        let auth = vector();
        for field in ["containerId", "instanceId", "agentRunId", "futureField"] {
            let mut value = serde_json::to_value(&auth).unwrap();
            value["payload"][field] = serde_json::json!("caller-selected");
            assert!(
                serde_json::from_value::<ProviderMaintenanceAuthorization>(value).is_err(),
                "{field}"
            );
        }
        for field in [
            "provider",
            "creationAttemptId",
            "instanceId",
            "sandboxId",
            "verificationDigest",
            "providerConfigDigest",
        ] {
            let mut value = serde_json::to_value(&auth).unwrap();
            value["payload"]["providerTarget"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                serde_json::from_value::<ProviderMaintenanceAuthorization>(value).is_err(),
                "{field}"
            );
        }
        let mut value = serde_json::to_value(&auth).unwrap();
        value["payload"]["providerTarget"]["containerId"] = serde_json::json!("alias");
        assert!(serde_json::from_value::<ProviderMaintenanceAuthorization>(value).is_err());
        for field in ["sandboxId", "environmentId", "controllerEpoch"] {
            let needle = format!("\"{field}\":");
            let duplicate = source.replacen(&needle, &format!("\"{field}\":null,{needle}"), 1);
            assert!(
                serde_json::from_str::<ProviderMaintenanceAuthorization>(&duplicate).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn provider_authority_validates_bounded_ascii_targets_and_owner() {
        let original = serde_json::to_value(vector().payload).unwrap();
        for (field, invalid) in [
            ("sandboxId", "contains space".to_string()),
            ("sandboxId", "sandbox_中文".to_string()),
            ("sandboxId", "x".repeat(257)),
            ("creationAttemptId", "x".repeat(257)),
            ("instanceId", format!("instance_{}", "a".repeat(63))),
            ("verificationDigest", format!("sha256:{}", "A".repeat(64))),
            ("providerConfigDigest", "sha256:invalid".to_string()),
            ("provider", "native".to_string()),
        ] {
            let mut value = original.clone();
            value["providerTarget"][field] = serde_json::json!(invalid);
            let binding: ProviderMaintenanceBinding = serde_json::from_value(value).unwrap();
            assert!(binding.digest().is_err(), "{field}");
        }
        for (field, invalid) in [
            (
                "schema",
                serde_json::json!("workspace.environment_maintenance_authorization.v1"),
            ),
            ("operationKind", serde_json::json!("provision")),
            ("controllerEpoch", serde_json::json!(0)),
            ("expiresAtMs", serde_json::json!(0)),
            (
                "controllerOwner",
                serde_json::json!({"kind":"agentRun", "runId":"prepare_test"}),
            ),
        ] {
            let mut value = original.clone();
            value[field] = invalid;
            let binding: ProviderMaintenanceBinding = serde_json::from_value(value).unwrap();
            assert!(binding.digest().is_err(), "{field}");
        }
        let mut maximum = vector().payload;
        maximum.controller_epoch = i64::MAX;
        maximum.expires_at_ms = i64::MAX;
        maximum.provider_target.sandbox_id = "x".repeat(256);
        maximum.provider_target.creation_attempt_id = "x".repeat(256);
        assert!(maximum.digest().is_ok());
    }
}
