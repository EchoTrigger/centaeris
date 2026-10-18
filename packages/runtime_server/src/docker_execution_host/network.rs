//! Translate the signed network capability into Docker and Core policy.
use super::{benchmark, environment, DockerExecutionHostRunner, ExecutionNetwork, ExecutionPolicy};
use centaeris_core::execution::WORKSPACE_DATA_ROOT;
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn mode(network: ExecutionNetwork) -> &'static str {
    match network {
        ExecutionNetwork::PublicInternet => "bridge",
        ExecutionNetwork::Disabled => "none",
    }
}

pub(super) fn policy(network: ExecutionNetwork, root: impl AsRef<Path>) -> ExecutionPolicy {
    match network {
        ExecutionNetwork::PublicInternet => {
            ExecutionPolicy::workspace_write_public_internet(root.as_ref())
        }
        ExecutionNetwork::Disabled => ExecutionPolicy::workspace_write_no_network(root.as_ref()),
    }
}

pub(super) fn proxy_environment(
    network: ExecutionNetwork,
) -> Result<BTreeMap<String, String>, String> {
    match network {
        ExecutionNetwork::PublicInternet => environment::public_internet_proxy_environment(),
        ExecutionNetwork::Disabled => Ok(BTreeMap::new()),
    }
}

pub(super) fn apply_proxy_environment(
    body: &mut serde_json::Value,
    network: ExecutionNetwork,
    proxy_environment: &BTreeMap<String, String>,
) {
    if network == ExecutionNetwork::PublicInternet && !proxy_environment.is_empty() {
        // Container defaults are inherited by ordinary commands, helpers and
        // independent verifier execs. Never inherit control-plane proxy values.
        body["Env"] = serde_json::json!(proxy_environment
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>());
    }
}

impl DockerExecutionHostRunner {
    pub(crate) fn execution_policy(&self) -> ExecutionPolicy {
        self.benchmark_profile.as_ref().map_or_else(
            || policy(self.network, WORKSPACE_DATA_ROOT),
            benchmark::policy,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_commands_receive_proxy_defaults_only_with_public_internet() {
        let proxy = BTreeMap::from([
            ("HTTP_PROXY".into(), "http://proxy.invalid:17890".into()),
            ("http_proxy".into(), "http://proxy.invalid:17890".into()),
            ("NO_PROXY".into(), "localhost,127.0.0.1,::1".into()),
        ]);
        let mut runner = super::super::tests::test_docker_execution_host_runner();
        for network in [ExecutionNetwork::Disabled, ExecutionNetwork::PublicInternet] {
            runner.network = network;
            let body = super::super::ordinary_container_create_body(
                &runner.container_expectation(),
                &proxy,
            )
            .unwrap();
            if network == ExecutionNetwork::Disabled {
                assert!(body["Env"].is_null());
            } else {
                let actual: BTreeMap<_, _> = body["Env"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| entry.as_str().unwrap().split_once('=').unwrap())
                    .collect();
                for (name, value) in &proxy {
                    assert_eq!(actual.get(name.as_str()), Some(&value.as_str()));
                }
            }
        }
    }
}
