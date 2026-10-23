use super::*;

fn wire() -> Value {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../../../tests/workspace/fixtures/agent_run_authorization/v1/valid.json"
    ))
    .unwrap();
    let profile = serde_json::json!({"schema":"runtime.environment_profile.v1", "imageDigest":format!("sha256:{}", "a".repeat(64)), "workspaceRoot":"/mnt/data", "user":"10001:10001", "network":"disabled", "resources":{"memoryBytes":1073741824,"cpuMilli":1000,"pidsLimit":128,"dataTmpfsBytes":1073741824,"storageBytes":1073741824}});
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
            profile
                .legacy()
                .unwrap()
                .resources
                .sandbox_resources()
                .memory_bytes,
            profile.legacy().unwrap().resources.memory_bytes
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
        "dataTmpfsBytes",
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

fn e2b_profile() -> Value {
    serde_json::from_str(include_str!(
        "../../../../tests/workspace/fixtures/e2b_environment_profile/v1/valid.json"
    ))
    .unwrap()
}

#[test]
fn e2b_profile_is_explicit_and_never_synthesizes_legacy_resource_promises() {
    let value = e2b_profile();
    let profile: EnvironmentProfile = serde_json::from_value(value.clone()).unwrap();
    profile.validate().unwrap();
    assert_eq!(serde_json::to_value(&profile).unwrap(), value);
    assert_eq!(profile.legacy().unwrap_err(), "e2b_execution_unavailable");
    assert_eq!(profile.input_budget_bytes().unwrap(), 67108864);
    for (field, value) in [
        (
            "resources",
            serde_json::json!({"storageBytes":4294967296_u64}),
        ),
        ("timeoutSeconds", serde_json::json!(86401)),
        ("inputBudgetBytes", serde_json::json!(67108865)),
        ("user", serde_json::json!("user; command")),
        ("provider", serde_json::json!("docker")),
    ] {
        let mut changed = e2b_profile();
        changed[field] = value;
        if let Ok(parsed) = serde_json::from_value::<EnvironmentProfile>(changed) {
            assert!(parsed.validate().is_err(), "{field}");
        }
    }
}

#[test]
fn e2b_authorization_binds_provider_build_and_input_transfer_limit() {
    let mut value = wire();
    value["environmentProfile"] = e2b_profile();
    value["environmentProfileDigest"] = format!(
        "sha256:{:x}",
        Sha256::digest(canonical_json(e2b_profile()).unwrap().as_bytes())
    )
    .into();
    let authorization: WorkspaceAgentRunAuthorization =
        serde_json::from_value(value.clone()).unwrap();
    authorization.validate().unwrap();
    let signature = authorization.signature(b"test-key").unwrap();
    value["environmentProfile"]["templateRef"]["buildId"] =
        "191c5835-5da7-4cf5-8a7c-cc3f7c4ebfa4".into();
    let changed: EnvironmentProfile =
        serde_json::from_value(value["environmentProfile"].clone()).unwrap();
    value["environmentProfileDigest"] = changed.digest().unwrap().into();
    let changed: WorkspaceAgentRunAuthorization = serde_json::from_value(value).unwrap();
    assert!(changed.verify_signature(b"test-key", &signature).is_err());
}

#[test]
fn profile_wire_rejects_duplicate_fields_in_both_variants_and_nested_contracts() {
    for value in [wire()["environmentProfile"].clone(), e2b_profile()] {
        let raw = serde_json::to_string(&value).unwrap();
        for (field, content) in value.as_object().unwrap() {
            let needle = format!(
                "{}:{}",
                serde_json::to_string(field).unwrap(),
                serde_json::to_string(content).unwrap()
            );
            let duplicate = raw.replacen(&needle, &format!("{needle},{needle}"), 1);
            assert!(
                serde_json::from_str::<EnvironmentProfile>(&duplicate).is_err(),
                "duplicate {field}"
            );
        }
        let nested_name = if value.get("resources").is_some() {
            "resources"
        } else {
            "templateRef"
        };
        for (field, content) in value[nested_name].as_object().unwrap() {
            let needle = format!(
                "{}:{}",
                serde_json::to_string(field).unwrap(),
                serde_json::to_string(content).unwrap()
            );
            let duplicate = raw.replacen(&needle, &format!("{needle},{needle}"), 1);
            assert!(
                serde_json::from_str::<EnvironmentProfile>(&duplicate).is_err(),
                "duplicate {nested_name}.{field}"
            );
        }
    }
}

#[test]
fn managed_workspace_initialization_is_explicit_shared_and_signed() {
    let value: Value = serde_json::from_str(include_str!(
        "../../../../tests/workspace/fixtures/e2b_environment_profile/v1/managed-files.json"
    ))
    .unwrap();
    let profile: EnvironmentProfile = serde_json::from_value(value.clone()).unwrap();
    profile.validate().unwrap();
    assert!(profile.managed_files());
    assert_eq!(serde_json::to_value(&profile).unwrap(), value);
    let mut original = value.clone();
    original
        .as_object_mut()
        .unwrap()
        .remove("workspaceInitialization");
    let old: EnvironmentProfile = serde_json::from_value(original.clone()).unwrap();
    old.validate().unwrap();
    assert!(!old.managed_files());
    assert_eq!(serde_json::to_value(&old).unwrap(), original);
    assert_ne!(profile.digest().unwrap(), old.digest().unwrap());

    let mut authorization = wire();
    authorization["environmentProfile"] = value;
    authorization["environmentProfileDigest"] = profile.digest().unwrap().into();
    let signed: WorkspaceAgentRunAuthorization =
        serde_json::from_value(authorization.clone()).unwrap();
    signed.validate().unwrap();
    let signature = signed.signature(b"test-key").unwrap();
    authorization["environmentProfile"] = original;
    authorization["environmentProfileDigest"] = old.digest().unwrap().into();
    let changed: WorkspaceAgentRunAuthorization = serde_json::from_value(authorization).unwrap();
    assert!(changed.verify_signature(b"test-key", &signature).is_err());
}

#[test]
fn managed_workspace_initialization_rejects_unknown_null_and_unmanaged_roots() {
    for value in [
        serde_json::json!(null),
        serde_json::json!(true),
        serde_json::json!(0),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!(""),
        serde_json::json!("preserve"),
        serde_json::json!("managed_files"),
    ] {
        let mut changed = e2b_profile();
        changed["workspaceInitialization"] = value;
        if let Ok(profile) = serde_json::from_value::<EnvironmentProfile>(changed) {
            assert!(profile.validate().is_err());
        }
    }
    for (root, user) in [("/home/user", "agent"), ("/mnt/data", "user")] {
        let mut changed = e2b_profile();
        changed["workspaceInitialization"] = "managedFiles".into();
        changed["workspaceRoot"] = root.into();
        changed["user"] = user.into();
        if let Ok(profile) = serde_json::from_value::<EnvironmentProfile>(changed) {
            assert!(profile.validate().is_err());
        }
    }
    let raw = serde_json::to_string(&e2b_profile()).unwrap();
    let duplicate = raw.replacen("{", "{\"workspaceInitialization\":\"managedFiles\",\"workspaceInitialization\":\"managedFiles\",", 1);
    assert!(serde_json::from_str::<EnvironmentProfile>(&duplicate).is_err());
}
