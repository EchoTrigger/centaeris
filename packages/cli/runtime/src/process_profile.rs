//! Native process policy for an isolated, single-response Session.
use centaeris_core::model::{
    JsonHttpFuture, JsonHttpRequest, JsonHttpTransport, ModelProviderRegistry, ModelSessionConfig,
    TransportError,
};
use centaeris_core::runtime::AgentRuntimeConfig;
use std::ffi::OsStr;
use std::future::Future;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const SINGLE_RESPONSE_TIMEOUT_ENV: &str = "CENTAERIS_SINGLE_RESPONSE_TIMEOUT_MS";
const DEFAULT_SINGLE_RESPONSE_TIMEOUT_MS: NonZeroU64 =
    NonZeroU64::new(10_000).expect("default single-response timeout is positive");

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ProcessProfile {
    #[default]
    Default,
    SingleResponse {
        timeout_ms: NonZeroU64,
    },
}
impl ProcessProfile {
    pub(crate) fn from_env() -> Result<Self, String> {
        match std::env::var("CENTAERIS_PROCESS_PROFILE") {
            Ok(raw) => Self::parse(
                Some(&raw),
                std::env::var_os(SINGLE_RESPONSE_TIMEOUT_ENV).as_deref(),
            ),
            Err(std::env::VarError::NotPresent) => Ok(Self::Default),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err("CENTAERIS_PROCESS_PROFILE must be Unicode".into())
            }
        }
    }
    fn parse(value: Option<&str>, timeout: Option<&OsStr>) -> Result<Self, String> {
        match value {
            None => Ok(Self::Default),
            Some("single_response") => {
                let timeout_ms = match timeout {
                    None => DEFAULT_SINGLE_RESPONSE_TIMEOUT_MS,
                    Some(raw) => raw
                        .to_str()
                        .and_then(|raw| raw.parse::<NonZeroU64>().ok())
                        .ok_or_else(|| {
                            format!("{SINGLE_RESPONSE_TIMEOUT_ENV} must be a positive integer in milliseconds")
                        })?,
                };
                Ok(Self::SingleResponse { timeout_ms })
            }
            _ => Err("Unsupported CENTAERIS_PROCESS_PROFILE".into()),
        }
    }
    pub(crate) fn is_single(self) -> bool {
        matches!(self, Self::SingleResponse { .. })
    }
    pub(crate) fn configure_engine(self, config: &mut AgentRuntimeConfig) {
        if !self.is_single() {
            return;
        }
        config.agent_instructions.clear();
        config.enable_system_prompt_template = false;
        config.enable_prompt_compaction = false;
        config.enable_tool_use_summary = false;
        config.max_recovery_attempts = 0;
        config.auto_continue_after_resume_wait = false;
        config.allowed_tools = Some(Vec::new());
        config.required_completion_tool = None;
    }
    pub(crate) fn validate_workspace(self, cwd: &Path) -> Result<(), String> {
        if !self.is_single() {
            return Ok(());
        }
        match std::fs::read_to_string(cwd.join("AGENTS.md")) {
            Ok(content) if content.trim().is_empty() => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err("SingleResponse requires an empty dedicated workspace AGENTS.md".into()),
            Err(error) => Err(format!(
                "SingleResponse workspace instructions could not be checked: {error}"
            )),
        }
    }
    pub(crate) fn configure_model(
        self,
        config: &mut ModelSessionConfig,
        registry: &mut ModelProviderRegistry,
    ) -> Result<(), String> {
        let Self::SingleResponse { timeout_ms } = self else {
            return Ok(());
        };
        // Core's zero is an inheritance sentinel. Set the resolved provider
        // default to zero as well, without changing any ordinary provider.
        let mut provider = registry
            .get(&config.provider_id)
            .cloned()
            .ok_or("SingleResponse model provider is unavailable")?;
        provider.default_max_retries = Some(0);
        registry.insert_user_defined(provider);
        config.max_retries = 0;
        config.timeout_ms = timeout_ms.get();
        Ok(())
    }
    pub(crate) async fn run_from<T>(
        self,
        started: tokio::time::Instant,
        future: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let Self::SingleResponse { timeout_ms } = self else {
            return future.await;
        };
        let deadline = started
            .checked_add(Duration::from_millis(timeout_ms.get()))
            .ok_or_else(|| {
                format!("{SINGLE_RESPONSE_TIMEOUT_ENV} exceeds the supported timer range")
            })?;
        let deadline_error =
            || format!("single_response_deadline_exceeded: generation exceeded {timeout_ms}ms");
        if tokio::time::Instant::now() >= deadline {
            return Err(deadline_error());
        }
        tokio::time::timeout_at(deadline, future)
            .await
            .map_err(|_| deadline_error())?
    }
}

// The protocol clients and the existing real transport remain unchanged. This
// host policy only narrows an already-resolved request and prevents a second
// physical dispatch, even if a protocol continuation unexpectedly asks for one.
pub(crate) struct ProfileTransport<T> {
    inner: T,
    profile: ProcessProfile,
    dispatched: AtomicBool,
}
impl<T> ProfileTransport<T> {
    pub(crate) fn new(inner: T, profile: ProcessProfile) -> Self {
        Self {
            inner,
            profile,
            dispatched: AtomicBool::new(false),
        }
    }
    fn prepare(&self, request: &JsonHttpRequest) -> Result<JsonHttpRequest, TransportError> {
        let mut request = request.clone();
        if let ProcessProfile::SingleResponse { timeout_ms } = self.profile {
            if self.dispatched.swap(true, Ordering::SeqCst) {
                return Err(TransportError::Build(
                    "SingleResponse permits one physical model request".into(),
                ));
            }
            request.max_retries = 0;
            request.timeout_ms = timeout_ms.get();
        }
        Ok(request)
    }
}
impl<T: JsonHttpTransport> JsonHttpTransport for ProfileTransport<T> {
    fn execute_json<'a>(&'a self, request: &'a JsonHttpRequest) -> JsonHttpFuture<'a> {
        if !self.profile.is_single() {
            return self.inner.execute_json(request);
        }
        Box::pin(async move {
            let request = self.prepare(request)?;
            self.inner.execute_json(&request).await
        })
    }
    fn execute_sse<'a>(
        &'a self,
        request: &'a JsonHttpRequest,
        on_data: &'a mut (dyn FnMut(String) + Send),
    ) -> JsonHttpFuture<'a> {
        if !self.profile.is_single() {
            return self.inner.execute_sse(request, on_data);
        }
        Box::pin(async move {
            let request = self.prepare(request)?;
            self.inner.execute_sse(&request, on_data).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_is_explicit_and_does_not_change_normal_engine() {
        assert_eq!(
            ProcessProfile::parse(None, Some(OsStr::new("invalid"))).unwrap(),
            ProcessProfile::Default
        );
        assert!(ProcessProfile::parse(Some("unknown"), None).is_err());
        let profile = ProcessProfile::parse(Some("single_response"), None).unwrap();
        let mut config = AgentRuntimeConfig::default();
        ProcessProfile::Default.configure_engine(&mut config);
        assert!(config.enable_system_prompt_template);
        assert!(config.enable_prompt_compaction);
        assert_eq!(config.max_recovery_attempts, 2);
        assert!(config.allowed_tools.is_none());
        profile.configure_engine(&mut config);
        assert!(!config.enable_system_prompt_template);
        assert!(!config.enable_prompt_compaction);
        assert!(!config.auto_continue_after_resume_wait);
        assert_eq!(config.max_recovery_attempts, 0);
        assert_eq!(config.allowed_tools, Some(Vec::new()));
    }
    #[test]
    fn single_response_rejects_invalid_timeouts() {
        for timeout in ["", "0", "-1", "1.5", "ten"] {
            let result = ProcessProfile::parse(Some("single_response"), Some(OsStr::new(timeout)));
            assert!(result.unwrap_err().contains(SINGLE_RESPONSE_TIMEOUT_ENV));
        }
    }

    #[test]
    fn single_response_configures_timeout_and_disables_provider_retries() {
        let mut registry = ModelProviderRegistry::new();
        let mut provider = registry.get("deepseek.default").unwrap().clone();
        provider.auth = centaeris_core::model::AuthSpec::None;
        registry.insert_user_defined(provider);
        let mut config = ModelSessionConfig {
            provider_id: "deepseek.default".into(),
            max_retries: 0,
            ..ModelSessionConfig::default()
        };
        assert!(
            registry
                .resolve_session_config(&config)
                .unwrap()
                .effective_max_retries
                > 0
        );
        let profile =
            ProcessProfile::parse(Some("single_response"), Some(OsStr::new("2500"))).unwrap();
        profile.configure_model(&mut config, &mut registry).unwrap();
        assert_eq!(
            registry
                .resolve_session_config(&config)
                .unwrap()
                .effective_max_retries,
            0
        );
        assert_eq!(config.timeout_ms, 2500);

        let request = JsonHttpRequest {
            method: "POST".into(),
            url: "https://example.invalid".into(),
            headers: Default::default(),
            timeout_ms: 30_000,
            sse_idle_timeout_ms: 5000,
            max_retries: 2,
            retry_backoff_ms: 100,
            body_json: "{}".into(),
        };
        let transport = ProfileTransport::new((), profile);
        let request = transport.prepare(&request).unwrap();
        assert_eq!(request.timeout_ms, 2500);
        assert_eq!(request.sse_idle_timeout_ms, 5000);
        assert_eq!(request.max_retries, 0);
        assert!(transport.prepare(&request).is_err());
    }
    #[test]
    fn single_response_requires_empty_owned_instructions() {
        let directory = tempfile::tempdir().unwrap();
        let profile = ProcessProfile::parse(Some("single_response"), None).unwrap();
        profile.validate_workspace(directory.path()).unwrap();
        std::fs::write(directory.path().join("AGENTS.md"), "\n").unwrap();
        profile.validate_workspace(directory.path()).unwrap();
        std::fs::write(
            directory.path().join("AGENTS.md"),
            "must never enter title request",
        )
        .unwrap();
        assert!(profile.validate_workspace(directory.path()).is_err());
        ProcessProfile::Default
            .validate_workspace(directory.path())
            .unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn whole_generation_has_a_deadline_instead_of_an_idle_timeout() {
        for (timeout, expected_ms) in [(None, 10_000), (Some(OsStr::new("2500")), 2500)] {
            let profile = ProcessProfile::parse(Some("single_response"), timeout).unwrap();
            let started = tokio::time::Instant::now();
            tokio::time::sleep(Duration::from_secs(1)).await;
            let result = profile
                .run_from(started, async {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Ok(())
                })
                .await;
            assert_eq!(
                result.unwrap_err(),
                format!("single_response_deadline_exceeded: generation exceeded {expected_ms}ms")
            );
            assert_eq!(started.elapsed(), Duration::from_millis(expected_ms));
        }
        let expired = tokio::time::Instant::now();
        tokio::time::sleep(Duration::from_secs(11)).await;
        let profile = ProcessProfile::parse(Some("single_response"), None).unwrap();
        assert!(profile
            .run_from::<()>(expired, async {
                panic!("expired setup must not begin generation");
            })
            .await
            .is_err());
        ProcessProfile::Default
            .run_from(tokio::time::Instant::now(), async {
                tokio::time::sleep(Duration::from_secs(11)).await;
                Ok(())
            })
            .await
            .unwrap();
    }
}
