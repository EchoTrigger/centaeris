//! Maintenance authority is checked against the current locked controller record.
use super::*;
use centaeris_core::execution::{
    EnvironmentTaskControllerPurpose, EnvironmentTaskFence, EnvironmentTaskOwner,
};

pub(crate) const SCHEMA: &str = "workspace.environment_maintenance_authorization.v1";
const DOMAIN: &[u8] = b"workspace:environment-maintenance-authorization:v1\0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MaintenanceBinding {
    pub schema: String,
    pub operation_kind: String,
    pub workspace_id: String,
    pub session_id: String,
    pub environment_id: String,
    pub instance_id: String,
    pub container_id: String,
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
pub(crate) struct MaintenanceAuthorization {
    pub payload: MaintenanceBinding,
    pub signature: String,
}

fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl MaintenanceBinding {
    pub(crate) fn digest(&self) -> Result<String, String> {
        let EnvironmentTaskOwner::MaintenanceOperation { operation_id } = &self.controller_owner
        else {
            return Err("maintenance_authorization_owner_invalid".into());
        };
        if self.schema != SCHEMA
            || self.operation_kind != "task"
            || self.controller_epoch <= 0
            || self.expires_at_ms <= 0
        {
            return Err("maintenance_authorization_fence_invalid".into());
        }
        for value in [
            &self.workspace_id,
            &self.session_id,
            &self.environment_id,
            operation_id,
        ] {
            if value.is_empty()
                || value.len() > 160
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
            {
                return Err("maintenance_authorization_identity_invalid".into());
            }
        }
        if !self.instance_id.strip_prefix("instance_").is_some_and(hex) || !hex(&self.container_id)
        {
            return Err("maintenance_authorization_target_invalid".into());
        }
        for value in [
            &self.profile_digest,
            &self.controller_lease_digest,
            &self.request_digest,
        ] {
            if !value.strip_prefix("sha256:").is_some_and(hex) {
                return Err("maintenance_authorization_digest_invalid".into());
            }
        }
        let canonical = crate::agent_run_authorization::canonical_json(
            serde_json::to_value(self).map_err(|_| "maintenance_authorization_encoding_invalid")?,
        )?;
        Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
    }
}

impl MaintenanceAuthorization {
    /// `current` must come from the locked durable controller/allocation records,
    /// never from the incoming request. A signature alone grants no authority.
    pub(crate) fn verify_current(
        &self,
        current: &MaintenanceBinding,
        now_ms: i64,
        key: &[u8],
    ) -> Result<EnvironmentTaskFence, String> {
        if self.payload != *current
            || key.is_empty()
            || now_ms < 0
            || now_ms >= current.expires_at_ms
        {
            return Err("maintenance_authorization_binding_rejected".into());
        }
        let digest = self.payload.digest()?;
        let encoded = self
            .signature
            .strip_prefix("hmac-sha256:")
            .filter(|value| hex(value))
            .ok_or("maintenance_authorization_signature_invalid")?;
        let bytes: Vec<_> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16).expect("validated hex"))
            .collect();
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| "maintenance_authorization_key_invalid")?;
        mac.update(DOMAIN);
        mac.update(digest.as_bytes());
        mac.verify_slice(&bytes)
            .map_err(|_| "maintenance_authorization_signature_invalid")?;
        let fence = EnvironmentTaskFence {
            environment_instance_id: current.instance_id.clone(),
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

    fn vector() -> MaintenanceAuthorization {
        serde_json::from_str(include_str!("../../../../tests/workspace/fixtures/environment_maintenance_authorization/v1/valid.json")).unwrap()
    }

    #[test]
    fn python_signature_binds_real_maintenance_owner_and_exact_current_target() {
        let grant = vector();
        let fence = grant
            .verify_current(&grant.payload, 1, b"test-key")
            .unwrap();
        assert_eq!(
            fence.controller_owner,
            EnvironmentTaskOwner::MaintenanceOperation {
                operation_id: "prepare_test".into()
            }
        );
        assert_eq!(fence.environment_instance_id, grant.payload.instance_id);
        for field in [
            "workspaceId",
            "sessionId",
            "environmentId",
            "instanceId",
            "containerId",
            "profileDigest",
            "controllerLeaseDigest",
            "requestDigest",
        ] {
            let mut value = serde_json::to_value(&grant.payload).unwrap();
            value[field] = serde_json::json!(format!("{}x", value[field].as_str().unwrap()));
            let changed: MaintenanceBinding = serde_json::from_value(value).unwrap();
            assert!(
                grant.verify_current(&changed, 1, b"test-key").is_err(),
                "{field}"
            );
        }
        let mut current = grant.payload.clone();
        current.controller_epoch += 1;
        assert!(grant.verify_current(&current, 1, b"test-key").is_err());
        let mut current = grant.payload.clone();
        current.controller_owner = EnvironmentTaskOwner::AgentRun {
            run_id: "prepare_test".into(),
        };
        assert!(grant.verify_current(&current, 1, b"test-key").is_err());
        assert!(grant
            .verify_current(&grant.payload, grant.payload.expires_at_ms, b"test-key")
            .is_err());
        assert!(grant
            .verify_current(&grant.payload, -1, b"test-key")
            .is_err());
        assert!(grant
            .verify_current(&grant.payload, 1, b"wrong-key")
            .is_err());
    }

    #[test]
    fn maintenance_wire_rejects_unknown_fields_and_signature_cannot_hide_tampering() {
        let original = serde_json::to_value(vector()).unwrap();
        for (field, replacement) in [
            ("controllerEpoch", serde_json::json!(3)),
            ("expiresAtMs", serde_json::json!(1800000000001i64)),
            ("controllerPurpose", serde_json::json!("cleanup")),
            (
                "requestDigest",
                serde_json::json!(format!("sha256:{}", "f".repeat(64))),
            ),
        ] {
            let mut value = original.clone();
            value["payload"][field] = replacement;
            let altered: MaintenanceAuthorization = serde_json::from_value(value).unwrap();
            assert!(altered
                .verify_current(&altered.payload, 1, b"test-key")
                .is_err());
        }
        let mut unknown = original.clone();
        unknown["payload"]["agentRunId"] = serde_json::json!("fake");
        assert!(serde_json::from_value::<MaintenanceAuthorization>(unknown).is_err());
        let mut unknown = original;
        unknown["payload"]["controllerOwner"]["runId"] = serde_json::json!("fake");
        assert!(serde_json::from_value::<MaintenanceAuthorization>(unknown).is_err());
    }
}
