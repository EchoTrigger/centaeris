//! Explicit Runtime-owned admission for model RPCs in this process only.
//! The host selects domains; credentials, model names and request bodies do not.
use centaeris_core::model::admission::ModelAdmission;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct HostedModelAdmission {
    domains: Arc<HashMap<String, ModelAdmission>>,
    selected_domain: Option<String>,
}

impl HostedModelAdmission {
    pub(crate) fn from_env() -> Result<Self, String> {
        fn setting(name: &str) -> Result<Option<String>, String> {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(std::env::VarError::NotUnicode(_)) => {
                    Err(format!("{name} must be valid Unicode"))
                }
            }
        }
        let domain = setting("HOSTED_MODEL_RPC_ADMISSION_DOMAIN")?;
        let limit = setting("HOSTED_MODEL_RPC_MAX_IN_FLIGHT")?;
        Self::from_values(domain.as_deref(), limit.as_deref())
    }

    fn from_values(domain: Option<&str>, limit: Option<&str>) -> Result<Self, String> {
        match (domain, limit) {
            (None, None) => Ok(Self::default()),
            (Some(domain), Some(limit)) => {
                let limit = limit.parse::<usize>().map_err(|_| {
                    "HOSTED_MODEL_RPC_MAX_IN_FLIGHT must be an integer from 1 to 1000000".to_owned()
                })?;
                Self::configured(&[(domain, limit)])
            }
            _ => Err("HOSTED_MODEL_RPC_ADMISSION_DOMAIN and HOSTED_MODEL_RPC_MAX_IN_FLIGHT must be configured together".into()),
        }
    }

    pub(crate) fn selected(&self) -> Option<ModelAdmission> {
        self.selected_domain.as_deref().map(|domain| {
            self.for_domain(domain)
                .expect("selected model RPC domain must be configured")
        })
    }

    pub(crate) fn for_domain(&self, domain: &str) -> Option<ModelAdmission> {
        self.domains.get(domain).cloned()
    }

    pub(crate) fn configured(entries: &[(&str, usize)]) -> Result<Self, String> {
        let mut domains = HashMap::new();
        for (domain, limit) in entries {
            if domain.is_empty()
                || domain.len() > 128
                || domain
                    .chars()
                    .any(|value| value.is_whitespace() || value.is_control())
            {
                return Err("HOSTED_MODEL_RPC_ADMISSION_DOMAIN must contain 1 to 128 bytes without whitespace or control characters".into());
            }
            let limit = NonZeroUsize::new(*limit)
                .filter(|value| value.get() <= 1_000_000)
                .ok_or("HOSTED_MODEL_RPC_MAX_IN_FLIGHT must be an integer from 1 to 1000000")?;
            if domains
                .insert((*domain).to_owned(), ModelAdmission::new(limit))
                .is_some()
            {
                return Err("model RPC admission domains must be unique".into());
            }
        }
        Ok(Self {
            domains: Arc::new(domains),
            selected_domain: entries.first().map(|(domain, _)| (*domain).to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_model_admission_is_disabled_without_explicit_configuration() {
        let admission = HostedModelAdmission::from_values(None, None).unwrap();
        assert!(admission.selected().is_none());
        assert!(admission.for_domain("unconfigured").is_none());
    }

    #[test]
    fn hosted_model_admission_rejects_partial_or_invalid_configuration() {
        for (domain, limit) in [
            (Some("team-a"), None),
            (None, Some("2")),
            (Some(""), Some("2")),
            (Some(" team-a"), Some("2")),
            (Some("team-a"), Some("0")),
            (Some("team-a"), Some("-1")),
            (Some("team-a"), Some("1.5")),
            (Some("team-a"), Some("1000001")),
        ] {
            assert!(
                HostedModelAdmission::from_values(domain, limit).is_err(),
                "invalid configuration was accepted"
            );
        }
    }

    #[test]
    fn hosted_model_admission_selects_only_the_configured_domain() {
        let admission = HostedModelAdmission::from_values(Some("team-a"), Some("2")).unwrap();
        assert!(admission.selected().is_some());
        assert!(admission.for_domain("team-a").is_some());
        assert!(admission.for_domain("team-b").is_none());
    }

    #[test]
    fn hosted_model_admission_rejects_duplicate_configuration_domains() {
        assert!(HostedModelAdmission::configured(&[("same", 1), ("same", 2)]).is_err());
        assert!(HostedModelAdmission::configured(&[("same", 1), ("same", 1)]).is_err());
    }
}
