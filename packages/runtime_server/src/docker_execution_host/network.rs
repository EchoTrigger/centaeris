//! Translate the signed network capability into Docker and Core policy.
use super::{environment, DockerExecutionHostRunner, ExecutionNetwork, ExecutionPolicy};
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
        let mut values = body["Env"].as_array().cloned().unwrap_or_default();
        values.extend(
            proxy_environment
                .iter()
                .map(|(name, value)| serde_json::json!(format!("{name}={value}"))),
        );
        body["Env"] = serde_json::Value::Array(values);
    }
}

impl DockerExecutionHostRunner {
    pub(crate) fn execution_policy(&self) -> ExecutionPolicy {
        self.environment_profile.as_ref().map_or_else(
            || policy(self.network, WORKSPACE_DATA_ROOT),
            super::environment_policy,
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
            runner.environment_profile.as_mut().unwrap().network = network;
            let body = super::super::environment_container::create_body_with_proxy_environment(
                &runner.container_expectation(),
                runner.environment_profile.as_ref().unwrap(),
                runner.benchmark_helper.as_deref().unwrap(),
                &proxy,
            )
            .unwrap();
            if network == ExecutionNetwork::Disabled {
                let actual = body["Env"].as_array().unwrap();
                assert_eq!(actual.len(), 2);
                assert!(actual
                    .iter()
                    .all(|entry| entry.as_str().unwrap().starts_with("CENTAERIS_WORKSPACE_")));
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
