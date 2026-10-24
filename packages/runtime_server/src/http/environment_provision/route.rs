//! The frozen environment profile selects a physical backend before host lookup.
use crate::agent_run_authorization::EnvironmentProfile;

pub(super) fn by_profile<R>(
    profile: &EnvironmentProfile,
    provider: impl FnOnce() -> R,
    native: impl FnOnce() -> R,
) -> R {
    match profile {
        EnvironmentProfile::E2b(_) => provider(),
        EnvironmentProfile::Legacy(_) => native(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn provider_profile_uses_provider_without_observing_or_allocating_a_native_host() {
        let mut profile: EnvironmentProfile = serde_json::from_str(include_str!(
            "../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        ))
        .unwrap();
        let EnvironmentProfile::E2b(ref mut value) = profile else {
            unreachable!()
        };
        value.lifetime = "retained".into();
        profile.validate().unwrap();
        let native_calls = AtomicUsize::new(0);
        let provider_calls = AtomicUsize::new(0);
        let result = by_profile(
            &profile,
            || {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                "provider-result"
            },
            || {
                native_calls.fetch_add(1, Ordering::SeqCst);
                "native-host-unavailable"
            },
        );
        assert_eq!(result, "provider-result");
        assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn native_profile_preserves_native_result_without_touching_provider() {
        let auth:serde_json::Value=serde_json::from_str(include_str!("../../../../../tests/workspace/fixtures/environment_provision_authorization/v1/valid.json")).unwrap();
        let profile: EnvironmentProfile =
            serde_json::from_value(auth["payload"]["environmentProfile"].clone()).unwrap();
        profile.validate().unwrap();
        assert!(matches!(profile, EnvironmentProfile::Legacy(_)));
        let provider_calls = AtomicUsize::new(0);
        let result = by_profile(
            &profile,
            || {
                provider_calls.fetch_add(1, Ordering::SeqCst);
                "provider"
            },
            || "original-native-result",
        );
        assert_eq!(result, "original-native-result");
        assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn provider_rejection_never_falls_back_to_another_backend() {
        let profile: EnvironmentProfile = serde_json::from_str(include_str!(
            "../../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
        ))
        .unwrap();
        let native_calls = AtomicUsize::new(0);
        let result = by_profile(
            &profile,
            || Err::<(), _>("provider-policy-rejected"),
            || {
                native_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(result, Err("provider-policy-rejected"));
        assert_eq!(native_calls.load(Ordering::SeqCst), 0);
    }
}
