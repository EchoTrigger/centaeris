use super::*;
use agent_run_authorization::{EnvironmentProfile, SessionWorkspace};

#[test]
fn managed_empty_workspace_alone_can_defer_physical_creation() {
    let managed: EnvironmentProfile = serde_json::from_str(include_str!(
        "../../../tests/workspace/fixtures/e2b_environment_profile/v1/managed-files.json"
    ))
    .unwrap();
    let mut old = managed.clone();
    let EnvironmentProfile::E2b(profile) = &mut old else {
        unreachable!()
    };
    profile.workspace_initialization = None;
    let empty = SessionWorkspace {
        generation: 0,
        snapshot_sha256: String::new(),
        snapshot_size_bytes: 0,
        expanded_size_bytes: 0,
        file_count: 0,
    };
    assert!(e2b_lazy_workspace_eligible(
        &managed, &empty, false, false, false
    ));
    assert!(!e2b_lazy_workspace_eligible(
        &old, &empty, false, false, false
    ));
    for (inputs, live, attempt) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        assert!(!e2b_lazy_workspace_eligible(
            &managed, &empty, inputs, live, attempt
        ));
    }
    let mut retained = managed.clone();
    let EnvironmentProfile::E2b(profile) = &mut retained else {
        unreachable!()
    };
    profile.lifetime = "retained".into();
    assert!(!e2b_lazy_workspace_eligible(
        &retained, &empty, false, false, false
    ));
    for field in ["hash", "size", "expanded", "count"] {
        let mut nonempty = empty.clone();
        match field {
            "hash" => nonempty.snapshot_sha256 = format!("sha256:{}", "a".repeat(64)),
            "size" => nonempty.snapshot_size_bytes = 1,
            "expanded" => nonempty.expanded_size_bytes = 1,
            "count" => nonempty.file_count = 1,
            _ => unreachable!(),
        }
        assert!(
            !e2b_lazy_workspace_eligible(&managed, &nonempty, false, false, false),
            "{field}"
        );
    }
    let mut later_empty = empty;
    later_empty.generation = 8;
    assert!(e2b_lazy_workspace_eligible(
        &managed,
        &later_empty,
        false,
        false,
        false
    ));
}
