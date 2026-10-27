//! One signed host configuration for every environment.
//! Validation binds configuration; resource enforcement remains a host obligation.
use super::{canonical_json, validate_sha256, ExecutionNetwork, SandboxResources};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentProfile {
    pub schema: String,
    pub image_digest: String,
    pub workspace_root: String,
    pub user: String,
    pub network: ExecutionNetwork,
    pub resources: EnvironmentResources,
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

impl EnvironmentProfile {
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
        #[cfg(test)]
        crate::environment_authorization::digest_observation::environment_profile(canonical.len());
        Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
    }
}
