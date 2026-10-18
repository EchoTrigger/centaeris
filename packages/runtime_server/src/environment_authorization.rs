use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;

pub(crate) mod maintenance;

const DOMAIN: &[u8] = b"workspace:environment-authorization:v1\0";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnvironmentBinding {
    pub schema: String,
    pub environment_id: String,
    pub session_id: String,
    pub agent_run_id: String,
    pub agent_run_authorization_digest: String,
    pub execution_mode: String,
    #[serde(flatten)]
    pub controller: Option<ControllerBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ControllerBinding {
    pub controller_id: String,
    pub controller_epoch: i64,
    pub controller_lease_digest: String,
    pub controller_purpose: String,
}

impl<'de> Deserialize<'de> for EnvironmentBinding {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        struct Fields;
        impl<'de> serde::de::Visitor<'de> for Fields {
            type Value = serde_json::Map<String, serde_json::Value>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an environment binding with unique fields")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut fields = serde_json::Map::new();
                while let Some((name, value)) = map.next_entry::<String, serde_json::Value>()? {
                    if fields.insert(name, value).is_some() {
                        return Err(M::Error::custom("duplicate environment binding field"));
                    }
                }
                Ok(fields)
            }
        }
        let mut fields = deserializer.deserialize_map(Fields)?;
        let retained = match fields.get("schema").and_then(|v| v.as_str()) {
            Some("workspace.environment_authorization.v1") => false,
            Some("workspace.environment_authorization.v2") => true,
            _ => return Err(D::Error::custom("environment_authorization_schema_invalid")),
        };
        let controller_names = [
            "controllerId",
            "controllerEpoch",
            "controllerLeaseDigest",
            "controllerPurpose",
        ];
        let controller = if retained {
            let mut values = serde_json::Map::new();
            for name in controller_names {
                values.insert(
                    name.into(),
                    fields
                        .remove(name)
                        .ok_or_else(|| D::Error::missing_field(name))?,
                );
            }
            Some(
                serde_json::from_value(serde_json::Value::Object(values))
                    .map_err(D::Error::custom)?,
            )
        } else {
            None
        };
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Common {
            schema: String,
            environment_id: String,
            session_id: String,
            agent_run_id: String,
            agent_run_authorization_digest: String,
            execution_mode: String,
        }
        let common: Common =
            serde_json::from_value(serde_json::Value::Object(fields)).map_err(D::Error::custom)?;
        Ok(Self {
            schema: common.schema,
            environment_id: common.environment_id,
            session_id: common.session_id,
            agent_run_id: common.agent_run_id,
            agent_run_authorization_digest: common.agent_run_authorization_digest,
            execution_mode: common.execution_mode,
            controller,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct EnvironmentAuthorization {
    pub payload: EnvironmentBinding,
    pub signature: String,
}

impl EnvironmentBinding {
    pub(crate) fn digest(&self) -> Result<String, String> {
        match (&*self.schema, &*self.execution_mode, &self.controller) {
            ("workspace.environment_authorization.v1", "perRun", None) => {}
            ("workspace.environment_authorization.v2", "retained", Some(controller)) => {
                if controller.controller_id != self.agent_run_id
                    || controller.controller_epoch < 1
                    || !matches!(
                        controller.controller_purpose.as_str(),
                        "execute" | "cleanup"
                    )
                    || !controller
                        .controller_lease_digest
                        .strip_prefix("sha256:")
                        .is_some_and(|hex| {
                            hex.len() == 64
                                && hex
                                    .bytes()
                                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        })
                {
                    return Err("environment_controller_binding_invalid".into());
                }
            }
            _ => return Err("environment_authorization_schema_or_mode_invalid".into()),
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
        mac.update(if self.payload.controller.is_some() {
            b"workspace:environment-authorization:v2\0"
        } else {
            DOMAIN
        });
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
            controller: None,
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
    fn controller_signature_binds_epoch_lease_and_purpose() {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/workspace/fixtures/environment_authorization/v2/valid.json"
        ))
        .unwrap();
        let authorization: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
            "../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
        ))
        .unwrap();
        let digest = authorization.digest().unwrap();
        let signed: EnvironmentAuthorization = serde_json::from_value(value.clone()).unwrap();
        signed.verify(&authorization, &digest, b"test-key").unwrap();
        for (field, replacement) in [
            ("controllerEpoch", serde_json::json!(2)),
            (
                "controllerLeaseDigest",
                serde_json::json!(format!("sha256:{}", "0".repeat(64))),
            ),
            ("controllerPurpose", serde_json::json!("cleanup")),
        ] {
            let mut changed = value.clone();
            changed["payload"][field] = replacement;
            let signed: EnvironmentAuthorization = serde_json::from_value(changed).unwrap();
            assert!(
                signed.verify(&authorization, &digest, b"test-key").is_err(),
                "{field}"
            );
        }
        for field in [
            "controllerId",
            "controllerEpoch",
            "controllerLeaseDigest",
            "controllerPurpose",
        ] {
            let mut missing = value.clone();
            missing["payload"].as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<EnvironmentAuthorization>(missing).is_err(),
                "{field}"
            );
        }
        let mut mixed = value;
        mixed["payload"]["schema"] = "workspace.environment_authorization.v1".into();
        mixed["payload"]["executionMode"] = "perRun".into();
        assert!(serde_json::from_value::<EnvironmentAuthorization>(mixed).is_err());
    }

    #[test]
    fn controller_binding_rejects_invalid_authority_and_duplicate_fields() {
        let original: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/workspace/fixtures/environment_authorization/v2/valid.json"
        ))
        .unwrap();
        for (field, value) in [
            ("controllerEpoch", serde_json::json!(0)),
            ("controllerEpoch", serde_json::json!(-1)),
            ("controllerEpoch", serde_json::json!(true)),
            ("controllerEpoch", serde_json::json!(1.5)),
            ("controllerEpoch", serde_json::json!("1")),
            ("controllerEpoch", serde_json::json!(null)),
            ("controllerEpoch", serde_json::json!(i64::MAX as u64 + 1)),
            ("controllerEpoch", serde_json::json!(u64::MAX)),
            ("controllerPurpose", serde_json::json!("destroy")),
            ("controllerId", serde_json::json!("another-run")),
            ("controllerLeaseDigest", serde_json::json!("sha256:invalid")),
        ] {
            let mut changed = original.clone();
            changed["payload"][field] = value;
            if let Ok(signed) = serde_json::from_value::<EnvironmentAuthorization>(changed) {
                assert!(signed.payload.digest().is_err(), "{field}");
            }
        }
        let mut maximum = original.clone();
        maximum["payload"]["controllerEpoch"] = serde_json::json!(i64::MAX);
        assert!(serde_json::from_value::<EnvironmentAuthorization>(maximum)
            .unwrap()
            .payload
            .digest()
            .is_ok());
        for name in ["controller_epoch", "instanceId", "futureField"] {
            let mut changed = original.clone();
            changed["payload"][name] = serde_json::json!(1);
            assert!(serde_json::from_value::<EnvironmentAuthorization>(changed).is_err());
        }
        for source in [
            include_str!(
                "../../../tests/workspace/fixtures/environment_authorization/v1/valid.json"
            ),
            include_str!(
                "../../../tests/workspace/fixtures/environment_authorization/v2/valid.json"
            ),
        ] {
            let duplicate = source.replacen(
                "\"environmentId\":",
                "\"environmentId\":\"duplicate\",\"environmentId\":",
                1,
            );
            assert!(serde_json::from_str::<EnvironmentAuthorization>(&duplicate).is_err());
        }
    }

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
            if let Ok(changed) = serde_json::from_value::<EnvironmentAuthorization>(value) {
                assert!(
                    changed
                        .verify(&authorization, &digest, b"test-key")
                        .is_err(),
                    "{field}"
                );
            }
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
