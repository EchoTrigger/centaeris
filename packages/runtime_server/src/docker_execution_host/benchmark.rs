//! Benchmark-only Docker policy. Default Workspace policy remains separate.
use super::*;
use crate::agent_run_authorization::{BenchmarkExecutionProfile, BenchmarkNetwork};

pub(super) const HELPER_DESTINATION: &str = "/opt/centaeris/benchmark/execution_agent";

fn require_quiescence_receipt(success: bool, bytes: &[u8]) -> Result<(), String> {
    let receipt: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "benchmark process quiescence unconfirmed")?;
    if !success
        || receipt
            != serde_json::json!({"schema":"runtime.benchmark.quiescence.v1","quiescent":true})
    {
        return Err("benchmark process quiescence unconfirmed".into());
    }
    Ok(())
}

pub(super) fn create_body(
    expected: &ContainerExpectation<'_>,
    profile: &BenchmarkExecutionProfile,
    helper_path: &str,
) -> Result<serde_json::Value, String> {
    let proxy_environment = match profile.network {
        BenchmarkNetwork::PublicInternet => environment::public_internet_proxy_environment()?,
        BenchmarkNetwork::Disabled => BTreeMap::new(),
    };
    create_body_with_proxy_environment(expected, profile, helper_path, &proxy_environment)
}

fn create_body_with_proxy_environment(
    expected: &ContainerExpectation<'_>,
    profile: &BenchmarkExecutionProfile,
    helper_path: &str,
    proxy_environment: &BTreeMap<String, String>,
) -> Result<serde_json::Value, String> {
    profile.validate()?;
    if !expected.retained
        || expected.oci_runtime != OciRuntime::Runc
        || expected.image_digest != profile.image_digest
        || expected.resources != profile.resources.sandbox_resources()
        || !expected.plugin_mounts.is_empty()
        || !helper_path.starts_with('/')
        || helper_path.chars().any(char::is_control)
    {
        return Err("benchmark signed host profile mismatch".into());
    }
    let mut labels = serde_json::json!({
        "centaeris.managed":"true", "centaeris.agent_run_id":expected.agent_run_id,
        "centaeris.environment_id":expected.environment_id, "centaeris.instance_id":expected.instance_id,
        "centaeris.environment_authorization_digest":expected.binding_digest,
        "centaeris.execution_id":expected.execution_id,
        "centaeris.benchmark_profile_digest":profile.digest()?,
        "centaeris.benchmark_data_budget":profile.resources.data_tmpfs_bytes.to_string(),
    });
    if let Some(digest) = expected.profile_digest {
        labels["centaeris.retained_profile_digest"] = serde_json::json!(digest);
    }
    let mut body = serde_json::json!({
        "Image":profile.image_digest, "Labels":labels, "User":profile.user,
        "WorkingDir":profile.workspace_root,
        "Entrypoint":[HELPER_DESTINATION], "Cmd":["benchmark-keeper"],
        "HostConfig":{
            "Runtime":"runc", "NetworkMode":network_mode(profile.network),
            "ReadonlyRootfs":false, "SecurityOpt":["no-new-privileges"],
            "Memory":profile.resources.memory_bytes, "CpuPeriod":100000,
            "CpuQuota":u64::from(profile.resources.cpu_milli)*100,
            "PidsLimit":profile.resources.pids_limit,
            "Mounts":[{"Type":"bind","Source":helper_path,"Target":HELPER_DESTINATION,"ReadOnly":true}]
        }
    });
    if profile.network == BenchmarkNetwork::PublicInternet && !proxy_environment.is_empty() {
        // Docker exec inherits container defaults for both model tools and
        // independent verifier commands; per-exec variables can still override.
        body["Env"] = serde_json::json!(proxy_environment
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>());
    }
    Ok(body)
}

pub(super) fn network_mode(network: BenchmarkNetwork) -> &'static str {
    match network {
        BenchmarkNetwork::PublicInternet => "bridge",
        BenchmarkNetwork::Disabled => "none",
    }
}

pub(super) fn helper_path() -> Result<String, String> {
    let source = env::var("BENCHMARK_EXECUTION_HELPER")
        .map_err(|_| "benchmark execution helper not configured")?;
    let expected_hash = env::var("BENCHMARK_EXECUTION_HELPER_SHA256")
        .map_err(|_| "benchmark execution helper digest not configured")?;
    if !source.starts_with('/') || source.chars().any(char::is_control) {
        return Err("benchmark execution helper path invalid".into());
    }
    let metadata = fs::symlink_metadata(&source).map_err(|_| "benchmark helper unavailable")?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > 128 * 1024 * 1024
    {
        return Err("benchmark helper is not a bounded regular file".into());
    }
    let bytes = fs::read(&source).map_err(|_| "benchmark helper read failed")?;
    if expected_hash != format!("{:x}", Sha256::digest(bytes)) {
        return Err("benchmark helper digest mismatch".into());
    }
    Ok(source)
}

pub(super) fn policy(profile: &BenchmarkExecutionProfile) -> ExecutionPolicy {
    let mut policy = match profile.network {
        BenchmarkNetwork::PublicInternet => {
            ExecutionPolicy::workspace_write_public_internet(&profile.workspace_root)
        }
        BenchmarkNetwork::Disabled => {
            ExecutionPolicy::workspace_write_no_network(&profile.workspace_root)
        }
    };
    policy.filesystem.read_only_roots = vec!["/".into()];
    policy.filesystem.writable_roots = vec!["/".into()];
    policy
}

impl DockerExecutionHostRunner {
    pub(crate) fn is_benchmark(&self) -> bool {
        self.benchmark_profile.is_some()
    }

    pub(crate) fn workspace_root(&self) -> &str {
        self.benchmark_profile
            .as_ref()
            .map_or(WORKSPACE_DATA_ROOT, |profile| {
                profile.workspace_root.as_str()
            })
    }

    pub(crate) fn execution_policy(&self) -> ExecutionPolicy {
        self.benchmark_profile.as_ref().map_or_else(
            || ExecutionPolicy::workspace_write_no_network(WORKSPACE_DATA_ROOT),
            policy,
        )
    }

    fn with_benchmark_target<T: Send>(
        &self,
        operation: impl FnOnce(&str) -> Result<T, String> + Send,
    ) -> Result<T, String> {
        let allocation = self
            .retained_allocation
            .as_ref()
            .ok_or("benchmark allocation missing")?;
        let authorization = self
            .environment_authorization
            .as_ref()
            .ok_or("benchmark authority missing")?;
        let signing_key = retained::signing_key()?;
        let request = crate::postgres_store::execution_instances::RetainedInstanceRequest {
            environment_authorization: authorization,
            signing_key: signing_key.as_bytes(),
            execution_id: &self.execution_id,
            workspace_id: self
                .retained_workspace_id
                .as_deref()
                .ok_or("benchmark workspace missing")?,
            host_id: &allocation.host_id,
            image_digest: &self.image_digest,
            profile_digest: &allocation.profile_digest,
            resources: declared_usage(self.resources),
            lease: self
                .resident_lease
                .as_ref()
                .ok_or("benchmark execution lease missing")?,
        };
        self.resident_store
            .as_ref()
            .ok_or("benchmark store missing")?
            .with_retained_instance_operation(&request, allocation, |actual| {
                let id = actual
                    .container_id
                    .as_deref()
                    .ok_or("benchmark container missing")?;
                let facts = inspect_container(id)?.ok_or("benchmark physical container absent")?;
                if facts.id != id || !facts.matches(&self.container_expectation()) {
                    return Err("benchmark physical identity rejected".into());
                }
                operation(id)
            })
    }

    pub(super) fn benchmark_exec_request(
        &self,
        command: Vec<String>,
    ) -> Result<ExecRequest, String> {
        let allocation = self
            .retained_allocation
            .as_ref()
            .ok_or("benchmark allocation missing")?;
        Ok(ExecRequest::owned(
            allocation
                .container_id
                .as_deref()
                .ok_or("benchmark container unknown")?,
            &allocation.allocation_agent_run_id,
            &allocation.allocation_execution_id,
            command,
        ))
    }

    pub(super) fn spawn_command(&self, request: ExecRequest) -> Result<ExecChild, String> {
        if self.is_benchmark() {
            self.with_benchmark_target(|id| {
                if request.container != id {
                    return Err("benchmark command target mismatch".into());
                }
                request.spawn()
            })
        } else {
            request.spawn()
        }
    }

    pub(super) fn cancel_command_container(&self) -> Result<(), String> {
        if self.is_benchmark() {
            let mut command = self.benchmark_exec_request(vec![
                HELPER_DESTINATION.into(),
                "benchmark-quiesce".into(),
            ])?;
            command.user = Some(
                self.benchmark_profile
                    .as_ref()
                    .ok_or("benchmark profile missing")?
                    .user
                    .clone(),
            );
            command.cwd = Some("/".into());
            self.with_benchmark_target(|id| {
                if command.container != id {
                    return Err("benchmark command target mismatch".into());
                }
                let output = exec_with_input(&command, &[], 4096)?;
                require_quiescence_receipt(output.status.success(), &output.stdout)
            })
        } else {
            self.teardown_owned()
        }
    }

    pub(super) fn benchmark_status(&self) -> Result<ExecutionHostStatus, ExecutionError> {
        self.with_benchmark_target(|_| {
            Ok(ExecutionHostStatus::remote(
                true,
                ExecutionHostHealth::Ready,
                None,
            ))
        })
        .map_err(|error| self.unavailable(error))
    }

    pub(super) fn benchmark_filesystem(
        &self,
        request: ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        self.validate_policy(&request.policy)
            .map_err(|error| filesystem_unavailable(error.to_string()))?;
        if request.cwd != Path::new(self.workspace_root()) || request.model_path.contains("://") {
            return Err(filesystem_unavailable(
                "benchmark filesystem binding invalid",
            ));
        }
        let limit = filesystem_response_limit(&request.operation)?;
        let bytes = serde_json::to_vec(&request)
            .map_err(|_| filesystem_unavailable("benchmark filesystem encoding failed"))?;
        if bytes.len() > HELPER_JSON_LIMIT {
            return Err(filesystem_unavailable(
                "benchmark filesystem request too large",
            ));
        }
        let mut exec = self
            .benchmark_exec_request(vec![
                HELPER_DESTINATION.into(),
                "benchmark-filesystem-once".into(),
            ])
            .map_err(filesystem_unavailable)?;
        exec.user = self
            .benchmark_profile
            .as_ref()
            .map(|profile| profile.user.clone());
        exec.cwd = Some(self.workspace_root().into());
        let output = self
            .with_benchmark_target(|id| {
                if exec.container != id {
                    return Err("benchmark filesystem target mismatch".into());
                }
                exec_with_input(&exec, &bytes, limit)
            })
            .map_err(filesystem_unavailable)?;
        if !output.status.success() {
            return Err(filesystem_unavailable("benchmark filesystem helper failed"));
        }
        serde_json::from_slice::<SandboxFileSystemResult>(&output.stdout)
            .map_err(|_| filesystem_unavailable("benchmark filesystem response invalid"))?
            .into_result()
    }

    pub(crate) fn benchmark_setup_ready(&self, session_id: &str) -> Result<bool, String> {
        let Some(profile) = &self.benchmark_profile else {
            return Ok(true);
        };
        let allocation = self
            .retained_allocation
            .as_ref()
            .ok_or("benchmark allocation missing")?;
        self.resident_store
            .as_ref()
            .ok_or("benchmark store missing")?
            .benchmark_setup_ready(
                session_id,
                &self.environment_id,
                &self.agent_run_id,
                &self.instance_id,
                allocation
                    .container_id
                    .as_deref()
                    .ok_or("benchmark container unknown")?,
                &profile.digest()?,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_run_authorization::BenchmarkResources;

    #[test]
    fn cancellation_requires_a_successful_exact_quiescence_receipt() {
        let valid = br#"{"schema":"runtime.benchmark.quiescence.v1","quiescent":true}"#;
        assert!(require_quiescence_receipt(true, valid).is_ok());
        assert!(require_quiescence_receipt(false, valid).is_err());
        for invalid in [
            b"".as_slice(),
            br#"{"schema":"runtime.benchmark.quiescence.v1","quiescent":false}"#,
            br#"{"schema":"runtime.benchmark.quiescence.v1","quiescent":1}"#,
            br#"{"schema":"runtime.benchmark.quiescence.v1","quiescent":true,"extra":true}"#,
        ] {
            assert!(require_quiescence_receipt(true, invalid).is_err());
        }
    }

    fn profile() -> BenchmarkExecutionProfile {
        BenchmarkExecutionProfile {
            schema: "runtime.benchmark_execution_profile.v1".into(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            workspace_root: "/app".into(),
            user: "0:0".into(),
            network: BenchmarkNetwork::PublicInternet,
            resources: BenchmarkResources {
                memory_bytes: 2 * 1024 * 1024 * 1024,
                cpu_milli: 1000,
                pids_limit: 512,
                data_tmpfs_bytes: 64 * 1024 * 1024,
                storage_bytes: 10 * 1024 * 1024 * 1024,
            },
        }
    }

    #[test]
    fn original_image_paths_user_and_download_policy_are_applied_without_private_volumes() {
        let mut runner = super::super::tests::retained_test_runner();
        let profile = profile();
        runner.image_digest.clone_from(&profile.image_digest);
        runner.resources = profile.resources.sandbox_resources();
        runner.plugin_mounts.clear();
        let body = create_body(
            &runner.container_expectation(),
            &profile,
            "/srv/helpers/execution_agent",
        )
        .unwrap();
        assert_eq!(body["Image"], profile.image_digest);
        assert_eq!(body["WorkingDir"], "/app");
        assert_eq!(body["User"], "0:0");
        assert_eq!(body["HostConfig"]["NetworkMode"], "bridge");
        assert_eq!(body["HostConfig"]["ReadonlyRootfs"], false);
        assert_eq!(body["HostConfig"]["Memory"], 2_u64 * 1024 * 1024 * 1024);
        assert_eq!(body["HostConfig"]["CpuQuota"], 100_000);
        assert_eq!(body["Entrypoint"], serde_json::json!([HELPER_DESTINATION]));
        let mounts = body["HostConfig"]["Mounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0]["Source"], "/srv/helpers/execution_agent");
        assert_eq!(mounts[0]["Target"], HELPER_DESTINATION);
        assert_eq!(mounts[0]["ReadOnly"], true);
        assert!(body["HostConfig"]["Tmpfs"].is_null());
    }

    #[test]
    fn docker_helper_bind_inspection_does_not_require_volume_options() {
        let runner = super::super::tests::retained_test_runner();
        let helper: super::super::ContainerMountFacts = serde_json::from_value(serde_json::json!({
            "Type": "bind", "Source": "/srv/helpers/execution_agent",
            "Target": HELPER_DESTINATION, "ReadOnly": true,
        }))
        .expect("Docker bind mount omits volume-only options");
        assert_eq!(helper.target, HELPER_DESTINATION);
        assert!(!super::super::memory_mount_matches(
            &[helper],
            &runner.memory_mount
        ));
        let missing_scope: super::super::ContainerMountFacts =
            serde_json::from_value(serde_json::json!({
                "Type": "volume", "Source": runner.memory_mount.volume_name,
                "Target": super::super::MEMORY_CONTAINER_ROOT,
            }))
            .expect("volume facts can be observed without granting a scope match");
        assert!(!super::super::memory_mount_matches(
            &[missing_scope],
            &runner.memory_mount
        ));
    }

    #[test]
    fn benchmark_profile_cannot_change_signed_image_or_resource_envelope() {
        let runner = super::super::tests::retained_test_runner();
        assert!(create_body(&runner.container_expectation(), &profile(), "/srv/helper").is_err());
    }

    #[test]
    fn public_task_and_verifier_inherit_the_same_host_proxy_defaults() {
        let mut runner = super::super::tests::retained_test_runner();
        let profile = profile();
        runner.image_digest.clone_from(&profile.image_digest);
        runner.resources = profile.resources.sandbox_resources();
        runner.plugin_mounts.clear();
        let proxy = BTreeMap::from([
            ("HTTP_PROXY".into(), "http://172.17.0.1:17890".into()),
            ("http_proxy".into(), "http://172.17.0.1:17890".into()),
            ("HTTPS_PROXY".into(), "http://172.17.0.1:17890".into()),
            ("https_proxy".into(), "http://172.17.0.1:17890".into()),
            ("NO_PROXY".into(), "localhost,127.0.0.1,::1".into()),
            ("no_proxy".into(), "localhost,127.0.0.1,::1".into()),
        ]);
        let body = create_body_with_proxy_environment(
            &runner.container_expectation(),
            &profile,
            "/srv/helper",
            &proxy,
        )
        .unwrap();
        let actual: BTreeMap<_, _> = body["Env"]
            .as_array()
            .expect("container defaults shared by all execs")
            .iter()
            .map(|value| value.as_str().unwrap().split_once('=').unwrap())
            .collect();
        for (key, value) in &proxy {
            assert_eq!(actual.get(key.as_str()), Some(&value.as_str()));
        }
        let offline = BenchmarkExecutionProfile {
            network: BenchmarkNetwork::Disabled,
            ..profile
        };
        let body = create_body_with_proxy_environment(
            &runner.container_expectation(),
            &offline,
            "/srv/helper",
            &proxy,
        )
        .unwrap();
        assert_eq!(body["HostConfig"]["NetworkMode"], "none");
        assert!(
            body["Env"].is_null(),
            "offline tasks receive no proxy capability"
        );
    }
}
