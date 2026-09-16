use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
#[derive(Default)]
pub enum NetworkPolicy {
    #[default]
    PublicInternet,
    Disabled,
    Allowlist {
        #[serde(rename = "allowedDomains")]
        allowed_domains: Vec<String>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NetworkPolicyWire {
    mode: NetworkModeWire,
    allowed_domains: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum NetworkModeWire {
    PublicInternet,
    Disabled,
    Allowlist,
}

impl<'de> Deserialize<'de> for NetworkPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = NetworkPolicyWire::deserialize(deserializer)?;
        let policy = match (wire.mode, wire.allowed_domains) {
            (NetworkModeWire::PublicInternet, None) => Self::PublicInternet,
            (NetworkModeWire::Disabled, None) => Self::Disabled,
            (NetworkModeWire::Allowlist, Some(allowed_domains)) => {
                Self::Allowlist { allowed_domains }
            }
            (NetworkModeWire::PublicInternet, Some(_)) => {
                return Err(serde::de::Error::custom(
                    "publicInternet network policy must not contain allowedDomains",
                ));
            }
            (NetworkModeWire::Disabled, Some(_)) => {
                return Err(serde::de::Error::custom(
                    "disabled network policy must not contain allowedDomains",
                ));
            }
            (NetworkModeWire::Allowlist, None) => {
                return Err(serde::de::Error::custom(
                    "allowlist network policy requires allowedDomains",
                ));
            }
        };
        policy.validate().map_err(serde::de::Error::custom)?;
        Ok(policy)
    }
}

impl NetworkPolicy {
    pub fn validate(&self) -> Result<(), String> {
        let Self::Allowlist { allowed_domains } = self else {
            return Ok(());
        };
        if allowed_domains.is_empty() {
            return Err(
                "network allowlist must contain at least one domain; use disabled for no network"
                    .to_string(),
            );
        }
        for domain in allowed_domains {
            let value = domain.strip_prefix("*.").unwrap_or(domain.as_str());
            let valid = !value.is_empty()
                && value.len() <= 253
                && value.is_ascii()
                && value.contains('.')
                && !value.starts_with('.')
                && !value.ends_with('.')
                && value.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                });
            if !valid || value.parse::<std::net::IpAddr>().is_ok() {
                return Err(format!(
                    "network allowlist contains an invalid domain: {domain}"
                ));
            }
        }
        Ok(())
    }

    pub fn uses_managed_egress(&self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSystemPolicy {
    pub workspace_root: PathBuf,
    pub read_only_roots: Vec<PathBuf>,
    pub writable_roots: Vec<PathBuf>,
    pub denied_read_paths: Vec<PathBuf>,
    pub denied_write_paths: Vec<PathBuf>,
    pub tmp_root: Option<PathBuf>,
}

impl FileSystemPolicy {
    pub fn workspace_write(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            read_only_roots: vec![workspace_root.clone()],
            writable_roots: vec![workspace_root.clone()],
            denied_read_paths: Vec::new(),
            denied_write_paths: Vec::new(),
            tmp_root: None,
            workspace_root,
        }
    }

    pub fn read_only(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            read_only_roots: vec![workspace_root.clone()],
            writable_roots: Vec::new(),
            denied_read_paths: Vec::new(),
            denied_write_paths: Vec::new(),
            tmp_root: None,
            workspace_root,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPolicy {
    pub filesystem: FileSystemPolicy,
    pub network: NetworkPolicy,
}

impl ExecutionPolicy {
    pub fn workspace_write_public_internet(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            filesystem: FileSystemPolicy::workspace_write(workspace_root),
            network: NetworkPolicy::PublicInternet,
        }
    }

    pub fn workspace_write_no_network(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            filesystem: FileSystemPolicy::workspace_write(workspace_root),
            network: NetworkPolicy::Disabled,
        }
    }

    pub fn workspace_write_with_network_allowlist(
        workspace_root: impl Into<PathBuf>,
        allowed_domains: Vec<String>,
    ) -> Self {
        Self {
            filesystem: FileSystemPolicy::workspace_write(workspace_root),
            network: NetworkPolicy::Allowlist { allowed_domains },
        }
    }

    pub fn read_only_no_network(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            filesystem: FileSystemPolicy::read_only(workspace_root),
            network: NetworkPolicy::Disabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workspace_metadata_is_writable_by_default() {
        let policy = FileSystemPolicy::workspace_write("/workspace");
        assert!(policy.denied_read_paths.is_empty());
        assert!(policy.denied_write_paths.is_empty());
    }
}
