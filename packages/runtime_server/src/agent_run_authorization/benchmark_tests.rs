use super::*;

fn benchmark_wire() -> Value {
    let mut payload = serde_json::to_value(tests::authorization()).unwrap();
    let profile = serde_json::json!({
        "schema": "runtime.benchmark_execution_profile.v1",
        "imageDigest": payload["imageDigest"],
        "workspaceRoot": "/app",
        "user": "0:0",
        "network": "publicInternet",
        "resources": {
            "cpuMilli": payload["resources"]["cpuMilli"],
            "memoryBytes": payload["resources"]["memoryBytes"],
            "pidsLimit": payload["resources"]["pidsLimit"],
            "dataTmpfsBytes": payload["resources"]["dataTmpfsBytes"],
            "storageBytes": 10737418240_u64
        }
    });
    payload["schema"] = "workspace.agent_run_authorization.v2".into();
    payload["imageCapability"] = "benchmark_task_v1".into();
    payload["benchmarkProfileDigest"] = format!(
        "sha256:{:x}",
        Sha256::digest(canonical_json(profile.clone()).unwrap().as_bytes())
    )
    .into();
    payload["benchmarkProfile"] = profile;
    payload
}

fn parse_valid(value: Value) -> WorkspaceAgentRunAuthorization {
    let parsed: WorkspaceAgentRunAuthorization =
        serde_json::from_value(value).expect("supported authorization wire");
    parsed.validate().expect("valid authorization");
    parsed
}

fn refresh_profile_digest(payload: &mut Value) {
    payload["benchmarkProfileDigest"] = format!(
        "sha256:{:x}",
        Sha256::digest(
            canonical_json(payload["benchmarkProfile"].clone())
                .unwrap()
                .as_bytes()
        )
    )
    .into();
}

#[test]
fn python_benchmark_authorization_fixture_reaches_the_rust_verifier() {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Expected {
        profile_digest: String,
        digest: String,
        signature: String,
    }
    let authorization: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
        "../../../../tests/workspace/fixtures/agent_run_authorization/v2/valid.json"
    ))
    .expect("Python v2 payload");
    let expected: Expected = serde_json::from_str(include_str!(
        "../../../../tests/workspace/fixtures/agent_run_authorization/v2/expected.json"
    ))
    .expect("Python v2 fingerprint");
    authorization.validate().unwrap();
    assert_eq!(
        authorization
            .benchmark_profile
            .as_ref()
            .unwrap()
            .digest()
            .unwrap(),
        expected.profile_digest
    );
    assert_eq!(authorization.digest().unwrap(), expected.digest);
    assert_eq!(
        authorization.signature(b"test-key").unwrap(),
        expected.signature
    );
    authorization
        .checked_binding("agent_run_1")
        .unwrap()
        .verify(&expected.digest, b"test-key", &expected.signature)
        .unwrap();
}

#[test]
fn benchmark_profile_is_authenticated_with_a_distinct_signature_domain() {
    let authorization = parse_valid(benchmark_wire());
    let signature = authorization.signature(b"test-key").unwrap();
    authorization
        .verify_signature(b"test-key", &signature)
        .unwrap();
    authorization
        .checked_binding(&authorization.agent_run_id)
        .unwrap()
        .verify(&authorization.digest().unwrap(), b"test-key", &signature)
        .unwrap();
    let mut legacy = Hmac::<Sha256>::new_from_slice(b"test-key").unwrap();
    legacy.update(b"workspace:agent-run-authorization:v1\0");
    legacy.update(authorization.digest().unwrap().as_bytes());
    let wrong_domain = format!("hmac-sha256:{:x}", legacy.finalize().into_bytes());
    assert!(authorization
        .verify_signature(b"test-key", &wrong_domain)
        .is_err());
}

#[test]
fn benchmark_authorization_requires_profile_digest_and_capability_binding() {
    for field in ["benchmarkProfile", "benchmarkProfileDigest"] {
        let mut payload = benchmark_wire();
        payload.as_object_mut().unwrap().remove(field);
        let parsed: WorkspaceAgentRunAuthorization = serde_json::from_value(payload).unwrap();
        assert!(parsed.validate().is_err(), "missing {field}");
    }
    for (pointer, changed) in [
        (
            "/benchmarkProfileDigest",
            Value::String(format!("sha256:{}", "0".repeat(64))),
        ),
        (
            "/imageCapability",
            Value::String("workspace_general_v1".into()),
        ),
        (
            "/benchmarkProfile/imageDigest",
            Value::String(format!("sha256:{}", "1".repeat(64))),
        ),
        ("/benchmarkProfile/resources/cpuMilli", Value::from(999_u64)),
        (
            "/benchmarkProfile/resources/memoryBytes",
            Value::from(999_u64),
        ),
        (
            "/benchmarkProfile/resources/pidsLimit",
            Value::from(999_u64),
        ),
        (
            "/benchmarkProfile/resources/dataTmpfsBytes",
            Value::from(999_u64),
        ),
    ] {
        let mut payload = benchmark_wire();
        *payload.pointer_mut(pointer).unwrap() = changed;
        if pointer.starts_with("/benchmarkProfile/") {
            refresh_profile_digest(&mut payload);
        }
        let parsed: WorkspaceAgentRunAuthorization = serde_json::from_value(payload).unwrap();
        assert!(parsed.validate().is_err(), "conflict at {pointer}");
    }
}

#[test]
fn general_authorization_bytes_and_legacy_signature_remain_unchanged() {
    let authorization = tests::authorization();
    let wire = serde_json::to_value(&authorization).unwrap();
    assert!(wire.get("benchmarkProfile").is_none());
    assert!(wire.get("benchmarkProfileDigest").is_none());
    assert_eq!(
        authorization.digest().unwrap(),
        "sha256:bd93e8ba466c0d9851e805dd2a8c8a5962b351065c4d982ae961ea0cdb0a6a9f"
    );
    assert_eq!(
        authorization.signature(b"test-key").unwrap(),
        "hmac-sha256:86d800a4e2517f1b169894646f9a7bb3297770235a7a3a0cbceffe1897969dd5"
    );
    let mut payload = benchmark_wire();
    payload["schema"] = AGENT_RUN_AUTHORIZATION_SCHEMA.into();
    payload["imageCapability"] = "workspace_general_v1".into();
    let parsed: WorkspaceAgentRunAuthorization = serde_json::from_value(payload).unwrap();
    assert!(parsed.validate().is_err());
    for field in ["benchmarkProfile", "benchmarkProfileDigest"] {
        let mut payload = serde_json::to_value(&authorization).unwrap();
        payload[field] = Value::Null;
        assert!(serde_json::from_value::<WorkspaceAgentRunAuthorization>(payload).is_err());
    }
}

#[test]
fn benchmark_profiles_reject_unsupported_paths_users_networks_and_resources() {
    for (pointer, bad) in [
        ("/workspaceRoot", Value::from("app")),
        ("/workspaceRoot", Value::from("/app/../etc")),
        ("/workspaceRoot", Value::from("/app//source")),
        ("/workspaceRoot", Value::from("/app/")),
        ("/workspaceRoot", Value::from("/app/./source")),
        ("/workspaceRoot", Value::from("/app\\source")),
        ("/workspaceRoot", Value::from("/app\nsource")),
        ("/workspaceRoot", Value::from("/app\u{7f}source")),
        ("/workspaceRoot", Value::from("/app\u{85}source")),
        ("/workspaceRoot", Value::from("/cafe\u{301}")),
        ("/workspaceRoot", Value::from("/run/centaeris")),
        ("/workspaceRoot", Value::from("/run/centaeris/control")),
        ("/user", Value::from("root")),
        ("/user", Value::from("01:0")),
        ("/user", Value::from("4294967296:0")),
        ("/user", Value::from("4294967295:0")),
        ("/network", Value::from("host")),
        ("/resources/storageBytes", Value::from(0)),
        ("/resources/memoryBytes", Value::from(0)),
    ] {
        let mut payload = benchmark_wire();
        *payload["benchmarkProfile"].pointer_mut(pointer).unwrap() = bad;
        refresh_profile_digest(&mut payload);
        let result = serde_json::from_value::<WorkspaceAgentRunAuthorization>(payload);
        assert!(
            result.is_err() || result.unwrap().validate().is_err(),
            "invalid {pointer}"
        );
    }
    let mut unknown = benchmark_wire();
    unknown["benchmarkProfile"]["hostPath"] = "/data".into();
    assert!(serde_json::from_value::<WorkspaceAgentRunAuthorization>(unknown).is_err());
}

#[test]
fn benchmark_profile_preserves_explicit_original_roots_users_and_network_choice() {
    for (root, user, network) in [
        ("/", "0:0", "disabled"),
        ("/app", "1000:1000", "publicInternet"),
        ("/caf\u{e9}", "1000:1000", "disabled"),
    ] {
        let mut payload = benchmark_wire();
        payload["benchmarkProfile"]["workspaceRoot"] = root.into();
        payload["benchmarkProfile"]["user"] = user.into();
        payload["benchmarkProfile"]["network"] = network.into();
        refresh_profile_digest(&mut payload);
        let authorization = parse_valid(payload);
        let roundtrip = serde_json::to_value(authorization).unwrap();
        assert_eq!(roundtrip["benchmarkProfile"]["workspaceRoot"], root);
        assert_eq!(roundtrip["benchmarkProfile"]["user"], user);
        assert_eq!(roundtrip["benchmarkProfile"]["network"], network);
    }
}
