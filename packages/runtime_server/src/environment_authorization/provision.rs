//! A signed provision intent owns an Environment before a physical incarnation exists.
use super::*;
use crate::agent_run_authorization::EnvironmentProfile;
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskFence, EnvironmentTaskOwner,
};
use centaeris_core::extension::{validate_plugin_activation_snapshot, PluginActivationSnapshotV1};

const SCHEMA: &str = "workspace.environment_provision_authorization.v1";
const DOMAIN: &[u8] = b"workspace:environment-provision-authorization:v1\0";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProvisionBinding {
    pub schema: String,
    pub operation_kind: String,
    pub operation_id: String,
    pub workspace_id: String,
    pub session_id: String,
    pub environment_id: String,
    pub environment_profile: EnvironmentProfile,
    pub environment_profile_digest: String,
    pub user_id: String,
    pub agent_id: String,
    pub plugin_activation: PluginActivationSnapshotV1,
    pub controller_owner: EnvironmentTaskOwner,
    pub controller_epoch: i64,
    pub controller_lease_digest: String,
    pub controller_purpose: EnvironmentTaskControllerPurpose,
    pub request_digest: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProvisionRequest {
    pub kind: String,
    pub environment_profile: EnvironmentProfile,
    pub environment_profile_digest: String,
    pub user_id: String,
    pub agent_id: String,
    pub plugin_activation: PluginActivationSnapshotV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProvisionAuthorization {
    pub payload: ProvisionBinding,
    pub signature: String,
}

impl ProvisionBinding {
    pub(crate) fn request(&self) -> ProvisionRequest {
        ProvisionRequest {
            kind: "provisionEnvironment".into(),
            environment_profile: self.environment_profile.clone(),
            environment_profile_digest: self.environment_profile_digest.clone(),
            user_id: self.user_id.clone(),
            agent_id: self.agent_id.clone(),
            plugin_activation: self.plugin_activation.clone(),
        }
    }
    pub(crate) fn validate_request(&self, request: &ProvisionRequest) -> Result<(), String> {
        if self.schema != SCHEMA
            || self.operation_kind != "provision"
            || self.controller_epoch <= 0
            || self.expires_at_ms <= 0
            || self.controller_purpose != EnvironmentTaskControllerPurpose::Execute
            || self.controller_owner
                != (EnvironmentTaskOwner::MaintenanceOperation {
                    operation_id: self.operation_id.clone(),
                })
        {
            return Err("provision_authorization_invalid".into());
        }
        for id in [
            &self.workspace_id,
            &self.session_id,
            &self.environment_id,
            &self.operation_id,
            &self.user_id,
            &self.agent_id,
        ] {
            if id.is_empty()
                || id.len() > 160
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
            {
                return Err("provision_authorization_identity_invalid".into());
            }
        }
        self.environment_profile.validate()?;
        validate_plugin_activation_snapshot(&self.plugin_activation)?;
        if self.environment_profile.digest()? != self.environment_profile_digest
            || request != &self.request()
        {
            return Err("provision_request_binding_rejected".into());
        }
        for digest in [
            &self.environment_profile_digest,
            &self.controller_lease_digest,
            &self.request_digest,
        ] {
            if !digest.strip_prefix("sha256:").is_some_and(|v| {
                v.len() == 64
                    && v.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            }) {
                return Err("provision_authorization_digest_invalid".into());
            }
        }
        let encoded = crate::agent_run_authorization::canonical_json(
            serde_json::to_value(request).map_err(|_| "provision_request_invalid")?,
        )?;
        if encoded.len() > 64 * 1024
            || format!("sha256:{:x}", Sha256::digest(encoded.as_bytes())) != self.request_digest
        {
            return Err("provision_request_binding_rejected".into());
        }
        Ok(())
    }
    pub(crate) fn fence(&self, instance: &str) -> Result<EnvironmentTaskFence, String> {
        let fence = EnvironmentTaskFence {
            environment_instance_id: instance.into(),
            controller_owner: self.controller_owner.clone(),
            controller_epoch: u64::try_from(self.controller_epoch)
                .map_err(|_| "provision_controller_invalid")?,
            controller_purpose: self.controller_purpose.clone(),
        };
        fence.validate()?;
        Ok(fence)
    }
}

impl ProvisionAuthorization {
    pub(crate) fn verify_current(
        &self,
        current: &ProvisionBinding,
        now_ms: i64,
        key: &[u8],
    ) -> Result<(), String> {
        if &self.payload != current
            || key.is_empty()
            || now_ms < 0
            || now_ms >= current.expires_at_ms
        {
            return Err("provision_authorization_binding_rejected".into());
        }
        self.payload.validate_request(&self.payload.request())?;
        self.verify_signature(key)
    }
    pub(crate) fn digest(&self) -> Result<String, String> {
        self.payload.validate_request(&self.payload.request())?;
        let canonical = crate::agent_run_authorization::canonical_json(
            serde_json::to_value(&self.payload).map_err(|_| "provision_authorization_invalid")?,
        )?;
        Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
    }
    pub(crate) fn verify_signature(&self, key: &[u8]) -> Result<(), String> {
        if key.is_empty() {
            return Err("provision_authorization_key_invalid".into());
        }
        let signature = self
            .signature
            .strip_prefix("hmac-sha256:")
            .filter(|v| {
                v.len() == 64
                    && v.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or("provision_authorization_signature_invalid")?;
        let bytes: Vec<_> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&signature[i..i + 2], 16).expect("validated hex"))
            .collect();
        let digest = self.digest()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| "provision_authorization_key_invalid")?;
        mac.update(DOMAIN);
        mac.update(digest.as_bytes());
        mac.verify_slice(&bytes)
            .map_err(|_| "provision_authorization_signature_invalid".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Vector {
        payload: ProvisionBinding,
        signature: String,
        request: ProvisionRequest,
    }
    fn vector() -> Vector {
        serde_json::from_str(include_str!("../../../../tests/workspace/fixtures/environment_provision_authorization/v1/valid.json")).unwrap()
    }
    #[test]
    fn python_provision_signature_binds_the_independent_owner_and_exact_request() {
        let v = vector();
        let auth = ProvisionAuthorization {
            payload: v.payload,
            signature: v.signature,
        };
        auth.payload.validate_request(&v.request).unwrap();
        auth.verify_current(&auth.payload, 1, b"test-key").unwrap();
        let mut request = v.request;
        request.user_id = "different-owner".into();
        assert!(auth.payload.validate_request(&request).is_err());
        let mut changed = auth.payload.clone();
        changed.controller_epoch += 1;
        assert!(auth.verify_current(&changed, 1, b"test-key").is_err());
        assert!(auth
            .verify_current(&auth.payload, auth.payload.expires_at_ms, b"test-key")
            .is_err());
    }
    #[test]
    fn provision_signature_does_not_authorize_changed_profile_or_plugin_projection() {
        let v = vector();
        let mut auth = ProvisionAuthorization {
            payload: v.payload,
            signature: v.signature,
        };
        auth.payload.agent_id = "other-agent".into();
        auth.payload.request_digest = format!(
            "sha256:{:x}",
            Sha256::digest(
                crate::agent_run_authorization::canonical_json(
                    serde_json::to_value(auth.payload.request()).unwrap()
                )
                .unwrap()
                .as_bytes()
            )
        );
        assert!(auth.verify_current(&auth.payload, 1, b"test-key").is_err());
    }
}
