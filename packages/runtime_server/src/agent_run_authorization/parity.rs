use super::*;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Case {
    id: String,
    changes: Vec<Value>,
    signer_error: String,
    verifier_stage: String,
    verifier_error: String,
    rationale: String,
    supported_signer_output: bool,
    digest: Option<String>,
    signature: Option<String>,
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/agent_run_authorization/v1")
}

fn cases() -> Vec<Case> {
    let cases: Vec<Case> = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("cases.json")).expect("case file"),
    )
    .expect("strict case corpus");
    assert!(!cases.is_empty(), "empty corpus");
    let mut ids = HashSet::new();
    for case in &cases {
        assert!(!case.id.is_empty() && ids.insert(&case.id));
        assert!(!case.rationale.is_empty());
        assert!(matches!(
            case.verifier_stage.as_str(),
            "accept" | "deserialize" | "validate"
        ));
        assert_eq!(
            case.verifier_error.is_empty(),
            case.verifier_stage == "accept"
        );
        assert_eq!(case.digest.is_some(), case.signer_error.is_empty());
        assert_eq!(case.signature.is_some(), case.signer_error.is_empty());
        if case.supported_signer_output {
            assert!(case.signer_error.is_empty() && case.verifier_stage == "accept");
        }
    }
    cases
}

fn payload(case: &Case) -> Value {
    let mut payload: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("valid.json")).expect("fixture file"),
    )
    .expect("fixture JSON");
    for change in &case.changes {
        let fields = change.as_object().expect("change object");
        assert!(fields.contains_key("path") && fields.keys().all(|k| k == "path" || k == "value"));
        let path = fields["path"].as_str().expect("change path");
        assert!(path.starts_with('/'));
        let (parent, key) = path.rsplit_once('/').expect("field pointer");
        let target = payload.pointer_mut(parent).expect("existing parent");
        if let Some(value) = fields.get("value") {
            match target {
                Value::Object(fields) => {
                    fields.insert(key.to_string(), value.clone());
                }
                Value::Array(items) => {
                    items[key.parse::<usize>().expect("array index")] = value.clone()
                }
                _ => panic!("change parent must be a collection"),
            }
        } else {
            assert!(target
                .as_object_mut()
                .expect("remove object field")
                .remove(key)
                .is_some());
        }
    }
    payload
}

#[test]
fn shared_authorization_corpus() {
    for case in cases() {
        let payload = payload(&case);
        let wire = serde_json::to_string(&payload).expect("wire JSON");
        let parsed = serde_json::from_str::<WorkspaceAgentRunAuthorization>(&wire);
        if case.verifier_stage == "deserialize" {
            let error = parsed.expect_err(&case.id).to_string();
            assert!(error.contains(&case.verifier_error), "{}: {error}", case.id);
            continue;
        }
        let authorization = parsed.unwrap_or_else(|e| panic!("{}: {e}", case.id));
        // Authenticate malformed test payloads without calling the production signer,
        // which correctly refuses them. A bad HMAC must not explain a validation rejection.
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(
                canonical_json(payload)
                    .expect("canonical test payload")
                    .as_bytes()
            )
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-key").expect("test key");
        mac.update(AGENT_RUN_AUTHORIZATION_SIGNATURE_DOMAIN.as_bytes());
        mac.update(digest.as_bytes());
        let wire_signature = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
        if case.verifier_stage == "validate" {
            let error = authorization.validate().expect_err(&case.id);
            assert!(error.contains(&case.verifier_error), "{}: {error}", case.id);
            let error = authorization
                .verify_signature(b"test-key", &wire_signature)
                .expect_err(&case.id);
            assert!(error.contains(&case.verifier_error), "{}: {error}", case.id);
            continue;
        }
        authorization
            .validate()
            .unwrap_or_else(|e| panic!("{}: {e}", case.id));
        authorization
            .verify_signature(b"test-key", &wire_signature)
            .expect(&case.id);
        if let Some(expected) = case.digest {
            assert_eq!(
                authorization.digest().expect(&case.id),
                expected,
                "{}",
                case.id
            );
            let signature = case.signature.expect("accepted signature");
            assert_eq!(
                authorization.signature(b"test-key").expect(&case.id),
                signature,
                "{}",
                case.id
            );
            authorization
                .verify_signature(b"test-key", &signature)
                .expect(&case.id);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PythonVector {
    id: String,
    payload_json: String,
    digest: String,
    signature: String,
}

#[test]
fn python_signed_authorizations_reach_rust() {
    let Some(path) = std::env::var_os("AGENT_RUN_AUTHORIZATION_VECTORS") else {
        // The standalone gate requires an artifact; ordinary cargo tests use the fixed corpus.
        return;
    };
    let vectors: Vec<PythonVector> = serde_json::from_str(
        &std::fs::read_to_string(path).expect("required Python vector artifact"),
    )
    .expect("Python vectors");
    let expected: HashSet<_> = cases()
        .into_iter()
        .filter(|c| c.signer_error.is_empty())
        .map(|c| c.id)
        .collect();
    assert!(!vectors.is_empty());
    let mut seen = HashSet::new();
    for vector in vectors {
        assert!(expected.contains(&vector.id) && seen.insert(vector.id.clone()));
        let authorization: WorkspaceAgentRunAuthorization =
            serde_json::from_str(&vector.payload_json).expect(&vector.id);
        assert_eq!(
            authorization.digest().expect(&vector.id),
            vector.digest,
            "{}",
            vector.id
        );
        authorization
            .verify_signature(b"test-key", &vector.signature)
            .expect(&vector.id);
    }
    assert_eq!(seen, expected, "every Python-accepted case must reach Rust");
    if let Some(receipt) = std::env::var_os("AGENT_RUN_AUTHORIZATION_RECEIPT") {
        std::fs::write(receipt, seen.len().to_string()).expect("gate receipt");
    }
}

#[test]
fn authentication_and_agent_run_binding_are_independent_checks() {
    let mut authorization = super::tests::authorization();
    let signature = authorization.signature(b"test-key").expect("signature");
    assert!(authorization
        .verify_signature(b"wrong-key", &signature)
        .unwrap_err()
        .contains("signature mismatch"));
    authorization.session_id = "different_session".into();
    assert!(authorization
        .verify_signature(b"test-key", &signature)
        .unwrap_err()
        .contains("signature mismatch"));
    authorization
        .validate_agent_run_binding("agent_run_1")
        .expect("correct binding");
    assert!(authorization
        .validate_agent_run_binding("another_run")
        .unwrap_err()
        .contains("agentRunId mismatch"));
    let mut mac = Hmac::<Sha256>::new_from_slice(b"test-key").expect("key");
    mac.update(b"another-protocol\0");
    mac.update(authorization.digest().expect("digest").as_bytes());
    let wrong_domain = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
    assert!(authorization
        .verify_signature(b"test-key", &wrong_domain)
        .unwrap_err()
        .contains("signature mismatch"));
}

#[test]
fn resource_u64_overflow_is_rejected_on_the_wire() {
    for field in ["memoryBytes", "dataTmpfsBytes"] {
        let mut value = serde_json::to_value(super::tests::authorization()).expect("fixture");
        // Preserve the out-of-range integer as JSON text, without a floating-point conversion.
        value["resources"][field] = Value::String("18446744073709551616".into());
        let wire = serde_json::to_string(&value)
            .expect("wire")
            .replace("\"18446744073709551616\"", "18446744073709551616");
        let error = serde_json::from_str::<WorkspaceAgentRunAuthorization>(&wire)
            .expect_err(field)
            .to_string();
        assert!(error.contains("expected u64"), "{field}: {error}");
    }
}
