use super::*;

fn wire(benchmark: bool) -> Value {
    serde_json::from_str(if benchmark {
        include_str!(
            "../../../../tests/workspace/fixtures/agent_run_authorization/v3/benchmark.json"
        )
    } else {
        include_str!(
            "../../../../tests/workspace/fixtures/agent_run_authorization/v3/ordinary.json"
        )
    })
    .unwrap()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Expected {
    digest: String,
    signature: String,
}

#[test]
fn python_network_authorization_fixtures_verify_with_the_v3_domain() {
    for benchmark in [false, true] {
        let payload = wire(benchmark);
        let authorization: WorkspaceAgentRunAuthorization =
            serde_json::from_value(payload.clone()).expect("supported v3 wire");
        authorization.validate().expect("valid v3 authorization");
        let expected: Expected = serde_json::from_str(if benchmark {
            include_str!("../../../../tests/workspace/fixtures/agent_run_authorization/v3/benchmark-expected.json")
        } else {
            include_str!("../../../../tests/workspace/fixtures/agent_run_authorization/v3/ordinary-expected.json")
        }).unwrap();
        assert_eq!(serde_json::to_value(&authorization).unwrap(), payload);
        assert_eq!(authorization.digest().unwrap(), expected.digest);
        assert_eq!(
            authorization.signature(b"test-key").unwrap(),
            expected.signature
        );
        authorization
            .checked_binding(&authorization.agent_run_id)
            .unwrap()
            .verify(&expected.digest, b"test-key", &expected.signature)
            .unwrap();
    }
}

fn rejected(payload: Value) {
    if let Ok(parsed) = serde_json::from_value::<WorkspaceAgentRunAuthorization>(payload) {
        assert!(parsed.validate().is_err());
    }
}

#[test]
fn network_authorization_requires_exact_network_and_profile_fields() {
    for benchmark in [false, true] {
        let mut missing = wire(benchmark);
        missing.as_object_mut().unwrap().remove("network");
        rejected(missing);
        for value in [
            Value::Null,
            "host".into(),
            "public_internet".into(),
            true.into(),
        ] {
            let mut invalid = wire(benchmark);
            invalid["network"] = value;
            rejected(invalid);
        }
        let mut unknown = wire(benchmark);
        unknown["networkMode"] = "bridge".into();
        rejected(unknown);
    }
    for field in ["benchmarkProfile", "benchmarkProfileDigest"] {
        let mut missing = wire(true);
        missing.as_object_mut().unwrap().remove(field);
        rejected(missing);
        let mut ordinary = wire(false);
        ordinary[field] = wire(true)[field].clone();
        rejected(ordinary);
    }
}

#[test]
fn network_authorization_authenticates_network_and_profile_equality() {
    let original: WorkspaceAgentRunAuthorization = serde_json::from_value(wire(false)).unwrap();
    let signature = original.signature(b"test-key").unwrap();
    let mut changed = wire(false);
    changed["network"] = "disabled".into();
    let changed: WorkspaceAgentRunAuthorization = serde_json::from_value(changed).unwrap();
    changed.validate().unwrap();
    assert!(changed.verify_signature(b"test-key", &signature).is_err());

    let mut mismatch = wire(true);
    mismatch["network"] = "disabled".into();
    rejected(mismatch);
    let mut mismatch = wire(true);
    mismatch["benchmarkProfile"]["network"] = "disabled".into();
    mismatch["benchmarkProfileDigest"] = format!(
        "sha256:{:x}",
        Sha256::digest(
            canonical_json(mismatch["benchmarkProfile"].clone())
                .unwrap()
                .as_bytes()
        )
    )
    .into();
    rejected(mismatch);
}

#[test]
fn historical_authorizations_cannot_accept_an_unsigned_network_field() {
    for payload in [
        serde_json::to_value(tests::authorization()).unwrap(),
        serde_json::from_str(include_str!(
            "../../../../tests/workspace/fixtures/agent_run_authorization/v2/valid.json"
        ))
        .unwrap(),
    ] {
        let original: WorkspaceAgentRunAuthorization =
            serde_json::from_value(payload.clone()).unwrap();
        original.validate().unwrap();
        assert!(serde_json::to_value(original)
            .unwrap()
            .get("network")
            .is_none());
        for network in ["disabled", "publicInternet"] {
            let mut changed = payload.clone();
            changed["network"] = network.into();
            rejected(changed);
        }
    }
}

#[test]
fn historical_network_reconstruction_preserves_original_signed_payloads() {
    for (payload, network) in [
        (
            serde_json::to_value(tests::authorization()).unwrap(),
            ExecutionNetwork::Disabled,
        ),
        (
            serde_json::from_str(include_str!(
                "../../../../tests/workspace/fixtures/agent_run_authorization/v2/valid.json"
            ))
            .unwrap(),
            ExecutionNetwork::PublicInternet,
        ),
    ] {
        let authorization: WorkspaceAgentRunAuthorization =
            serde_json::from_value(payload.clone()).unwrap();
        authorization.validate().unwrap();
        let digest = authorization.digest().unwrap();
        let signature = authorization.signature(b"test-key").unwrap();
        assert_eq!(authorization.effective_network().unwrap(), network);
        assert_eq!(serde_json::to_value(&authorization).unwrap(), payload);
        assert_eq!(authorization.digest().unwrap(), digest);
        assert_eq!(authorization.signature(b"test-key").unwrap(), signature);
    }
}
