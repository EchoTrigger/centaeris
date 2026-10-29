//! Container configuration for the current Session environment profile.
use super::*;
use crate::agent_run_authorization::EnvironmentProfile;

pub(super) const WORKSPACE_BACKING_LABEL: &str = "centaeris.workspace_backing";
pub(super) const WORKSPACE_BACKING_DOCKER_OVERLAYFS: &str = "dockerOverlayfs";

pub(super) const HELPER_DESTINATION: &str = super::AGENT_BINARY;

pub(super) fn create_body(
    expected: &ContainerExpectation<'_>,
    profile: &EnvironmentProfile,
    helper_path: &str,
) -> Result<serde_json::Value, String> {
    let proxy_environment = network::proxy_environment(expected.network)?;
    create_body_with_proxy_environment(expected, profile, helper_path, &proxy_environment)
}

pub(super) fn create_body_with_proxy_environment(
    expected: &ContainerExpectation<'_>,
    profile: &EnvironmentProfile,
    helper_path: &str,
    proxy_environment: &BTreeMap<String, String>,
) -> Result<serde_json::Value, String> {
    profile.validate()?;
    if expected.image_digest != profile.image_digest
        || expected.resources != profile.resources.sandbox_resources()
        || expected.network != profile.network
        || !helper_path.starts_with('/')
        || helper_path.chars().any(char::is_control)
    {
        return Err("environment signed host profile mismatch".into());
    }
    let owner = expected.allocation_owner.cloned().unwrap_or(
        centaeris_core::execution::EnvironmentTaskOwner::AgentRun {
            run_id: expected.agent_run_id.into(),
        },
    );
    let mut labels = serde_json::json!({
        "centaeris.managed":"true", "centaeris.allocation_owner":serde_json::to_string(&owner).map_err(|_|"environment_owner_invalid")?,
        "centaeris.environment_id":expected.environment_id, "centaeris.instance_id":expected.instance_id,
        "centaeris.environment_authorization_digest":expected.binding_digest,
        "centaeris.execution_id":expected.execution_id,
        "centaeris.environment_profile_digest":profile.digest()?,
        "centaeris.data_budget":profile.resources.data_tmpfs_bytes.to_string(),
    });
    labels[WORKSPACE_BACKING_LABEL] = serde_json::json!(WORKSPACE_BACKING_DOCKER_OVERLAYFS);
    if let centaeris_core::execution::EnvironmentTaskOwner::AgentRun { run_id } = &owner {
        labels["centaeris.agent_run_id"] = serde_json::json!(run_id);
    }
    let mut mounts=expected.plugin_mounts.iter().map(|mount|serde_json::json!({"Type":"volume","Source":expected.plugin_volume_name,"Target":mount.destination,"ReadOnly":true,"VolumeOptions":{"Subpath":mount.source_subpath}})).collect::<Vec<_>>();
    mounts.push(serde_json::json!({"Type":"bind","Source":helper_path,"Target":HELPER_DESTINATION,"ReadOnly":true}));
    let mut body = serde_json::json!({
        "Image":profile.image_digest, "Labels":labels, "User":profile.user,
        "WorkingDir":"/","Entrypoint":[],
        "Cmd":[HELPER_DESTINATION,"environment-keeper"],
        "Env":[format!("CENTAERIS_WORKSPACE_ROOT={}",profile.workspace_root),format!("CENTAERIS_WORKSPACE_USER={}",profile.user)],
        "HostConfig":{
            "Runtime":expected.oci_runtime.docker_runtime_name(), "NetworkMode":network::mode(expected.network),
            "ReadonlyRootfs":false,"CapDrop":["ALL"],"CapAdd":[],"SecurityOpt":["no-new-privileges"],
            "Memory":profile.resources.memory_bytes, "CpuPeriod":100000,
            "CpuQuota":u64::from(profile.resources.cpu_milli)*100,
            "PidsLimit":profile.resources.pids_limit,"Mounts":mounts
        }
    });
    network::apply_proxy_environment(&mut body, expected.network, proxy_environment);
    Ok(body)
}

pub(super) fn helper_path() -> Result<String, String> {
    helper_path_with(|key| env::var(key), |path| fs::symlink_metadata(path))
}

fn helper_path_with(
    config: impl Fn(&str) -> Result<String, env::VarError>,
    metadata: impl Fn(&str) -> std::io::Result<fs::Metadata>,
) -> Result<String, String> {
    let source = config("ENVIRONMENT_EXECUTION_HELPER")
        .map_err(|_| "environment execution helper not configured")?;
    if !source.starts_with('/') || source.chars().any(char::is_control) {
        return Err("environment execution helper path invalid".into());
    }
    let metadata = metadata(&source).map_err(|_| "environment helper unavailable")?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("environment helper is not a regular file".into());
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_run_authorization::EnvironmentResources;

    fn profile() -> EnvironmentProfile {
        EnvironmentProfile {
            schema: "runtime.environment_profile.v1".into(),
            image_digest: format!("sha256:{}", "a".repeat(64)),
            workspace_root: "/app".into(),
            user: "0:0".into(),
            network: ExecutionNetwork::PublicInternet,
            resources: EnvironmentResources {
                memory_bytes: 2 * 1024 * 1024 * 1024,
                cpu_milli: 1000,
                pids_limit: 512,
                data_tmpfs_bytes: 64 * 1024 * 1024,
                storage_bytes: 10 * 1024 * 1024 * 1024,
            },
        }
    }

    #[test]
    fn private_package_versions_mount_exact_digest_and_refuse_existing_version_replacement() {
        let root = tempfile::tempdir().unwrap();
        let mut versions = Vec::new();
        for version in ["1.0.0", "2.0.0"] {
            let staged = root.path().join(version);
            fs::create_dir_all(staged.join(".centaeris-plugin")).unwrap();
            fs::write(
                staged.join(".centaeris-plugin/plugin.json"),
                serde_json::json!({"name":"banana","version":version,"paths":{}}).to_string(),
            )
            .unwrap();
            let activation = centaeris_core::extension::build_plugin_activation_snapshot(
                std::slice::from_ref(&staged),
            )
            .unwrap();
            let hash = activation.packages[0]
                .package_digest
                .strip_prefix("sha256:")
                .unwrap();
            let pinned = root.path().join(".packages").join(hash).join("banana");
            fs::create_dir_all(pinned.parent().unwrap()).unwrap();
            fs::rename(staged, &pinned).unwrap();
            let subpath = format!(".packages/{hash}/banana");
            versions.push((activation, subpath));
        }
        let old_mounts = super::super::plugin_mounts_at(&versions[0].0, root.path()).unwrap();
        let new_mounts = super::super::plugin_mounts_at(&versions[1].0, root.path()).unwrap();
        let mut runner = super::super::tests::retained_test_runner();
        let profile = profile();
        runner.image_digest.clone_from(&profile.image_digest);
        runner.resources = profile.resources.sandbox_resources();
        runner.network = profile.network;
        runner.plugin_mounts = old_mounts.clone();
        let body = create_body_with_proxy_environment(
            &runner.container_expectation(),
            &profile,
            "/srv/helpers/execution_agent",
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            body["HostConfig"]["Mounts"][0]["VolumeOptions"]["Subpath"],
            versions[0].1
        );
        assert_eq!(
            body["HostConfig"]["Mounts"][0]["Target"],
            "/opt/centaeris/plugins/banana"
        );
        assert_eq!(body["HostConfig"]["Mounts"][0]["ReadOnly"], true);
        let observed: Vec<super::super::ContainerMountFacts> =
            serde_json::from_value(body["HostConfig"]["Mounts"].clone()).unwrap();
        super::super::require_plugin_mounts_match(
            &observed,
            &runner.plugin_volume_name,
            &old_mounts,
        )
        .unwrap();
        assert_eq!(
            super::super::require_plugin_mounts_match(
                &observed,
                &runner.plugin_volume_name,
                &new_mounts
            )
            .unwrap_err(),
            "environment_plugin_version_mismatch"
        );
        // A fresh environment uses V2. The existing V1 request and its bytes are
        // unchanged; rejection never invokes replacement or container mutation.
        runner.plugin_mounts = new_mounts;
        let fresh = create_body_with_proxy_environment(
            &runner.container_expectation(),
            &profile,
            "/srv/helpers/execution_agent",
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            fresh["HostConfig"]["Mounts"][0]["VolumeOptions"]["Subpath"],
            versions[1].1
        );
        assert!(root.path().join(&versions[0].1).is_dir());
    }

    #[test]
    fn configured_helper_without_digest_builds_the_current_environment() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let source = "/srv/managed/execution_agent";
        let selected = helper_path_with(
            |key| match key {
                "ENVIRONMENT_EXECUTION_HELPER" => Ok(source.into()),
                _ => Err(env::VarError::NotPresent),
            },
            |path| {
                assert_eq!(path, source);
                fs::symlink_metadata(file.path())
            },
        )
        .expect("a configured regular helper needs no operator-supplied digest");
        let mut runner = super::super::tests::retained_test_runner();
        let profile = profile();
        runner.image_digest.clone_from(&profile.image_digest);
        runner.resources = profile.resources.sandbox_resources();
        runner.network = profile.network;
        runner.plugin_mounts.clear();
        let body = create_body_with_proxy_environment(
            &runner.container_expectation(),
            &profile,
            &selected,
            &BTreeMap::new(),
        )
        .unwrap();
        let mounts = body["HostConfig"]["Mounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0]["Source"], source);
        assert_eq!(mounts[0]["Target"], HELPER_DESTINATION);
        assert_eq!(mounts[0]["ReadOnly"], true);
        assert_eq!(
            body["Cmd"],
            serde_json::json!([HELPER_DESTINATION, "environment-keeper"])
        );
    }

    #[test]
    fn runsc_creates_the_signed_environment_without_downgrading_the_runtime() {
        let mut runner = super::super::tests::retained_test_runner();
        let profile = profile();
        runner.oci_runtime = OciRuntime::Runsc;
        runner.image_digest.clone_from(&profile.image_digest);
        runner.resources = profile.resources.sandbox_resources();
        runner.network = profile.network;
        runner.plugin_mounts.clear();
        let body = create_body_with_proxy_environment(
            &runner.container_expectation(),
            &profile,
            "/srv/helpers/execution_agent",
            &BTreeMap::new(),
        )
        .expect("runsc must accept the same signed environment profile");
        assert_eq!(body["HostConfig"]["Runtime"], "runsc");
        assert_eq!(
            body["Labels"][WORKSPACE_BACKING_LABEL],
            WORKSPACE_BACKING_DOCKER_OVERLAYFS
        );
        assert_eq!(body["Image"], profile.image_digest);
        let mut facts = serde_json::json!({"Id":"physical","Image":body["Image"],"Config":body,"HostConfig":body["HostConfig"]});
        crate::docker_engine::verify_created(&facts, &body)
            .expect("new backing policy matches created configuration");
        facts["Config"]["Labels"]
            .as_object_mut()
            .unwrap()
            .remove(WORKSPACE_BACKING_LABEL);
        assert!(
            crate::docker_engine::verify_created(&facts, &body).is_err(),
            "an old unlabeled container cannot be adopted as host-backed"
        );
        assert_eq!(body["User"], profile.user);
        assert_eq!(body["HostConfig"]["CapDrop"], serde_json::json!(["ALL"]));
        assert_eq!(body["HostConfig"]["Mounts"][0]["ReadOnly"], true);
        runner.resources.memory_bytes += 1;
        assert!(create_body_with_proxy_environment(
            &runner.container_expectation(),
            &profile,
            "/srv/helpers/execution_agent",
            &BTreeMap::new(),
        )
        .is_err());
    }

    #[test]
    fn helper_path_rejects_missing_configuration_and_invalid_paths() {
        assert_eq!(
            helper_path_with(
                |_| Err(env::VarError::NotPresent),
                |_| panic!("no configured path")
            )
            .unwrap_err(),
            "environment execution helper not configured"
        );
        for source in ["relative/helper", "/srv/helper\n"] {
            assert_eq!(
                helper_path_with(
                    |_| Ok(source.into()),
                    |_| panic!("invalid path must not reach filesystem")
                )
                .unwrap_err(),
                "environment execution helper path invalid"
            );
        }
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            helper_path_with(
                |_| Ok("/srv/missing/helper".into()),
                |_| fs::symlink_metadata(root.path().join("missing"))
            )
            .unwrap_err(),
            "environment helper unavailable"
        );
    }

    #[test]
    fn helper_path_rejects_a_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(helper_path_with(
            |_| Ok("/srv/helper".into()),
            |_| fs::symlink_metadata(root.path())
        )
        .is_err());
    }

    #[test]
    fn original_image_paths_user_and_download_policy_are_applied_without_private_volumes() {
        let mut runner = super::super::tests::retained_test_runner();
        let profile = profile();
        runner.image_digest.clone_from(&profile.image_digest);
        runner.resources = profile.resources.sandbox_resources();
        runner.network = profile.network;
        runner.plugin_mounts.clear();
        let body = create_body(
            &runner.container_expectation(),
            &profile,
            "/srv/helpers/execution_agent",
        )
        .unwrap();
        assert_eq!(body["Image"], profile.image_digest);
        let mut facts = serde_json::json!({"Id":"physical","Image":body["Image"],"Config":body,"HostConfig":body["HostConfig"]});
        crate::docker_engine::verify_created(&facts, &body)
            .expect("new backing policy matches created configuration");
        facts["Config"]["Labels"]
            .as_object_mut()
            .unwrap()
            .remove(WORKSPACE_BACKING_LABEL);
        assert!(
            crate::docker_engine::verify_created(&facts, &body).is_err(),
            "an old unlabeled container cannot be adopted as host-backed"
        );
        assert_eq!(body["WorkingDir"], "/");
        assert_eq!(body["Env"][0], "CENTAERIS_WORKSPACE_ROOT=/app");
        assert_eq!(body["Env"][1], "CENTAERIS_WORKSPACE_USER=0:0");
        assert_eq!(body["User"], "0:0");
        assert_eq!(body["HostConfig"]["NetworkMode"], "bridge");
        assert_eq!(body["HostConfig"]["ReadonlyRootfs"], false);
        assert_eq!(body["HostConfig"]["Memory"], 2_u64 * 1024 * 1024 * 1024);
        assert_eq!(body["HostConfig"]["CpuQuota"], 100_000);
        assert_eq!(body["Entrypoint"], serde_json::json!([]));
        assert_eq!(
            body["Cmd"],
            serde_json::json!([HELPER_DESTINATION, "environment-keeper"])
        );
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
    fn environment_profile_cannot_change_signed_image_or_resource_envelope() {
        let runner = super::super::tests::retained_test_runner();
        assert!(create_body(&runner.container_expectation(), &profile(), "/srv/helper").is_err());
    }

    #[test]
    fn public_task_and_verifier_inherit_the_same_host_proxy_defaults() {
        let mut runner = super::super::tests::retained_test_runner();
        let profile = profile();
        runner.image_digest.clone_from(&profile.image_digest);
        runner.resources = profile.resources.sandbox_resources();
        runner.network = profile.network;
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
        let offline = EnvironmentProfile {
            network: ExecutionNetwork::Disabled,
            ..profile
        };
        runner.network = offline.network;
        let body = create_body_with_proxy_environment(
            &runner.container_expectation(),
            &offline,
            "/srv/helper",
            &proxy,
        )
        .unwrap();
        assert_eq!(body["HostConfig"]["NetworkMode"], "none");
        let actual = body["Env"]
            .as_array()
            .expect("trusted workspace configuration");
        assert_eq!(actual.len(), 2);
        assert_eq!(actual[0], "CENTAERIS_WORKSPACE_ROOT=/app");
        assert_eq!(actual[1], "CENTAERIS_WORKSPACE_USER=0:0");
    }
    #[test]
    #[ignore = "requires explicit paths for an owned physical storage probe"]
    fn export_owned_runsc_storage_probe_request() {
        let input = std::env::var("CENTAERIS_STORAGE_PROBE_INPUT").expect("probe input required");
        let output =
            std::env::var("CENTAERIS_STORAGE_PROBE_OUTPUT").expect("probe output required");
        let input: serde_json::Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
        let profile: EnvironmentProfile = serde_json::from_value(input["profile"].clone()).unwrap();
        let mut runner = super::super::tests::retained_test_runner();
        runner.oci_runtime = OciRuntime::Runsc;
        runner.image_digest.clone_from(&profile.image_digest);
        runner.resources = profile.resources.sandbox_resources();
        runner.network = profile.network;
        runner.plugin_mounts.clear();
        let body = create_body_with_proxy_environment(
            &runner.container_expectation(),
            &profile,
            input["helperPath"].as_str().unwrap(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(body["HostConfig"]["Runtime"], "runsc");
        assert_eq!(
            body["Labels"][WORKSPACE_BACKING_LABEL],
            WORKSPACE_BACKING_DOCKER_OVERLAYFS
        );
        assert_eq!(body["HostConfig"]["Memory"], profile.resources.memory_bytes);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .unwrap();
        use std::io::Write;
        file.write_all(&serde_json::to_vec_pretty(&body).unwrap())
            .unwrap();
        file.sync_all().unwrap();
    }
}
