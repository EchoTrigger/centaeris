use super::*;

fn wire() -> Value {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap();
    let profile = serde_json::json!({"schema":"runtime.environment_profile.v1", "imageDigest":format!("sha256:{}", "a".repeat(64)), "workspaceRoot":"/mnt/data", "user":"10001:10001", "network":"disabled", "resources":{"memoryBytes":1073741824,"cpuMilli":1000,"pidsLimit":128,"workspaceBytes":1073741824,"storageBytes":1073741824}});
    value["schema"] = AGENT_RUN_AUTHORIZATION_SCHEMA.into();
    value["environmentProfileDigest"] = format!(
        "sha256:{:x}",
        Sha256::digest(canonical_json(profile.clone()).unwrap().as_bytes())
    )
    .into();
    value["environmentProfile"] = profile;
    value
}

#[test]
fn generic_environment_profile_is_required_and_authenticated_by_the_only_authorization_schema() {
    let value = wire();
    let auth: WorkspaceAgentRunAuthorization =
        serde_json::from_value(value.clone()).expect("one strict generic environment profile");
    auth.validate().unwrap();
    assert_eq!(serde_json::to_value(&auth).unwrap(), value);
    let signature = auth.signature(b"test-key").unwrap();
    let mut changed = value;
    changed["environmentProfile"]["network"] = "publicInternet".into();
    changed["environmentProfileDigest"] = format!(
        "sha256:{:x}",
        Sha256::digest(
            canonical_json(changed["environmentProfile"].clone())
                .unwrap()
                .as_bytes()
        )
    )
    .into();
    let changed: WorkspaceAgentRunAuthorization = serde_json::from_value(changed).unwrap();
    changed.validate().unwrap();
    assert!(changed.verify_signature(b"test-key", &signature).is_err());
}

#[test]
fn generic_environment_authorization_requires_complete_profile_and_matching_digest() {
    let mut unknown = wire();
    unknown["futureField"] = Value::Null;
    assert!(serde_json::from_value::<WorkspaceAgentRunAuthorization>(unknown).is_err());
    for field in ["environmentProfile", "environmentProfileDigest"] {
        let mut value = wire();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<WorkspaceAgentRunAuthorization>(value).is_err(),
            "{field}"
        );
    }
    let mut mismatch = wire();
    mismatch["environmentProfile"]["user"] = "0:0".into();
    let mismatch: WorkspaceAgentRunAuthorization = serde_json::from_value(mismatch).unwrap();
    assert!(mismatch.validate().is_err());
}

#[test]
fn generic_environment_profile_accepts_explicit_roots_users_and_networks() {
    for (root, user, network) in [
        ("/", "0:0", "disabled"),
        ("/app/work", "123:456", "publicInternet"),
    ] {
        let mut value = wire()["environmentProfile"].clone();
        value["workspaceRoot"] = root.into();
        value["user"] = user.into();
        value["network"] = network.into();
        let profile: EnvironmentProfile = serde_json::from_value(value).unwrap();
        profile.validate().unwrap();
        assert_eq!(
            profile.resources.sandbox_resources().memory_bytes,
            profile.resources.memory_bytes
        );
    }
}
#[test]
fn generic_environment_profile_rejects_unsafe_targets_and_unbounded_resources() {
    for (field, value) in [
        ("workspaceRoot", serde_json::json!("/a/../b")),
        ("workspaceRoot", serde_json::json!("/run/centaeris/secret")),
        ("user", serde_json::json!("01:1")),
        ("network", serde_json::json!("host")),
    ] {
        let mut profile = wire()["environmentProfile"].clone();
        profile[field] = value;
        if let Ok(profile) = serde_json::from_value::<EnvironmentProfile>(profile) {
            assert!(profile.validate().is_err());
        }
    }
    for field in [
        "cpuMilli",
        "memoryBytes",
        "pidsLimit",
        "workspaceBytes",
        "storageBytes",
    ] {
        for value in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(true),
            serde_json::json!(null),
        ] {
            let mut profile = wire()["environmentProfile"].clone();
            profile["resources"][field] = value;
            if let Ok(profile) = serde_json::from_value::<EnvironmentProfile>(profile) {
                assert!(profile.validate().is_err(), "{field}");
            }
        }
    }
}

#[test]
fn workspace_budget_has_one_current_wire_name() {
    let current = wire()["environmentProfile"].clone();
    let resources = current["resources"].as_object().unwrap();
    assert!(serde_json::from_value::<EnvironmentProfile>(current.clone()).is_ok());
    for retain_current in [false, true] {
        let mut old_name = current.clone();
        old_name["resources"]["dataTmpfsBytes"] = resources["workspaceBytes"].clone();
        if !retain_current {
            old_name["resources"]
                .as_object_mut()
                .unwrap()
                .remove("workspaceBytes");
        }
        assert!(serde_json::from_value::<EnvironmentProfile>(old_name).is_err());
    }
}
