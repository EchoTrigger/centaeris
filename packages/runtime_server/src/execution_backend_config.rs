//! Select one startup deployment; signed Environments keep their frozen provider.
use crate::agent_run_authorization::EnvironmentProfile;
use crate::e2b_runtime::E2bRuntimeConfig;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

pub(crate) enum ExecutionBackendConfig {
    Legacy,
    E2b(Box<E2bRuntimeConfig>),
}

impl ExecutionBackendConfig {
    pub(crate) fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        match get("RUNTIME_EXECUTION_PROVIDER")
            .as_deref()
            .unwrap_or("e2b")
        {
            "docker" => return Ok(Self::Legacy),
            "e2b" => {}
            _ => return Err("runtime_execution_provider_unsupported".into()),
        }
        let required = |name| {
            get(name)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{name} is required"))
        };
        let path = PathBuf::from(required("RUNTIME_E2B_PROFILE_PATH")?);
        if !path.is_absolute() {
            return Err("runtime_e2b_profile_path_invalid".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(65537).read_to_end(&mut bytes))
            .map_err(|_| "runtime_e2b_profile_unavailable")?;
        if bytes.len() > 65536 {
            return Err("runtime_e2b_profile_invalid".into());
        }
        let profile: EnvironmentProfile =
            serde_json::from_slice(&bytes).map_err(|_| "runtime_e2b_profile_invalid")?;
        let timeout = get("RUNTIME_E2B_REQUEST_TIMEOUT_SECONDS")
            .unwrap_or_else(|| "20".into())
            .parse::<u64>()
            .map_err(|_| "runtime_e2b_timeout_invalid")?;
        let running_limit = get("RUNTIME_E2B_MAX_RUNNING_ENVIRONMENTS")
            .map(|value| {
                value
                    .parse::<u32>()
                    .map_err(|_| "e2b_running_limit_invalid".to_string())
            })
            .transpose()?;
        let idle_pause_timeout = get("RUNTIME_E2B_IDLE_PAUSE_MS")
            .map(|value| {
                value
                    .parse::<u64>()
                    .map(Duration::from_millis)
                    .map_err(|_| "e2b_idle_pause_configuration_invalid".to_string())
            })
            .transpose()?;
        E2bRuntimeConfig::new(
            profile,
            required("RUNTIME_E2B_PROVIDER_CONFIG_DIGEST")?,
            PathBuf::from(required("RUNTIME_E2B_PYTHON")?),
            PathBuf::from(required("RUNTIME_E2B_ADAPTER_PACKAGE_ROOT")?),
            Duration::from_secs(timeout),
        )
        .and_then(|config| config.with_running_limit(running_limit))
        .and_then(|config| config.with_idle_pause_timeout(idle_pause_timeout))
        .map(|config| Self::E2b(Box::new(config)))
        .map_err(|error| match error {
            crate::e2b_runtime::E2bRuntimeError::Rejected(code)
            | crate::e2b_runtime::E2bRuntimeError::Unavailable(code)
            | crate::e2b_runtime::E2bRuntimeError::Unknown(code) => code.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_run_authorization::EnvironmentProfile;
    use std::collections::HashMap;
    use std::fs;

    #[test]
    fn default_requires_explicit_e2b_deployment_and_legacy_requires_explicit_selection() {
        assert!(ExecutionBackendConfig::from_lookup(|_| None).is_err());
        assert!(matches!(
            ExecutionBackendConfig::from_lookup(|name| {
                (name == "RUNTIME_EXECUTION_PROVIDER").then(|| "docker".into())
            }),
            Ok(ExecutionBackendConfig::Legacy)
        ));
        assert!(ExecutionBackendConfig::from_lookup(|name| {
            (name == "RUNTIME_EXECUTION_PROVIDER").then(|| "unknown".into())
        })
        .is_err());
    }

    #[test]
    fn selected_profile_is_loaded_once_and_preserved_without_reconstructing_provider_fields() {
        let root = tempfile::tempdir().unwrap();
        let python = root.path().join("python");
        fs::write(&python, b"validation fixture").unwrap();
        let package = root.path().join("adapter");
        fs::create_dir_all(package.join("centaeris_e2b_adapter")).unwrap();
        for name in ["__init__.py", "__main__.py"] {
            fs::write(package.join("centaeris_e2b_adapter").join(name), b"").unwrap();
        }
        let mut profile: EnvironmentProfile = serde_json::from_str(include_str!(
            "../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        ))
        .unwrap();
        let EnvironmentProfile::E2b(selected) = &mut profile else {
            unreachable!()
        };
        selected.workspace_root = "/mnt/data".into();
        selected.user = "agent".into();
        let digest = selected.provider_config_digest.clone();
        let path = root.path().join("profile.json");
        fs::write(&path, serde_json::to_vec(&profile).unwrap()).unwrap();
        let values: HashMap<&str, String> = [
            ("RUNTIME_EXECUTION_PROVIDER", "e2b".into()),
            ("RUNTIME_E2B_PROFILE_PATH", path.to_str().unwrap().into()),
            ("RUNTIME_E2B_PROVIDER_CONFIG_DIGEST", digest),
            ("RUNTIME_E2B_PYTHON", python.to_str().unwrap().into()),
            (
                "RUNTIME_E2B_ADAPTER_PACKAGE_ROOT",
                package.to_str().unwrap().into(),
            ),
        ]
        .into_iter()
        .collect();
        let configuration =
            ExecutionBackendConfig::from_lookup(|name| values.get(name).cloned()).unwrap();
        let ExecutionBackendConfig::E2b(configuration) = configuration else {
            panic!("selected E2B deployment must not require Docker");
        };
        assert_eq!(configuration.profile(), &profile);
        fs::write(&path, b"changed after startup").unwrap();
        assert_eq!(configuration.profile(), &profile);
        for invalid in [b"{}".as_slice(), &[b' '; 65537]] {
            fs::write(&path, invalid).unwrap();
            assert!(ExecutionBackendConfig::from_lookup(|name| values.get(name).cloned()).is_err());
        }
    }
}
