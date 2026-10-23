use std::collections::HashSet;

use centaeris_core::runtime::contracts::RuntimeRecoveryCheckpointV1;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContractCase {
    name: String,
    serialized: String,
    expected_valid: bool,
}

#[test]
fn recovery_checkpoint_shared_corpus_matches_core_wire_contract() {
    let cases: Vec<ContractCase> =
        serde_json::from_str(include_str!("fixtures/recovery_checkpoints.json"))
            .expect("shared checkpoint corpus is valid JSON");
    assert!(
        !cases.is_empty(),
        "shared checkpoint corpus must not be empty"
    );
    let mut names = HashSet::new();
    for case in cases {
        assert!(!case.name.is_empty() && names.insert(case.name.clone()));
        let parsed = serde_json::from_str::<RuntimeRecoveryCheckpointV1>(&case.serialized)
            .map_err(|error| error.to_string())
            .and_then(|checkpoint| checkpoint.validate().map(|()| checkpoint));
        assert_eq!(parsed.is_ok(), case.expected_valid, "case {}", case.name);
        if let Ok(checkpoint) = parsed {
            let mut expected: Value =
                serde_json::from_str(&case.serialized).expect("valid checkpoint source is JSON");
            // Serde accepts the missing Option on input. Persisted serialization
            // always makes this field explicit, including an empty snapshot.
            expected["workspaceSnapshot"]
                .as_object_mut()
                .expect("snapshot is an object")
                .entry("objectRef")
                .or_insert(Value::Null);
            let canonical = serde_json::to_value(checkpoint).expect("valid checkpoint serializes");
            assert_eq!(canonical, expected, "canonical shape drift: {}", case.name);
        }
    }
}
