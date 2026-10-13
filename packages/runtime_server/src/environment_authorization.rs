use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;

const DOMAIN: &[u8] = b"workspace:environment-authorization:v1\0";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EnvironmentBinding {
    pub schema: String,
    pub environment_id: String,
    pub session_id: String,
    pub agent_run_id: String,
    pub agent_run_authorization_digest: String,
    pub execution_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EnvironmentAuthorization {
    pub payload: EnvironmentBinding,
    pub signature: String,
}

impl EnvironmentBinding {
    pub(crate) fn digest(&self) -> Result<String, String> {
        if self.schema != "workspace.environment_authorization.v1"
            || self.execution_mode != "perRun"
        {
            return Err("environment_authorization_schema_or_mode_invalid".into());
        }
        for id in [&self.environment_id, &self.session_id, &self.agent_run_id] {
            if id.is_empty()
                || id.len() > 160
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
            {
                return Err("environment_authorization_identity_invalid".into());
            }
        }
        let digest = self
            .agent_run_authorization_digest
            .strip_prefix("sha256:")
            .ok_or("environment_authorization_digest_invalid")?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("environment_authorization_digest_invalid".into());
        }
        let canonical = crate::agent_run_authorization::canonical_json(
            serde_json::to_value(self).map_err(|e| e.to_string())?,
        )?;
        Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
    }
}

impl EnvironmentAuthorization {
    pub(crate) fn verify(
        &self,
        authorization: &WorkspaceAgentRunAuthorization,
        digest: &str,
        key: &[u8],
    ) -> Result<(), String> {
        if self.payload.agent_run_id != authorization.agent_run_id
            || self.payload.session_id != authorization.session_id
            || self.payload.agent_run_authorization_digest != digest
            || key.is_empty()
        {
            return Err("environment_authorization_binding_invalid".into());
        }
        let digest = self.payload.digest()?;
        let encoded = self
            .signature
            .strip_prefix("hmac-sha256:")
            .ok_or("environment_authorization_signature_invalid")?;
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("environment_authorization_signature_invalid".into());
        }
        let bytes: Vec<_> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16).expect("validated hex"))
            .collect();
        let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|e| e.to_string())?;
        mac.update(DOMAIN);
        mac.update(digest.as_bytes());
        mac.verify_slice(&bytes)
            .map_err(|_| "environment_authorization_signature_invalid".into())
    }

    #[cfg(test)]
    pub(crate) fn for_test(authorization: &WorkspaceAgentRunAuthorization, key: &[u8]) -> Self {
        let payload = EnvironmentBinding {
            schema: "workspace.environment_authorization.v1".into(),
            environment_id: "environment_test".into(),
            session_id: authorization.session_id.clone(),
            agent_run_id: authorization.agent_run_id.clone(),
            agent_run_authorization_digest: authorization.digest().unwrap(),
            execution_mode: "perRun".into(),
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
        mac.update(DOMAIN);
        mac.update(payload.digest().unwrap().as_bytes());
        let signature = format!(
            "hmac-sha256:{}",
            mac.finalize()
                .into_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        Self { payload, signature }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_python_signature_binds_run_session_digest_and_mode() {
        let vector: EnvironmentAuthorization = serde_json::from_str(include_str!(
            "../../../tests/workspace/fixtures/environment_authorization/v1/valid.json"
        ))
        .unwrap();
        let authorization: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
            "../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
        ))
        .unwrap();
        let digest = authorization.digest().unwrap();
        vector.verify(&authorization, &digest, b"test-key").unwrap();
        for field in [
            "environmentId",
            "sessionId",
            "agentRunId",
            "agentRunAuthorizationDigest",
            "executionMode",
            "schema",
        ] {
            let mut value = serde_json::to_value(&vector).unwrap();
            value["payload"][field] = "foreign".into();
            let changed: EnvironmentAuthorization = serde_json::from_value(value).unwrap();
            assert!(
                changed
                    .verify(&authorization, &digest, b"test-key")
                    .is_err(),
                "{field}"
            );
        }
        assert!(vector
            .verify(&authorization, &digest, b"wrong-key")
            .is_err());
        assert!(vector.verify(&authorization, &digest, b"").is_err());
        let mut value = serde_json::to_value(&vector).unwrap();
        value["payload"]["instanceId"] = "caller-selected".into();
        assert!(serde_json::from_value::<EnvironmentAuthorization>(value).is_err());
        for field in [
            "schema",
            "environmentId",
            "sessionId",
            "agentRunId",
            "agentRunAuthorizationDigest",
            "executionMode",
        ] {
            let mut value = serde_json::to_value(&vector).unwrap();
            value["payload"].as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<EnvironmentAuthorization>(value).is_err(),
                "{field}"
            );
        }
        for signature in [
            "",
            "hmac-sha256:abc",
            "hmac-sha256:ZZ",
            "unsupported:signature",
        ] {
            let mut changed = vector.clone();
            changed.signature = signature.into();
            assert!(changed
                .verify(&authorization, &digest, b"test-key")
                .is_err());
        }
    }
}
