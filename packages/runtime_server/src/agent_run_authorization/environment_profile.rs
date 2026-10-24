//! One signed host configuration for every environment.
//! Validation binds configuration; resource enforcement remains a host obligation.
use super::{canonical_json, validate_sha256, ExecutionNetwork, SandboxResources};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum EnvironmentProfile {
    Legacy(LegacyEnvironmentProfile),
    E2b(E2bEnvironmentProfile),
}

impl<'de> Deserialize<'de> for EnvironmentProfile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = UniqueJson::deserialize(deserializer)?.0;
        if value.get("schema").and_then(serde_json::Value::as_str)
            == Some("runtime.e2b_environment_profile.v1")
        {
            serde_json::from_value(value)
                .map(Self::E2b)
                .map_err(serde::de::Error::custom)
        } else {
            // Preserve the released legacy wire shape and strict field errors.
            serde_json::from_value(value)
                .map(Self::Legacy)
                .map_err(serde::de::Error::custom)
        }
    }
}

// Keep duplicate rejection while choosing the provider by its explicit schema.
// Parsing straight into Value would silently retain only the last field.
struct UniqueJson(serde_json::Value);

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> serde::de::Visitor<'de> for UniqueVisitor {
            type Value = UniqueJson;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("JSON with unique object fields")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                use serde::de::Error;
                let mut fields = serde_json::Map::new();
                while let Some(name) = map.next_key::<String>()? {
                    if fields.contains_key(&name) {
                        return Err(M::Error::custom(format!("duplicate field `{name}`")));
                    }
                    fields.insert(name, map.next_value::<UniqueJson>()?.0);
                }
                Ok(UniqueJson(serde_json::Value::Object(fields)))
            }

            fn visit_seq<S: serde::de::SeqAccess<'de>>(
                self,
                mut seq: S,
            ) -> Result<Self::Value, S::Error> {
                let mut items = Vec::new();
                while let Some(value) = seq.next_element::<UniqueJson>()? {
                    items.push(value.0);
                }
                Ok(UniqueJson(serde_json::Value::Array(items)))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::String(value.into())))
            }

            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::String(value)))
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::Bool(value)))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::Number(value.into())))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::Number(value.into())))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| UniqueJson(serde_json::Value::Number(number)))
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJson(serde_json::Value::Null))
            }

            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                self.visit_unit()
            }

            fn visit_some<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                UniqueJson::deserialize(deserializer)
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyEnvironmentProfile {
    pub schema: String,
    pub image_digest: String,
    pub workspace_root: String,
    pub user: String,
    pub network: ExecutionNetwork,
    pub resources: EnvironmentResources,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct E2bEnvironmentProfile {
    pub schema: String,
    pub provider: String,
    pub provider_config_digest: String,
    pub template_ref: E2bTemplateRef,
    pub workspace_root: String,
    pub user: String,
    pub network: ExecutionNetwork,
    pub lifetime: String,
    pub timeout_seconds: u32,
    pub input_budget_bytes: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_workspace_initialization"
    )]
    pub workspace_initialization: Option<WorkspaceInitialization>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceInitialization {
    ManagedFiles,
    TaskImage,
}

fn deserialize_workspace_initialization<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<WorkspaceInitialization>, D::Error> {
    // Only absence is optional; explicit null must not create another wire spelling.
    WorkspaceInitialization::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct E2bTemplateRef {
    pub template_id: String,
    pub build_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentResources {
    pub cpu_milli: u32,
    pub memory_bytes: u64,
    pub pids_limit: u32,
    pub data_tmpfs_bytes: u64,
    pub storage_bytes: u64,
}

impl EnvironmentResources {
    pub fn sandbox_resources(self) -> SandboxResources {
        SandboxResources {
            cpu_milli: self.cpu_milli,
            memory_bytes: self.memory_bytes,
            pids_limit: self.pids_limit,
            data_tmpfs_bytes: self.data_tmpfs_bytes,
        }
    }
}

impl LegacyEnvironmentProfile {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != "runtime.environment_profile.v1" {
            return Err("environment_profile_schema_invalid".into());
        }
        validate_sha256("environmentProfile.imageDigest", &self.image_digest)?;
        let root = &self.workspace_root;
        if !root.starts_with('/')
            || root.len() > 4096
            || root.contains('\\')
            || root.chars().any(char::is_control)
            || root.nfc().collect::<String>() != *root
            || (root != "/"
                && root
                    .split('/')
                    .skip(1)
                    .any(|part| part.is_empty() || part == "." || part == ".."))
            || root == "/run/centaeris"
            || root.starts_with("/run/centaeris/")
        {
            return Err("environment_profile_workspace_root_invalid".into());
        }
        let valid_id = |value: &str| {
            !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && (value == "0" || !value.starts_with('0'))
                && value.parse::<u32>().is_ok_and(|id| id < u32::MAX)
        };
        if !self
            .user
            .split_once(':')
            .is_some_and(|(uid, gid)| valid_id(uid) && valid_id(gid))
        {
            return Err("environment_profile_user_invalid".into());
        }
        if self.resources.cpu_milli == 0
            || self.resources.memory_bytes == 0
            || self.resources.pids_limit == 0
            || self.resources.data_tmpfs_bytes == 0
            || self.resources.storage_bytes == 0
        {
            return Err("environment_profile_resources_invalid".into());
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        let value =
            serde_json::to_value(self).map_err(|_| "benchmark profile serialization failed")?;
        let canonical = canonical_json(value)?;
        Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
    }
}

impl E2bEnvironmentProfile {
    pub fn validate(&self) -> Result<(), String> {
        if self.workspace_initialization == Some(WorkspaceInitialization::ManagedFiles)
            && (self.workspace_root != "/mnt/data" || self.user != "agent")
        {
            return Err("environment_profile_workspace_initialization_invalid".into());
        }
        if self.workspace_initialization == Some(WorkspaceInitialization::TaskImage)
            && self.lifetime != "retained"
        {
            return Err("environment_profile_workspace_initialization_invalid".into());
        }
        if self.schema != "runtime.e2b_environment_profile.v1" || self.provider != "e2b" {
            return Err("environment_profile_schema_invalid".into());
        }
        validate_sha256(
            "environmentProfile.providerConfigDigest",
            &self.provider_config_digest,
        )?;
        let template = &self.template_ref.template_id;
        let build = self.template_ref.build_id.as_bytes();
        if template.is_empty()
            || template.len() > 128
            || !template.as_bytes()[0].is_ascii_alphanumeric()
            || !template
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || build.len() != 36
            || !build.iter().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    *b == b'-'
                } else {
                    b.is_ascii_digit() || (b'a'..=b'f').contains(b)
                }
            })
        {
            return Err("environment_profile_template_invalid".into());
        }
        validate_workspace_root(&self.workspace_root)?;
        let user = self.user.as_bytes();
        if user.is_empty()
            || user.len() > 32
            || !(user[0].is_ascii_lowercase() || user[0] == b'_')
            || !user
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
        {
            return Err("environment_profile_user_invalid".into());
        }
        if !matches!(self.lifetime.as_str(), "perRun" | "retained") {
            return Err("environment_profile_lifetime_invalid".into());
        }
        if !(1..=86400).contains(&self.timeout_seconds) {
            return Err("environment_profile_timeout_invalid".into());
        }
        if !(1..=centaeris_core::execution::MAX_EXECUTION_INPUT_BYTES)
            .contains(&self.input_budget_bytes)
        {
            return Err("environment_profile_input_budget_invalid".into());
        }
        Ok(())
    }
}

fn validate_workspace_root(root: &str) -> Result<(), String> {
    if !root.starts_with('/')
        || root.len() > 4096
        || root.contains('\\')
        || root.chars().any(char::is_control)
        || root.nfc().collect::<String>() != root
        || (root != "/"
            && root
                .split('/')
                .skip(1)
                .any(|part| part.is_empty() || part == "." || part == ".."))
        || root == "/run/centaeris"
        || root.starts_with("/run/centaeris/")
    {
        return Err("environment_profile_workspace_root_invalid".into());
    }
    Ok(())
}

impl EnvironmentProfile {
    pub fn task_image(&self) -> bool {
        matches!(self, Self::E2b(profile)
            if profile.workspace_initialization == Some(WorkspaceInitialization::TaskImage))
    }

    pub fn managed_files(&self) -> bool {
        matches!(self, Self::E2b(profile)
            if profile.workspace_initialization == Some(WorkspaceInitialization::ManagedFiles))
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Legacy(value) => value.validate(),
            Self::E2b(value) => value.validate(),
        }
    }

    pub fn legacy(&self) -> Result<&LegacyEnvironmentProfile, String> {
        match self {
            Self::Legacy(value) => Ok(value),
            Self::E2b(_) => Err("e2b_execution_unavailable".into()),
        }
    }

    #[cfg(test)]
    pub fn legacy_mut(&mut self) -> Result<&mut LegacyEnvironmentProfile, String> {
        match self {
            Self::Legacy(value) => Ok(value),
            Self::E2b(_) => Err("e2b_execution_unavailable".into()),
        }
    }

    pub fn workspace_root(&self) -> &str {
        match self {
            Self::Legacy(value) => &value.workspace_root,
            Self::E2b(value) => &value.workspace_root,
        }
    }

    pub fn user(&self) -> &str {
        match self {
            Self::Legacy(value) => &value.user,
            Self::E2b(value) => &value.user,
        }
    }

    pub fn network(&self) -> ExecutionNetwork {
        match self {
            Self::Legacy(value) => value.network,
            Self::E2b(value) => value.network,
        }
    }

    pub fn input_budget_bytes(&self) -> Result<u64, String> {
        self.validate()?;
        Ok(match self {
            Self::Legacy(value) => value.resources.data_tmpfs_bytes / 2,
            Self::E2b(value) => value.input_budget_bytes,
        })
    }

    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        let value =
            serde_json::to_value(self).map_err(|_| "environment profile serialization failed")?;
        let canonical = canonical_json(value)?;
        Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
    }
}

#[cfg(test)]
mod task_image_tests {
    use super::*;
    fn selected() -> serde_json::Value {
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        ))
        .unwrap();
        value["workspaceInitialization"] = "taskImage".into();
        value["lifetime"] = "retained".into();
        value["workspaceRoot"] = "/app".into();
        value["user"] = "root".into();
        value
    }
    #[test]
    fn task_image_profile_explicitly_preserves_original_user_and_working_directory() {
        let value = selected();
        let profile: EnvironmentProfile = serde_json::from_value(value.clone()).unwrap();
        profile.validate().unwrap();
        assert_eq!(profile.user(), "root");
        assert_eq!(profile.workspace_root(), "/app");
        assert_eq!(serde_json::to_value(profile).unwrap(), value);
    }
    #[test]
    fn task_image_profile_requires_retained_lifetime_and_has_no_aliases() {
        let mut value = selected();
        value["lifetime"] = "perRun".into();
        let profile: EnvironmentProfile = serde_json::from_value(value).unwrap();
        assert!(profile.validate().is_err());
        for unsupported in [
            serde_json::Value::Null,
            serde_json::json!("task_image"),
            serde_json::json!("native"),
        ] {
            let mut value = selected();
            value["workspaceInitialization"] = unsupported;
            assert!(serde_json::from_value::<EnvironmentProfile>(value).is_err());
        }
    }
}
