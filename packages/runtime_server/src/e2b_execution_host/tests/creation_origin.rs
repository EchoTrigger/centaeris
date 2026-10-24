use super::*;

fn maintenance_profile() -> EnvironmentProfile {
    let mut value = profile();
    let EnvironmentProfile::E2b(selected) = &mut value else {
        unreachable!()
    };
    selected.lifetime = "retained".into();
    value
}

fn maintenance_binding() -> E2bSandboxBinding {
    let mut value = binding();
    value.metadata.insert(
        "profileDigest".into(),
        maintenance_profile().digest().unwrap(),
    );
    value.metadata.remove("agentRunId");
    value.metadata.remove("processingJobId");
    value
        .metadata
        .insert("creationOwnerKind".into(), "maintenanceOperation".into());
    value
        .metadata
        .insert("maintenanceOperationId".into(), "provision-original".into());
    value
}

#[test]
fn maintenance_created_instance_keeps_its_original_provenance_when_a_run_receives_the_host() {
    let original = maintenance_binding();
    let runner = E2bExecutionHostRunner::new(
        maintenance_profile(),
        original.clone(),
        Arc::new(FixtureTransport::new()),
        Arc::new(FixtureAuthority::new()),
    )
    .expect(
        "a real maintenance-created instance must be consumable without inventing a creator Run",
    );
    assert_eq!(runner.binding(), &original);
}

#[test]
fn mixed_or_unknown_creation_provenance_cannot_construct_an_execution_host() {
    for (key, value) in [
        ("agentRunId", "invented-run"),
        ("processingJobId", "invented-job"),
        ("creationOwnerKind", "agentRun"),
        ("creationOwnerKind", "unknown"),
        ("maintenanceOperationId", ""),
        ("maintenanceOperationId", "invalid operation"),
        ("maintenanceOperationId", "invalid\noperation"),
    ] {
        let mut invalid = maintenance_binding();
        invalid.metadata.insert(key.into(), value.into());
        assert!(
            E2bExecutionHostRunner::new(
                maintenance_profile(),
                invalid,
                Arc::new(FixtureTransport::new()),
                Arc::new(FixtureAuthority::new()),
            )
            .is_err(),
            "invalid creation provenance {key} must be rejected"
        );
    }
}

#[test]
fn maintenance_creation_cannot_silently_turn_per_run_profiles_into_retained_environments() {
    let mut invalid = maintenance_binding();
    invalid
        .metadata
        .insert("profileDigest".into(), profile().digest().unwrap());
    assert!(E2bExecutionHostRunner::new(
        profile(),
        invalid,
        Arc::new(FixtureTransport::new()),
        Arc::new(FixtureAuthority::new()),
    )
    .is_err());
}
